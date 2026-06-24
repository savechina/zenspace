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
fn normalize_entity_name(name: &str) -> String {
    let mut s = name.trim().to_lowercase();
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
        _ => EntityType::Other,
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
            "SELECT id, name, entity_type, first_seen FROM entities ORDER BY name",
            &[],
            |row| {
                let id: String = row.get(0)?;
                let name: String = row.get(1)?;
                let type_str: String = row.get(2)?;
                let first_seen: String = row.get(3)?;
                let entity_type = parse_entity_type(&type_str);
                let created_at = chrono::DateTime::parse_from_rfc3339(&first_seen)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .unwrap_or_else(|_| chrono::Utc::now());
                let mut entity = Entity::new(name, entity_type, "graph-db");
                entity.id = id;
                entity.created_at = created_at;
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
}
