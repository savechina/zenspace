//! Graph structural-integrity verification (005-agentic-loop, T014, FR-015)
//! and placeholder-based graph ingest registry (T032, FR-031).
//!
//! Four mechanical checks after each processing cycle (data-model §6):
//! 1. every wiki concept page has a DB entity → [`GapKind::WikiPageWithoutEntities`]
//! 2. no orphan entities (no page, no relationships) → [`GapKind::OrphanEntity`]
//! 3. all relationships resolve to existing entities, canonical names via the
//!    existing `notion_aliases` table (I1) → [`GapKind::UnresolvedRelationship`]
//! 4. no two distinct canonical entities resolve to the same normalized alias
//!    (left-joins `notion_aliases` via `normalize_alias`) → [`GapKind::DuplicateEntityAlias`]
//!
//! The [`PlaceholderRegistry`] implements FR-031 concurrency control: target
//! page slugs declared during Agent planning as `GraphPlaceholder` slots, with
//! concurrent ingest tasks downgraded to updates when the slot is owned by
//! another agent — avoiding LLM task stalls from lock contention.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::distill::types::{GapKind, GapRecord, GraphPlaceholder, PlaceholderStatus};

/// Wiki concept page → DB entity consistency verifier.
///
/// All queries go through `NotionsRepo` (Principle XII); page inventory is
/// the caller-supplied `(concept_name, path)` list (vault-relative names).
pub struct GraphIntegrityVerifier<'a> {
    repo: zen_repo::NotionsRepo<'a>,
    cycle_id: String,
}

impl<'a> GraphIntegrityVerifier<'a> {
    /// Create a verifier backed by the given DB client.
    pub fn new(client: &'a zen_repo::SqliteClient, cycle_id: &str) -> Self {
        Self {
            repo: zen_repo::NotionsRepo::new(client),
            cycle_id: cycle_id.to_string(),
        }
    }

    /// Run all three checks. `pages` = (concept name, vault-relative path)
    /// for every wiki page; relationship resolution canonicalizes through
    /// `notion_aliases` with `LEFT JOIN`-style fallback to identity (F1).
    pub async fn verify(&self, pages: &[(String, String)]) -> Result<Vec<GapRecord>> {
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

        // ── Check 4: alias collision — two distinct canonical entities ──
        // sharing the same normalized name or alias (T039, FR-022).
        // Left-joins `notion_aliases` via `normalize_alias` on both entity
        // names and each entity's stored aliases.
        let mut normalized_map: HashMap<String, Vec<String>> = HashMap::new();
        for entity in &entities {
            let norm = zen_repo::normalize_alias(&entity.name);
            normalized_map
                .entry(norm)
                .or_default()
                .push(entity.id.clone());
        }
        // Also check aliases: an alias of entity A that normalizes to
        // entity B's name means both resolve to the same normalized form.
        for entity in &entities {
            if let Ok(aliases) = self.repo.load_aliases_for_entity(&entity.id).await {
                for alias in aliases {
                    let norm = zen_repo::normalize_alias(&alias);
                    let entry = normalized_map.entry(norm).or_default();
                    if !entry.contains(&entity.id) {
                        entry.push(entity.id.clone());
                    }
                }
            }
        }
        for (norm, entity_ids_in_group) in &normalized_map {
            if entity_ids_in_group.len() < 2 {
                continue;
            }
            let names: Vec<String> = entity_ids_in_group
                .iter()
                .filter_map(|id| {
                    entities
                        .iter()
                        .find(|e| &e.id == id)
                        .map(|e| e.name.clone())
                })
                .collect();
            gaps.push(
                GapRecord::new(
                    GapKind::DuplicateEntityAlias,
                    &self.cycle_id,
                    format!("alias collision: entities {names:?} all normalize to `{norm}`"),
                )
                .with_entity(names.first().cloned().unwrap_or_default()),
            );
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

// ── Placeholder-based graph ingest (FR-031, T032a) ─────────────────────

/// Outcome of a concurrent claim against a declared placeholder slot (FR-031).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimDecision {
    /// The caller owns the slot and may create the target page.
    Created,
    /// The slot is owned by another agent (or already merged into the wiki)
    /// — the caller must downgrade its create into an update to avoid LLM
    /// task stalls from lock contention.
    DowngradeToUpdate,
}

/// Placeholder-based graph ingest registry (FR-031, T032a).
///
/// During Agent planning, target page names are declared as placeholder
/// slugs ([`GraphPlaceholder`]). Concurrent ingest tasks claiming an
/// already-owned slug receive [`ClaimDecision::DowngradeToUpdate`] and become
/// updates instead of competing creates.
///
/// Persistence: [`save`](Self::save)/[`load`](Self::load) a JSON file (the
/// caller picks the path, typically `<logs>/placeholders.json`) so the
/// registry survives process restarts within a cycle window.
///
/// # Examples
///
/// ```
/// use zen_vault::graph_verify::{ClaimDecision, PlaceholderRegistry};
///
/// let mut reg = PlaceholderRegistry::new();
/// reg.declare("rust-async", "agent-a");
/// assert_eq!(reg.claim("rust-async", "agent-b"), ClaimDecision::DowngradeToUpdate);
/// assert_eq!(reg.claim("rust-async", "agent-a"), ClaimDecision::Created);
/// ```
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PlaceholderRegistry {
    slots: HashMap<String, GraphPlaceholder>,
}

impl PlaceholderRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Borrow the slot map (crate-internal: merge-save in
    /// [`crate::distill::placeholders_store`], T180).
    pub(crate) fn slots(&self) -> &HashMap<String, GraphPlaceholder> {
        &self.slots
    }

    /// Build a registry from a pre-merged slot map (crate-internal:
    /// merge-save in [`crate::distill::placeholders_store`], T180).
    pub(crate) fn from_slots(slots: HashMap<String, GraphPlaceholder>) -> Self {
        Self { slots }
    }

    /// Declare a target page slug during planning. Idempotent: the earliest
    /// declaration wins and a re-declare by any agent is a no-op, so the
    /// slot keeps its original owner.
    pub fn declare(&mut self, slug: &str, claimed_by: &str) {
        self.slots
            .entry(slug.to_string())
            .or_insert_with(|| GraphPlaceholder {
                slug: slug.to_string(),
                status: PlaceholderStatus::Placeholder,
                claimed_by: Some(claimed_by.to_string()),
            });
    }

    /// Claim a slug for an ingest task.
    ///
    /// Returns [`ClaimDecision::Created`] when the slot is free or already
    /// owned by `claimed_by` (ownership is taken/refreshed, status advances
    /// to `Claimed`). Returns [`ClaimDecision::DowngradeToUpdate`] when the
    /// slot is `Placeholder`/`Claimed` by a different owner, or `Merged`
    /// (any owner — the page already exists, so creates become updates).
    pub fn claim(&mut self, slug: &str, claimed_by: &str) -> ClaimDecision {
        let free = match self.slots.get(slug) {
            None => true,
            Some(slot) => {
                slot.status != PlaceholderStatus::Merged
                    && slot
                        .claimed_by
                        .as_deref()
                        .is_none_or(|owner| owner == claimed_by)
            }
        };
        if free {
            self.slots.insert(
                slug.to_string(),
                GraphPlaceholder {
                    slug: slug.to_string(),
                    status: PlaceholderStatus::Claimed,
                    claimed_by: Some(claimed_by.to_string()),
                },
            );
            ClaimDecision::Created
        } else {
            ClaimDecision::DowngradeToUpdate
        }
    }

    /// Mark a slot as merged into the main wiki graph. Merging an unknown
    /// slug records a `Merged` slot so post-hoc lookups still resolve.
    pub fn merge(&mut self, slug: &str) {
        match self.slots.get_mut(slug) {
            Some(slot) => slot.status = PlaceholderStatus::Merged,
            None => {
                self.slots.insert(
                    slug.to_string(),
                    GraphPlaceholder {
                        slug: slug.to_string(),
                        status: PlaceholderStatus::Merged,
                        claimed_by: None,
                    },
                );
            }
        }
    }

    /// Look up the placeholder record for `slug`, if declared.
    pub fn lookup(&self, slug: &str) -> Option<&GraphPlaceholder> {
        self.slots.get(slug)
    }

    /// Owner-less pre-flight check for task planners: true when `slug` is
    /// reserved in any state (Placeholder, Claimed, or Merged), meaning a
    /// create targeting it must downgrade to an update. Agents that already
    /// hold the slot use [`Self::claim`] instead.
    pub fn downgrade_to_update(&self, slug: &str) -> bool {
        self.slots.contains_key(slug)
    }

    /// Persist the registry as pretty JSON at `path`. Parent directories are
    /// the caller's responsibility (typically the logs dir).
    pub fn save(&self, path: &Path) -> Result<()> {
        let json =
            serde_json::to_string_pretty(self).context("serializing placeholder registry")?;
        zen_core::atomic_file::write_atomic(path, json.as_bytes())
            .with_context(|| format!("writing placeholder registry {}", path.display()))
    }

    /// Load a registry from `path`. A missing file yields an empty registry
    /// (first run within a cycle window); a corrupt file is an error so
    /// concurrency state is never silently discarded.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(json) => serde_json::from_str(&json)
                .with_context(|| format!("parsing placeholder registry {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => {
                Err(e).with_context(|| format!("reading placeholder registry {}", path.display()))
            }
        }
    }
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

    #[test]
    fn placeholder_declare_claim_conflict_downgrades() {
        let mut reg = PlaceholderRegistry::new();
        reg.declare("rust-async", "agent-a");
        assert_eq!(
            reg.lookup("rust-async").unwrap().status,
            PlaceholderStatus::Placeholder
        );

        // Concurrent task by another agent downgrades instead of competing.
        assert_eq!(
            reg.claim("rust-async", "agent-b"),
            ClaimDecision::DowngradeToUpdate
        );
        assert_eq!(
            reg.lookup("rust-async").unwrap().claimed_by.as_deref(),
            Some("agent-a")
        );

        // Owner re-claim advances the slot to Claimed.
        assert_eq!(reg.claim("rust-async", "agent-a"), ClaimDecision::Created);
        assert_eq!(
            reg.lookup("rust-async").unwrap().status,
            PlaceholderStatus::Claimed
        );

        // Re-declare after claiming is a no-op (earliest declaration wins).
        reg.declare("rust-async", "agent-z");
        assert_eq!(
            reg.lookup("rust-async").unwrap().claimed_by.as_deref(),
            Some("agent-a")
        );

        // Undeclared slug → fresh create.
        assert_eq!(reg.claim("fresh-page", "agent-b"), ClaimDecision::Created);
        assert!(reg.downgrade_to_update("fresh-page"));
        assert!(!reg.downgrade_to_update("never-declared"));
    }

    #[test]
    fn placeholder_merge_state() {
        let mut reg = PlaceholderRegistry::new();
        reg.declare("tokio-runtime", "agent-a");
        reg.merge("tokio-runtime");
        assert_eq!(
            reg.lookup("tokio-runtime").unwrap().status,
            PlaceholderStatus::Merged
        );

        // Merged slot: every later claim downgrades (the page exists).
        assert_eq!(
            reg.claim("tokio-runtime", "agent-a"),
            ClaimDecision::DowngradeToUpdate
        );
        assert_eq!(
            reg.claim("tokio-runtime", "agent-c"),
            ClaimDecision::DowngradeToUpdate
        );

        // Merging an unknown slug records a Merged slot for later lookups.
        reg.merge("late-page");
        assert_eq!(
            reg.lookup("late-page").unwrap().status,
            PlaceholderStatus::Merged
        );
    }

    #[test]
    fn placeholder_save_load_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("placeholders.json");

        let mut reg = PlaceholderRegistry::new();
        reg.declare("rust-async", "agent-a");
        reg.claim("rust-async", "agent-a");
        reg.declare("stale-note", "agent-b");
        reg.merge("merged-page");
        reg.save(&path).unwrap();

        let loaded = PlaceholderRegistry::load(&path).unwrap();
        assert_eq!(
            loaded.lookup("rust-async").unwrap().status,
            PlaceholderStatus::Claimed
        );
        assert_eq!(
            loaded.lookup("rust-async").unwrap().claimed_by.as_deref(),
            Some("agent-a")
        );
        assert_eq!(
            loaded.lookup("stale-note").unwrap().claimed_by.as_deref(),
            Some("agent-b")
        );
        assert_eq!(
            loaded.lookup("merged-page").unwrap().status,
            PlaceholderStatus::Merged
        );

        // Missing file → empty registry (first run in a cycle window).
        let fresh = PlaceholderRegistry::load(&tmp.path().join("absent.json")).unwrap();
        assert!(fresh.lookup("rust-async").is_none());

        // Corrupt file → hard error (concurrency state never silently dropped).
        let corrupt = tmp.path().join("corrupt.json");
        std::fs::write(&corrupt, "{ not json").unwrap();
        assert!(PlaceholderRegistry::load(&corrupt).is_err());
    }

    #[tokio::test]
    async fn alias_collision_emits_duplicate_entity_alias_gap() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("test.db");
        let client = zen_repo::SqliteClient::open(&db).await.unwrap();
        let repo = zen_repo::NotionsRepo::new(&client);

        repo.insert_entity("e1", "Rust", "Concept", "2026-01-01")
            .await
            .unwrap();
        repo.insert_entity("e2", "rust-lang", "Concept", "2026-01-01")
            .await
            .unwrap();

        let verifier = GraphIntegrityVerifier::new(&client, "cycle-collision-test");
        let gaps = verifier.verify(&[]).await.unwrap();

        let collision_gaps: Vec<_> = gaps
            .iter()
            .filter(|g| g.kind == GapKind::DuplicateEntityAlias)
            .collect();
        assert_eq!(
            collision_gaps.len(),
            1,
            "expected exactly one DuplicateEntityAlias gap, got {}",
            collision_gaps.len()
        );
        assert!(
            collision_gaps[0].detail.contains("Rust"),
            "gap detail should mention the colliding entity names"
        );
    }

    #[tokio::test]
    async fn no_collision_when_names_differ() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("test.db");
        let client = zen_repo::SqliteClient::open(&db).await.unwrap();
        let repo = zen_repo::NotionsRepo::new(&client);

        repo.insert_entity("e1", "Rust", "Concept", "2026-01-01")
            .await
            .unwrap();
        repo.insert_entity("e2", "Python", "Concept", "2026-01-01")
            .await
            .unwrap();

        let verifier = GraphIntegrityVerifier::new(&client, "cycle-no-collision");
        let gaps = verifier.verify(&[]).await.unwrap();

        let collision_gaps: Vec<_> = gaps
            .iter()
            .filter(|g| g.kind == GapKind::DuplicateEntityAlias)
            .collect();
        assert!(
            collision_gaps.is_empty(),
            "no collision expected for distinct names"
        );
    }
}
