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
use super::entity_extraction::EntityExtractor;
use super::wiki_compile::WikiCompiler;

use crate::note::{Note, parse_frontmatter};

#[derive(Debug, Clone)]
pub struct ConsolidationReport {
    pub notes_processed: usize,
    pub entities_extracted: usize,
    pub wiki_pages_created: usize,
    pub contradictions_found: usize,
}

pub struct ConsolidationPipeline {
    extractor: EntityExtractor,
    compiler: WikiCompiler,
    detector: ContradictionDetector,
}

impl ConsolidationPipeline {
    pub fn new() -> Self {
        Self {
            extractor: EntityExtractor::new(),
            compiler: WikiCompiler::new(),
            detector: ContradictionDetector::new(),
        }
    }

    pub fn run(&self, inbox_dir: &Path, wiki_dir: &Path) -> Result<ConsolidationReport> {
        let notes = self.load_notes(inbox_dir)?;
        let notes_processed = notes.len();
        info!(
            notes_processed,
            "Loaded notes from inbox, starting consolidation pipeline"
        );

        let entities = self.extractor.extract_batch(&notes)?;
        let entities_extracted = entities.len();
        info!(entities_extracted, "Entity extraction complete");

        let pages = self.compiler.compile(&notes, wiki_dir)?;
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

        Ok(ConsolidationReport {
            notes_processed,
            entities_extracted,
            wiki_pages_created,
            contradictions_found,
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

impl Default for ConsolidationPipeline {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Workflow for ConsolidationPipeline {
    type Input = ConsolidationPipelineInput;
    type Output = ConsolidationReport;

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
            return Ok(ConsolidationReport {
                notes_processed: 0,
                entities_extracted: 0,
                wiki_pages_created: 0,
                contradictions_found: 0,
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
            info!("Entity extraction skill completed successfully");
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

        Ok(ConsolidationReport {
            notes_processed,
            entities_extracted,
            wiki_pages_created,
            contradictions_found,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ConsolidationPipelineInput {
    pub inbox_dir: PathBuf,
    pub wiki_dir: PathBuf,
    pub dry_run: bool,
}

impl ConsolidationPipelineInput {
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

        let pipeline = ConsolidationPipeline::new();
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

        let pipeline = ConsolidationPipeline::new();
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

        let pipeline = ConsolidationPipeline::new();
        let report = pipeline.run(&inbox_dir, &wiki_dir).unwrap();

        assert_eq!(report.notes_processed, 1);
        assert!(
            report.entities_extracted > 0,
            "Should extract Rust/Tokio entities"
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

        let pipeline = ConsolidationPipeline::new();
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

        let pipeline = ConsolidationPipeline::new();
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

        let pipeline = ConsolidationPipeline::new();
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

        let pipeline = ConsolidationPipeline::new();
        // WikiStructure::ensure_directories creates wiki_dir hierarchy
        let result = pipeline.run(&inbox_dir, &wiki_dir);
        assert!(result.is_ok());
    }

    #[test]
    fn test_report_debug_derive() {
        let report = ConsolidationReport {
            notes_processed: 5,
            entities_extracted: 3,
            wiki_pages_created: 2,
            contradictions_found: 1,
        };
        let debug_str = format!("{:?}", report);
        assert!(debug_str.contains("notes_processed"));
        assert!(debug_str.contains("5"));
    }

    #[test]
    fn test_report_clone() {
        let report = ConsolidationReport {
            notes_processed: 1,
            entities_extracted: 0,
            wiki_pages_created: 0,
            contradictions_found: 0,
        };
        let cloned = report.clone();
        assert_eq!(report.notes_processed, cloned.notes_processed);
    }

    #[test]
    fn test_pipeline_default() {
        let pipeline = ConsolidationPipeline::default();
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        let wiki_dir = tmp.path().join("wiki");

        let result = pipeline.run(&inbox_dir, &wiki_dir);
        assert!(result.is_ok());
    }
}
