use serde::{Deserialize, Serialize};

/// Types of relationships between notions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RelationKind {
    DependsOn,
    Implements,
    RelatedTo,
    References,
    Wikilinks,
    Contradicts,
    Extends,
    Uses,
    Contains,
    SelfBelieves,
    SelfAims,
    SelfCapableOf,
    SelfPartOf,
    ServesGoal,
    AlternativeTo,
    DecidedAbout,
    CorrectedBy,
    ExtractedFrom,
    Supports,
}

impl std::fmt::Display for RelationKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            RelationKind::DependsOn => "depends_on",
            RelationKind::Implements => "implements",
            RelationKind::RelatedTo => "related_to",
            RelationKind::References => "references",
            RelationKind::Wikilinks => "wikilinks",
            RelationKind::Contradicts => "contradicts",
            RelationKind::Extends => "extends",
            RelationKind::Uses => "uses",
            RelationKind::Contains => "contains",
            RelationKind::SelfBelieves => "self_believes",
            RelationKind::SelfAims => "self_aims",
            RelationKind::SelfCapableOf => "self_capable_of",
            RelationKind::SelfPartOf => "self_part_of",
            RelationKind::ServesGoal => "serves_goal",
            RelationKind::AlternativeTo => "alternative_to",
            RelationKind::DecidedAbout => "decided_about",
            RelationKind::CorrectedBy => "corrected_by",
            RelationKind::ExtractedFrom => "extracted_from",
            RelationKind::Supports => "supports",
        };
        write!(f, "{s}")
    }
}

pub fn parse_relation_type(s: &str) -> Option<RelationKind> {
    match s {
        "depends_on" => Some(RelationKind::DependsOn),
        "implements" => Some(RelationKind::Implements),
        "related_to" => Some(RelationKind::RelatedTo),
        "references" => Some(RelationKind::References),
        "wikilinks" | "wikilink" => Some(RelationKind::Wikilinks),
        "contradicts" => Some(RelationKind::Contradicts),
        "extends" => Some(RelationKind::Extends),
        "uses" => Some(RelationKind::Uses),
        "contains" => Some(RelationKind::Contains),
        "self_believes" => Some(RelationKind::SelfBelieves),
        "self_aims" => Some(RelationKind::SelfAims),
        "self_capable_of" => Some(RelationKind::SelfCapableOf),
        "self_part_of" => Some(RelationKind::SelfPartOf),
        "serves_goal" => Some(RelationKind::ServesGoal),
        "alternative_to" => Some(RelationKind::AlternativeTo),
        "decided_about" => Some(RelationKind::DecidedAbout),
        "corrected_by" => Some(RelationKind::CorrectedBy),
        "extracted_from" => Some(RelationKind::ExtractedFrom),
        "supports" => Some(RelationKind::Supports),
        _ => None,
    }
}

impl RelationKind {
    pub fn as_verb(&self) -> &str {
        match self {
            RelationKind::DependsOn => "depends on",
            RelationKind::Implements => "implements",
            RelationKind::RelatedTo => "related to",
            RelationKind::References => "references",
            RelationKind::Wikilinks => "links to",
            RelationKind::Contradicts => "contradicts",
            RelationKind::Extends => "extends",
            RelationKind::Uses => "uses",
            RelationKind::Contains => "contains",
            RelationKind::SelfBelieves => "self believes",
            RelationKind::SelfAims => "self aims",
            RelationKind::SelfCapableOf => "self capable of",
            RelationKind::SelfPartOf => "self part of",
            RelationKind::ServesGoal => "serves goal",
            RelationKind::AlternativeTo => "alternative to",
            RelationKind::DecidedAbout => "decided about",
            RelationKind::CorrectedBy => "corrected by",
            RelationKind::ExtractedFrom => "extracted from",
            RelationKind::Supports => "supports",
        }
    }
}

/// A directed relationship between two notions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relationship {
    pub id: String,
    pub source_notion_id: String,
    pub target_notion_id: String,
    pub relation_type: RelationKind,
    pub description: String,
    pub source_note_id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl Relationship {
    pub fn new(
        source_notion_id: impl Into<String>,
        target_notion_id: impl Into<String>,
        relation_type: RelationKind,
        source_note_id: impl Into<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            source_notion_id: source_notion_id.into(),
            target_notion_id: target_notion_id.into(),
            relation_type,
            description: String::new(),
            source_note_id: source_note_id.into(),
            created_at: chrono::Utc::now(),
        }
    }
}
