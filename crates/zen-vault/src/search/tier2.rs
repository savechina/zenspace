use anyhow::Result;
use serde_json::{Value, json};
use std::path::Path;
use tracing::debug;

use crate::tools::{
    ZenTool, ZenToolError, ZenToolResult, args_schema_string_limit, result_schema_array,
};

pub use zen_repo::{FtsResult, IndexNoteRequest, NotesRepo, SqliteClient};

#[derive(Debug)]
pub struct Tier2Search;

impl Tier2Search {
    pub async fn search(
        &self,
        client: &SqliteClient,
        query: &str,
        limit: usize,
    ) -> Result<Vec<FtsResult>> {
        let results = NotesRepo::new(client).search(query, limit).await?;

        debug!(
            "Tier2Search: found {} results for query='{}' (limit={})",
            results.len(),
            query,
            limit
        );
        Ok(results)
    }

    pub async fn search_in_dir(
        &self,
        client: &SqliteClient,
        query: &str,
        base_dir: &Path,
        limit: usize,
    ) -> Result<Vec<FtsResult>> {
        let all_results = NotesRepo::new(client).search(query, limit * 2).await?;
        let base_str = base_dir.to_string_lossy().to_string();
        let filtered: Vec<FtsResult> = all_results
            .into_iter()
            .filter(|r| r.path.starts_with(&base_str))
            .take(limit)
            .collect();

        debug!(
            "Tier2Search::search_in_dir: found {} results in {} for query='{}'",
            filtered.len(),
            base_str,
            query
        );
        Ok(filtered)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn index_note(
        &self,
        client: &SqliteClient,
        id: &str,
        title: &str,
        content: &str,
        tags: &str,
        file_path: &str,
        source: &str,
    ) -> Result<()> {
        NotesRepo::new(client)
            .index_note(IndexNoteRequest {
                id,
                title,
                content,
                tags,
                file_path,
                source,
            })
            .await?;

        debug!("Tier2Search: indexed note '{id}' (title='{title}', tags='{tags}')");
        Ok(())
    }
}

impl ZenTool for Tier2Search {
    fn schema(&self) -> crate::tools::ToolSchema {
        crate::tools::ToolSchema {
            name: "tier2_search".to_string(),
            description: "Full-text search over notes with BM25 ranking using SQLite FTS5."
                .to_string(),
            args_schema: args_schema_string_limit(),
            result_schema: result_schema_array(),
        }
    }

    async fn invoke(&self, args: Value) -> ZenToolResult {
        let query = args.get("query").and_then(Value::as_str).ok_or_else(|| {
            ZenToolError::InvalidArgs("missing required field: query".to_string())
        })?;
        let db_path = args
            .get("db_path")
            .and_then(Value::as_str)
            .unwrap_or("state.db");
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;

        let client = zen_repo::SqliteClient::open(std::path::Path::new(db_path))
            .await
            .map_err(|e| ZenToolError::ExecutionFailed(format!("failed to open state db: {e}")))?;

        let results = self
            .search(&client, query, limit)
            .await
            .map_err(|e| ZenToolError::ExecutionFailed(e.to_string()))?;

        let formatted: Vec<Value> = results
            .into_iter()
            .map(|r| {
                json!({
                    "path": r.path,
                    "score": r.score,
                    "snippet": r.snippet,
                })
            })
            .collect();

        Ok(json!({ "results": formatted }))
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
    async fn test_tier2_search_empty_query_returns_empty() {
        let (_dir, client) = setup_test_db().await;
        let tier2 = Tier2Search;
        let results = tier2.search(&client, "test", 10).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_tier2_index_and_search() {
        let (_dir, client) = setup_test_db().await;
        let tier2 = Tier2Search;

        tier2
            .index_note(
                &client,
                "note-1",
                "Hello World",
                "This is a test note about rust programming.",
                "rust,test",
                "notes/hello.md",
                "manual",
            )
            .await
            .unwrap();

        let results = tier2.search(&client, "rust", 10).await.unwrap();
        assert!(!results.is_empty());
        assert!(results[0].path.contains("hello.md"));
    }

    #[tokio::test]
    async fn test_tier2_bm25_ranking() {
        let (_dir, client) = setup_test_db().await;
        let tier2 = Tier2Search;

        tier2
            .index_note(
                &client,
                "note-a",
                "rust",
                "rust rust rust rust rust",
                "rust",
                "a.md",
                "test",
            )
            .await
            .unwrap();
        tier2
            .index_note(
                &client,
                "note-b",
                "rust",
                "rust and other things",
                "rust",
                "b.md",
                "test",
            )
            .await
            .unwrap();

        let results = tier2.search(&client, "rust", 10).await.unwrap();
        assert!(results.len() >= 2);
    }

    #[test]
    fn test_tier2_tool_schema() {
        let tier2 = Tier2Search;
        let schema = tier2.schema();
        assert_eq!(schema.name, "tier2_search");
        assert!(schema.description.contains("BM25"));
    }
}
