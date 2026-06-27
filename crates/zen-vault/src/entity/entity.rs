use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::relationship::RelationType;

/// Types of entities that can be extracted from workspace content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EntityType {
    Function,
    Class,
    Module,
    Concept,
    Person,
    Organization,
    Event,
    Product,
    Technology,
    Other,
    SelfModel,
    Belief,
    Goal,
}

/// A typed entity extracted from workspace content (notes, code, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub id: String,
    pub name: String,
    pub entity_type: EntityType,
    pub description: String,
    pub source_note_id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub metadata: HashMap<String, String>,
}

impl Entity {
    pub fn new(
        name: impl Into<String>,
        entity_type: EntityType,
        source_note_id: impl Into<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            name: name.into(),
            entity_type,
            description: String::new(),
            source_note_id: source_note_id.into(),
            created_at: chrono::Utc::now(),
            metadata: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EntityData {
    pub entity: Entity,
    pub facts: Vec<String>,
    pub relationships: Vec<(String, RelationType)>,
}
