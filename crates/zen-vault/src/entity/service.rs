use anyhow::Result;
use std::collections::HashSet;
use std::path::Path;

use rusqlite::params;

use zen_repo::sqlite_repo::{SqliteRepo, init_graph_schema};

use crate::maintenance::compute_embeddings_for_text;
use crate::search::Tier3Search;

use super::entity::Entity;
use super::relationship::Relationship;

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
    /// Returns an empty set if the database does not exist yet.
    pub fn load_known_entity_names(&self, db_path: &Path) -> Result<HashSet<String>> {
        if !db_path.exists() {
            return Ok(HashSet::new());
        }
        let repo = SqliteRepo::open(db_path)?;
        let names = repo.query_map(
            "SELECT DISTINCT name FROM entities",
            &[],
            |row| row.get::<_, String>(0),
        )?;
        Ok(names.into_iter().collect())
    }

    /// Upsert an entity into graph.db.
    /// Ensures the schema exists before writing.
    pub fn upsert_entity(&self, db_path: &Path, entity: &Entity) -> Result<()> {
        let repo = SqliteRepo::open(db_path)?;
        init_graph_schema(&repo)?;
        let type_str = format!("{:?}", entity.entity_type);
        let first_seen = entity.created_at.to_rfc3339();
        let last_updated = chrono::Utc::now().to_rfc3339();
        repo.execute(
            "INSERT INTO entities (id, name, entity_type, first_seen, last_updated)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(name, entity_type) DO UPDATE SET last_updated = ?5",
            params![entity.id, entity.name, type_str, first_seen, last_updated],
        )?;
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
}
