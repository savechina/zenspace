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
    SelfBelieves,
    SelfAims,
    SelfCapableOf,
    SelfPartOf,
    ServesGoal,
    AlternativeTo,
    DecidedAbout,
    CorrectedBy,
}

impl std::fmt::Display for RelationType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            RelationType::DependsOn => "depends_on",
            RelationType::Implements => "implements",
            RelationType::RelatedTo => "related_to",
            RelationType::References => "references",
            RelationType::Contradicts => "contradicts",
            RelationType::Extends => "extends",
            RelationType::Uses => "uses",
            RelationType::Contains => "contains",
            RelationType::SelfBelieves => "self_believes",
            RelationType::SelfAims => "self_aims",
            RelationType::SelfCapableOf => "self_capable_of",
            RelationType::SelfPartOf => "self_part_of",
            RelationType::ServesGoal => "serves_goal",
            RelationType::AlternativeTo => "alternative_to",
            RelationType::DecidedAbout => "decided_about",
            RelationType::CorrectedBy => "corrected_by",
        };
        write!(f, "{s}")
    }
}

pub fn parse_relation_type(s: &str) -> Option<RelationType> {
    match s {
        "depends_on" => Some(RelationType::DependsOn),
        "implements" => Some(RelationType::Implements),
        "related_to" => Some(RelationType::RelatedTo),
        "references" => Some(RelationType::References),
        "contradicts" => Some(RelationType::Contradicts),
        "extends" => Some(RelationType::Extends),
        "uses" => Some(RelationType::Uses),
        "contains" => Some(RelationType::Contains),
        "self_believes" => Some(RelationType::SelfBelieves),
        "self_aims" => Some(RelationType::SelfAims),
        "self_capable_of" => Some(RelationType::SelfCapableOf),
        "self_part_of" => Some(RelationType::SelfPartOf),
        "serves_goal" => Some(RelationType::ServesGoal),
        "alternative_to" => Some(RelationType::AlternativeTo),
        "decided_about" => Some(RelationType::DecidedAbout),
        "corrected_by" => Some(RelationType::CorrectedBy),
        _ => None,
    }
}

impl RelationType {
    pub fn as_verb(&self) -> &str {
        match self {
            RelationType::DependsOn => "depends on",
            RelationType::Implements => "implements",
            RelationType::RelatedTo => "related to",
            RelationType::References => "references",
            RelationType::Contradicts => "contradicts",
            RelationType::Extends => "extends",
            RelationType::Uses => "uses",
            RelationType::Contains => "contains",
            RelationType::SelfBelieves => "self believes",
            RelationType::SelfAims => "self aims",
            RelationType::SelfCapableOf => "self capable of",
            RelationType::SelfPartOf => "self part of",
            RelationType::ServesGoal => "serves goal",
            RelationType::AlternativeTo => "alternative to",
            RelationType::DecidedAbout => "decided about",
            RelationType::CorrectedBy => "corrected by",
        }
    }
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
