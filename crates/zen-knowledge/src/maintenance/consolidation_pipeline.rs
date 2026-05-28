use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;
use rig_compose::context::InvestigationContext;
use rig_compose::registry::KernelError;
use rig_compose::workflow::Workflow;
use serde::{Deserialize, Serialize};
use tracing::info;

use super::contradiction_detector::ContradictionDetectorSkill;
use super::entity_extraction::EntityExtractionSkill;
use super::wiki_compiler::WikiCompilerSkill;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaintenanceReport {
    pub notes_processed: usize,
    pub entities_extracted: usize,
    pub wiki_pages_compiled: usize,
    pub contradictions_detected: usize,
    pub steps_executed: Vec<String>,
}

pub struct ConsolidationPipelineSkill {
    inbox_dir: PathBuf,
    wiki_dir: PathBuf,
    extractor: EntityExtractionSkill,
    compiler: WikiCompilerSkill,
    detector: ContradictionDetectorSkill,
}

impl ConsolidationPipelineSkill {
    pub fn new(inbox_dir: PathBuf, wiki_dir: PathBuf) -> Self {
        Self {
            inbox_dir,
            wiki_dir: wiki_dir.clone(),
            extractor: EntityExtractionSkill::new(),
            compiler: WikiCompilerSkill::new(wiki_dir.clone()),
            detector: ContradictionDetectorSkill::new(wiki_dir),
        }
    }

    pub fn load_notes(&self) -> Result<Vec<serde_json::Value>> {
        let mut notes = Vec::new();

        if !self.inbox_dir.is_dir() {
            info!(
                inbox_dir = %self.inbox_dir.display(),
                "Inbox directory does not exist, returning empty notes"
            );
            return Ok(notes);
        }

        let mut entries: Vec<_> = std::fs::read_dir(&self.inbox_dir)?
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .map(|ext| ext == "md")
                    .unwrap_or(false)
            })
            .collect();

        entries.sort_by_key(|e| e.path());

        for entry in entries {
            let path = entry.path();
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    notes.push(serde_json::json!({
                        "id": path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
                        "content": content,
                        "tags": [],
                    }));
                },
                Err(e) => {
                    info!(path = %path.display(), error = %e, "Failed to read note, skipping");
                },
            }
        }

        Ok(notes)
    }

    fn extract_entities_from_notes(&self, notes: &[serde_json::Value]) -> Vec<String> {
        let mut entity_names = Vec::new();

        for note in notes {
            let content = note
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("");

            let note_id = note
                .get("id")
                .and_then(|i| i.as_str())
                .unwrap_or("unknown");

            let entities = self.extractor.extract_entities(content, note_id);
            for entity in entities {
                entity_names.push(entity.name);
            }
        }

        let mut seen = std::collections::HashSet::new();
        entity_names.retain(|n| seen.insert(n.to_lowercase()));

        entity_names
    }
}

impl Default for ConsolidationPipelineSkill {
    fn default() -> Self {
        Self {
            inbox_dir: PathBuf::from("."),
            wiki_dir: PathBuf::from("."),
            extractor: EntityExtractionSkill::new(),
            compiler: WikiCompilerSkill::new(PathBuf::from(".")),
            detector: ContradictionDetectorSkill::new(PathBuf::from(".")),
        }
    }
}

#[async_trait]
impl Workflow for ConsolidationPipelineSkill {
    type Input = PipelineInput;
    type Output = MaintenanceReport;

    fn name(&self) -> &str {
        "zen-consolidation-pipeline-maintenance"
    }

    async fn run(&self, input: Self::Input) -> Result<Self::Output, KernelError> {
        let notes = self.load_notes()
            .map_err(|e| KernelError::SkillFailed(e.to_string()))?;

        let notes_processed = notes.len();
        info!(notes_processed, "loaded notes for consolidation pipeline");

        if notes.is_empty() {
            return Ok(MaintenanceReport {
                notes_processed: 0,
                entities_extracted: 0,
                wiki_pages_compiled: 0,
                contradictions_detected: 0,
                steps_executed: vec!["pipeline_started".to_string(), "no_notes_found".to_string()],
            });
        }

        let mut steps = vec!["pipeline_started".to_string()];

        let ctx = InvestigationContext::new("consolidation", "zen-maintenance")
            .with_block(uuid::Uuid::now_v7());

        let mut ctx = ctx;
        ctx.evidence.push(rig_compose::context::Evidence {
            recorded_at: std::time::SystemTime::now(),
            source_skill: "consolidation-setup".to_string(),
            label: "consolidation".to_string(),
            detail: serde_json::json!({
                "notes": notes,
                "wiki_dir": self.wiki_dir.to_string_lossy(),
            }),
        });

        let tools = rig_compose::registry::ToolRegistry::new();

        steps.push("extracting_entities".to_string());
        let extractor = EntityExtractionSkill::new();
        let extracted_entities: Vec<String> = self.extract_entities_from_notes(&notes);
        let entities_extracted = extracted_entities.len();
        info!(entities_extracted, "entity extraction complete");

        if entities_extracted > 0 {
            ctx.signals.push(rig_compose::context::Signal::new("entities_extracted"));
        }

        ctx.evidence.push(rig_compose::context::Evidence::new(
            "zen-maintenance-entity-extraction",
            "extracted_entities",
        ).with_detail(serde_json::json!({
            "entities": extracted_entities,
            "count": entities_extracted,
        })));

        let _ = extractor.execute(&mut ctx, &tools).await?;
        steps.push("entity_extraction_complete".to_string());

        let compiler = WikiCompilerSkill::new(self.wiki_dir.clone());
        let mut ctx_compile = InvestigationContext::new("wiki-compilation", "zen-maintenance")
            .with_block(uuid::Uuid::now_v7());

        ctx_compile.evidence.push(rig_compose::context::Evidence {
            recorded_at: std::time::SystemTime::now(),
            source_skill: "wiki-setup".to_string(),
            label: "wiki-compilation".to_string(),
            detail: serde_json::json!({
                "notes": notes,
                "wiki_dir": self.wiki_dir.to_string_lossy(),
            }),
        });

        let _ = compiler.execute(&mut ctx_compile, &tools).await?;
        let wiki_pages_compiled = notes_processed;
        steps.push("wiki_compilation_complete".to_string());

        let detector = ContradictionDetectorSkill::new(self.wiki_dir.clone());
        let mut ctx_detect = InvestigationContext::new("contradiction-detection", "zen-maintenance")
            .with_block(uuid::Uuid::now_v7());

        ctx_detect.evidence.push(rig_compose::context::Evidence {
            recorded_at: std::time::SystemTime::now(),
            source_skill: "contradiction-setup".to_string(),
            label: "contradiction-detection".to_string(),
            detail: serde_json::json!({
                "notes": notes,
                "wiki_dir": self.wiki_dir.to_string_lossy(),
            }),
        });

        let _ = detector.execute(&mut ctx_detect, &tools).await?;

        let mut all_contradictions = Vec::new();
        for ev in &ctx_detect.evidence {
            if let Some(contradictions) = ev.detail.get("contradictions") {
                if let Some(arr) = contradictions.as_array() {
                    all_contradictions.extend(arr.clone());
                }
            }
        }
        let contradictions_detected = all_contradictions.len();
        steps.push("contradiction_detection_complete".to_string());

        info!(
            notes_processed,
            entities_extracted,
            wiki_pages_compiled,
            contradictions_detected,
            "ConsolidationPipelineSkill: pipeline execution complete"
        );

        Ok(MaintenanceReport {
            notes_processed,
            entities_extracted,
            wiki_pages_compiled,
            contradictions_detected,
            steps_executed: steps,
        })
    }
}

#[derive(Debug, Clone)]
pub struct PipelineInput {
    pub inbox_dir: PathBuf,
    pub wiki_dir: PathBuf,
}

impl PipelineInput {
    pub fn new(inbox_dir: PathBuf, wiki_dir: PathBuf) -> Self {
        Self { inbox_dir, wiki_dir }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_empty_inbox() {
        let tmp = tempfile::tempdir().unwrap();
        let pipeline = ConsolidationPipelineSkill::new(
            tmp.path().join("inbox"),
            tmp.path().join("wiki"),
        );
        let result = rt().block_on(async {
            let input = PipelineInput::new(
                tmp.path().join("inbox"),
                tmp.path().join("wiki"),
            );
            pipeline.run(input).await
        });

        assert!(result.is_ok());
        let report = result.unwrap();
        assert_eq!(report.notes_processed, 0);
    }

    #[test]
    fn test_pipeline_with_notes() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox = tmp.path().join("inbox");
        fs::create_dir(&inbox).unwrap();
        let wiki = tmp.path().join("wiki");

        let note = "---\nid: \"test-1\"\nsource: \"test\"\nsensitivity: private\n---\n\n# Rust Guide\n\nRust is a systems language using Tokio.";
        fs::write(inbox.join("note1.md"), note).unwrap();

        let pipeline = ConsolidationPipelineSkill::new(inbox.clone(), wiki.clone());
        let result = rt().block_on(async {
            let input = PipelineInput::new(inbox, wiki);
            pipeline.run(input).await
        });

        assert!(result.is_ok());
        let report = result.unwrap();
        assert_eq!(report.notes_processed, 1);
        assert!(report.entities_extracted > 0);
        assert_eq!(report.wiki_pages_compiled, 1);
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }
}
