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
    Path,
}

impl std::fmt::Display for EntityType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            EntityType::Function => "function",
            EntityType::Class => "class",
            EntityType::Module => "module",
            EntityType::Concept => "concept",
            EntityType::Person => "person",
            EntityType::Organization => "organization",
            EntityType::Event => "event",
            EntityType::Product => "product",
            EntityType::Technology => "technology",
            EntityType::Other => "other",
            EntityType::SelfModel => "self_model",
            EntityType::Belief => "belief",
            EntityType::Goal => "goal",
            EntityType::Path => "path",
        };
        write!(f, "{s}")
    }
}

pub fn parse_entity_type(s: &str) -> Option<EntityType> {
    match s {
        "function" => Some(EntityType::Function),
        "class" => Some(EntityType::Class),
        "module" => Some(EntityType::Module),
        "concept" => Some(EntityType::Concept),
        "person" => Some(EntityType::Person),
        "organization" => Some(EntityType::Organization),
        "event" => Some(EntityType::Event),
        "product" => Some(EntityType::Product),
        "technology" => Some(EntityType::Technology),
        "other" => Some(EntityType::Other),
        "self_model" => Some(EntityType::SelfModel),
        "belief" => Some(EntityType::Belief),
        "goal" => Some(EntityType::Goal),
        "path" => Some(EntityType::Path),
        _ => None,
    }
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
