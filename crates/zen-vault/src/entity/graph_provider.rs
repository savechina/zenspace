use anyhow::{anyhow, Result};
use async_trait::async_trait;
use zen_core::entity_graph::{EntityGraphProvider, EntitySummary, ImportanceScore, SimpleEntity};
use zen_repo::SqliteClient;

use super::entity::EntityType;
use super::service::EntityService;

pub struct EntityGraphAdapter {
    client: SqliteClient,
}

impl EntityGraphAdapter {
    pub fn from_client(client: SqliteClient) -> Self {
        Self { client }
    }

    fn parse_entity_type(s: &str) -> EntityType {
        match s {
            "Technology" => EntityType::Technology,
            "Concept" => EntityType::Concept,
            "Person" => EntityType::Person,
            "Organization" => EntityType::Organization,
            "Event" => EntityType::Event,
            "Product" => EntityType::Product,
            "Function" => EntityType::Function,
            "Class" => EntityType::Class,
            "Module" => EntityType::Module,
            "SelfModel" => EntityType::SelfModel,
            "Belief" => EntityType::Belief,
            "Goal" => EntityType::Goal,
            "Path" => EntityType::Path,
            "Decision" => EntityType::Decision,
            _ => EntityType::Other,
        }
    }
}

#[async_trait]
impl EntityGraphProvider for EntityGraphAdapter {
    async fn upsert_entity(&self, entity: &SimpleEntity) -> Result<()> {
        let svc = EntityService::new();
        let et = Self::parse_entity_type(&entity.entity_type);
        let mut e = crate::Entity::new(&entity.name, et, &entity.source);
        e.id = entity.id.clone();
        svc.upsert_entity(&self.client, &e)
            .await
            .map_err(|e| anyhow!(e))
    }

    async fn insert_alias(&self, alias: &str, canonical_entity_id: &str) -> Result<()> {
        let repo = zen_repo::EntitiesRepo::new(&self.client);
        repo.insert_alias(alias, canonical_entity_id)
            .await
            .map_err(|e| anyhow!(e))
    }

    async fn find_entity_by_name(&self, name: &str) -> Result<Option<EntitySummary>> {
        let repo = zen_repo::EntitiesRepo::new(&self.client);
        let row = repo
            .find_entity_by_name(name)
            .await
            .map_err(|e| anyhow!(e))?;
        Ok(row.map(|r| EntitySummary {
            id: r.id,
            name: r.name,
            entity_type: r.entity_type,
            description: r.description,
            confidence: r.confidence,
        }))
    }

    async fn apply_confidence_decay(&self, half_life_days: f64) -> Result<usize> {
        let repo = zen_repo::EntitiesRepo::new(&self.client);
        repo.apply_confidence_decay(half_life_days)
            .await
            .map_err(|e| anyhow!(e))
    }

    async fn auto_promote_entities(&self, threshold: i64) -> Result<usize> {
        let repo = zen_repo::EntitiesRepo::new(&self.client);
        repo.auto_promote_entities(threshold)
            .await
            .map_err(|e| anyhow!(e))
    }

    async fn compute_importance(
        &self,
        iterations: usize,
        damping: f64,
    ) -> Result<Vec<ImportanceScore>> {
        let repo = zen_repo::EntitiesRepo::new(&self.client);
        let scores = repo
            .compute_importance(iterations, damping)
            .await
            .map_err(|e| anyhow!(e))?;
        Ok(scores
            .into_iter()
            .map(|s| ImportanceScore {
                entity_id: s.entity,
                score: s.score,
            })
            .collect())
    }

    async fn load_aliases(&self, entity_id: &str) -> Result<Vec<String>> {
        let repo = zen_repo::EntitiesRepo::new(&self.client);
        repo.load_aliases_for_entity(entity_id)
            .await
            .map_err(|e| anyhow!(e))
    }

    fn is_available(&self) -> bool {
        true
    }
}
