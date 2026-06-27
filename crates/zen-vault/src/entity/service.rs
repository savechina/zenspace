use anyhow::Result;
use std::collections::HashSet;
use std::path::Path;

use rusqlite::params;

use zen_repo::sqlite_repo::{SqliteRepo, init_graph_schema};

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

/// Parse the `aliases` TEXT column from graph.db into a `Vec<String>`.
/// Accepts JSON array format `["a","b"]` or comma-separated `"a, b"`.
/// Returns an empty vec for NULL or empty input.
fn parse_aliases_column(raw: Option<&str>) -> Vec<String> {
    let raw = match raw {
        Some(s) if !s.is_empty() => s,
        _ => return Vec::new(),
    };

    // Try JSON array first
    if let Ok(arr) = serde_json::from_str::<Vec<String>>(raw) {
        return arr;
    }

    // Fall back to comma-separated
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
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

    /// Extract typed entities from note content.
    /// Stub — deferred to zen-provider integration.
    pub fn extract_entities(&self, _note_content: &str) -> Result<Vec<Entity>> {
        tracing::info!("Entity extraction deferred to zen-provider integration");
        Ok(Vec::new())
    }

    /// Extract relationships between entities.
    /// Stub — deferred to zen-provider integration.
    pub fn extract_relationships(&self, _entities: &[Entity]) -> Result<Vec<Relationship>> {
        tracing::info!("Relationship extraction deferred to zen-provider integration");
        Ok(Vec::new())
    }

    /// Load all known entity names from graph.db.
    /// Returns canonical names AND known aliases.
    /// Returns an empty set if the database does not exist yet.
    pub fn load_known_entity_names(&self, db_path: &Path) -> Result<HashSet<String>> {
        if !db_path.exists() {
            return Ok(HashSet::new());
        }
        let repo = SqliteRepo::open(db_path)?;
        let names = repo.query_map(
            "SELECT DISTINCT name FROM entities
             UNION
             SELECT DISTINCT alias FROM entity_aliases",
            &[],
            |row| row.get::<_, String>(0),
        )?;
        Ok(names.into_iter().collect())
    }

    /// Upsert an entity into graph.db.
    /// Normalizes the name and checks aliases before insert.
    /// Ensures the schema exists before writing.
    pub fn upsert_entity(&self, db_path: &Path, entity: &Entity) -> Result<()> {
        let mut repo = SqliteRepo::open(db_path)?;
        init_graph_schema(&repo)?;

        let canonical_name = normalize_entity_name(&entity.name);
        let type_str = format!("{:?}", entity.entity_type);
        let first_seen = entity.created_at.to_rfc3339();
        let last_updated = chrono::Utc::now().to_rfc3339();

        // Check if an alias already maps to a canonical entity
        let existing_canonical: Option<String> = repo
            .query_row(
                "SELECT canonical_entity_id FROM entity_aliases WHERE alias = ?1",
                params![canonical_name],
                |row| row.get(0),
            )
            .ok();

        if let Some(canonical_id) = existing_canonical {
            // Alias found — just update last_updated on the canonical entity
            repo.execute(
                "UPDATE entities SET last_updated = ?1 WHERE id = ?2",
                params![last_updated, canonical_id],
            )?;
            return Ok(());
        }

        // No alias match — insert the entity with normalized name
        let tx = repo.transaction()?;
        tx.execute(
            "INSERT INTO entities (id, name, entity_type, first_seen, last_updated)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(name, entity_type) DO UPDATE SET last_updated = ?5",
            params![entity.id, canonical_name, type_str, first_seen, last_updated],
        )?;

        // Register the alias (INSERT OR IGNORE in case of concurrent insert)
        tx.execute(
            "INSERT OR IGNORE INTO entity_aliases (alias, canonical_entity_id)
             VALUES (?1, ?2)",
            params![canonical_name, entity.id],
        )?;

        tx.commit()?;
        Ok(())
    }

    /// Insert a relationship into graph.db.
    /// Confidence defaults to 0.8 for auto-extracted edges.
    pub fn insert_relationship(&self, db_path: &Path, rel: &Relationship) -> Result<()> {
        let repo = SqliteRepo::open(db_path)?;
        init_graph_schema(&repo)?;
        let type_str = format!("{:?}", rel.relation_type);
        let created = rel.created_at.to_rfc3339();
        repo.execute(
            "INSERT INTO relationships
                (id, source_entity_id, target_entity_id, relation_type, confidence, source_note_ids, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                rel.id,
                rel.source_entity_id,
                rel.target_entity_id,
                type_str,
                0.8f64,
                rel.source_note_id,
                created,
            ],
        )?;
        Ok(())
    }

    /// Compute a 384-dim embedding for entity text and store in vec.db.
    /// Falls back to hash-based embedding when no LLM provider is available.
    /// Fails gracefully (returns Err) if the vec0 extension is not loaded.
    pub fn store_entity_embedding(
        &self,
        vec_db_path: &Path,
        entity_id: &str,
        text: &str,
    ) -> Result<()> {
        let embedding = compute_embeddings_for_text(text)?;
        Tier3Search.insert_entity_embedding(vec_db_path, entity_id, &embedding)
    }

    pub fn load_all_entities(&self, db_path: &Path) -> Result<Vec<Entity>> {
        if !db_path.exists() {
            return Ok(Vec::new());
        }
        let repo = SqliteRepo::open(db_path)?;
        let rows = repo.query_map(
            "SELECT id, name, entity_type, first_seen, domain, aliases, last_updated \
             FROM entities ORDER BY name",
            &[],
            |row| {
                let id: String = row.get(0)?;
                let name: String = row.get(1)?;
                let type_str: String = row.get(2)?;
                let first_seen: String = row.get(3)?;
                let domain: Option<String> = row.get(4)?;
                let aliases_raw: Option<String> = row.get(5)?;
                let last_updated: Option<String> = row.get(6)?;

                let entity_type = parse_entity_type(&type_str);
                let created_at = chrono::DateTime::parse_from_rfc3339(&first_seen)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .unwrap_or_else(|_| chrono::Utc::now());
                let updated_at = last_updated
                    .as_deref()
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .unwrap_or(created_at);

                let aliases = parse_aliases_column(aliases_raw.as_deref());

                let mut entity = Entity::new(name, entity_type, "graph-db");
                entity.id = id;
                entity.created_at = created_at;
                entity.last_updated = updated_at;
                entity.domain = domain;
                entity.aliases = aliases;
                Ok(entity)
            },
        )?;
        Ok(rows)
    }

    /// Load entities that have been updated since the given timestamp.
    /// Used by incremental wiki compilation.
    pub fn load_entities_updated_since(
        &self,
        db_path: &Path,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<Entity>> {
        if !db_path.exists() {
            return Ok(Vec::new());
        }
        let repo = SqliteRepo::open(db_path)?;
        let since_str = since.to_rfc3339();
        let rows = repo.query_map(
            "SELECT id, name, entity_type, first_seen, domain, aliases, last_updated \
             FROM entities WHERE last_updated > ?1 ORDER BY name",
            rusqlite::params![since_str],
            |row| {
                let id: String = row.get(0)?;
                let name: String = row.get(1)?;
                let type_str: String = row.get(2)?;
                let first_seen: String = row.get(3)?;
                let domain: Option<String> = row.get(4)?;
                let aliases_raw: Option<String> = row.get(5)?;
                let last_updated: Option<String> = row.get(6)?;

                let entity_type = parse_entity_type(&type_str);
                let created_at = chrono::DateTime::parse_from_rfc3339(&first_seen)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .unwrap_or_else(|_| chrono::Utc::now());
                let updated_at = last_updated
                    .as_deref()
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .unwrap_or(created_at);

                let aliases = parse_aliases_column(aliases_raw.as_deref());

                let mut entity = Entity::new(name, entity_type, "graph-db");
                entity.id = id;
                entity.created_at = created_at;
                entity.last_updated = updated_at;
                entity.domain = domain;
                entity.aliases = aliases;
                Ok(entity)
            },
        )?;
        Ok(rows)
    }

    pub fn load_relationships_for_entity(
        &self,
        db_path: &Path,
        entity_id: &str,
    ) -> Result<Vec<(String, RelationType)>> {
        if !db_path.exists() {
            return Ok(Vec::new());
        }
        let repo = SqliteRepo::open(db_path)?;
        let rows: Vec<(String, RelationType)> = repo.query_map(
            "SELECT target_entity_id, relation_type FROM relationships WHERE source_entity_id = ?1",
            params![entity_id],
            |row| {
                let target_id: String = row.get(0)?;
                let rel_str: String = row.get(1)?;
                let rt = match rel_str.as_str() {
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
                };
                Ok((target_id, rt))
            },
        )?;

        let mut result = Vec::new();
        for (target_id, rt) in rows {
            let target_name = repo
                .query_map(
                    "SELECT name FROM entities WHERE id = ?1",
                    params![target_id],
                    |row| row.get::<_, String>(0),
                )?
                .into_iter()
                .next()
                .unwrap_or(target_id);
            result.push((target_name, rt));
        }
        Ok(result)
    }

    /// Upserts a Self-Model item as a graph entity.
    /// This enables graph-based queries for self-knowledge.
    ///
    /// `layer` is one of: "Knowledge", "Skill", "SocialRole", "SelfConcept", "Trait", "Motivation"
    pub fn upsert_self_model_entity(
        &self,
        db_path: &Path,
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
        self.upsert_entity(db_path, &entity)
    }

    /// Upserts a SelfNode into the dedicated self_nodes table.
    /// This is the Phase C3 implementation with typed columns for 6-layer introspective typing.
    pub fn upsert_self_node(
        &self,
        db_path: &Path,
        node: &super::self_node::SelfNode,
    ) -> Result<()> {
        let repo = SqliteRepo::open(db_path)?;
        init_graph_schema(&repo)?;

        let is_explicit = node.is_explicit.map(|b| b as i32);
        let sufficient_for = serde_json::to_string(&node.sufficient_for).unwrap_or_default();
        let necessary_for = serde_json::to_string(&node.necessary_for).unwrap_or_default();
        let evidence_refs = serde_json::to_string(&node.evidence_refs).unwrap_or_default();

        repo.conn().execute(
            "INSERT OR REPLACE INTO self_nodes (
                id, name, layer, description, domain,
                is_explicit, sufficient_for, necessary_for, controllability,
                humility_score, optionality_count, core_pursuit,
                source, confidence, evidence_refs, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                node.id,
                node.name,
                node.layer.to_string(),
                node.description,
                node.domain,
                is_explicit,
                sufficient_for,
                necessary_for,
                node.controllability,
                node.humility_score,
                node.optionality_count,
                node.core_pursuit,
                node.source,
                node.confidence,
                evidence_refs,
                node.created_at.to_rfc3339(),
                node.updated_at.to_rfc3339(),
            ],
        )?;

        Ok(())
    }

    /// Loads all SelfNodes from the self_nodes table.
    pub fn load_self_nodes(&self, db_path: &Path) -> Result<Vec<super::self_node::SelfNode>> {
        let repo = SqliteRepo::open(db_path)?;
        init_graph_schema(&repo)?;

        let mut stmt = repo.conn().prepare(
            "SELECT id, name, layer, description, domain,
                    is_explicit, sufficient_for, necessary_for, controllability,
                    humility_score, optionality_count, core_pursuit,
                    source, confidence, evidence_refs, created_at, updated_at
             FROM self_nodes ORDER BY name"
        )?;

        let nodes = stmt.query_map([], |row| {
            let layer_str: String = row.get(2)?;
            let layer = layer_str.parse().unwrap_or(super::self_node::SelfModelLayer::Knowledge);

            let is_explicit_i32: Option<i32> = row.get(5)?;
            let is_explicit = is_explicit_i32.map(|b| b != 0);

            let sufficient_for_str: String = row.get(6)?;
            let sufficient_for: Vec<String> = serde_json::from_str(&sufficient_for_str).unwrap_or_default();

            let necessary_for_str: String = row.get(7)?;
            let necessary_for: Vec<String> = serde_json::from_str(&necessary_for_str).unwrap_or_default();

            let evidence_refs_str: String = row.get(14)?;
            let evidence_refs: Vec<String> = serde_json::from_str(&evidence_refs_str).unwrap_or_default();

            let created_at_str: String = row.get(15)?;
            let created_at = chrono::DateTime::parse_from_rfc3339(&created_at_str)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now());

            let updated_at_str: String = row.get(16)?;
            let updated_at = chrono::DateTime::parse_from_rfc3339(&updated_at_str)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now());

            Ok(super::self_node::SelfNode {
                id: row.get(0)?,
                name: row.get(1)?,
                layer,
                description: row.get(3)?,
                domain: row.get(4)?,
                is_explicit,
                sufficient_for,
                necessary_for,
                controllability: row.get(8)?,
                humility_score: row.get(9)?,
                optionality_count: row.get(10)?,
                core_pursuit: row.get(11)?,
                source: row.get(12)?,
                confidence: row.get(13)?,
                evidence_refs,
                created_at,
                updated_at,
            })
        })?;

        let mut result = Vec::new();
        for node in nodes {
            result.push(node?);
        }
        Ok(result)
    }

    /// Loads SelfNodes filtered by layer from the self_nodes table.
    pub fn load_self_nodes_by_layer(
        &self,
        db_path: &Path,
        layer: &super::self_node::SelfModelLayer,
    ) -> Result<Vec<super::self_node::SelfNode>> {
        let repo = SqliteRepo::open(db_path)?;
        init_graph_schema(&repo)?;

        let mut stmt = repo.conn().prepare(
            "SELECT id, name, layer, description, domain,
                    is_explicit, sufficient_for, necessary_for, controllability,
                    humility_score, optionality_count, core_pursuit,
                    source, confidence, evidence_refs, created_at, updated_at
             FROM self_nodes WHERE layer = ?1 ORDER BY name"
        )?;

        let nodes = stmt.query_map([layer.to_string().as_str()], |row| {
            let layer_str: String = row.get(2)?;
            let layer = layer_str.parse().unwrap_or(super::self_node::SelfModelLayer::Knowledge);

            let is_explicit_i32: Option<i32> = row.get(5)?;
            let is_explicit = is_explicit_i32.map(|b| b != 0);

            let sufficient_for_str: String = row.get(6)?;
            let sufficient_for: Vec<String> = serde_json::from_str(&sufficient_for_str).unwrap_or_default();

            let necessary_for_str: String = row.get(7)?;
            let necessary_for: Vec<String> = serde_json::from_str(&necessary_for_str).unwrap_or_default();

            let evidence_refs_str: String = row.get(14)?;
            let evidence_refs: Vec<String> = serde_json::from_str(&evidence_refs_str).unwrap_or_default();

            let created_at_str: String = row.get(15)?;
            let created_at = chrono::DateTime::parse_from_rfc3339(&created_at_str)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now());

            let updated_at_str: String = row.get(16)?;
            let updated_at = chrono::DateTime::parse_from_rfc3339(&updated_at_str)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now());

            Ok(super::self_node::SelfNode {
                id: row.get(0)?,
                name: row.get(1)?,
                layer,
                description: row.get(3)?,
                domain: row.get(4)?,
                is_explicit,
                sufficient_for,
                necessary_for,
                controllability: row.get(8)?,
                humility_score: row.get(9)?,
                optionality_count: row.get(10)?,
                core_pursuit: row.get(11)?,
                source: row.get(12)?,
                confidence: row.get(13)?,
                evidence_refs,
                created_at,
                updated_at,
            })
        })?;

        let mut result = Vec::new();
        for node in nodes {
            result.push(node?);
        }
        Ok(result)
    }

    pub fn upsert_goal_node(
        &self,
        db_path: &Path,
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
        self.upsert_entity(db_path, &entity)
    }

    pub fn upsert_decision_node(
        &self,
        db_path: &Path,
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
        self.upsert_entity(db_path, &entity)
    }

    pub fn upsert_belief_node(
        &self,
        db_path: &Path,
        id: &str,
        name: &str,
        proposition: &str,
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
            .insert("posterior".to_string(), posterior.to_string());
        entity
            .metadata
            .insert("evidence_count".to_string(), evidence_count.to_string());
        self.upsert_entity(db_path, &entity)
    }

    pub fn upsert_path_node(
        &self,
        db_path: &Path,
        id: &str,
        name: &str,
        serves_goal: &str,
        is_default: bool,
        crowdedness: f64,
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
        self.upsert_entity(db_path, &entity)?;

        // Create ServesGoal relationship from this path to the goal
        let goal_entities = self.load_all_entities(db_path)?;
        if let Some(goal) = goal_entities.iter().find(|e| e.name == normalize_entity_name(serves_goal)) {
            let rel = Relationship::new(id, goal.id.clone(), RelationType::ServesGoal, "path-model");
            self.insert_relationship(db_path, &rel)?;
        }

        Ok(())
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

    #[test]
    fn test_alias_resolution() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");

        let service = EntityService::new();

        // Create first entity with name "Rust"
        let mut entity1 = Entity::new("Rust", super::super::entity::EntityType::Technology, "note1");
        entity1.id = "entity-rust-1".to_string();
        service.upsert_entity(&db_path, &entity1).unwrap();

        // Create second entity with name "rust" — should resolve to the same entity
        let mut entity2 = Entity::new("rust", super::super::entity::EntityType::Technology, "note2");
        entity2.id = "entity-rust-2".to_string();
        service.upsert_entity(&db_path, &entity2).unwrap();

        // Verify: only one entity exists (the canonical one)
        let repo = SqliteRepo::open(&db_path).unwrap();
        let count: i32 = repo
            .query_row(
                "SELECT COUNT(*) FROM entities WHERE name = 'rust'",
                &[],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        // Verify alias exists
        let alias_count: i32 = repo
            .query_row(
                "SELECT COUNT(*) FROM entity_aliases WHERE alias = 'rust'",
                &[],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(alias_count, 1);

        // Verify load_known_entity_names includes the canonical name
        let names = service.load_known_entity_names(&db_path).unwrap();
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

    #[test]
    fn test_upsert_self_model_entity() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let service = EntityService::new();

        service
            .upsert_self_model_entity(
                &db_path,
                "sm-1",
                "Rust Proficiency",
                "Skill",
                Some("programming"),
            )
            .unwrap();

        let entities = service.load_all_entities(&db_path).unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].id, "sm-1");
        assert_eq!(entities[0].name, "rust proficiency");
        assert_eq!(entities[0].entity_type, EntityType::SelfModel);
    }

    #[test]
    fn test_upsert_self_model_entity_without_domain() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let service = EntityService::new();

        service
            .upsert_self_model_entity(
                &db_path,
                "sm-2",
                "Curiosity",
                "Trait",
                None,
            )
            .unwrap();

        let entities = service.load_all_entities(&db_path).unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].entity_type, EntityType::SelfModel);
        assert_eq!(entities[0].name, "curiosity");

        let repo = SqliteRepo::open(&db_path).unwrap();
        let stored_type: String = repo
            .query_row(
                "SELECT entity_type FROM entities WHERE id = 'sm-2'",
                &[],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_type, "SelfModel");
    }

    #[test]
    fn test_upsert_self_model_entity_idempotent() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let service = EntityService::new();

        service
            .upsert_self_model_entity(&db_path, "sm-3", "Writing", "Skill", None)
            .unwrap();
        service
            .upsert_self_model_entity(&db_path, "sm-3", "Writing", "Skill", None)
            .unwrap();

        let repo = SqliteRepo::open(&db_path).unwrap();
        let count: i32 = repo
            .query_row(
                "SELECT COUNT(*) FROM entities WHERE id = 'sm-3'",
                &[],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_self_model_entity_loads_correct_type() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let service = EntityService::new();

        service
            .upsert_self_model_entity(&db_path, "sm-4", "Empathy", "Trait", Some("social"))
            .unwrap();

        let entities = service.load_all_entities(&db_path).unwrap();
        let entity = &entities[0];
        assert_eq!(entity.entity_type, EntityType::SelfModel);

        let type_str = format!("{:?}", entity.entity_type);
        let reparsed = parse_entity_type(&type_str);
        assert_eq!(reparsed, EntityType::SelfModel);
    }

    #[test]
    fn test_upsert_goal_node() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let service = EntityService::new();

        service
            .upsert_goal_node(&db_path, "g-1", "Learn Rust", 0.8, "mastery", Some("2026-12-31"))
            .unwrap();

        let entities = service.load_all_entities(&db_path).unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].id, "g-1");
        assert_eq!(entities[0].name, "learn rust");
        assert_eq!(entities[0].entity_type, EntityType::Goal);
    }

    #[test]
    fn test_upsert_goal_node_without_deadline() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let service = EntityService::new();

        service
            .upsert_goal_node(&db_path, "g-2", "Ship Feature", 0.5, "delivery", None)
            .unwrap();

        let entities = service.load_all_entities(&db_path).unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].entity_type, EntityType::Goal);
    }

    #[test]
    fn test_upsert_goal_node_metadata() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let service = EntityService::new();

        service
            .upsert_goal_node(&db_path, "g-3", "Run Marathon", 0.9, "fitness", Some("2027-06-01"))
            .unwrap();

        let repo = SqliteRepo::open(&db_path).unwrap();
        let stored_type: String = repo
            .query_row(
                "SELECT entity_type FROM entities WHERE id = 'g-3'",
                &[],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_type, "Goal");
    }

    #[test]
    fn test_upsert_goal_node_idempotent() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let service = EntityService::new();

        service
            .upsert_goal_node(&db_path, "g-4", "Write Book", 0.6, "creative", None)
            .unwrap();
        service
            .upsert_goal_node(&db_path, "g-4", "Write Book", 0.6, "creative", None)
            .unwrap();

        let repo = SqliteRepo::open(&db_path).unwrap();
        let count: i32 = repo
            .query_row(
                "SELECT COUNT(*) FROM entities WHERE id = 'g-4'",
                &[],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_upsert_path_node() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let service = EntityService::new();

        service
            .upsert_goal_node(&db_path, "g-5", "Learn Rust", 0.8, "mastery", None)
            .unwrap();

        service
            .upsert_path_node(&db_path, "p-1", "Online Courses", "Learn Rust", true, 0.3)
            .unwrap();

        let entities = service.load_all_entities(&db_path).unwrap();
        assert_eq!(entities.len(), 2);
        let path = entities.iter().find(|e| e.id == "p-1").unwrap();
        assert_eq!(path.entity_type, EntityType::Path);
        assert_eq!(path.name, "online courses");
    }

    #[test]
    fn test_upsert_path_node_creates_serves_goal_relationship() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let service = EntityService::new();

        service
            .upsert_goal_node(&db_path, "g-6", "Master Cooking", 0.7, "hobby", None)
            .unwrap();
        service
            .upsert_path_node(&db_path, "p-2", "Cooking Classes", "Master Cooking", false, 0.8)
            .unwrap();

        let rels = service.load_relationships_for_entity(&db_path, "p-2").unwrap();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].1, RelationType::ServesGoal);
        assert_eq!(rels[0].0, "master cooking");
    }

    #[test]
    fn test_upsert_path_node_without_existing_goal() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let service = EntityService::new();

        service
            .upsert_path_node(&db_path, "p-3", "Mentorship", "Nonexistent Goal", true, 0.5)
            .unwrap();

        let entities = service.load_all_entities(&db_path).unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].entity_type, EntityType::Path);

        let rels = service.load_relationships_for_entity(&db_path, "p-3").unwrap();
        assert!(rels.is_empty());
    }

    #[test]
    fn test_upsert_path_node_idempotent() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let service = EntityService::new();

        service
            .upsert_goal_node(&db_path, "g-7", "Get Fit", 0.9, "health", None)
            .unwrap();
        service
            .upsert_path_node(&db_path, "p-4", "Gym", "Get Fit", true, 0.6)
            .unwrap();
        service
            .upsert_path_node(&db_path, "p-4", "Gym", "Get Fit", true, 0.6)
            .unwrap();

        let repo = SqliteRepo::open(&db_path).unwrap();
        let count: i32 = repo
            .query_row(
                "SELECT COUNT(*) FROM entities WHERE id = 'p-4'",
                &[],
                |row| row.get(0),
            )
            .unwrap();
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

    #[test]
    fn test_upsert_decision_node() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let service = EntityService::new();

        service
            .upsert_decision_node(
                &db_path,
                "d-1",
                "Choose Framework",
                "Build web app",
                "React",
                Some("executed"),
            )
            .unwrap();

        let entities = service.load_all_entities(&db_path).unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].id, "d-1");
        assert_eq!(entities[0].name, "choose framework");
        assert_eq!(entities[0].entity_type, EntityType::Decision);
    }

    #[test]
    fn test_upsert_decision_node_without_outcome() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let service = EntityService::new();

        service
            .upsert_decision_node(
                &db_path,
                "d-2",
                "Pick Language",
                "Write CLI",
                "Rust",
                None,
            )
            .unwrap();

        let entities = service.load_all_entities(&db_path).unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].entity_type, EntityType::Decision);
    }

    #[test]
    fn test_upsert_decision_node_idempotent() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let service = EntityService::new();

        service
            .upsert_decision_node(&db_path, "d-3", "Deploy", "Ship", "AWS", Some("done"))
            .unwrap();
        service
            .upsert_decision_node(&db_path, "d-3", "Deploy", "Ship", "AWS", Some("done"))
            .unwrap();

        let repo = SqliteRepo::open(&db_path).unwrap();
        let count: i32 = repo
            .query_row(
                "SELECT COUNT(*) FROM entities WHERE id = 'd-3'",
                &[],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_upsert_belief_node() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let service = EntityService::new();

        service
            .upsert_belief_node(
                &db_path,
                "b-1",
                "Rust is fast",
                "Rust has zero-cost abstractions",
                0.95,
                12,
            )
            .unwrap();

        let entities = service.load_all_entities(&db_path).unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].id, "b-1");
        assert_eq!(entities[0].name, "rust is fast");
        assert_eq!(entities[0].entity_type, EntityType::Belief);
    }

    #[test]
    fn test_upsert_belief_node_idempotent() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let service = EntityService::new();

        service
            .upsert_belief_node(&db_path, "b-2", "TS is safe", "TypeScript catches bugs", 0.8, 5)
            .unwrap();
        service
            .upsert_belief_node(&db_path, "b-2", "TS is safe", "TypeScript catches bugs", 0.8, 5)
            .unwrap();

        let repo = SqliteRepo::open(&db_path).unwrap();
        let count: i32 = repo
            .query_row(
                "SELECT COUNT(*) FROM entities WHERE id = 'b-2'",
                &[],
                |row| row.get(0),
            )
            .unwrap();
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
}
