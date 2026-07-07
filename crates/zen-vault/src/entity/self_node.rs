use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub use zen_core::entity_graph::SelfModelLayer;

/// A self-model node with 6-layer introspective typing.
/// Stored in the dedicated self_nodes table, separate from EntityNode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelfNode {
    pub id: String,
    pub name: String,
    pub layer: SelfModelLayer,
    pub description: String,
    pub domain: String,

    pub is_explicit: Option<bool>,      // Knowledge
    pub sufficient_for: Vec<String>,    // Skill
    pub necessary_for: Vec<String>,     // Skill
    pub controllability: Option<f64>,   // SocialRole
    pub humility_score: Option<f64>,    // SelfConcept
    pub optionality_count: Option<u32>, // Trait
    pub core_pursuit: Option<String>,   // Motivation

    pub source: String,
    pub confidence: f64,
    pub evidence_refs: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl SelfNode {
    pub fn new(id: String, layer: SelfModelLayer, name: String, domain: String) -> Self {
        let now = Utc::now();
        Self {
            id,
            layer,
            name,
            description: String::new(),
            domain,
            is_explicit: None,
            sufficient_for: Vec::new(),
            necessary_for: Vec::new(),
            controllability: None,
            humility_score: None,
            optionality_count: None,
            core_pursuit: None,
            source: "manual".to_string(),
            confidence: 0.5,
            evidence_refs: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }
}
