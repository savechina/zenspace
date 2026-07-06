use anyhow::Result;
use serde_json::Value;
use tracing::debug;

use crate::tools::{ZenTool, ZenToolError, ZenToolResult, args_schema_entity, result_schema_array};
use zen_repo::{
    ComponentResult, EntitiesRepo, GraphSearchResult, InsertRelationshipRequest,
    PageRankResult, ShortestPathResult, SqliteClient,
};

pub struct GraphResult {
    pub entity: String,
    pub depth: u32,
    pub relation: String,
    pub target: String,
    pub source_entity: String,
    pub direction: String,
}

impl From<GraphSearchResult> for GraphResult {
    fn from(r: GraphSearchResult) -> Self {
        GraphResult {
            entity: r.entity,
            depth: r.depth,
            relation: r.relation,
            target: r.target,
            source_entity: r.source_entity,
            direction: r.direction,
        }
    }
}

/// Tier 4 search: entity graph traversal with BFS.
#[derive(Debug)]
pub struct Tier4Search;

impl Tier4Search {
    pub async fn search(
        &self,
        client: &SqliteClient,
        entity_name: &str,
        max_depth: u32,
    ) -> Result<Vec<GraphResult>> {
        if entity_name.trim().is_empty() {
            return Ok(Vec::new());
        }

        let results = EntitiesRepo::new(client).bfs_search(entity_name, max_depth).await?;

        let graph_results: Vec<GraphResult> = results.into_iter().map(GraphResult::from).collect();

        debug!(
            "Tier4Search: found {} entities for '{}' (depth={})",
            graph_results.len(),
            entity_name,
            max_depth
        );
        Ok(graph_results)
    }

    pub async fn insert_entity(
        &self,
        client: &SqliteClient,
        id: &str,
        name: &str,
        entity_type: &str,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let repo = EntitiesRepo::new(client);

        repo.insert_entity(id, name, entity_type, &now).await?;

        // Register normalized alias for entity deduplication.
        use unicode_normalization::UnicodeNormalization;
        let normalized: String = name.nfc().collect();
        let normalized = normalized.trim().to_lowercase();
        repo.insert_alias(&normalized, id).await?;

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_relationship(
        &self,
        client: &SqliteClient,
        id: &str,
        source_id: &str,
        target_id: &str,
        relation_type: &str,
        confidence: f64,
        source_note_ids: Option<&str>,
        created_at: &str,
    ) -> Result<()> {
        let req = InsertRelationshipRequest {
            id,
            source_id,
            target_id,
            rel_type: relation_type,
            confidence,
            source_note_ids,
            created_at,
            description: None,
            valid_from: None,
            valid_until: None,
            weight: None,
        };
        EntitiesRepo::new(client).insert_relationship(&req).await?;
        Ok(())
    }

    pub async fn shortest_path(
        &self,
        client: &SqliteClient,
        src_name: &str,
        dst_name: &str,
        max_depth: u32,
    ) -> Result<Option<ShortestPathResult>> {
        EntitiesRepo::new(client)
            .shortest_path(src_name, dst_name, max_depth)
            .await
            .map_err(Into::into)
    }

    pub async fn pagerank(
        &self,
        client: &SqliteClient,
        iterations: usize,
        damping: f64,
    ) -> Result<Vec<PageRankResult>> {
        EntitiesRepo::new(client)
            .pagerank(iterations, damping)
            .await
            .map_err(Into::into)
    }

    pub async fn connected_components(
        &self,
        client: &SqliteClient,
    ) -> Result<Vec<ComponentResult>> {
        EntitiesRepo::new(client)
            .connected_components()
            .await
            .map_err(Into::into)
    }
}

impl ZenTool for Tier4Search {
    fn schema(&self) -> crate::tools::ToolSchema {
        crate::tools::ToolSchema {
            name: "tier4_search".to_string(),
            description: "Entity graph traversal using BFS from a starting entity.".to_string(),
            args_schema: args_schema_entity(),
            result_schema: result_schema_array(),
        }
    }

    async fn invoke(&self, args: Value) -> ZenToolResult {
        let entity_name = args
            .get("entity_name")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ZenToolError::InvalidArgs("missing required field: entity_name".to_string())
            })?;
        let max_depth = args.get("max_depth").and_then(Value::as_u64).unwrap_or(3) as u32;
        let db_path = args
            .get("db_path")
            .and_then(Value::as_str)
            .unwrap_or("state.db");

        let client = zen_repo::SqliteClient::open(std::path::Path::new(db_path))
            .await
            .map_err(|e| {
                ZenToolError::ExecutionFailed(format!("failed to open state db: {e}"))
            })?;

        let results = self
            .search(&client, entity_name, max_depth)
            .await
            .map_err(|e| ZenToolError::ExecutionFailed(e.to_string()))?;

        let formatted: Vec<Value> = results
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "entity": r.entity,
                    "depth": r.depth,
                    "relation": r.relation,
                    "target": r.target,
                    "source_entity": r.source_entity,
                    "direction": r.direction,
                })
            })
            .collect();

        Ok(serde_json::json!({ "entities": formatted }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    async fn setup_test_db() -> (tempfile::TempDir, SqliteClient) {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("state.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        (dir, client)
    }

    #[tokio::test]
    async fn test_tier4_empty_query_returns_empty() {
        let (_dir, client) = setup_test_db().await;
        let tier4 = Tier4Search;
        let results = tier4.search(&client, "test", 3).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_tier4_insert_and_search_graph() {
        let (_dir, client) = setup_test_db().await;
        let tier4 = Tier4Search;

        tier4
            .insert_entity(&client, "e1", "Alice", "person")
            .await
            .unwrap();
        tier4
            .insert_entity(&client, "e2", "Bob", "person")
            .await
            .unwrap();
        tier4
            .insert_entity(&client, "e3", "Charly", "person")
            .await
            .unwrap();

        let now = chrono::Utc::now().to_rfc3339();
        tier4
            .insert_relationship(&client, "r1", "e1", "e2", "knows", 0.9, None, &now)
            .await
            .unwrap();
        tier4
            .insert_relationship(&client, "r2", "e2", "e3", "knows", 0.8, None, &now)
            .await
            .unwrap();

        let results = tier4.search(&client, "Alice", 2).await.unwrap();
        assert!(results.len() >= 2);
    }

    #[tokio::test]
    async fn test_tier4_bfs_depth_limit() {
        let (_dir, client) = setup_test_db().await;
        let tier4 = Tier4Search;

        for i in 0..5 {
            tier4
                .insert_entity(&client, &format!("e{i}"), &format!("N{i}"), "node")
                .await
                .unwrap();
        }
        for i in 0..4 {
            let now = chrono::Utc::now().to_rfc3339();
            tier4
                .insert_relationship(
                    &client,
                    &format!("r{i}"),
                    &format!("e{i}"),
                    &format!("e{}", i + 1),
                    "next",
                    1.0,
                    None,
                    &now,
                )
                .await
                .unwrap();
        }

        let results_depth1 = tier4.search(&client, "N0", 1).await.unwrap();
        let results_depth3 = tier4.search(&client, "N0", 3).await.unwrap();
        assert!(results_depth3.len() >= results_depth1.len());
    }

    #[tokio::test]
    async fn test_tier4_tool_schema() {
        let tier4 = Tier4Search;
        let schema = tier4.schema();
        assert_eq!(schema.name, "tier4_search");
        assert!(schema.description.contains("BFS"));
    }
}
