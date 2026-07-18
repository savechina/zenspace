use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::relationship::RelationKind;

/// Types of notions that can be extracted from workspace content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NotionKind {
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
    Decision,
}

impl std::fmt::Display for NotionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            NotionKind::Function => "function",
            NotionKind::Class => "class",
            NotionKind::Module => "module",
            NotionKind::Concept => "concept",
            NotionKind::Person => "person",
            NotionKind::Organization => "organization",
            NotionKind::Event => "event",
            NotionKind::Product => "product",
            NotionKind::Technology => "technology",
            NotionKind::Other => "other",
            NotionKind::SelfModel => "self_model",
            NotionKind::Belief => "belief",
            NotionKind::Goal => "goal",
            NotionKind::Path => "path",
            NotionKind::Decision => "decision",
        };
        write!(f, "{s}")
    }
}

pub fn parse_kind(s: &str) -> Option<NotionKind> {
    match s {
        "function" => Some(NotionKind::Function),
        "class" => Some(NotionKind::Class),
        "module" => Some(NotionKind::Module),
        "concept" => Some(NotionKind::Concept),
        "person" => Some(NotionKind::Person),
        "organization" => Some(NotionKind::Organization),
        "event" => Some(NotionKind::Event),
        "product" => Some(NotionKind::Product),
        "technology" => Some(NotionKind::Technology),
        "other" => Some(NotionKind::Other),
        "self_model" => Some(NotionKind::SelfModel),
        "belief" => Some(NotionKind::Belief),
        "goal" => Some(NotionKind::Goal),
        "path" => Some(NotionKind::Path),
        "decision" => Some(NotionKind::Decision),
        _ => None,
    }
}

/// A typed notion extracted from workspace content (notes, code, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notion {
    pub id: String,
    pub name: String,
    pub kind: NotionKind,
    pub description: String,
    pub source_note_id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_updated: chrono::DateTime<chrono::Utc>,
    pub domain: Option<String>,
    pub aliases: Vec<String>,
    pub metadata: HashMap<String, String>,
    /// OKF resource URI — objective referent (Layer 1: Ontological)
    pub subject_uri: Option<String>,
    /// OKF tags — thematic grouping (lightweight, not graph nodes)
    pub topics: Vec<String>,
}

impl Notion {
    pub fn new(
        name: impl Into<String>,
        kind: NotionKind,
        source_note_id: impl Into<String>,
    ) -> Self {
        let now = chrono::Utc::now();
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            name: name.into(),
            kind,
            description: String::new(),
            source_note_id: source_note_id.into(),
            created_at: now,
            last_updated: now,
            domain: None,
            aliases: Vec::new(),
            metadata: HashMap::new(),
            subject_uri: None,
            topics: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NotionData {
    pub notion: Notion,
    pub facts: Vec<String>,
    pub relationships: Vec<(String, RelationKind)>,
}
