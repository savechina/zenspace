use anyhow::Result;
use serde_json::Value;
use tracing::debug;

use crate::tools::{
    SharedSqliteClient, ZenTool, ZenToolError, ZenToolResult, args_schema_entity,
    result_schema_array,
};
use zen_repo::{
    ComponentResult, GraphSearchResult, InsertRelationshipRequest, NotionsRepo, PageRankResult,
    ShortestPathResult, SqliteClient,
};

pub struct GraphResult {
    pub notion: String,
    pub depth: u32,
    pub relation: String,
    pub target: String,
    pub source_entity: String,
    pub direction: String,
}

impl From<GraphSearchResult> for GraphResult {
    fn from(r: GraphSearchResult) -> Self {
        GraphResult {
            notion: r.notion,
            depth: r.depth,
            relation: r.relation,
            target: r.target,
            source_entity: r.source_entity,
            direction: r.direction,
        }
    }
}

/// Tier 4 search: notion graph traversal with BFS.
#[derive(Debug)]
pub struct Tier4Search;

impl Tier4Search {
    pub async fn search(
        &self,
        client: &SqliteClient,
        notion_name: &str,
        max_depth: u32,
    ) -> Result<Vec<GraphResult>> {
        if notion_name.trim().is_empty() {
            return Ok(Vec::new());
        }

        let results = NotionsRepo::new(client)
            .bfs_search(notion_name, max_depth)
            .await?;

        let graph_results: Vec<GraphResult> = results.into_iter().map(GraphResult::from).collect();

        debug!(
            "Tier4Search: found {} notions for '{}' (depth={})",
            graph_results.len(),
            notion_name,
            max_depth
        );
        Ok(graph_results)
    }

    pub async fn insert_entity(
        &self,
        client: &SqliteClient,
        id: &str,
        name: &str,
        kind: &str,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let repo = NotionsRepo::new(client);

        repo.insert_entity(id, name, kind, &now).await?;

        // Register normalized alias for notion deduplication.
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
        NotionsRepo::new(client).insert_relationship(&req).await?;
        Ok(())
    }

    pub async fn shortest_path(
        &self,
        client: &SqliteClient,
        src_name: &str,
        dst_name: &str,
        max_depth: u32,
    ) -> Result<Option<ShortestPathResult>> {
        NotionsRepo::new(client)
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
        NotionsRepo::new(client)
            .pagerank(iterations, damping)
            .await
            .map_err(Into::into)
    }

    pub async fn connected_components(
        &self,
        client: &SqliteClient,
    ) -> Result<Vec<ComponentResult>> {
        NotionsRepo::new(client)
            .connected_components()
            .await
            .map_err(Into::into)
    }
}

/// Agent-facing `tier4_search` tool bound to a workspace-resolved DB.
///
/// The DB path is injected at construction (via [`SharedSqliteClient`]);
/// invocations never open a client themselves — the pre-D7 impl opened
/// `./state.db` relative to the process CWD, which silently queried the
/// wrong (or a nonexistent) database.
pub struct Tier4SearchTool {
    db: SharedSqliteClient,
    inner: Tier4Search,
}

impl Tier4SearchTool {
    pub fn new(db: SharedSqliteClient) -> Self {
        Self {
            db,
            inner: Tier4Search,
        }
    }
}

impl ZenTool for Tier4SearchTool {
    fn schema(&self) -> crate::tools::ToolSchema {
        crate::tools::ToolSchema {
            name: "tier4_search".to_string(),
            description: "Notion graph traversal using BFS from a starting notion.".to_string(),
            args_schema: args_schema_entity(),
            result_schema: result_schema_array(),
        }
    }

    async fn invoke(&self, args: Value) -> ZenToolResult {
        let notion_name = args
            .get("notion_name")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ZenToolError::InvalidArgs("missing required field: notion_name".to_string())
            })?;
        let max_depth = args.get("max_depth").and_then(Value::as_u64).unwrap_or(3) as u32;

        let client = self.db.get().await.map_err(ZenToolError::ExecutionFailed)?;

        let results = self
            .inner
            .search(&client, notion_name, max_depth)
            .await
            .map_err(|e| ZenToolError::ExecutionFailed(e.to_string()))?;

        let formatted: Vec<Value> = results
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "notion": r.notion,
                    "depth": r.depth,
                    "relation": r.relation,
                    "target": r.target,
                    "source_entity": r.source_entity,
                    "direction": r.direction,
                })
            })
            .collect();

        Ok(serde_json::json!({ "notions": formatted }))
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
        let tool = Tier4SearchTool::new(SharedSqliteClient::new(std::path::PathBuf::from(
            "unused.db",
        )));
        let schema = tool.schema();
        assert_eq!(schema.name, "tier4_search");
        assert!(schema.description.contains("BFS"));
        assert!(
            schema
                .args_schema
                .get("properties")
                .and_then(|p| p.get("db_path"))
                .is_none(),
            "db_path must not be an invocable arg — it was the CWD bug vector"
        );
    }

    #[tokio::test]
    async fn tier4_tool_uses_injected_db() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("state.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let tier4 = Tier4Search;
        tier4
            .insert_entity(&client, "e1", "Alice", "person")
            .await
            .unwrap();
        tier4
            .insert_entity(&client, "e2", "Bob", "person")
            .await
            .unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        tier4
            .insert_relationship(&client, "r1", "e1", "e2", "knows", 1.0, None, &now)
            .await
            .unwrap();

        let tool = Tier4SearchTool::new(SharedSqliteClient::new(db_path));
        let result = tool
            .invoke(serde_json::json!({ "notion_name": "Alice", "max_depth": 2 }))
            .await
            .unwrap();
        let notions = result["notions"].as_array().unwrap();
        assert!(!notions.is_empty(), "got: {notions:?}");
    }
}
