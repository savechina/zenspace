use sqlx::FromRow;

#[derive(FromRow)]
pub struct FtsResult {
    pub path: String,
    pub score: f64,
    pub snippet: String,
}

pub struct IndexNoteRequest<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub content: &'a str,
    pub tags: &'a str,
    pub file_path: &'a str,
    pub source: &'a str,
}

#[derive(FromRow)]
pub struct VecSearchResult {
    pub file: String,
    pub line: u32,
    pub content: String,
}

pub struct InsertNoteEmbeddingRequest<'a> {
    pub note_id: &'a str,
    pub embedding: &'a [f32],
}

pub struct InsertEntityEmbeddingRequest<'a> {
    pub entity_id: &'a str,
    pub embedding: &'a [f32],
}

#[derive(FromRow)]
pub struct EntityRow {
    pub id: String,
    pub name: String,
    pub entity_type: String,
    pub created_at: String,
    pub domain: Option<String>,
    pub last_updated: Option<String>,
    pub description: String,
    pub properties: String,
    pub access_count: i64,
    pub last_accessed_at: Option<String>,
    pub confidence: f64,
    pub source: String,
    pub promoted_at: Option<String>,
}

#[derive(FromRow)]
pub struct RelationshipRow {
    pub id: String,
    pub source_entity_id: String,
    pub target_entity_id: String,
    pub relation_type: String,
    pub confidence: f64,
    pub source_note_ids: Option<String>,
    pub created_at: String,
    pub description: String,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    pub recorded_at: Option<String>,
    pub weight: f64,
}

#[derive(FromRow)]
pub struct GraphSearchResult {
    pub entity: String,
    pub depth: u32,
    pub relation: String,
    pub target: String,
    pub source_entity: String,
    pub direction: String,
}

#[derive(FromRow, Clone)]
pub struct SelfNodeRow {
    pub id: String,
    pub name: String,
    pub layer: String,
    pub description: String,
    pub domain: String,
    pub is_explicit: Option<bool>,
    pub sufficient_for: Vec<String>,
    pub necessary_for: Vec<String>,
    pub controllability: Option<f64>,
    pub humility_score: Option<f64>,
    pub optionality_count: Option<i64>,
    pub core_pursuit: Option<String>,
    pub source: String,
    pub confidence: f64,
    pub evidence_refs: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(FromRow)]
pub struct GoalNodeRow {
    pub id: String,
    pub name: String,
    pub controllability: f64,
    pub core_pursuit: String,
    pub deadline: Option<String>,
}

#[derive(FromRow)]
pub struct PathNodeRow {
    pub id: String,
    pub name: String,
    pub serves_goal_id: Option<String>,
    pub is_default: bool,
    pub crowdedness: f64,
    pub alternatives: String,
}

#[derive(FromRow)]
pub struct BeliefNodeRow {
    pub id: String,
    pub name: String,
    pub proposition: String,
    pub prior: f64,
    pub posterior: f64,
    pub evidence_count: i64,
}

pub struct InsertRelationshipRequest<'a> {
    pub id: &'a str,
    pub source_id: &'a str,
    pub target_id: &'a str,
    pub rel_type: &'a str,
    pub confidence: f64,
    pub source_note_ids: Option<&'a str>,
    pub created_at: &'a str,
    pub description: Option<&'a str>,
    pub valid_from: Option<&'a str>,
    pub valid_until: Option<&'a str>,
    pub weight: Option<f64>,
}

pub struct UpsertGoalNodeRequest<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub controllability: f64,
    pub core_pursuit: &'a str,
    pub deadline: Option<&'a str>,
    pub now: &'a str,
}

pub struct UpsertPathNodeRequest<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub serves_goal_id: Option<&'a str>,
    pub is_default: bool,
    pub crowdedness: f64,
    pub alternatives: &'a str,
    pub now: &'a str,
}

pub struct UpsertBeliefNodeRequest<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub proposition: &'a str,
    pub prior: f64,
    pub posterior: f64,
    pub evidence_count: usize,
    pub now: &'a str,
}
