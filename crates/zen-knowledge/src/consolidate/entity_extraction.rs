use anyhow::Result;
use async_trait::async_trait;
use rig_compose::context::InvestigationContext;
use rig_compose::registry::{KernelError, ToolRegistry};
use rig_compose::skill::{Skill, SkillOutcome};
use tracing::info;

use crate::entity::{Entity, EntityType};
use crate::note::Note;

/// Entity extractor — extracts entities from notes.
///
/// Phase 1: Uses keyword heuristics and pattern matching.
/// Phase 3: LLM-based entity extraction via zen-provider.
pub struct EntityExtractor;

impl EntityExtractor {
    pub fn new() -> Self {
        Self
    }

    /// Extract entities from a single note.
    ///
    /// Uses keyword heuristics to identify technologies, concepts, and patterns
    /// in the note content. Returns extracted entities with confidence scores.
    pub fn extract(&self, note: &Note) -> Result<Vec<Entity>> {
        let mut entities = Vec::new();

        let known_techs = [
            "Rust",
            "Python",
            "JavaScript",
            "TypeScript",
            "Go",
            "SQLite",
            "PostgreSQL",
            "Redis",
            "Docker",
            "Kubernetes",
            "React",
            "Vue",
            "Tokio",
            "async",
            "LLM",
            "AI",
            "MCP",
            "WASM",
            "rig-core",
            "ratatui",
        ];

        let content_lower = note.content.to_lowercase();

        for tech in &known_techs {
            if content_lower.contains(&tech.to_lowercase()) {
                let entity = Entity::new(tech.to_string(), EntityType::Technology, note.id.clone());
                entities.push(entity);
            }
        }

        let lines: Vec<&str> = note.content.lines().collect();
        for line in &lines {
            if line.starts_with('#') && line.len() > 2 {
                let title = line.trim_start_matches('#').trim();
                if !title.is_empty() && title.len() > 2 {
                    let entity =
                        Entity::new(title.to_string(), EntityType::Concept, note.id.clone());
                    entities.push(entity);
                }
            }
        }

        info!(
            note_id = %note.id,
            entity_count = entities.len(),
            "Entity extraction complete"
        );
        Ok(entities)
    }

    /// Extract entities from multiple notes, deduplicating by name.
    pub fn extract_batch(&self, notes: &[Note]) -> Result<Vec<Entity>> {
        let mut seen = std::collections::HashMap::new();

        for note in notes {
            let extracted = self.extract(note)?;
            for entity in extracted {
                seen.entry(entity.name.clone()).or_insert(entity);
            }
        }

        Ok(seen.into_values().collect())
    }

    fn extract_from_json(&self, notes_val: &serde_json::Value) -> Result<Vec<Entity>> {
        let notes_array = notes_val
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("expected notes array"))?;

        let mut all_entities = Vec::new();

        for note_val in notes_array {
            let content = note_val
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("");

            let note_id = note_val
                .get("id")
                .and_then(|i| i.as_str())
                .unwrap_or("unknown")
                .to_string();

            for tech in &[
                "Rust",
                "Python",
                "JavaScript",
                "TypeScript",
                "Go",
                "SQLite",
                "PostgreSQL",
                "Redis",
                "Docker",
                "Kubernetes",
                "React",
                "Vue",
                "Tokio",
                "async",
                "LLM",
                "AI",
                "MCP",
                "WASM",
                "rig-core",
                "ratatui",
            ] {
                if content.to_lowercase().contains(&tech.to_lowercase()) {
                    let mut entity =
                        Entity::new(tech.to_string(), EntityType::Technology, note_id.clone());
                    entity.metadata.insert(
                        "extraction_method".to_string(),
                        "skill_heuristic".to_string(),
                    );
                    all_entities.push(entity);
                }
            }
        }

        Ok(all_entities)
    }
}

impl Default for EntityExtractor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Skill for EntityExtractor {
    fn id(&self) -> &str {
        "zen-entity-extraction"
    }

    fn description(&self) -> &str {
        "Extract entities (technologies, concepts, people) from notes using keyword heuristics and LLM augmentation"
    }

    fn applies(&self, ctx: &InvestigationContext) -> bool {
        !ctx.evidence.is_empty() || !ctx.signals.is_empty()
    }

    async fn execute(
        &self,
        ctx: &mut InvestigationContext,
        _tools: &ToolRegistry,
    ) -> Result<SkillOutcome, KernelError> {
        let notes_val = ctx
            .evidence
            .iter()
            .filter_map(|ev| ev.detail.get("notes").cloned())
            .next();

        let entities = if let Some(notes) = notes_val {
            self.extract_from_json(&notes)
                .map_err(|e| KernelError::SkillFailed(e.to_string()))?
        } else {
            info!("EntityExtractor: no notes in context, using heuristic-only extraction");
            Vec::new()
        };

        let entity_count = entities.len();
        info!(
            entity_count,
            confidence = ctx.confidence,
            "Entity extraction skill complete"
        );

        Ok(SkillOutcome::noop().with_delta(if entity_count > 0 { 0.1 } else { 0.0 }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use zen_core::types::Sensitivity;

    fn make_note(content: &str) -> Note {
        Note {
            id: uuid::Uuid::now_v7().to_string(),
            tags: vec![],
            source: "test".to_string(),
            source_id: None,
            sensitivity: Sensitivity::Private,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            domain: vec![],
            project: None,
            content: content.to_string(),
            file_path: None,
        }
    }

    #[test]
    fn test_extract_finds_technology() {
        let extractor = EntityExtractor;
        let note = make_note("I love using Rust and Tokio for async programming.");
        let entities = extractor.extract(&note).unwrap();
        assert!(entities.iter().any(|e| e.name == "Rust"));
        assert!(entities.iter().any(|e| e.name == "Tokio"));
        assert!(entities.iter().any(|e| e.name == "async"));
    }

    #[test]
    fn test_extract_finds_heading_concept() {
        let extractor = EntityExtractor;
        let note = make_note("# Async Runtime\n\nThis is about async runtimes.");
        let entities = extractor.extract(&note).unwrap();
        assert!(entities.iter().any(|e| e.name == "Async Runtime"));
    }

    #[test]
    fn test_extract_batch_deduplicates() {
        let extractor = EntityExtractor;
        let note1 = make_note("I use Rust for systems programming.");
        let note2 = make_note("Rust is great for performance.");
        let entities = extractor.extract_batch(&[note1, note2]).unwrap();
        let rust_count = entities.iter().filter(|e| e.name == "Rust").count();
        assert_eq!(rust_count, 1);
    }
}
