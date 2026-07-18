use anyhow::{anyhow, Result};
use async_trait::async_trait;
use zen_core::notion_graph::{NotionGraphProvider, NotionSummary, ImportanceScore, SimpleNotion};
use zen_repo::SqliteClient;

use super::notion::NotionKind;
use super::service::NotionService;

pub struct NotionGraphAdapter {
    client: SqliteClient,
}

impl NotionGraphAdapter {
    pub fn from_client(client: SqliteClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl NotionGraphProvider for NotionGraphAdapter {
    async fn upsert_entity(&self, notion: &SimpleNotion) -> Result<()> {
        let svc = NotionService::new();
        let et = super::notion::parse_kind(&notion.kind).unwrap_or(NotionKind::Other);
        let mut e = crate::Notion::new(&notion.name, et, &notion.source);
        e.id = notion.id.clone();
        svc.upsert_entity(&self.client, &e)
            .await
            .map_err(|e| anyhow!(e))
    }

    async fn insert_alias(&self, alias: &str, canonical_notion_id: &str) -> Result<()> {
        let repo = zen_repo::NotionsRepo::new(&self.client);
        repo.insert_alias(alias, canonical_notion_id)
            .await
            .map_err(|e| anyhow!(e))
    }

    async fn find_entity_by_name(&self, name: &str) -> Result<Option<NotionSummary>> {
        let repo = zen_repo::NotionsRepo::new(&self.client);
        let row = repo
            .find_entity_by_name(name)
            .await
            .map_err(|e| anyhow!(e))?;
        Ok(row.map(|r| NotionSummary {
            id: r.id,
            name: r.name,
            kind: r.kind,
            description: r.description,
            confidence: r.confidence,
        }))
    }

    async fn apply_confidence_decay(&self, half_life_days: f64) -> Result<usize> {
        let repo = zen_repo::NotionsRepo::new(&self.client);
        repo.apply_confidence_decay(half_life_days)
            .await
            .map_err(|e| anyhow!(e))
    }

    async fn auto_promote_entities(&self, threshold: i64) -> Result<usize> {
        let repo = zen_repo::NotionsRepo::new(&self.client);
        repo.auto_promote_entities(threshold)
            .await
            .map_err(|e| anyhow!(e))
    }

    async fn compute_importance(
        &self,
        iterations: usize,
        damping: f64,
    ) -> Result<Vec<ImportanceScore>> {
        let repo = zen_repo::NotionsRepo::new(&self.client);
        let scores = repo
            .compute_importance(iterations, damping)
            .await
            .map_err(|e| anyhow!(e))?;
        Ok(scores
            .into_iter()
            .map(|s| ImportanceScore {
                notion_id: s.notion,
                score: s.score,
            })
            .collect())
    }

    async fn load_aliases(&self, notion_id: &str) -> Result<Vec<String>> {
        let repo = zen_repo::NotionsRepo::new(&self.client);
        repo.load_aliases_for_entity(notion_id)
            .await
            .map_err(|e| anyhow!(e))
    }

    fn is_available(&self) -> bool {
        true
    }
}
