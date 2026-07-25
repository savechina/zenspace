#![allow(async_fn_in_trait)]

use crate::client::Result;
use crate::types::{
    ComponentResult, GraphSearchResult, InsertRelationshipRequest, NotionRow, PageRankResult,
    RelationRow, ShortestPathResult,
};

pub trait NotionsRepository {
    async fn insert_entity(&self, id: &str, name: &str, kind: &str, now: &str) -> Result<()>;

    #[allow(clippy::too_many_arguments)]
    async fn insert_entity_with(
        &self,
        id: &str,
        name: &str,
        kind: &str,
        now: &str,
        description: &str,
        source: &str,
        confidence: f64,
    ) -> Result<()>;

    async fn upsert_entity(
        &self,
        id: &str,
        name: &str,
        kind: &str,
        created_at: &str,
        last_updated: &str,
    ) -> Result<()>;

    #[allow(clippy::too_many_arguments)]
    async fn upsert_entity_with(
        &self,
        id: &str,
        name: &str,
        kind: &str,
        created_at: &str,
        last_updated: &str,
        description: &str,
        source: &str,
        confidence: f64,
    ) -> Result<()>;

    async fn update_entity_timestamp(&self, notion_id: &str, last_updated: &str) -> Result<()>;

    async fn update_entity_access(&self, notion_id: &str) -> Result<()>;

    async fn update_entity_confidence(&self, notion_id: &str, confidence: f64) -> Result<()>;

    async fn promote_entity(&self, notion_id: &str) -> Result<()>;

    async fn insert_alias(&self, alias: &str, canonical_notion_id: &str) -> Result<()>;

    async fn load_aliases_for_entity(&self, notion_id: &str) -> Result<Vec<String>>;

    async fn insert_relationship(&self, req: &InsertRelationshipRequest<'_>) -> Result<()>;

    async fn load_known_notion_names(&self) -> Result<Vec<String>>;

    async fn load_all_entities(&self) -> Result<Vec<NotionRow>>;

    async fn load_entities_updated_since(&self, since: &str) -> Result<Vec<NotionRow>>;

    async fn resolve_alias(&self, alias: &str) -> Result<Option<String>>;

    async fn load_relationships(&self, notion_id: &str) -> Result<Vec<RelationRow>>;

    async fn load_relationships_all(&self, notion_id: &str) -> Result<Vec<RelationRow>>;

    async fn notion_name(&self, notion_id: &str) -> Result<Option<String>>;

    async fn find_entity_by_name(&self, name: &str) -> Result<Option<NotionRow>>;

    async fn search_notions_fts(&self, query: &str) -> Result<Vec<NotionRow>>;

    async fn bfs_search(&self, notion_name: &str, max_depth: u32)
    -> Result<Vec<GraphSearchResult>>;

    async fn bfs_search_filtered(
        &self,
        notion_name: &str,
        max_depth: u32,
        relation_type_filter: &str,
    ) -> Result<Vec<GraphSearchResult>>;

    async fn shortest_paths_all(
        &self,
        notion_name: &str,
        max_depth: u32,
    ) -> Result<Vec<ShortestPathResult>>;

    async fn shortest_path(
        &self,
        src_name: &str,
        dst_name: &str,
        max_depth: u32,
    ) -> Result<Option<ShortestPathResult>>;

    async fn pagerank(&self, iterations: usize, damping: f64) -> Result<Vec<PageRankResult>>;

    async fn connected_components(&self) -> Result<Vec<ComponentResult>>;

    async fn apply_confidence_decay(&self, half_life_days: f64) -> Result<usize>;

    async fn auto_promote_entities(&self, access_threshold: i64) -> Result<usize>;

    async fn compute_importance(
        &self,
        iterations: usize,
        damping: f64,
    ) -> Result<Vec<PageRankResult>>;
}
