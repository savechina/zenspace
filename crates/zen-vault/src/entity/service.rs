use anyhow::Result;
use std::collections::HashSet;

use zen_repo::{
    BeliefsRepo, EntitiesRepo, GoalsRepo, SelfModelRepo, SqliteClient,
    types::{
        InsertRelationshipRequest, UpsertBeliefNodeRequest, UpsertGoalNodeRequest,
        UpsertPathNodeRequest,
    },
};

use crate::maintenance::compute_embeddings_for_text;
use crate::search::Tier3Search;

use super::entity::{Entity, EntityType};
use super::relationship::{RelationType, Relationship};

/// Normalize an entity name for canonical matching.
/// Rules: lowercase, trim, strip common suffixes (.js, .rs, .py, -lang, " language").
use unicode_normalization::UnicodeNormalization;

fn normalize_entity_name(name: &str) -> String {
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

/// Parse an entity type string (as stored in graph.db) back into the enum variant.
fn parse_entity_type(s: &str) -> EntityType {
    match s {
        "Function" => EntityType::Function,
        "Class" => EntityType::Class,
        "Module" => EntityType::Module,
        "Concept" => EntityType::Concept,
        "Person" => EntityType::Person,
        "Organization" => EntityType::Organization,
        "Event" => EntityType::Event,
        "Product" => EntityType::Product,
        "Technology" => EntityType::Technology,
        "SelfModel" => EntityType::SelfModel,
        "Belief" => EntityType::Belief,
        "Goal" => EntityType::Goal,
        "Path" => EntityType::Path,
        "Decision" => EntityType::Decision,
        _ => EntityType::Other,
    }
}

/// Convert a `RelationshipRow` string to a `RelationType` enum.
fn relation_type_from_str(s: &str) -> RelationType {
    match s {
        "DependsOn" => RelationType::DependsOn,
        "Implements" => RelationType::Implements,
        "RelatedTo" => RelationType::RelatedTo,
        "References" => RelationType::References,
        "Contradicts" => RelationType::Contradicts,
        "Extends" => RelationType::Extends,
        "Uses" => RelationType::Uses,
        "Contains" => RelationType::Contains,
        "SelfBelieves" => RelationType::SelfBelieves,
        "SelfAims" => RelationType::SelfAims,
        "SelfCapableOf" => RelationType::SelfCapableOf,
        "SelfPartOf" => RelationType::SelfPartOf,
        "ServesGoal" => RelationType::ServesGoal,
        "AlternativeTo" => RelationType::AlternativeTo,
        "DecidedAbout" => RelationType::DecidedAbout,
        "CorrectedBy" => RelationType::CorrectedBy,
        "ExtractedFrom" => RelationType::ExtractedFrom,
        "Supports" => RelationType::Supports,
        _ => RelationType::RelatedTo,
    }
}

/// Convert an `EntityRow` from the repo to the domain `Entity` type.
fn entity_row_to_entity(row: zen_repo::types::EntityRow) -> Entity {
    let entity_type = parse_entity_type(&row.entity_type);
    let created_at = chrono::DateTime::parse_from_rfc3339(&row.created_at)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now());
    let last_updated = row
        .last_updated
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or(created_at);

    let mut entity = Entity::new(row.name, entity_type, "graph-db");
    entity.id = row.id;
    entity.created_at = created_at;
    entity.last_updated = last_updated;
    entity.domain = row.domain;
    entity.description = row.description;
    entity
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

/// Service for extracting entities and relationships from workspace content.
pub struct EntityService;

impl Default for EntityService {
    fn default() -> Self {
        Self::new()
    }
}

impl EntityService {
    pub fn new() -> Self {
        Self
    }

    /// Extract typed entities from note content using keyword heuristics.
    ///
    /// Scans for known technology names, heading patterns, and capitalized
    /// multi-word terms. Returns unique entities deduplicated by name.
    /// Provides a baseline extraction when LLM extraction is unavailable.
    pub fn extract_entities(&self, note_content: &str) -> Result<Vec<Entity>> {
        let mut entities: Vec<Entity> = Vec::new();
        let content_lower = note_content.to_lowercase();
        let note_id = "note-content";

        // Known technology keywords (common in development)
        const KNOWN_TECHS: &[&str] = &[
            "rust", "python", "javascript", "typescript", "go", "java", "c++",
            "react", "vue", "angular", "svelte", "next.js", "node.js",
            "postgresql", "mysql", "sqlite", "mongodb", "redis",
            "docker", "kubernetes", "terraform", "aws", "gcp", "azure",
            "graphql", "rest", "grpc", "websocket",
            "linux", "macos", "windows",
            "git", "github", "gitlab",
            "wasm", "llm", "ai", "ml",
            "openai", "anthropic", "ollama", "deepseek",
            "sqlx", "rusqlite", "wasmtime", "rig-core", "rig-compose",
        ];

        for tech in KNOWN_TECHS {
            if content_lower.contains(tech) {
                let name = capitalize(tech);
                if !entities.iter().any(|e| e.name == name) {
                    entities.push(Entity::new(&name, EntityType::Technology, note_id));
                }
            }
        }

        // Extract ## heading concepts
        for line in note_content.lines() {
            let trimmed = line.trim();
            if let Some(heading) = trimmed.strip_prefix("## ").or_else(|| trimmed.strip_prefix("# ")) {
                let heading = heading.trim();
                if heading.len() >= 3 && !entities.iter().any(|e| e.name == heading) {
                    let typ = classify_heading(heading);
                    entities.push(Entity::new(heading, typ, note_id));
                }
            }
        }

        // Extract capitalized multi-word terms (likely proper nouns)
        let mut word = String::new();
        let chars: Vec<char> = note_content.chars().collect();
        for (i, &ch) in chars.iter().enumerate() {
            if ch.is_uppercase() && i > 0 && !chars[i - 1].is_alphabetic() {
                word.clear();
                word.push(ch);
            } else if ch.is_alphabetic() && !word.is_empty() {
                word.push(ch);
            } else if !ch.is_alphabetic() && !word.is_empty() {
                if word.len() >= 4 {
                    let name = word.clone();
                    if !entities.iter().any(|e| e.name == name) {
                        entities.push(Entity::new(&name, EntityType::Concept, note_id));
                    }
                }
                word.clear();
            }
        }

        Ok(entities)
    }

    /// Extract relationships between co-occurring entities.
    ///
    /// Creates RELATED_TO relationships for every pair of entities.
    /// Future enhancement: use LLM to classify relationship types
    /// (e.g., USES, DEPENDS_ON, IMPLEMENTS).
    pub fn extract_relationships(&self, entities: &[Entity]) -> Result<Vec<Relationship>> {
        let mut relationships = Vec::new();
        let now = chrono::Utc::now();

        for i in 0..entities.len() {
            for j in (i + 1)..entities.len() {
                relationships.push(Relationship {
                    id: uuid::Uuid::now_v7().to_string(),
                    source_entity_id: entities[i].id.clone(),
                    target_entity_id: entities[j].id.clone(),
                    relation_type: RelationType::RelatedTo,
                    description: format!(
                        "{} relates to {}",
                        entities[i].name, entities[j].name
                    ),
                    source_note_id: entities[i].source_note_id.clone(),
                    created_at: now,
                });
            }
        }
        Ok(relationships)
    }

    /// Load all known entity names from graph.db.
    /// Returns canonical names AND known aliases.
    /// Returns an empty set if the database does not exist yet.
    pub async fn load_known_entity_names(&self, client: &SqliteClient) -> Result<HashSet<String>> {
        let names = EntitiesRepo::new(client).load_known_entity_names().await?;
        Ok(names.into_iter().collect())
    }

    /// Upsert an entity into graph.db.
    /// Normalizes the name and checks aliases before insert.
    /// Ensures the schema exists before writing.
    pub async fn upsert_entity(&self, client: &SqliteClient, entity: &Entity) -> Result<()> {
        let repo = EntitiesRepo::new(client);

        let canonical_name = normalize_entity_name(&entity.name);
        let type_str = format!("{:?}", entity.entity_type);
        let created_at = entity.created_at.to_rfc3339();
        let last_updated = chrono::Utc::now().to_rfc3339();

        // Check if an alias already maps to a canonical entity
        let existing_canonical = repo.resolve_alias(&canonical_name).await?;

        if let Some(canonical_id) = existing_canonical {
            // Alias found — just update last_updated on the canonical entity
            repo.update_entity_timestamp(&canonical_id, &last_updated).await?;
            return Ok(());
        }

        // No alias match — insert the entity with normalized name
        repo.upsert_entity_with(
            &entity.id,
            &canonical_name,
            &type_str,
            &created_at,
            &last_updated,
            &entity.description,
            &entity.source_note_id,
            0.5,
        )
        .await?;

        // Register the alias (INSERT OR IGNORE in case of concurrent insert)
        repo.insert_alias(&canonical_name, &entity.id).await?;

        Ok(())
    }

    /// Insert a relationship into graph.db.
    /// Confidence defaults to 0.8 for auto-extracted edges.
    pub async fn insert_relationship(&self, client: &SqliteClient, rel: &Relationship) -> Result<()> {
        let type_str = format!("{:?}", rel.relation_type);
        let created = rel.created_at.to_rfc3339();

        EntitiesRepo::new(client)
            .insert_relationship(&InsertRelationshipRequest {
                id: &rel.id,
                source_id: &rel.source_entity_id,
                target_id: &rel.target_entity_id,
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

    /// Compute a 384-dim embedding for entity text and store in state.db.
    /// Falls back to hash-based embedding when no LLM provider is available.
    /// Fails gracefully (returns Err) if the vec0 extension is not loaded.
    pub async fn store_entity_embedding(
        &self,
        client: &zen_repo::SqliteClient,
        entity_id: &str,
        text: &str,
    ) -> Result<()> {
        let embedding = compute_embeddings_for_text(text)?;
        Tier3Search
            .insert_entity_embedding(client, entity_id, &embedding)
            .await
    }

    pub async fn load_all_entities(&self, client: &SqliteClient) -> Result<Vec<Entity>> {
        let rows = EntitiesRepo::new(client).load_all_entities().await?;
        Ok(rows.into_iter().map(entity_row_to_entity).collect())
    }

    /// Load entities that have been updated since the given timestamp.
    /// Used by incremental wiki compilation.
    pub async fn load_entities_updated_since(
        &self,
        client: &SqliteClient,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<Entity>> {
        let since_str = since.to_rfc3339();
        let rows = EntitiesRepo::new(client)
            .load_entities_updated_since(&since_str)
            .await?;
        Ok(rows.into_iter().map(entity_row_to_entity).collect())
    }

    pub async fn load_relationships_for_entity(
        &self,
        client: &SqliteClient,
        entity_id: &str,
    ) -> Result<Vec<(String, RelationType)>> {
        let repo = EntitiesRepo::new(client);
        let rows = repo.load_relationships(entity_id).await?;

        let mut result = Vec::new();
        for row in rows {
            let rt = relation_type_from_str(&row.relation_type);
            let target_name = repo
                .entity_name(&row.target_entity_id)
                .await?
                .unwrap_or(row.target_entity_id);
            result.push((target_name, rt));
        }
        Ok(result)
    }

    /// Upserts a Self-Model item as a graph entity.
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
        let mut entity = Entity::new(name, EntityType::SelfModel, "self-model");
        entity.id = id.to_string();
        entity
            .metadata
            .insert("layer".to_string(), layer.to_string());
        if let Some(d) = domain {
            entity
                .metadata
                .insert("domain".to_string(), d.to_string());
        }
        self.upsert_entity(client, &entity).await
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
        let mut entity = Entity::new(name, EntityType::Goal, "goal-model");
        entity.id = id.to_string();
        entity
            .metadata
            .insert("controllability".to_string(), controllability.to_string());
        entity
            .metadata
            .insert("core_pursuit".to_string(), core_pursuit.to_string());
        if let Some(d) = deadline {
            entity
                .metadata
                .insert("deadline".to_string(), d.to_string());
        }
        self.upsert_entity(client, &entity).await?;

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
        let mut entity = Entity::new(name, EntityType::Decision, "decision-model");
        entity.id = id.to_string();
        entity
            .metadata
            .insert("goal".to_string(), goal.to_string());
        entity
            .metadata
            .insert("choice".to_string(), choice.to_string());
        if let Some(status) = outcome_status {
            entity
                .metadata
                .insert("outcome_status".to_string(), status.to_string());
        }
        self.upsert_entity(client, &entity).await
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
        let mut entity = Entity::new(name, EntityType::Belief, "belief-model");
        entity.id = id.to_string();
        entity
            .metadata
            .insert("proposition".to_string(), proposition.to_string());
        entity
            .metadata
            .insert("prior".to_string(), prior.to_string());
        entity
            .metadata
            .insert("posterior".to_string(), posterior.to_string());
        entity
            .metadata
            .insert("evidence_count".to_string(), evidence_count.to_string());
        self.upsert_entity(client, &entity).await?;

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
        let mut entity = Entity::new(name, EntityType::Path, "path-model");
        entity.id = id.to_string();
        entity
            .metadata
            .insert("serves_goal".to_string(), serves_goal.to_string());
        entity
            .metadata
            .insert("is_default".to_string(), is_default.to_string());
        entity
            .metadata
            .insert("crowdedness".to_string(), crowdedness.to_string());
        entity
            .metadata
            .insert("alternatives".to_string(), alternatives.to_string());
        self.upsert_entity(client, &entity).await?;

        // Also write to dedicated path_nodes table
        let goal_entities = self.load_all_entities(client).await?;
        let goal_id = goal_entities
            .iter()
            .find(|e| e.name == normalize_entity_name(serves_goal) && e.entity_type == EntityType::Goal)
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
        if let Some(goal) = goal_entities.iter().find(|e| e.name == normalize_entity_name(serves_goal)) {
            let rel = Relationship::new(id, goal.id.clone(), RelationType::ServesGoal, "path-model");
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
    fn test_normalize_entity_name() {
        assert_eq!(normalize_entity_name("Rust"), "rust");
        assert_eq!(normalize_entity_name("Rust Lang"), "rust");
        assert_eq!(normalize_entity_name("JavaScript.js"), "javascript");
        assert_eq!(normalize_entity_name("  Python  "), "python");
        assert_eq!(normalize_entity_name("TypeScript.ts"), "typescript");
        assert_eq!(normalize_entity_name("Go-lang"), "go");
        assert_eq!(normalize_entity_name("C Language"), "c");
        assert_eq!(normalize_entity_name("rust language"), "rust");
    }

    #[test]
    fn test_normalize_entity_name_nfc() {
        // Combining accent (U+0301) vs precomposed (U+00E9) → same NFC output
        let combining = "cafe\u{0301}";
        let precomposed = "caf\u{00E9}";
        assert_eq!(normalize_entity_name(combining), normalize_entity_name(precomposed));
    }

    #[tokio::test]
    async fn test_alias_resolution() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();

        let service = EntityService::new();

        // Create first entity with name "Rust"
        let mut entity1 = Entity::new("Rust", super::super::entity::EntityType::Technology, "note1");
        entity1.id = "entity-rust-1".to_string();
        service.upsert_entity(&client, &entity1).await.unwrap();

        // Create second entity with name "rust" — should resolve to the same entity
        let mut entity2 = Entity::new("rust", super::super::entity::EntityType::Technology, "note2");
        entity2.id = "entity-rust-2".to_string();
        service.upsert_entity(&client, &entity2).await.unwrap();

        // Verify: only one entity exists (the canonical one)
        let entities_repo = EntitiesRepo::new(&client);
        let entities = entities_repo.load_all_entities().await.unwrap();
        let rust_count = entities.iter().filter(|e| e.name == "rust").count();
        assert_eq!(rust_count, 1);

        // Verify alias exists
        let alias_resolved = entities_repo.resolve_alias("rust").await.unwrap();
        assert!(alias_resolved.is_some());

        // Verify load_known_entity_names includes the canonical name
        let names = service.load_known_entity_names(&client).await.unwrap();
        assert!(names.contains("rust"));
    }

    #[test]
    fn test_parse_entity_type_self_model() {
        let parsed = parse_entity_type("SelfModel");
        assert_eq!(parsed, EntityType::SelfModel);

        let parsed_belief = parse_entity_type("Belief");
        assert_eq!(parsed_belief, EntityType::Belief);

        let parsed_goal = parse_entity_type("Goal");
        assert_eq!(parsed_goal, EntityType::Goal);
    }

    #[test]
    fn test_parse_entity_type_unknown_falls_back_to_other() {
        let parsed = parse_entity_type("UnknownType");
        assert_eq!(parsed, EntityType::Other);
    }

    #[test]
    fn test_entity_type_self_model_debug_roundtrip() {
        let et = EntityType::SelfModel;
        let debug_str = format!("{:?}", et);
        assert_eq!(debug_str, "SelfModel");
        let reparsed = parse_entity_type(&debug_str);
        assert_eq!(reparsed, et);
    }

    #[test]
    fn test_relation_type_self_model_variants() {
        let variants = [
            RelationType::SelfBelieves,
            RelationType::SelfAims,
            RelationType::SelfCapableOf,
            RelationType::SelfPartOf,
        ];
        let expected = ["SelfBelieves", "SelfAims", "SelfCapableOf", "SelfPartOf"];
        for (variant, name) in variants.iter().zip(expected.iter()) {
            assert_eq!(format!("{:?}", variant), *name);
        }
    }

    #[test]
    fn test_relation_type_self_believes_roundtrip() {
        let rel = RelationType::SelfBelieves;
        let s = format!("{:?}", rel);
        assert_eq!(s, "SelfBelieves");

        let roundtripped = match s.as_str() {
            "SelfBelieves" => RelationType::SelfBelieves,
            _ => panic!("unexpected relation type"),
        };
        assert_eq!(roundtripped, RelationType::SelfBelieves);
    }

    #[tokio::test]
    async fn test_upsert_self_model_entity() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = EntityService::new();

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

        let entities = service.load_all_entities(&client).await.unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].id, "sm-1");
        assert_eq!(entities[0].name, "rust proficiency");
        assert_eq!(entities[0].entity_type, EntityType::SelfModel);
    }

    #[tokio::test]
    async fn test_upsert_self_model_entity_without_domain() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = EntityService::new();

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

        let entities = service.load_all_entities(&client).await.unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].entity_type, EntityType::SelfModel);
        assert_eq!(entities[0].name, "curiosity");

        let entities_repo = EntitiesRepo::new(&client);
        let all = entities_repo.load_all_entities().await.unwrap();
        let stored = all.iter().find(|e| e.id == "sm-2").unwrap();
        assert_eq!(stored.entity_type, "SelfModel");
    }

    #[tokio::test]
    async fn test_upsert_self_model_entity_idempotent() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = EntityService::new();

        service
            .upsert_self_model_entity(&client, "sm-3", "Writing", "Skill", None)
            .await
            .unwrap();
        service
            .upsert_self_model_entity(&client, "sm-3", "Writing", "Skill", None)
            .await
            .unwrap();

        let entities_repo = EntitiesRepo::new(&client);
        let all = entities_repo.load_all_entities().await.unwrap();
        let count = all.iter().filter(|e| e.id == "sm-3").count();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_self_model_entity_loads_correct_type() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = EntityService::new();

        service
            .upsert_self_model_entity(&client, "sm-4", "Empathy", "Trait", Some("social"))
            .await
            .unwrap();

        let entities = service.load_all_entities(&client).await.unwrap();
        let entity = &entities[0];
        assert_eq!(entity.entity_type, EntityType::SelfModel);

        let type_str = format!("{:?}", entity.entity_type);
        let reparsed = parse_entity_type(&type_str);
        assert_eq!(reparsed, EntityType::SelfModel);
    }

    #[tokio::test]
    async fn test_upsert_goal_node() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = EntityService::new();

        service
            .upsert_goal_node(&client, "g-1", "Learn Rust", 0.8, "mastery", Some("2026-12-31"))
            .await
            .unwrap();

        let entities = service.load_all_entities(&client).await.unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].id, "g-1");
        assert_eq!(entities[0].name, "learn rust");
        assert_eq!(entities[0].entity_type, EntityType::Goal);
    }

    #[tokio::test]
    async fn test_upsert_goal_node_without_deadline() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = EntityService::new();

        service
            .upsert_goal_node(&client, "g-2", "Ship Feature", 0.5, "delivery", None)
            .await
            .unwrap();

        let entities = service.load_all_entities(&client).await.unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].entity_type, EntityType::Goal);
    }

    #[tokio::test]
    async fn test_upsert_goal_node_metadata() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = EntityService::new();

        service
            .upsert_goal_node(&client, "g-3", "Run Marathon", 0.9, "fitness", Some("2027-06-01"))
            .await
            .unwrap();

        let entities_repo = EntitiesRepo::new(&client);
        let all = entities_repo.load_all_entities().await.unwrap();
        let stored = all.iter().find(|e| e.id == "g-3").unwrap();
        assert_eq!(stored.entity_type, "Goal");
    }

    #[tokio::test]
    async fn test_upsert_goal_node_idempotent() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = EntityService::new();

        service
            .upsert_goal_node(&client, "g-4", "Write Book", 0.6, "creative", None)
            .await
            .unwrap();
        service
            .upsert_goal_node(&client, "g-4", "Write Book", 0.6, "creative", None)
            .await
            .unwrap();

        let entities_repo = EntitiesRepo::new(&client);
        let all = entities_repo.load_all_entities().await.unwrap();
        let count = all.iter().filter(|e| e.id == "g-4").count();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_upsert_path_node() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = EntityService::new();

        service
            .upsert_goal_node(&client, "g-5", "Learn Rust", 0.8, "mastery", None)
            .await
            .unwrap();

        service
            .upsert_path_node(&client, "p-1", "Online Courses", "Learn Rust", true, 0.3, "")
            .await
            .unwrap();

        let entities = service.load_all_entities(&client).await.unwrap();
        assert_eq!(entities.len(), 2);
        let path = entities.iter().find(|e| e.id == "p-1").unwrap();
        assert_eq!(path.entity_type, EntityType::Path);
        assert_eq!(path.name, "online courses");
    }

    #[tokio::test]
    async fn test_upsert_path_node_creates_serves_goal_relationship() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = EntityService::new();

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
        assert_eq!(rels[0].1, RelationType::ServesGoal);
        assert_eq!(rels[0].0, "master cooking");
    }

    #[tokio::test]
    async fn test_upsert_path_node_without_existing_goal() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = EntityService::new();

        service
            .upsert_path_node(&client, "p-3", "Mentorship", "Nonexistent Goal", true, 0.5, "")
            .await
            .unwrap();

        let entities = service.load_all_entities(&client).await.unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].entity_type, EntityType::Path);

        let rels = service.load_relationships_for_entity(&client, "p-3").await.unwrap();
        assert!(rels.is_empty());
    }

    #[tokio::test]
    async fn test_upsert_path_node_idempotent() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = EntityService::new();

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

        let entities_repo = EntitiesRepo::new(&client);
        let all = entities_repo.load_all_entities().await.unwrap();
        let count = all.iter().filter(|e| e.id == "p-4").count();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_new_relation_types_display() {
        use crate::entity::relationship::{parse_relation_type, RelationType};

        assert_eq!(RelationType::ServesGoal.to_string(), "serves_goal");
        assert_eq!(RelationType::AlternativeTo.to_string(), "alternative_to");
        assert_eq!(RelationType::DecidedAbout.to_string(), "decided_about");
        assert_eq!(RelationType::CorrectedBy.to_string(), "corrected_by");

        assert_eq!(parse_relation_type("serves_goal"), Some(RelationType::ServesGoal));
        assert_eq!(parse_relation_type("alternative_to"), Some(RelationType::AlternativeTo));
        assert_eq!(parse_relation_type("decided_about"), Some(RelationType::DecidedAbout));
        assert_eq!(parse_relation_type("corrected_by"), Some(RelationType::CorrectedBy));
        assert_eq!(parse_relation_type("bogus"), None);
    }

    #[test]
    fn test_new_relation_types_as_verb() {
        use crate::entity::relationship::RelationType;

        assert_eq!(RelationType::ServesGoal.as_verb(), "serves goal");
        assert_eq!(RelationType::AlternativeTo.as_verb(), "alternative to");
        assert_eq!(RelationType::DecidedAbout.as_verb(), "decided about");
        assert_eq!(RelationType::CorrectedBy.as_verb(), "corrected by");
    }

    #[test]
    fn test_entity_type_path_display_and_parse() {
        use crate::entity::entity::{EntityType, parse_entity_type};

        assert_eq!(EntityType::Path.to_string(), "path");
        assert_eq!(parse_entity_type("path"), Some(EntityType::Path));
        assert_eq!(parse_entity_type("bogus"), None);
    }

    #[tokio::test]
    async fn test_upsert_decision_node() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = EntityService::new();

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

        let entities = service.load_all_entities(&client).await.unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].id, "d-1");
        assert_eq!(entities[0].name, "choose framework");
        assert_eq!(entities[0].entity_type, EntityType::Decision);
    }

    #[tokio::test]
    async fn test_upsert_decision_node_without_outcome() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = EntityService::new();

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

        let entities = service.load_all_entities(&client).await.unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].entity_type, EntityType::Decision);
    }

    #[tokio::test]
    async fn test_upsert_decision_node_idempotent() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = EntityService::new();

        service
            .upsert_decision_node(&client, "d-3", "Deploy", "Ship", "AWS", Some("done"))
            .await
            .unwrap();
        service
            .upsert_decision_node(&client, "d-3", "Deploy", "Ship", "AWS", Some("done"))
            .await
            .unwrap();

        let entities_repo = EntitiesRepo::new(&client);
        let all = entities_repo.load_all_entities().await.unwrap();
        let count = all.iter().filter(|e| e.id == "d-3").count();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_upsert_belief_node() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = EntityService::new();

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

        let entities = service.load_all_entities(&client).await.unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].id, "b-1");
        assert_eq!(entities[0].name, "rust is fast");
        assert_eq!(entities[0].entity_type, EntityType::Belief);
    }

    #[tokio::test]
    async fn test_upsert_belief_node_idempotent() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = EntityService::new();

        service
            .upsert_belief_node(&client, "b-2", "TS is safe", "TypeScript catches bugs", 0.5, 0.8, 5)
            .await
            .unwrap();
        service
            .upsert_belief_node(&client, "b-2", "TS is safe", "TypeScript catches bugs", 0.5, 0.8, 5)
            .await
            .unwrap();

        let entities_repo = EntitiesRepo::new(&client);
        let all = entities_repo.load_all_entities().await.unwrap();
        let count = all.iter().filter(|e| e.id == "b-2").count();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_new_relation_types_extracted_from_and_supports() {
        use crate::entity::relationship::{parse_relation_type, RelationType};

        assert_eq!(
            RelationType::ExtractedFrom.to_string(),
            "extracted_from"
        );
        assert_eq!(RelationType::Supports.to_string(), "supports");

        assert_eq!(
            parse_relation_type("extracted_from"),
            Some(RelationType::ExtractedFrom)
        );
        assert_eq!(
            parse_relation_type("supports"),
            Some(RelationType::Supports)
        );

        assert_eq!(RelationType::ExtractedFrom.as_verb(), "extracted from");
        assert_eq!(RelationType::Supports.as_verb(), "supports");
    }

    #[test]
    fn test_entity_type_decision_display_and_parse() {
        use crate::entity::entity::{EntityType, parse_entity_type};

        assert_eq!(EntityType::Decision.to_string(), "decision");
        assert_eq!(parse_entity_type("decision"), Some(EntityType::Decision));
    }

    #[tokio::test]
    async fn test_upsert_and_load_self_node() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = EntityService::new();

        let mut node = crate::entity::self_node::SelfNode::new(
            "sn-1".to_string(),
            crate::entity::self_node::SelfModelLayer::Knowledge,
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
        assert_eq!(loaded[0].layer, crate::entity::self_node::SelfModelLayer::Knowledge);
        assert_eq!(loaded[0].is_explicit, Some(true));
        assert_eq!(loaded[0].confidence, 0.9);
        assert_eq!(loaded[0].source, "fact");
    }

    #[tokio::test]
    async fn test_load_self_nodes_by_layer() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let service = EntityService::new();

        let k1 = crate::entity::self_node::SelfNode::new(
            "k-1".to_string(),
            crate::entity::self_node::SelfModelLayer::Knowledge,
            "knows Rust".to_string(),
            "programming".to_string(),
        );
        let k2 = crate::entity::self_node::SelfNode::new(
            "k-2".to_string(),
            crate::entity::self_node::SelfModelLayer::Knowledge,
            "knows SQL".to_string(),
            "programming".to_string(),
        );
        let s1 = crate::entity::self_node::SelfNode::new(
            "s-1".to_string(),
            crate::entity::self_node::SelfModelLayer::Skill,
            "writes async code".to_string(),
            "programming".to_string(),
        );

        service.upsert_self_node(&client, &k1).await.unwrap();
        service.upsert_self_node(&client, &k2).await.unwrap();
        service.upsert_self_node(&client, &s1).await.unwrap();

        let knowledge = service
            .load_self_nodes_by_layer(&client, &crate::entity::self_node::SelfModelLayer::Knowledge)
            .await
            .unwrap();
        assert_eq!(knowledge.len(), 2);

        let skills = service
            .load_self_nodes_by_layer(&client, &crate::entity::self_node::SelfModelLayer::Skill)
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
        let service = EntityService::new();

        let mut node = crate::entity::self_node::SelfNode::new(
            "skill-1".to_string(),
            crate::entity::self_node::SelfModelLayer::Skill,
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

fn capitalize(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap().to_uppercase().collect::<String>();
    first + &chars.as_str().to_string()
}

fn classify_heading(heading: &str) -> EntityType {
    let lower = heading.to_lowercase();
    if lower.contains("api") || lower.contains("database") || lower.contains("service")
        || lower.contains("cli") || lower.contains("library") || lower.contains("tool")
    {
        EntityType::Technology
    } else if lower.contains("company") || lower.contains("team") || lower.contains("org") {
        EntityType::Organization
    } else if lower.contains("person") || lower.contains("author") {
        EntityType::Person
    } else {
        EntityType::Concept
    }
}
