use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimpleNotion {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct ImportanceScore {
    pub notion_id: String,
    pub score: f64,
}

#[derive(Debug, Clone)]
pub struct NotionSummary {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub description: String,
    pub confidence: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelfModelLayer {
    Knowledge,
    Skill,
    SocialRole,
    SelfConcept,
    Trait,
    Motivation,
    Value,
    Limit,
}

impl std::fmt::Display for SelfModelLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            SelfModelLayer::Knowledge => "knowledge",
            SelfModelLayer::Skill => "skill",
            SelfModelLayer::SocialRole => "social_role",
            SelfModelLayer::SelfConcept => "self_concept",
            SelfModelLayer::Trait => "trait",
            SelfModelLayer::Motivation => "motivation",
            SelfModelLayer::Value => "value",
            SelfModelLayer::Limit => "limit",
        };
        write!(f, "{s}")
    }
}

impl std::str::FromStr for SelfModelLayer {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "knowledge" => Ok(SelfModelLayer::Knowledge),
            "skill" => Ok(SelfModelLayer::Skill),
            "social_role" => Ok(SelfModelLayer::SocialRole),
            "self_concept" => Ok(SelfModelLayer::SelfConcept),
            "trait" => Ok(SelfModelLayer::Trait),
            "motivation" => Ok(SelfModelLayer::Motivation),
            "value" => Ok(SelfModelLayer::Value),
            "limit" => Ok(SelfModelLayer::Limit),
            _ => Err(format!("invalid SelfModelLayer: {s}")),
        }
    }
}

#[async_trait]
pub trait NotionGraphProvider: Send + Sync {
    async fn upsert_entity(&self, notion: &SimpleNotion) -> anyhow::Result<()>;

    async fn insert_alias(&self, alias: &str, canonical_notion_id: &str) -> anyhow::Result<()>;

    async fn find_entity_by_name(&self, name: &str) -> anyhow::Result<Option<NotionSummary>>;

    async fn apply_confidence_decay(&self, half_life_days: f64) -> anyhow::Result<usize>;

    async fn auto_promote_entities(&self, threshold: i64) -> anyhow::Result<usize>;

    async fn compute_importance(
        &self,
        iterations: usize,
        damping: f64,
    ) -> anyhow::Result<Vec<ImportanceScore>>;

    async fn load_aliases(&self, notion_id: &str) -> anyhow::Result<Vec<String>>;

    fn is_available(&self) -> bool;
}
