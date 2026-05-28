use serde::{Deserialize, Serialize};

/// Types of relationships between entities.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RelationType {
    DependsOn,
    Implements,
    RelatedTo,
    References,
    Contradicts,
    Extends,
    Uses,
    Contains,
}

/// A directed relationship between two entities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relationship {
    pub id: String,
    pub source_entity_id: String,
    pub target_entity_id: String,
    pub relation_type: RelationType,
    pub description: String,
    pub source_note_id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl Relationship {
    pub fn new(
        source_entity_id: impl Into<String>,
        target_entity_id: impl Into<String>,
        relation_type: RelationType,
        source_note_id: impl Into<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            source_entity_id: source_entity_id.into(),
            target_entity_id: target_entity_id.into(),
            relation_type,
            description: String::new(),
            source_note_id: source_note_id.into(),
            created_at: chrono::Utc::now(),
        }
    }
}
