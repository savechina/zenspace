use anyhow::Result;
use std::collections::HashSet;

use zen_repo::{
    BeliefsRepo, NotionsRepo, GoalsRepo, SelfModelRepo, SqliteClient,
    types::{
        InsertRelationshipRequest, UpsertBeliefNodeRequest, UpsertGoalNodeRequest,
        UpsertPathNodeRequest,
    },
};

use crate::maintenance::compute_embeddings_for_text;
use crate::search::Tier3Search;

use super::notion::{Notion, NotionKind};
use super::relationship::{RelationKind, Relationship};

/// Normalize an notion name for canonical matching.
/// Rules: lowercase, trim, strip common suffixes (.js, .rs, .py, -lang, " language").
use unicode_normalization::UnicodeNormalization;

fn normalize_notion_name(name: &str) -> String {
    let nfc: String = name.nfc().collect();
    let mut s = nfc.trim().to_lowercase();
    for suffix in [".js", ".rs", ".py", ".ts", "-lang", " lang", " language"] {
        if s.ends_with(suffix) {
            s = s[..s.len() - suffix.len()].trim_end().to_string();
            break;
        }
    }
    s
}

/// Parse an notion type string (as stored in graph.db) back into the enum variant.
fn parse_kind(s: &str) -> NotionKind {
    match s {
        "Function" => NotionKind::Function,
        "Class" => NotionKind::Class,
        "Module" => NotionKind::Module,
        "Concept" => NotionKind::Concept,
        "Person" => NotionKind::Person,
        "Organization" => NotionKind::Organization,
        "Event" => NotionKind::Event,
        "Product" => NotionKind::Product,
        "Technology" => NotionKind::Technology,
        "SelfModel" => NotionKind::SelfModel,
        "Belief" => NotionKind::Belief,
        "Goal" => NotionKind::Goal,
        "Path" => NotionKind::Path,
        "Decision" => NotionKind::Decision,
        _ => NotionKind::Other,
    }
}

/// Convert a `RelationshipRow` string to a `RelationKind` enum.
fn relation_type_from_str(s: &str) -> RelationKind {
    match s {
        "DependsOn" => RelationKind::DependsOn,
        "Implements" => RelationKind::Implements,
        "RelatedTo" => RelationKind::RelatedTo,
        "References" => RelationKind::References,
        "Contradicts" => RelationKind::Contradicts,
        "Extends" => RelationKind::Extends,
        "Uses" => RelationKind::Uses,
        "Contains" => RelationKind::Contains,
        "SelfBelieves" => RelationKind::SelfBelieves,
        "SelfAims" => RelationKind::SelfAims,
        "SelfCapableOf" => RelationKind::SelfCapableOf,
        "SelfPartOf" => RelationKind::SelfPartOf,
        "ServesGoal" => RelationKind::ServesGoal,
        "AlternativeTo" => RelationKind::AlternativeTo,
        "DecidedAbout" => RelationKind::DecidedAbout,
        "CorrectedBy" => RelationKind::CorrectedBy,
        "ExtractedFrom" => RelationKind::ExtractedFrom,
        "Supports" => RelationKind::Supports,
        _ => RelationKind::RelatedTo,
    }
}

/// Convert an `NotionRow` from the repo to the domain `Notion` type.
fn entity_row_to_entity(row: zen_repo::types::NotionRow) -> Notion {
    let kind = parse_kind(&row.kind);
    let created_at = chrono::DateTime::parse_from_rfc3339(&row.created_at)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now());
    let last_updated = row
        .last_updated
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or(created_at);

    let mut notion = Notion::new(row.name, kind, "graph-db");
    notion.id = row.id;
    notion.created_at = created_at;
    notion.last_updated = last_updated;
    notion.domain = row.domain;
    notion.description = row.description;
    notion
}

/// Convert a `SelfNodeRow` from the repo to the domain `SelfNode` type.
fn self_node_row_to_self_node(row: zen_repo::types::SelfNodeRow) -> super::self_node::SelfNode {
    let layer = row
        .layer
        .parse()
        .unwrap_or(super::self_node::SelfModelLayer::Knowledge);

    let created_at = chrono::DateTime::parse_from_rfc3339(&row.created_at)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now());
    let updated_at = chrono::DateTime::parse_from_rfc3339(&row.updated_at)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now());

    super::self_node::SelfNode {
        id: row.id,
        name: row.name,
        layer,
        description: row.description,
        domain: row.domain,
        is_explicit: row.is_explicit,
        sufficient_for: row.sufficient_for,
        necessary_for: row.necessary_for,
        controllability: row.controllability,
        humility_score: row.humility_score,
        optionality_count: row.optionality_count.map(|v| v as u32),
        core_pursuit: row.core_pursuit,
        source: row.source,
        confidence: row.confidence,
        evidence_refs: row.evidence_refs,
        created_at,
        updated_at,
    }
}

/// Convert a domain `SelfNode` to a `SelfNodeRow` for the repo.
fn self_node_to_row(node: &super::self_node::SelfNode) -> zen_repo::types::SelfNodeRow {
    zen_repo::types::SelfNodeRow {
        id: node.id.clone(),
        name: node.name.clone(),
        layer: node.layer.to_string(),
        description: node.description.clone(),
        domain: node.domain.clone(),
        is_explicit: node.is_explicit,
        sufficient_for: node.sufficient_for.clone(),
        necessary_for: node.necessary_for.clone(),
        controllability: node.controllability,
        humility_score: node.humility_score,
        optionality_count: node.optionality_count.map(|v| v as i64),
        core_pursuit: node.core_pursuit.clone(),
        source: node.source.clone(),
        confidence: node.confidence,
        evidence_refs: node.evidence_refs.clone(),
        created_at: node.created_at.to_rfc3339(),
        updated_at: node.updated_at.to_rfc3339(),
    }
}

/// Service for extracting notions and relationships from workspace content.
pub struct NotionService;

impl Default for NotionService {
    fn default() -> Self {
        Self::new()
    }
}

impl NotionService {
    pub fn new() -> Self {
        Self
    }

    /// Extract relationships between co-occurring notions.
    ///
    /// Creates RELATED_TO relationships for every pair of notions.
    /// Future enhancement: use LLM to classify relationship types
    /// (e.g., USES, DEPENDS_ON, IMPLEMENTS).
    pub fn extract_relationships(&self, notions: &[Notion]) -> Result<Vec<Relationship>> {
        let mut relationships = Vec::new();
        let now = chrono::Utc::now();

        for i in 0..notions.len() {
            for j in (i + 1)..notions.len() {
                relationships.push(Relationship {
                    id: uuid::Uuid::now_v7().to_string(),
                    source_notion_id: notions[i].id.clone(),
                    target_notion_id: notions[j].id.clone(),
                    relation_type: RelationKind::RelatedTo,
                    description: format!(
                        "{} relates to {}",
                        notions[i].name, notions[j].name
                    ),
                    source_note_id: notions[i].source_note_id.clone(),
                    created_at: now,
                });
            }
        }
        Ok(relationships)
    }

    /// Load all known notion names from graph.db.
    /// Returns canonical names AND known aliases.
    /// Returns an empty set if the database does not exist yet.
    pub async fn load_known_notion_names(&self, client: &SqliteClient) -> Result<HashSet<String>> {
        let names = NotionsRepo::new(client).load_known_notion_names().await?;
        Ok(names.into_iter().collect())
    }

    /// Upsert an notion into graph.db.
    /// Normalizes the name and checks aliases before insert.
    /// Ensures the schema exists before writing.
    pub async fn upsert_entity(&self, client: &SqliteClient, notion: &Notion) -> Result<()> {
        let repo = NotionsRepo::new(client);

        let canonical_name = normalize_notion_name(&notion.name);
        let type_str = format!("{:?}", notion.kind);
        let created_at = notion.created_at.to_rfc3339();
        let last_updated = chrono::Utc::now().to_rfc3339();

        // Check if an alias already maps to a canonical notion
        let existing_canonical = repo.resolve_alias(&canonical_name).await?;

        if let Some(canonical_id) = existing_canonical {
            // Alias found — just update last_updated on the canonical notion
            repo.update_entity_timestamp(&canonical_id, &last_updated).await?;
            return Ok(());
        }

        // No alias match — insert the notion with normalized name
        repo.upsert_entity_with(
            &notion.id,
            &canonical_name,
            &type_str,
            &created_at,
            &last_updated,
            &notion.description,
            &notion.source_note_id,
            0.5,
        )
        .await?;

        // Register the alias (INSERT OR IGNORE in case of concurrent insert)
        repo.insert_alias(&canonical_name, &notion.id).await?;

        Ok(())
    }

    /// Insert a relationship into graph.db.
    /// Confidence defaults to 0.8 for auto-extracted edges.
    pub async fn insert_relationship(&self, client: &SqliteClient, rel: &Relationship) -> Result<()> {
        let type_str = format!("{:?}", rel.relation_type);
        let created = rel.created_at.to_rfc3339();

        NotionsRepo::new(client)
            .insert_relationship(&InsertRelationshipRequest {
                id: &rel.id,
                source_id: &rel.source_notion_id,
                target_id: &rel.target_notion_id,
                rel_type: &type_str,
                confidence: 0.8,
                source_note_ids: Some(&rel.source_note_id),
                created_at: &created,
                description: Some(&rel.description),
                valid_from: None,
                valid_until: None,
                weight: None,
            })
            .await?;
        Ok(())
    }

    /// Compute a 384-dim embedding for notion text and store in state.db.
    /// Falls back to hash-based embedding when no LLM provider is available.
    /// Fails gracefully (returns Err) if the vec0 extension is not loaded.
    pub async fn store_entity_embedding(
        &self,
        client: &zen_repo::SqliteClient,
        notion_id: &str,
        text: &str,
    ) -> Result<()> {
        let embedding = compute_embeddings_for_text(text)?;
        Tier3Search
            .insert_entity_embedding(client, notion_id, &embedding)
            .await
    }

    pub async fn load_all_entities(&self, client: &SqliteClient) -> Result<Vec<Notion>> {
        let rows = NotionsRepo::new(client).load_all_entities().await?;
        Ok(rows.into_iter().map(entity_row_to_entity).collect())
    }

    pub async fn run_graph_maintenance(
        &self,
        client: &SqliteClient,
    ) -> Result<(usize, usize, Vec<String>)> {
        let repo = NotionsRepo::new(client);
        let decayed = repo.apply_confidence_decay(30.0).await?;
        let promoted = repo.auto_promote_entities(3).await?;
        let scores = repo.compute_importance(40, 0.85).await?;
        let top = scores.iter().take(5).map(|s| s.notion.clone()).collect();
        Ok((decayed, promoted, top))
    }

    /// Load notions that have been updated since the given timestamp.
    /// Used by incremental wiki compilation.
    pub async fn load_entities_updated_since(
        &self,
        client: &SqliteClient,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<Notion>> {
        let since_str = since.to_rfc3339();
        let rows = NotionsRepo::new(client)
            .load_entities_updated_since(&since_str)
            .await?;
        Ok(rows.into_iter().map(entity_row_to_entity).collect())
    }

    pub async fn load_relationships_for_entity(
        &self,
        client: &SqliteClient,
        notion_id: &str,
    ) -> Result<Vec<(String, RelationKind)>> {
        let repo = NotionsRepo::new(client);
        let rows = repo.load_relationships(notion_id).await?;

        let mut result = Vec::new();
        for row in rows {
            let rt = relation_type_from_str(&row.relation_type);
            let target_name = repo
                .notion_name(&row.target_notion_id)
                .await?
                .unwrap_or(row.target_notion_id);
            result.push((target_name, rt));
        }
        Ok(result)
    }

    /// Upserts a Self-Model item as a graph notion.
    /// This enables graph-based queries for self-knowledge.
    ///
    /// `layer` is one of: "Knowledge", "Skill", "SocialRole", "SelfConcept", "Trait", "Motivation"
    pub async fn upsert_self_model_entity(
        &self,
        client: &SqliteClient,
        id: &str,
        name: &str,
        layer: &str,
        domain: Option<&str>,
    ) -> Result<()> {
        let mut notion = Notion::new(name, NotionKind::SelfModel, "self-model");
        notion.id = id.to_string();
        notion
            .metadata
            .insert("layer".to_string(), layer.to_string());
        if let Some(d) = domain {
            notion
                .metadata
                .insert("domain".to_string(), d.to_string());
        }
        self.upsert_entity(client, &notion).await
    }

    /// Upserts a SelfNode into the dedicated self_nodes table.
    /// This is the Phase C3 implementation with typed columns for 6-layer introspective typing.
    pub async fn upsert_self_node(
        &self,
        client: &SqliteClient,
        node: &super::self_node::SelfNode,
    ) -> Result<()> {
        let row = self_node_to_row(node);
        SelfModelRepo::new(client).upsert(&row).await?;
        Ok(())
    }

    /// Loads all SelfNodes from the self_nodes table.
    pub async fn load_self_nodes(
        &self,
        client: &SqliteClient,
    ) -> Result<Vec<super::self_node::SelfNode>> {
        let rows = SelfModelRepo::new(client).load_all().await?;
        Ok(rows.into_iter().map(self_node_row_to_self_node).collect())
    }

    /// Loads SelfNodes filtered by layer from the self_nodes table.
    pub async fn load_self_nodes_by_layer(
        &self,
        client: &SqliteClient,
        layer: &super::self_node::SelfModelLayer,
    ) -> Result<Vec<super::self_node::SelfNode>> {
        let rows = SelfModelRepo::new(client)
            .load_by_layer(&layer.to_string())
            .await?;
        Ok(rows.into_iter().map(self_node_row_to_self_node).collect())
    }

    pub async fn upsert_goal_node(
        &self,
        client: &SqliteClient,
        id: &str,
        name: &str,
        controllability: f64,
        core_pursuit: &str,
        deadline: Option<&str>,
    ) -> Result<()> {
        let mut notion = Notion::new(name, NotionKind::Goal, "goal-model");
        notion.id = id.to_string();
        notion
            .metadata
            .insert("controllability".to_string(), controllability.to_string());
        notion
            .metadata
            .insert("core_pursuit".to_string(), core_pursuit.to_string());
        if let Some(d) = deadline {
            notion
                .metadata
                .insert("deadline".to_string(), d.to_string());
        }
        self.upsert_entity(client, &notion).await?;

        // Also write to dedicated goal_nodes table
        let now = chrono::Utc::now().to_rfc3339();
        GoalsRepo::new(client)
            .upsert_goal(&UpsertGoalNodeRequest {
                id,
                name,
                controllability,
                core_pursuit,
                deadline,
                now: &now,
            })
            .await?;

        Ok(())
    }

    pub async fn upsert_decision_node(
        &self,
        client: &SqliteClient,
        id: &str,
        name: &str,
        goal: &str,
        choice: &str,
        outcome_status: Option<&str>,
    ) -> Result<()> {
        let mut notion = Notion::new(name, NotionKind::Decision, "decision-model");
        notion.id = id.to_string();
        notion
            .metadata
            .insert("goal".to_string(), goal.to_string());
        notion
            .metadata
            .insert("choice".to_string(), choice.to_string());
        if let Some(status) = outcome_status {
            notion
                .metadata
                .insert("outcome_status".to_string(), status.to_string());
        }
        self.upsert_entity(client, &notion).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_belief_node(
        &self,
        client: &SqliteClient,
        id: &str,
        name: &str,
        proposition: &str,
        prior: f64,
        posterior: f64,
        evidence_count: usize,
    ) -> Result<()> {
        let mut notion = Notion::new(name, NotionKind::Belief, "belief-model");
        notion.id = id.to_string();
        notion
            .metadata
            .insert("proposition".to_string(), proposition.to_string());
        notion
            .metadata
            .insert("prior".to_string(), prior.to_string());
        notion
            .metadata
            .insert("posterior".to_string(), posterior.to_string());
        notion
            .metadata
            .insert("evidence_count".to_string(), evidence_count.to_string());
        self.upsert_entity(client, &notion).await?;

        // Also write to dedicated belief_nodes table
        let now = chrono::Utc::now().to_rfc3339();
        BeliefsRepo::new(client)
            .upsert(&UpsertBeliefNodeRequest {
                id,
                name,
                proposition,
                prior,
                posterior,
                evidence_count,
                now: &now,
            })
            .await?;

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_path_node(
        &self,
        client: &SqliteClient,
        id: &str,
        name: &str,
        serves_goal: &str,
        is_default: bool,
        crowdedness: f64,
        alternatives: &str,
    ) -> Result<()> {
        let mut notion = Notion::new(name, NotionKind::Path, "path-model");
        notion.id = id.to_string();
        notion
            .metadata
            .insert("serves_goal".to_string(), serves_goal.to_string());
        notion
            .metadata
            .insert("is_default".to_string(), is_default.to_string());
        notion
            .metadata
            .insert("crowdedness".to_string(), crowdedness.to_string());
        notion
            .metadata
            .insert("alternatives".to_string(), alternatives.to_string());
        self.upsert_entity(client, &notion).await?;

        // Also write to dedicated path_nodes table
        let goal_entities = self.load_all_entities(client).await?;
        let goal_id = goal_entities
            .iter()
            .find(|e| e.name == normalize_notion_name(serves_goal) && e.kind == NotionKind::Goal)
            .map(|e| e.id.clone());

        let now = chrono::Utc::now().to_rfc3339();
        GoalsRepo::new(client)
            .upsert_path(&UpsertPathNodeRequest {
                id,
                name,
                serves_goal_id: goal_id.as_deref(),
                is_default,
                crowdedness,
                alternatives,
                now: &now,
            })
            .await?;

        // Create ServesGoal relationship from this path to the goal
        if let Some(goal) = goal_entities.iter().find(|e| e.name == normalize_notion_name(serves_goal)) {
            let rel = Relationship::new(id, goal.id.clone(), RelationKind::ServesGoal, "path-model");
            self.insert_relationship(client, &rel).await?;
        }

        Ok(())
    }

    /// Loads a goal node from the goal_nodes table.
    #[allow(clippy::type_complexity)]
    pub async fn load_goal_node(
        &self,
        client: &SqliteClient,
        id: &str,
    ) -> Result<Option<(String, f64, String, Option<String>)>> {
        let row = GoalsRepo::new(client).load_goal(id).await?;
        Ok(row.map(|r| (r.name, r.controllability, r.core_pursuit, r.deadline)))
    }

    /// Loads a path node from the path_nodes table.
    #[allow(clippy::type_complexity)]
    pub async fn load_path_node(
        &self,
        client: &SqliteClient,
        id: &str,
    ) -> Result<Option<(String, String, bool, f64, String)>> {
        let row = GoalsRepo::new(client).load_path(id).await?;
        Ok(row.map(|r| (r.name, r.serves_goal_id.unwrap_or_default(), r.is_default, r.crowdedness, r.alternatives)))
    }

    /// Loads a belief node from the belief_nodes table.
    #[allow(clippy::type_complexity)]
    pub async fn load_belief_node(
        &self,
        client: &SqliteClient,
        id: &str,
    ) -> Result<Option<(String, String, f64, f64, usize)>> {
        let row = BeliefsRepo::new(client).load(id).await?;
        Ok(row.map(|r| (r.name, r.proposition, r.prior, r.posterior, r.evidence_count as usize)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_normalize_notion_name() {
        assert_eq!(normalize_notion_name("Rust"), "rust");
        assert_eq!(normalize_notion_name("Rust Lang"), "rust");
        assert_eq!(normalize_notion_name("JavaScript.js"), "javascript");
        assert_eq!(normalize_notion_name("  Python  "), "python");
        assert_eq!(normalize_notion_name("TypeScript.ts"), "typescript");
        assert_eq!(normalize_notion_name("Go-lang"), "go");
        assert_eq!(normalize_notion_name("C Language"), "c");
        assert_eq!(normalize_notion_name("rust language"), "rust");
    }

    #[test]
    fn test_normalize_notion_name_nfc() {
        // Combining accent (U+0301) vs precomposed (U+00E9) → same NFC output
        let combining = "cafe\u{0301}";
        let precomposed = "caf\u{00E9}";
        assert_eq!(normalize_notion_name(combining), normalize_notion_name(precomposed));
    }

    #[tokio::test]
    async fn test_alias_resolution() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();

        let service = NotionService::new();

        // Create first notion with name "Rust"
        let mut entity1 = Notion::new("Rust", super::super::notion::NotionKind::Technology, "note1");
        entity1.id = "notion-rust-1".to_string();
        service.upsert_entity(&client, &entity1).await.unwrap();

        // Create second notion with name "rust" — should resolve to the same notion
        let mut entity2 = Notion::new("rust", super::super::notion::NotionKind::Technology, "note2");
        entity2.id = "notion-rust-2".to_string();
        service.upsert_entity(&client, &entity2).await.unwrap();

        // Verify: only one notion exists (the canonical one)
        let notions_repo = NotionsRepo::new(&client);
        let notions = notions_repo.load_all_entities().await.unwrap();
        let rust_count = notions.iter().filter(|e| e.name == "rust").count();
        assert_eq!(rust_count, 1);

        // Verify alias exists
        let alias_resolved = notions_repo.resolve_alias("rust").await.unwrap();
        assert!(alias_resolved.is_some());

        // Verify load_known_notion_names includes the canonical name
        let names = service.load_known_notion_names(&client).await.unwrap();
        assert!(names.contains("rust"));
    }

    #[test]
    fn test_parse_kind_self_model() {
        let parsed = parse_kind("SelfModel");
        assert_eq!(parsed, NotionKind::SelfModel);

        let parsed_belief = parse_kind("Belief");
        assert_eq!(parsed_belief, NotionKind::Belief);

        let parsed_goal = parse_kind("Goal");
        assert_eq!(parsed_goal, NotionKind::Goal);
    }

    #[test]
    fn test_parse_kind_unknown_falls_back_to_other() {
        let parsed = parse_kind("UnknownType");
        assert_eq!(parsed, NotionKind::Other);
    }

    #[test]
    fn test_kind_self_model_debug_roundtrip() {
        let et = NotionKind::SelfModel;
        let debug_str = format!("{:?}", et);
        assert_eq!(debug_str, "SelfModel");
        let reparsed = parse_kind(&debug_str);
        assert_eq!(reparsed, et);
    }

    #[test]
    fn test_relation_type_self_model_variants() {
        let variants = [
            RelationKind::SelfBelieves,
            RelationKind::SelfAims,
            RelationKind::SelfCapableOf,
            RelationKind::SelfPartOf,
        ];
        let expected = ["SelfBelieves", "SelfAims", "SelfCapableOf", "SelfPartOf"];
        for (variant, name) in variants.iter().zip(expected.iter()) {
            assert_eq!(format!("{:?}", variant), *name);
        }
    }

    #[test]
    fn test_relation_type_self_believes_roundtrip() {
        let rel = RelationKind::SelfBelieves;
        let s = format!("{:?}", rel);
        assert_eq!(s, "SelfBelieves");

        let roundtripped = match s.as_str() {
            "SelfBelieves" => RelationKind::SelfBelieves,
            _ => panic!("unexpected relation type"),
        };
        assert_eq!(roundtripped, RelationKind::SelfBelieves);
    }

    #[tokio::test]
    async fn test_upsert_self_model_entity() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = NotionService::new();

        service
            .upsert_self_model_entity(
                &client,
                "sm-1",
                "Rust Proficiency",
                "Skill",
                Some("programming"),
            )
            .await
            .unwrap();

        let notions = service.load_all_entities(&client).await.unwrap();
        assert_eq!(notions.len(), 1);
        assert_eq!(notions[0].id, "sm-1");
        assert_eq!(notions[0].name, "rust proficiency");
        assert_eq!(notions[0].kind, NotionKind::SelfModel);
    }

    #[tokio::test]
    async fn test_upsert_self_model_entity_without_domain() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = NotionService::new();

        service
            .upsert_self_model_entity(
                &client,
                "sm-2",
                "Curiosity",
                "Trait",
                None,
            )
            .await
            .unwrap();

        let notions = service.load_all_entities(&client).await.unwrap();
        assert_eq!(notions.len(), 1);
        assert_eq!(notions[0].kind, NotionKind::SelfModel);
        assert_eq!(notions[0].name, "curiosity");

        let notions_repo = NotionsRepo::new(&client);
        let all = notions_repo.load_all_entities().await.unwrap();
        let stored = all.iter().find(|e| e.id == "sm-2").unwrap();
        assert_eq!(stored.kind, "SelfModel");
    }

    #[tokio::test]
    async fn test_upsert_self_model_notion_idempotent() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = NotionService::new();

        service
            .upsert_self_model_entity(&client, "sm-3", "Writing", "Skill", None)
            .await
            .unwrap();
        service
            .upsert_self_model_entity(&client, "sm-3", "Writing", "Skill", None)
            .await
            .unwrap();

        let notions_repo = NotionsRepo::new(&client);
        let all = notions_repo.load_all_entities().await.unwrap();
        let count = all.iter().filter(|e| e.id == "sm-3").count();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_self_model_entity_loads_correct_type() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = NotionService::new();

        service
            .upsert_self_model_entity(&client, "sm-4", "Empathy", "Trait", Some("social"))
            .await
            .unwrap();

        let notions = service.load_all_entities(&client).await.unwrap();
        let notion = &notions[0];
        assert_eq!(notion.kind, NotionKind::SelfModel);

        let type_str = format!("{:?}", notion.kind);
        let reparsed = parse_kind(&type_str);
        assert_eq!(reparsed, NotionKind::SelfModel);
    }

    #[tokio::test]
    async fn test_upsert_goal_node() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = NotionService::new();

        service
            .upsert_goal_node(&client, "g-1", "Learn Rust", 0.8, "mastery", Some("2026-12-31"))
            .await
            .unwrap();

        let notions = service.load_all_entities(&client).await.unwrap();
        assert_eq!(notions.len(), 1);
        assert_eq!(notions[0].id, "g-1");
        assert_eq!(notions[0].name, "learn rust");
        assert_eq!(notions[0].kind, NotionKind::Goal);
    }

    #[tokio::test]
    async fn test_upsert_goal_node_without_deadline() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = NotionService::new();

        service
            .upsert_goal_node(&client, "g-2", "Ship Feature", 0.5, "delivery", None)
            .await
            .unwrap();

        let notions = service.load_all_entities(&client).await.unwrap();
        assert_eq!(notions.len(), 1);
        assert_eq!(notions[0].kind, NotionKind::Goal);
    }

    #[tokio::test]
    async fn test_upsert_goal_node_metadata() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = NotionService::new();

        service
            .upsert_goal_node(&client, "g-3", "Run Marathon", 0.9, "fitness", Some("2027-06-01"))
            .await
            .unwrap();

        let notions_repo = NotionsRepo::new(&client);
        let all = notions_repo.load_all_entities().await.unwrap();
        let stored = all.iter().find(|e| e.id == "g-3").unwrap();
        assert_eq!(stored.kind, "Goal");
    }

    #[tokio::test]
    async fn test_upsert_goal_node_idempotent() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = NotionService::new();

        service
            .upsert_goal_node(&client, "g-4", "Write Book", 0.6, "creative", None)
            .await
            .unwrap();
        service
            .upsert_goal_node(&client, "g-4", "Write Book", 0.6, "creative", None)
            .await
            .unwrap();

        let notions_repo = NotionsRepo::new(&client);
        let all = notions_repo.load_all_entities().await.unwrap();
        let count = all.iter().filter(|e| e.id == "g-4").count();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_upsert_path_node() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = NotionService::new();

        service
            .upsert_goal_node(&client, "g-5", "Learn Rust", 0.8, "mastery", None)
            .await
            .unwrap();

        service
            .upsert_path_node(&client, "p-1", "Online Courses", "Learn Rust", true, 0.3, "")
            .await
            .unwrap();

        let notions = service.load_all_entities(&client).await.unwrap();
        assert_eq!(notions.len(), 2);
        let path = notions.iter().find(|e| e.id == "p-1").unwrap();
        assert_eq!(path.kind, NotionKind::Path);
        assert_eq!(path.name, "online courses");
    }

    #[tokio::test]
    async fn test_upsert_path_node_creates_serves_goal_relationship() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = NotionService::new();

        service
            .upsert_goal_node(&client, "g-6", "Master Cooking", 0.7, "hobby", None)
            .await
            .unwrap();
        service
            .upsert_path_node(&client, "p-2", "Cooking Classes", "Master Cooking", false, 0.8, "")
            .await
            .unwrap();

        let rels = service.load_relationships_for_entity(&client, "p-2").await.unwrap();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].1, RelationKind::ServesGoal);
        assert_eq!(rels[0].0, "master cooking");
    }

    #[tokio::test]
    async fn test_upsert_path_node_without_existing_goal() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = NotionService::new();

        service
            .upsert_path_node(&client, "p-3", "Mentorship", "Nonexistent Goal", true, 0.5, "")
            .await
            .unwrap();

        let notions = service.load_all_entities(&client).await.unwrap();
        assert_eq!(notions.len(), 1);
        assert_eq!(notions[0].kind, NotionKind::Path);

        let rels = service.load_relationships_for_entity(&client, "p-3").await.unwrap();
        assert!(rels.is_empty());
    }

    #[tokio::test]
    async fn test_upsert_path_node_idempotent() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = NotionService::new();

        service
            .upsert_goal_node(&client, "g-7", "Get Fit", 0.9, "health", None)
            .await
            .unwrap();
        service
            .upsert_path_node(&client, "p-4", "Gym", "Get Fit", true, 0.6, "")
            .await
            .unwrap();
        service
            .upsert_path_node(&client, "p-4", "Gym", "Get Fit", true, 0.6, "")
            .await
            .unwrap();

        let notions_repo = NotionsRepo::new(&client);
        let all = notions_repo.load_all_entities().await.unwrap();
        let count = all.iter().filter(|e| e.id == "p-4").count();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_new_relation_types_display() {
        use crate::notion::relationship::{parse_relation_type, RelationKind};

        assert_eq!(RelationKind::ServesGoal.to_string(), "serves_goal");
        assert_eq!(RelationKind::AlternativeTo.to_string(), "alternative_to");
        assert_eq!(RelationKind::DecidedAbout.to_string(), "decided_about");
        assert_eq!(RelationKind::CorrectedBy.to_string(), "corrected_by");

        assert_eq!(parse_relation_type("serves_goal"), Some(RelationKind::ServesGoal));
        assert_eq!(parse_relation_type("alternative_to"), Some(RelationKind::AlternativeTo));
        assert_eq!(parse_relation_type("decided_about"), Some(RelationKind::DecidedAbout));
        assert_eq!(parse_relation_type("corrected_by"), Some(RelationKind::CorrectedBy));
        assert_eq!(parse_relation_type("bogus"), None);
    }

    #[test]
    fn test_new_relation_types_as_verb() {
        use crate::notion::relationship::RelationKind;

        assert_eq!(RelationKind::ServesGoal.as_verb(), "serves goal");
        assert_eq!(RelationKind::AlternativeTo.as_verb(), "alternative to");
        assert_eq!(RelationKind::DecidedAbout.as_verb(), "decided about");
        assert_eq!(RelationKind::CorrectedBy.as_verb(), "corrected by");
    }

    #[test]
    fn test_kind_path_display_and_parse() {
        use crate::notion::notion::{NotionKind, parse_kind};

        assert_eq!(NotionKind::Path.to_string(), "path");
        assert_eq!(parse_kind("path"), Some(NotionKind::Path));
        assert_eq!(parse_kind("bogus"), None);
    }

    #[tokio::test]
    async fn test_upsert_decision_node() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = NotionService::new();

        service
            .upsert_decision_node(
                &client,
                "d-1",
                "Choose Framework",
                "Build web app",
                "React",
                Some("executed"),
            )
            .await
            .unwrap();

        let notions = service.load_all_entities(&client).await.unwrap();
        assert_eq!(notions.len(), 1);
        assert_eq!(notions[0].id, "d-1");
        assert_eq!(notions[0].name, "choose framework");
        assert_eq!(notions[0].kind, NotionKind::Decision);
    }

    #[tokio::test]
    async fn test_upsert_decision_node_without_outcome() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = NotionService::new();

        service
            .upsert_decision_node(
                &client,
                "d-2",
                "Pick Language",
                "Write CLI",
                "Rust",
                None,
            )
            .await
            .unwrap();

        let notions = service.load_all_entities(&client).await.unwrap();
        assert_eq!(notions.len(), 1);
        assert_eq!(notions[0].kind, NotionKind::Decision);
    }

    #[tokio::test]
    async fn test_upsert_decision_node_idempotent() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = NotionService::new();

        service
            .upsert_decision_node(&client, "d-3", "Deploy", "Ship", "AWS", Some("done"))
            .await
            .unwrap();
        service
            .upsert_decision_node(&client, "d-3", "Deploy", "Ship", "AWS", Some("done"))
            .await
            .unwrap();

        let notions_repo = NotionsRepo::new(&client);
        let all = notions_repo.load_all_entities().await.unwrap();
        let count = all.iter().filter(|e| e.id == "d-3").count();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_upsert_belief_node() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = NotionService::new();

        service
            .upsert_belief_node(
                &client,
                "b-1",
                "Rust is fast",
                "Rust has zero-cost abstractions",
                0.8,
                0.95,
                12,
            )
            .await
            .unwrap();

        let notions = service.load_all_entities(&client).await.unwrap();
        assert_eq!(notions.len(), 1);
        assert_eq!(notions[0].id, "b-1");
        assert_eq!(notions[0].name, "rust is fast");
        assert_eq!(notions[0].kind, NotionKind::Belief);
    }

    #[tokio::test]
    async fn test_upsert_belief_node_idempotent() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = NotionService::new();

        service
            .upsert_belief_node(&client, "b-2", "TS is safe", "TypeScript catches bugs", 0.5, 0.8, 5)
            .await
            .unwrap();
        service
            .upsert_belief_node(&client, "b-2", "TS is safe", "TypeScript catches bugs", 0.5, 0.8, 5)
            .await
            .unwrap();

        let notions_repo = NotionsRepo::new(&client);
        let all = notions_repo.load_all_entities().await.unwrap();
        let count = all.iter().filter(|e| e.id == "b-2").count();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_new_relation_types_extracted_from_and_supports() {
        use crate::notion::relationship::{parse_relation_type, RelationKind};

        assert_eq!(
            RelationKind::ExtractedFrom.to_string(),
            "extracted_from"
        );
        assert_eq!(RelationKind::Supports.to_string(), "supports");

        assert_eq!(
            parse_relation_type("extracted_from"),
            Some(RelationKind::ExtractedFrom)
        );
        assert_eq!(
            parse_relation_type("supports"),
            Some(RelationKind::Supports)
        );

        assert_eq!(RelationKind::ExtractedFrom.as_verb(), "extracted from");
        assert_eq!(RelationKind::Supports.as_verb(), "supports");
    }

    #[test]
    fn test_kind_decision_display_and_parse() {
        use crate::notion::notion::{NotionKind, parse_kind};

        assert_eq!(NotionKind::Decision.to_string(), "decision");
        assert_eq!(parse_kind("decision"), Some(NotionKind::Decision));
    }

    #[tokio::test]
    async fn test_upsert_and_load_self_node() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = NotionService::new();

        let mut node = crate::notion::self_node::SelfNode::new(
            "sn-1".to_string(),
            crate::notion::self_node::SelfModelLayer::Knowledge,
            "knows GTD".to_string(),
            "productivity".to_string(),
        );
        node.is_explicit = Some(true);
        node.confidence = 0.9;
        node.source = "fact".to_string();

        service.upsert_self_node(&client, &node).await.unwrap();

        let loaded = service.load_self_nodes(&client).await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "sn-1");
        assert_eq!(loaded[0].name, "knows GTD");
        assert_eq!(loaded[0].layer, crate::notion::self_node::SelfModelLayer::Knowledge);
        assert_eq!(loaded[0].is_explicit, Some(true));
        assert_eq!(loaded[0].confidence, 0.9);
        assert_eq!(loaded[0].source, "fact");
    }

    #[tokio::test]
    async fn test_load_self_nodes_by_layer() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = NotionService::new();

        let k1 = crate::notion::self_node::SelfNode::new(
            "k-1".to_string(),
            crate::notion::self_node::SelfModelLayer::Knowledge,
            "knows Rust".to_string(),
            "programming".to_string(),
        );
        let k2 = crate::notion::self_node::SelfNode::new(
            "k-2".to_string(),
            crate::notion::self_node::SelfModelLayer::Knowledge,
            "knows SQL".to_string(),
            "programming".to_string(),
        );
        let s1 = crate::notion::self_node::SelfNode::new(
            "s-1".to_string(),
            crate::notion::self_node::SelfModelLayer::Skill,
            "writes async code".to_string(),
            "programming".to_string(),
        );

        service.upsert_self_node(&client, &k1).await.unwrap();
        service.upsert_self_node(&client, &k2).await.unwrap();
        service.upsert_self_node(&client, &s1).await.unwrap();

        let knowledge = service
            .load_self_nodes_by_layer(&client, &crate::notion::self_node::SelfModelLayer::Knowledge)
            .await
            .unwrap();
        assert_eq!(knowledge.len(), 2);

        let skills = service
            .load_self_nodes_by_layer(&client, &crate::notion::self_node::SelfModelLayer::Skill)
            .await
            .unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "writes async code");
    }

    #[tokio::test]
    async fn test_self_node_with_skill_fields() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = NotionService::new();

        let mut node = crate::notion::self_node::SelfNode::new(
            "skill-1".to_string(),
            crate::notion::self_node::SelfModelLayer::Skill,
            "writes Rust async".to_string(),
            "programming".to_string(),
        );
        node.sufficient_for = vec!["architect".to_string(), "engineer".to_string()];
        node.necessary_for = vec!["systems-engineer".to_string()];

        service.upsert_self_node(&client, &node).await.unwrap();

        let loaded = service.load_self_nodes(&client).await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].sufficient_for, vec!["architect", "engineer"]);
        assert_eq!(loaded[0].optionality_count, None);
    }
}


