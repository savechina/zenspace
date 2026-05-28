use anyhow::Result;

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
}
