//! Graph structural-integrity verification (005-agentic-loop, T014, FR-015).
//!
//! Three mechanical checks after each processing cycle (data-model §6):
//! 1. every wiki concept page has a DB entity → [`GapKind::WikiPageWithoutEntities`]
//! 2. no orphan entities (no page, no relationships) → [`GapKind::OrphanEntity`]
//! 3. all relationships resolve to existing entities, canonical names via the
//!    existing `notion_aliases` table (I1) → [`GapKind::UnresolvedRelationship`]

use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;

use crate::distill::types::{GapKind, GapRecord};

/// Wiki concept page → DB entity consistency verifier.
///
/// All queries go through `NotionsRepo` (Principle XII); page inventory is
/// the caller-supplied `(concept_name, path)` list (vault-relative names).
pub struct GraphIntegrityVerifier<'a> {
    repo: zen_repo::NotionsRepo<'a>,
    cycle_id: String,
}

impl<'a> GraphIntegrityVerifier<'a> {
    pub fn new(client: &'a zen_repo::SqliteClient, cycle_id: &str) -> Self {
        Self {
            repo: zen_repo::NotionsRepo::new(client),
            cycle_id: cycle_id.to_string(),
        }
    }

    /// Run all three checks. `pages` = (concept name, vault-relative path)
    /// for every wiki page; relationship resolution canonicalizes through
    /// `notion_aliases` with `LEFT JOIN`-style fallback to identity (F1).
    pub async fn verify(
        &self,
        pages: &[(String, String)],
    ) -> Result<Vec<GapRecord>> {
        let mut gaps = Vec::new();

        let entities = self.repo.load_all_entities().await?;
        let entity_ids: HashSet<&str> = entities.iter().map(|e| e.id.as_str()).collect();
        let entity_names: HashSet<String> = entities
            .iter()
            .map(|e| zen_repo::normalize_alias(&e.name))
            .collect();

        // ── Check 1: wiki concept page without DB entity (canonical via aliases) ──
        for (name, path) in pages {
            let canonical = self.canonical_name(name).await;
            let found = self.repo.find_entity_by_name(&canonical).await?.is_some()
                || entity_names.contains(&zen_repo::normalize_alias(name));
            if !found {
                gaps.push(
                    GapRecord::new(
                        GapKind::WikiPageWithoutEntities,
                        &self.cycle_id,
                        format!("concept page `{name}` has no DB entity"),
                    )
                    .with_path(path.clone()),
                );
            }
        }

        // ── Check 2: orphan entities (no page AND no relationships) ──
        let page_names: HashSet<String> = pages
            .iter()
            .map(|(n, _)| zen_repo::normalize_alias(n))
            .collect();
        for entity in &entities {
            let normalized = zen_repo::normalize_alias(&entity.name);
            if page_names.contains(&normalized) {
                continue;
            }
            let rels = self.repo.load_relationships_all(&entity.id).await?;
            if rels.is_empty() {
                gaps.push(
                    GapRecord::new(
                        GapKind::OrphanEntity,
                        &self.cycle_id,
                        format!(
                            "entity `{}` has no wiki page and no relationships",
                            entity.name
                        ),
                    )
                    .with_entity(entity.name.clone()),
                );
            }
        }

        // ── Check 3: relationships resolve to existing entities ──
        for entity in &entities {
            let rels = self.repo.load_relationships_all(&entity.id).await?;
            for rel in rels {
                let source_ok = entity_ids.contains(rel.source_notion_id.as_str());
                let target_ok = entity_ids.contains(rel.target_notion_id.as_str());
                if !source_ok || !target_ok {
                    gaps.push(
                        GapRecord::new(
                            GapKind::UnresolvedRelationship,
                            &self.cycle_id,
                            format!(
                                "relationship {}-{}->{} missing {}",
                                rel.source_notion_id,
                                rel.relation_type,
                                rel.target_notion_id,
                                if !source_ok { "source" } else { "target" }
                            ),
                        )
                        .with_entity(entity.name.clone()),
                    );
                }
            }
        }

        Ok(gaps)
    }

    /// Resolve a page concept name to its canonical entity name through
    /// `notion_aliases` (existing table, I1); falls back to identity.
    async fn canonical_name(&self, name: &str) -> String {
        match self.repo.resolve_alias(name).await {
            Ok(Some(id)) => self
                .repo
                .notion_name(&id)
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| name.to_string()),
            _ => name.to_string(),
        }
    }
}

/// Collect (concept name, vault-relative path) for every wiki page under
/// `wiki_dir` (`.md` files, recursively).
pub fn wiki_page_inventory(wiki_dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![wiki_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                let name = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                let rel = path
                    .strip_prefix(wiki_dir)
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();
                out.push((name, rel));
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_lists_md_files_vault_relative() {
        let tmp = tempfile::tempdir().unwrap();
        let wiki = tmp.path().join("wiki");
        std::fs::create_dir_all(wiki.join("notions")).unwrap();
        std::fs::write(wiki.join("rust.md"), "# Rust").unwrap();
        std::fs::write(wiki.join("notions").join("tokio.md"), "# Tokio").unwrap();
        std::fs::write(wiki.join("skip.txt"), "nope").unwrap();

        let inv = wiki_page_inventory(&wiki);
        let names: Vec<&str> = inv.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"rust"));
        assert!(names.contains(&"tokio"));
        assert_eq!(inv.len(), 2, "non-md excluded");
        let (_, rel) = &inv[0];
        assert!(!rel.starts_with('/'), "vault-relative");
    }
}
