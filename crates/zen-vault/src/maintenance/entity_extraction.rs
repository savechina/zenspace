use std::collections::HashSet;

use anyhow::Result;
use async_trait::async_trait;
use rig_compose::context::InvestigationContext;
use rig_compose::registry::{KernelError, ToolRegistry};
use rig_compose::skill::{Skill, SkillOutcome};
use serde::{Deserialize, Serialize};
use tracing::info;

const KNOWN_TECHS: &[&str] = &[
    "rust", "python", "javascript", "typescript", "go", "sqlite", "postgresql",
    "redis", "docker", "kubernetes", "react", "vue", "tokio", "async", "llm",
    "ai", "mcp", "wasm", "rig-core", "ratatui",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedEntity {
    pub name: String,
    pub category: EntityCategory,
    pub source_note_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum EntityCategory {
    Technology,
    Concept,
    Person,
    Organization,
    Unknown,
}

impl EntityCategory {
    fn from_name(name: &str) -> Self {
        let lower = name.to_lowercase();
        if KNOWN_TECHS.contains(&lower.as_str()) {
            return EntityCategory::Technology;
        }

        if name.chars().next().is_some_and(|c| c.is_uppercase())
            && name.len() > 2
        {
            if lower.contains("project") || lower.contains("initiative") || lower.contains("program") {
                return EntityCategory::Organization;
            }
            return EntityCategory::Concept;
        }

        EntityCategory::Unknown
    }
}

impl std::fmt::Display for EntityCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EntityCategory::Technology => write!(f, "technology"),
            EntityCategory::Concept => write!(f, "concept"),
            EntityCategory::Person => write!(f, "person"),
            EntityCategory::Organization => write!(f, "organization"),
            EntityCategory::Unknown => write!(f, "unknown"),
        }
    }
}

pub struct EntityExtractionSkill {
    min_entity_length: usize,
}

impl EntityExtractionSkill {
    pub fn new() -> Self {
        Self {
            min_entity_length: 2,
        }
    }

    pub fn extract_entities(&self, content: &str, note_id: &str) -> Vec<ExtractedEntity> {
        let mut entities: Vec<ExtractedEntity> = Vec::new();
        let content_lower = content.to_lowercase();

        for tech in KNOWN_TECHS {
            if content_lower.contains(tech) {
                let name = if tech.len() <= 4 {
                    tech.to_string()
                } else {
                    tech[..1].to_uppercase() + &tech[1..]
                };
                entities.push(ExtractedEntity {
                    name,
                    category: EntityCategory::Technology,
                    source_note_ids: vec![note_id.to_string()],
                });
            }
        }

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("## ") && trimmed.len() > 3 {
                let heading = &trimmed[3..];
                if heading.len() >= self.min_entity_length {
                    entities.push(ExtractedEntity {
                        name: heading.to_string(),
                        category: EntityCategory::Concept,
                        source_note_ids: vec![note_id.to_string()],
                    });
                }
            } else if trimmed.starts_with("# ") && trimmed.len() > 2 {
                let heading = &trimmed[2..];
                if heading.len() >= self.min_entity_length {
                    let lower = heading.to_lowercase();
                    if !KNOWN_TECHS.contains(&lower.as_str()) {
                        entities.push(ExtractedEntity {
                            name: heading.to_string(),
                            category: EntityCategory::Concept,
                            source_note_ids: vec![note_id.to_string()],
                        });
                    }
                }
            }
        }

        entities
    }

    pub fn extract_from_context(&self, ctx: &InvestigationContext) -> Result<Vec<ExtractedEntity>> {
        let notes_val = ctx
            .evidence
            .iter()
            .filter_map(|ev| ev.detail.get("notes").cloned())
            .next();

        let mut all_entities: Vec<ExtractedEntity> = Vec::new();

        if let Some(notes) = notes_val {
            let notes_array = notes
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("expected notes array"))?;

            for note_val in notes_array {
                let content = note_val
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("");

                let note_id = note_val
                    .get("id")
                    .and_then(|i| i.as_str())
                    .unwrap_or("unknown");

                let entities = self.extract_entities(content, note_id);
                all_entities.extend(entities);
            }
        }

        all_entities = self.deduplicate(all_entities);

        info!(
            entity_count = all_entities.len(),
            "EntityExtractionSkill: extraction complete"
        );

        Ok(all_entities)
    }

    fn deduplicate(&self, entities: Vec<ExtractedEntity>) -> Vec<ExtractedEntity> {
        let mut seen: HashSet<(String, EntityCategory)> = HashSet::new();
        let mut result: Vec<ExtractedEntity> = Vec::new();

        for entity in entities {
            let key = (entity.name.to_lowercase(), entity.category.clone());
            if seen.insert(key) {
                result.push(entity);
            }
        }

        result
    }
}

impl Default for EntityExtractionSkill {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Skill for EntityExtractionSkill {
    fn id(&self) -> &str {
        "zen-maintenance-entity-extraction"
    }

    fn description(&self) -> &str {
        "Extract entities (technologies, concepts) from notes using keyword heuristics"
    }

    fn applies(&self, ctx: &InvestigationContext) -> bool {
        ctx.evidence.iter().any(|ev| ev.detail.get("notes").is_some())
    }

    async fn execute(
        &self,
        ctx: &mut InvestigationContext,
        _tools: &ToolRegistry,
    ) -> Result<SkillOutcome, KernelError> {
        let entities = self
            .extract_from_context(ctx)
            .map_err(|e| KernelError::SkillFailed(e.to_string()))?;

        let entity_count = entities.len();

        ctx.evidence.push(rig_compose::context::Evidence::new(
            self.id(),
            "extracted_entities",
        ).with_detail(serde_json::json!({
            "entities": entities,
            "count": entity_count,
        })));

        if entity_count > 0 {
            ctx.signals.push(rig_compose::context::Signal::new("entities_extracted"));
        }

        info!(
            entity_count,
            "EntityExtractionSkill: execution complete"
        );

        Ok(SkillOutcome::noop().with_delta(if entity_count > 0 { 0.1 } else { 0.0 }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_tech_entity() {
        let skill = EntityExtractionSkill::new();
        let entities = skill.extract_entities(
            "I use Rust and Tokio for async programming.",
            "note-1",
        );
        assert!(entities.iter().any(|e| e.name == "Rust"));
        assert!(entities.iter().any(|e| e.name == "Tokio"));
    }

    #[test]
    fn test_extract_concept_from_heading() {
        let skill = EntityExtractionSkill::new();
        let entities = skill.extract_entities(
            "## System Design Patterns\n\nSome content here.",
            "note-2",
        );
        assert!(entities.iter().any(|e| e.name == "System Design Patterns"));
    }

    #[test]
    fn test_deduplicate_entities() {
        let skill = EntityExtractionSkill::new();
        let entities = vec![
            ExtractedEntity {
                name: "Rust".to_string(),
                category: EntityCategory::Technology,
                source_note_ids: vec!["note-1".to_string()],
            },
            ExtractedEntity {
                name: "rust".to_string(),
                category: EntityCategory::Technology,
                source_note_ids: vec!["note-2".to_string()],
            },
        ];
        let deduped = skill.deduplicate(entities);
        assert_eq!(deduped.len(), 1);
    }

    #[test]
    fn test_entity_category_from_name() {
        assert_eq!(EntityCategory::from_name("Rust"), EntityCategory::Technology);
        assert_eq!(EntityCategory::from_name("System Design"), EntityCategory::Concept);
        assert_eq!(EntityCategory::from_name("sqlite"), EntityCategory::Technology);
    }

    #[test]
    fn test_empty_content() {
        let skill = EntityExtractionSkill::new();
        let entities = skill.extract_entities("", "note-0");
        assert!(entities.is_empty());
    }

    #[test]
    fn test_no_tech_matches() {
        let skill = EntityExtractionSkill::new();
        let entities = skill.extract_entities(
            "Some random text without any known technologies.",
            "note-3",
        );
        assert!(entities.is_empty());
    }
}
