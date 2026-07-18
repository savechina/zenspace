use std::path::Path;
use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;
use rig_compose::context::InvestigationContext;
use rig_compose::registry::{KernelError, ToolRegistry};
use rig_compose::skill::Skill;
use rig_compose::workflow::Workflow;
use tracing::info;

use super::contradiction::ContradictionDetector;
use super::notion_extraction::NotionExtractor;
use super::wiki_compile::WikiCompiler;

use crate::note::{Note, parse_frontmatter};
use crate::wiki::WikiPage;


#[derive(Debug, Clone)]
pub struct DistillationReport {
    pub notes_processed: usize,
    pub entities_extracted: usize,
    pub wiki_pages_created: usize,
    pub contradictions_found: usize,
    /// Notes migrated from inbox to wiki domain directories after distillation.
    pub migrated_files: Vec<(PathBuf, PathBuf)>,
}

/// Scan content for known entity names and wrap them in `[[wikilinks]]`
/// if they appear as plain text and are not already linked.
///
/// Uses `WikiPage::extract_wikilinks()` to identify existing links and
/// avoid double-linking.
pub fn auto_link_wikilinks(content: &str, known_entities: &[String]) -> String {
    let existing_links = WikiPage::extract_wikilinks(content);
    let mut result = content.to_string();

    // Sort entities by length (longest first) to avoid partial matches
    let mut sorted_entities: Vec<&String> = known_entities.iter().collect();
    sorted_entities.sort_by_key(|b| std::cmp::Reverse(b.len()));

    for entity in sorted_entities {
        // Skip if already linked
        if existing_links.iter().any(|l| l == entity) {
            continue;
        }

        // Only wrap if the entity appears as plain text (not inside [[...]] or `...`)
        let entity_lower = entity.to_lowercase();
        let mut new_result = String::with_capacity(result.len());
        let mut i = 0;
        let bytes = result.as_bytes();
        let result_lower = result.to_lowercase();

        while i < bytes.len() {
            // Check if we're inside a wikilink or backtick
            if bytes[i] == b'[' && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                // Skip until ]]
                new_result.push_str(&result[i..i + 2]);
                i += 2;
                while i < bytes.len() - 1 {
                    if bytes[i] == b']' && bytes[i + 1] == b']' {
                        new_result.push_str("]]");
                        i += 2;
                        break;
                    }
                    new_result.push(bytes[i] as char);
                    i += 1;
                }
                continue;
            }
            if bytes[i] == b'`' {
                // Skip until closing backtick
                new_result.push(bytes[i] as char);
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'`' {
                        new_result.push(bytes[i] as char);
                        i += 1;
                        break;
                    }
                    new_result.push(bytes[i] as char);
                    i += 1;
                }
                continue;
            }

            // Check if entity starts at this position
            let remaining_lower = &result_lower[i..];
            if remaining_lower.starts_with(&entity_lower) {
                // Ensure word boundary
                let end = i + entity.len();
                let before_ok = i == 0
                    || !result.as_bytes()[i - 1].is_ascii_alphanumeric();
                let after_ok = end >= result.len()
                    || !result.as_bytes()[end].is_ascii_alphanumeric();

                if before_ok && after_ok {
                    new_result.push_str(&format!("[[{entity}]]"));
                    i = end;
                    continue;
                }
            }

            new_result.push(bytes[i] as char);
            i += 1;
        }

        result = new_result;
    }

    result
}

/// Returns (source, dest) pairs for successfully migrated files.
fn migrate_inbox_to_wiki(notes: &[Note], wiki_dir: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut migrated = Vec::new();

    for note in notes {
        let source = match &note.file_path {
            Some(p) if p.exists() => p.clone(),
            _ => continue,
        };

        let domain_dir = note.domain.first().map(|d| d.to_string()).unwrap_or_else(|| "general".to_string());
        let dest_dir = wiki_dir.join(&domain_dir);

        if let Err(e) = std::fs::create_dir_all(&dest_dir) {
            tracing::warn!(
                source = %source.display(),
                error = %e,
                "Failed to create wiki domain directory, skipping migration"
            );
            continue;
        }

        let filename = source.file_name().unwrap_or_default();
        let mut dest = dest_dir.join(filename);

        if dest.exists() {
            let stem = source.file_stem().unwrap_or_default();
            let ext = source.extension().unwrap_or_default();
            let ts = chrono::Utc::now().format("%Y%m%d%H%M%S");
            dest = dest_dir.join(format!("{}_{}.{}", stem.to_string_lossy(), ts, ext.to_string_lossy()));
        }

        match std::fs::rename(&source, &dest) {
            Ok(()) => {
                info!(
                    source = %source.display(),
                    dest = %dest.display(),
                    "Migrated inbox note to wiki domain"
                );
                migrated.push((source, dest));
            }
            Err(e) => {
                tracing::warn!(
                    source = %source.display(),
                    error = %e,
                    "Failed to migrate inbox note, leaving in inbox"
                );
            }
        }
    }

    migrated
}

pub struct DistillationPipeline {
    extractor: NotionExtractor,
    compiler: WikiCompiler,
    detector: ContradictionDetector,
}

impl DistillationPipeline {
    pub fn new() -> Self {
        Self {
            extractor: NotionExtractor::new(),
            compiler: WikiCompiler::new(),
            detector: ContradictionDetector::new(),
        }
    }

    pub fn run(&self, inbox_dir: &Path, wiki_dir: &Path) -> Result<DistillationReport> {
        let notes = self.load_notes(inbox_dir)?;
        let notes_processed = notes.len();
        info!(
            notes_processed,
            "Loaded notes from inbox, starting consolidation pipeline"
        );

        let notions = self.extractor.extract_batch(&notes)?;
        let entities_extracted = notions.len();
        info!(entities_extracted, "Notion extraction complete");

        let entity_names: Vec<String> = notions.iter().map(|n| n.name.clone()).collect();
        let linked_notes: Vec<Note> = notes
            .clone()
            .into_iter()
            .map(|mut note| {
                note.content = auto_link_wikilinks(&note.content, &entity_names);
                note
            })
            .collect();

        let pages = self.compiler.compile(&linked_notes, wiki_dir)?;
        let wiki_pages_created = pages.len();
        if wiki_pages_created > 0 {
            info!(wiki_pages_created, "Wiki pages compiled and written");
        }

        let contradictions = self.detector.detect(&notes)?;
        let contradictions_found = contradictions.len();
        if contradictions_found > 0 {
            self.detector
                .log_contradictions(&contradictions, wiki_dir)?;
            info!(contradictions_found, "Contradictions detected and logged");
        } else {
            info!("No contradictions found");
        }

        let migrated = migrate_inbox_to_wiki(&notes, wiki_dir);

        Ok(DistillationReport {
            notes_processed,
            entities_extracted,
            wiki_pages_created,
            contradictions_found,
            migrated_files: migrated,
        })
    }

    /// Load all .md notes from the inbox directory.
    fn load_notes(&self, inbox_dir: &Path) -> Result<Vec<Note>> {
        let mut notes = Vec::new();

        if !inbox_dir.is_dir() {
            info!(
                inbox_dir = %inbox_dir.display(),
                "Inbox directory does not exist, skipping"
            );
            return Ok(notes);
        }

        let mut entries: Vec<_> = std::fs::read_dir(inbox_dir)?
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .map(|ext| ext == "md")
                    .unwrap_or(false)
            })
            .collect();

        // Sort by filename for deterministic ordering
        entries.sort_by_key(|e| e.path());

        for entry in entries {
            let path = entry.path();
            match std::fs::read_to_string(&path) {
                Ok(content) => match parse_frontmatter(&content) {
                    Ok(mut note) => {
                        note.file_path = Some(path.clone());
                        notes.push(note);
                    }
                    Err(e) => {
                        info!(
                            path = %path.display(),
                            error = %e,
                            "Failed to parse frontmatter, skipping note"
                        );
                    }
                },
                Err(e) => {
                    info!(
                        path = %path.display(),
                        error = %e,
                        "Failed to read note file, skipping"
                    );
                }
            }
        }

        Ok(notes)
    }
}

impl Default for DistillationPipeline {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Workflow for DistillationPipeline {
    type Input = DistillationPipelineInput;
    type Output = DistillationReport;

    fn name(&self) -> &str {
        "zen-consolidation-pipeline"
    }

    async fn run(&self, input: Self::Input) -> Result<Self::Output, KernelError> {
        let inbox_dir = &input.inbox_dir;
        let wiki_dir = &input.wiki_dir;
        let dry_run = input.dry_run;

        let notes = self
            .load_notes(inbox_dir)
            .map_err(|e| KernelError::ToolFailed(e.to_string()))?;
        let notes_processed = notes.len();
        info!(
            notes_processed,
            "Loaded notes from inbox, starting consolidation pipeline"
        );

        if notes.is_empty() {
            info!("No notes to process, returning empty report");
            return Ok(DistillationReport {
                notes_processed: 0,
                entities_extracted: 0,
                wiki_pages_created: 0,
                contradictions_found: 0,
                migrated_files: Vec::new(),
            });
        }

        let _ctx = InvestigationContext::new("consolidation", "zen-vault")
            .with_block(uuid::Uuid::now_v7());

        let tools = ToolRegistry::new();

        let notes_json = serde_json::json!({
            "notes": notes.iter().map(|n| {
                serde_json::json!({
                    "id": n.id,
                    "content": n.content,
                    "tags": n.tags,
                    "source": n.source,
                    "sensitivity": format!("{:?}", n.sensitivity),
                })
            }).collect::<Vec<_>>(),
            "wiki_dir": wiki_dir.to_string_lossy(),
        });

        let ctx_with_notes = InvestigationContext::new("consolidation", "zen-vault")
            .with_block(uuid::Uuid::now_v7());

        let mut ctx = ctx_with_notes;
        ctx.evidence.push(rig_compose::context::Evidence {
            recorded_at: std::time::SystemTime::now(),
            source_skill: "consolidation-setup".to_string(),
            label: "consolidation".to_string(),
            detail: serde_json::json!({
                "notes": notes_json["notes"],
                "wiki_dir": wiki_dir.to_string_lossy(),
            }),
        });

        if dry_run {
            info!("Dry-run mode enabled, executing skills without writing to disk");
        }

        let outcome1 = self.extractor.execute(&mut ctx, &tools).await?;
        let entities_extracted = if outcome1.confidence_delta > 0.0 {
            info!("Notion extraction skill completed successfully");
            notes_processed.max(1)
        } else {
            0
        };

        let mut ctx = InvestigationContext::new("consolidation-wiki", "zen-vault")
            .with_block(uuid::Uuid::now_v7());

        ctx.evidence.push(rig_compose::context::Evidence {
            recorded_at: std::time::SystemTime::now(),
            source_skill: "wiki-setup".to_string(),
            label: "wiki-compilation".to_string(),
            detail: serde_json::json!({
                "notes": notes_json["notes"],
                "wiki_dir": wiki_dir.to_string_lossy(),
            }),
        });

        let outcome2 = self.compiler.execute(&mut ctx, &tools).await?;
        let wiki_pages_created = if outcome2.confidence_delta > 0.0 {
            info!("Wiki compilation skill completed successfully");
            notes_processed
        } else {
            0
        };

        let contradictions_found = if notes_processed > 0 && !dry_run {
            let contradictions = self
                .detector
                .detect(&notes)
                .map_err(|e| KernelError::SkillFailed(e.to_string()))?;
            let count = contradictions.len();
            if count > 0 {
                let reports_dir = wiki_dir.join("reports");
                std::fs::create_dir_all(&reports_dir).map_err(|e| {
                    KernelError::SkillFailed(format!("failed to create reports dir: {e}"))
                })?;
                self.detector
                    .log_contradictions(&contradictions, &reports_dir)
                    .map_err(|e| KernelError::SkillFailed(e.to_string()))?;
                info!(count, "Contradictions detected and logged");
            } else {
                info!("No contradictions found");
            }
            count
        } else {
            let mut ctx = InvestigationContext::new("consolidation-contradiction", "zen-vault")
                .with_block(uuid::Uuid::now_v7());

            ctx.evidence.push(rig_compose::context::Evidence {
                recorded_at: std::time::SystemTime::now(),
                source_skill: "contradiction-setup".to_string(),
                label: "contradiction-detection".to_string(),
                detail: serde_json::json!({
                    "notes": notes_json["notes"],
                    "wiki_dir": wiki_dir.to_string_lossy(),
                }),
            });

            let outcome3 = self.detector.execute(&mut ctx, &tools).await?;
            info!(
                contradiction_delta = outcome3.confidence_delta,
                "Contradiction detection skill completed"
            );
            0
        };

        let migrated = if !dry_run {
            migrate_inbox_to_wiki(&notes, wiki_dir)
        } else {
            Vec::new()
        };

        Ok(DistillationReport {
            notes_processed,
            entities_extracted,
            wiki_pages_created,
            contradictions_found,
            migrated_files: migrated,
        })
    }
}

#[derive(Debug, Clone)]
pub struct DistillationPipelineInput {
    pub inbox_dir: PathBuf,
    pub wiki_dir: PathBuf,
    pub dry_run: bool,
}

impl DistillationPipelineInput {
    pub fn new(inbox_dir: PathBuf, wiki_dir: PathBuf) -> Self {
        Self {
            inbox_dir,
            wiki_dir,
            dry_run: false,
        }
    }

    pub fn with_dry_run(mut self) -> Self {
        self.dry_run = true;
        self
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn create_test_note(id: &str, content: &str) -> String {
        format!(
            r#"---
id: "{}"
source: "test"
sensitivity: private
created_at: "2026-05-23T15:00:00+00:00"
updated_at: "2026-05-23T15:00:00+00:00"
---

{}"#,
            id, content
        )
    }

    #[test]
    fn test_pipeline_empty_inbox() {
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        fs::create_dir(&inbox_dir).unwrap();
        let wiki_dir = tmp.path().join("wiki");

        let pipeline = DistillationPipeline::new();
        let report = pipeline.run(&inbox_dir, &wiki_dir).unwrap();

        assert_eq!(report.notes_processed, 0);
        assert_eq!(report.entities_extracted, 0);
        assert_eq!(report.wiki_pages_created, 0);
        assert_eq!(report.contradictions_found, 0);
    }

    #[test]
    fn test_pipeline_nonexistent_inbox() {
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        let wiki_dir = tmp.path().join("wiki");

        let pipeline = DistillationPipeline::new();
        let report = pipeline.run(&inbox_dir, &wiki_dir).unwrap();

        assert_eq!(report.notes_processed, 0);
        assert_eq!(report.entities_extracted, 0);
        assert_eq!(report.wiki_pages_created, 0);
        assert_eq!(report.contradictions_found, 0);
    }

    #[test]
    fn test_pipeline_with_single_note() {
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        fs::create_dir(&inbox_dir).unwrap();
        let wiki_dir = tmp.path().join("wiki");

        let note_content = create_test_note(
            "note-1",
            "# Rust Project\n\nI love using Rust and Tokio for async programming.",
        );
        fs::write(inbox_dir.join("note1.md"), &note_content).unwrap();

        let pipeline = DistillationPipeline::new();
        let report = pipeline.run(&inbox_dir, &wiki_dir).unwrap();

        assert_eq!(report.notes_processed, 1);
        assert!(
            report.entities_extracted > 0,
            "Should extract Rust/Tokio notions"
        );
        assert_eq!(report.wiki_pages_created, 1, "Should create one wiki page");
        assert_eq!(report.contradictions_found, 0);
    }

    #[test]
    fn test_pipeline_filters_non_md_files() {
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        fs::create_dir(&inbox_dir).unwrap();
        let wiki_dir = tmp.path().join("wiki");

        let note_content = create_test_note("note-1", "# Hello\n\nSome content about Python.");
        fs::write(inbox_dir.join("note1.md"), &note_content).unwrap();
        fs::write(inbox_dir.join("readme.txt"), "not a note").unwrap();
        fs::write(inbox_dir.join("data.json"), "{}").unwrap();

        let pipeline = DistillationPipeline::new();
        let report = pipeline.run(&inbox_dir, &wiki_dir).unwrap();

        assert_eq!(report.notes_processed, 1);
    }

    #[test]
    fn test_pipeline_with_multiple_notes() {
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        fs::create_dir(&inbox_dir).unwrap();
        let wiki_dir = tmp.path().join("wiki");

        let note1 = create_test_note(
            "note-1",
            "# Systems Programming\n\nRust is great for performance-critical code.",
        );
        let note2 = create_test_note(
            "note-2",
            "# Data Science\n\nPython and its ecosystem for machine learning.",
        );
        fs::write(inbox_dir.join("01-rust.md"), &note1).unwrap();
        fs::write(inbox_dir.join("02-python.md"), &note2).unwrap();

        let pipeline = DistillationPipeline::new();
        let report = pipeline.run(&inbox_dir, &wiki_dir).unwrap();

        assert_eq!(report.notes_processed, 2);
        // Entities are deduplicated across notes
        assert!(report.entities_extracted > 0);
        assert!(
            report.entities_extracted >= 2,
            "Should find at least Python and Rust"
        );
    }

    #[test]
    fn test_pipeline_skips_malformed_notes() {
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        fs::create_dir(&inbox_dir).unwrap();
        let wiki_dir = tmp.path().join("wiki");

        // Valid note
        let note1 = create_test_note("note-1", "# Hello\n\nSome content.");
        fs::write(inbox_dir.join("01-good.md"), &note1).unwrap();

        // Malformed frontmatter (missing closing ---)
        fs::write(inbox_dir.join("02-bad.md"), "---\nid: \"note-2\"\n\nbody").unwrap();

        let pipeline = DistillationPipeline::new();
        let report = pipeline.run(&inbox_dir, &wiki_dir).unwrap();

        assert_eq!(report.notes_processed, 1, "Should skip the malformed note");
    }

    #[test]
    fn test_pipeline_creates_wiki_dir_if_missing() {
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        fs::create_dir(&inbox_dir).unwrap();
        // wiki_dir is intentionally NOT created
        let wiki_dir = tmp.path().join("wiki");

        let note = create_test_note("note-1", "# Hello");
        fs::write(inbox_dir.join("note.md"), &note).unwrap();

        let pipeline = DistillationPipeline::new();
        // WikiStructure::ensure_directories creates wiki_dir hierarchy
        let result = pipeline.run(&inbox_dir, &wiki_dir);
        assert!(result.is_ok());
    }

    #[test]
    fn test_report_debug_derive() {
        let report = DistillationReport {
            notes_processed: 5,
            entities_extracted: 3,
            wiki_pages_created: 2,
            contradictions_found: 1,
            migrated_files: Vec::new(),
        };
        let debug_str = format!("{:?}", report);
        assert!(debug_str.contains("notes_processed"));
        assert!(debug_str.contains("5"));
    }

    #[test]
    fn test_report_clone() {
        let report = DistillationReport {
            notes_processed: 1,
            entities_extracted: 0,
            wiki_pages_created: 0,
            contradictions_found: 0,
            migrated_files: Vec::new(),
        };
        let cloned = report.clone();
        assert_eq!(report.notes_processed, cloned.notes_processed);
    }

    #[test]
    fn test_pipeline_default() {
        let pipeline = DistillationPipeline::default();
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        let wiki_dir = tmp.path().join("wiki");

        let result = pipeline.run(&inbox_dir, &wiki_dir);
        assert!(result.is_ok());
    }

    #[test]
    fn test_auto_link_wikilinks_wraps_entity() {
        let content = "I love using Rust for systems programming.";
        let entities = vec!["Rust".to_string()];
        let result = auto_link_wikilinks(content, &entities);
        assert_eq!(result, "I love using [[Rust]] for systems programming.");
    }

    #[test]
    fn test_auto_link_wikilinks_skips_already_linked() {
        let content = "I love using [[Rust]] for systems programming.";
        let entities = vec!["Rust".to_string()];
        let result = auto_link_wikilinks(content, &entities);
        assert_eq!(result, "I love using [[Rust]] for systems programming.");
    }

    #[test]
    fn test_auto_link_wikilinks_multiple_entities() {
        let content = "Rust and Tokio are great for async programming.";
        let entities = vec!["Rust".to_string(), "Tokio".to_string()];
        let result = auto_link_wikilinks(content, &entities);
        assert_eq!(result, "[[Rust]] and [[Tokio]] are great for async programming.");
    }

    #[test]
    fn test_auto_link_wikilinks_longest_first() {
        let content = "PostgreSQL and SQL are databases.";
        let entities = vec!["SQL".to_string(), "PostgreSQL".to_string()];
        let result = auto_link_wikilinks(content, &entities);
        assert_eq!(result, "[[PostgreSQL]] and [[SQL]] are databases.");
    }

    #[test]
    fn test_auto_link_wikilinks_word_boundary() {
        let content = "Rustic is not Rust.";
        let entities = vec!["Rust".to_string()];
        let result = auto_link_wikilinks(content, &entities);
        assert_eq!(result, "Rustic is not [[Rust]].");
    }

    #[test]
    fn test_auto_link_wikilinks_no_entities() {
        let content = "No entities here.";
        let entities = vec![];
        let result = auto_link_wikilinks(content, &entities);
        assert_eq!(result, "No entities here.");
    }

    #[test]
    fn test_auto_link_wikilinks_empty_content() {
        let content = "";
        let entities = vec!["Rust".to_string()];
        let result = auto_link_wikilinks(content, &entities);
        assert_eq!(result, "");
    }

    #[test]
    fn test_auto_link_wikilinks_preserves_backticks() {
        let content = "Use `Rust` for programming.";
        let entities = vec!["Rust".to_string()];
        let result = auto_link_wikilinks(content, &entities);
        assert_eq!(result, "Use `Rust` for programming.");
    }

    #[test]
    fn test_migrate_inbox_to_wiki_moves_by_domain() {
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        let wiki_dir = tmp.path().join("wiki");
        fs::create_dir_all(&inbox_dir).unwrap();

        let content = create_test_note("note-1", "# Work note");
        let source = inbox_dir.join("work-note.md");
        fs::write(&source, &content).unwrap();

        let notes = vec![Note {
            id: "note-1".to_string(),
            domain: vec![crate::note::Domain::Work],
            file_path: Some(source.clone()),
            ..Note::default()
        }];

        let migrated = migrate_inbox_to_wiki(&notes, &wiki_dir);

        assert_eq!(migrated.len(), 1);
        let (src, dst) = &migrated[0];
        assert_eq!(src, &source);
        assert!(dst.starts_with(&wiki_dir.join("work")));
        assert!(!source.exists(), "inbox file should be gone");
        assert!(dst.exists(), "wiki file should exist");
    }

    #[test]
    fn test_migrate_inbox_to_wiki_defaults_to_general() {
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        let wiki_dir = tmp.path().join("wiki");
        fs::create_dir_all(&inbox_dir).unwrap();

        let content = create_test_note("note-2", "# Untagged note");
        let source = inbox_dir.join("untagged.md");
        fs::write(&source, &content).unwrap();

        let notes = vec![Note {
            id: "note-2".to_string(),
            domain: Vec::new(),
            file_path: Some(source.clone()),
            ..Note::default()
        }];

        let migrated = migrate_inbox_to_wiki(&notes, &wiki_dir);

        assert_eq!(migrated.len(), 1);
        let (_, dst) = &migrated[0];
        assert!(dst.starts_with(&wiki_dir.join("general")));
        assert!(dst.exists());
    }

    #[test]
    fn test_migrate_inbox_to_wiki_avoids_overwrite() {
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        let wiki_dir = tmp.path().join("wiki");
        let work_dir = wiki_dir.join("work");
        fs::create_dir_all(&inbox_dir).unwrap();
        fs::create_dir_all(&work_dir).unwrap();

        let content = create_test_note("note-3", "# Another work note");
        let source = inbox_dir.join("duplicate.md");
        fs::write(&source, &content).unwrap();

        fs::write(work_dir.join("duplicate.md"), "existing").unwrap();

        let notes = vec![Note {
            id: "note-3".to_string(),
            domain: vec![crate::note::Domain::Work],
            file_path: Some(source.clone()),
            ..Note::default()
        }];

        let migrated = migrate_inbox_to_wiki(&notes, &wiki_dir);

        assert_eq!(migrated.len(), 1);
        let (_, dst) = &migrated[0];
        assert!(work_dir.join("duplicate.md").exists());
        let dst_name = dst.file_name().unwrap().to_string_lossy();
        assert!(dst_name.starts_with("duplicate_"), "expected timestamp suffix: {dst_name}");
        assert!(dst.exists());
    }

    #[test]
    fn test_migrate_inbox_to_wiki_inbox_empty_after() {
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        let wiki_dir = tmp.path().join("wiki");
        fs::create_dir_all(&inbox_dir).unwrap();

        let c1 = create_test_note("n1", "# Note one");
        let c2 = create_test_note("n2", "# Note two");
        let s1 = inbox_dir.join("note1.md");
        let s2 = inbox_dir.join("note2.md");
        fs::write(&s1, &c1).unwrap();
        fs::write(&s2, &c2).unwrap();

        let notes = vec![
            Note {
                id: "n1".to_string(),
                domain: vec![crate::note::Domain::Personal],
                file_path: Some(s1.clone()),
                ..Note::default()
            },
            Note {
                id: "n2".to_string(),
                domain: vec![crate::note::Domain::Learning],
                file_path: Some(s2.clone()),
                ..Note::default()
            },
        ];

        let migrated = migrate_inbox_to_wiki(&notes, &wiki_dir);

        assert_eq!(migrated.len(), 2);
        assert!(!s1.exists());
        assert!(!s2.exists());

        let inbox_entries: Vec<_> = fs::read_dir(&inbox_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(inbox_entries.is_empty(), "inbox should be empty after migration");
    }
}
