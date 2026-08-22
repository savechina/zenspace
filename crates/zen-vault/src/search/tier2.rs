use anyhow::Result;
use serde_json::{Value, json};
use std::path::Path;
use tracing::debug;

use crate::tools::{
    SharedSqliteClient, ZenTool, ZenToolError, ZenToolResult, args_schema_string_limit,
    result_schema_array,
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

/// Agent-facing `tier2_search` tool bound to a workspace-resolved DB.
///
/// The DB path is injected at construction (via [`SharedSqliteClient`]);
/// invocations never open a client themselves — the pre-D7 impl opened
/// `./state.db` relative to the process CWD, which silently queried the
/// wrong (or a nonexistent) database.
pub struct Tier2SearchTool {
    db: SharedSqliteClient,
    inner: Tier2Search,
}

impl Tier2SearchTool {
    pub fn new(db: SharedSqliteClient) -> Self {
        Self {
            db,
            inner: Tier2Search,
        }
    }
}

impl ZenTool for Tier2SearchTool {
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
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;

        let client = self.db.get().await.map_err(ZenToolError::ExecutionFailed)?;

        let results = self
            .inner
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
        let tool = Tier2SearchTool::new(SharedSqliteClient::new(std::path::PathBuf::from(
            "unused.db",
        )));
        let schema = tool.schema();
        assert_eq!(schema.name, "tier2_search");
        assert!(schema.description.contains("BM25"));
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
    async fn tier2_tool_uses_injected_db_not_cwd() {
        // CRITICAL regression test (review D7): the pre-D7 impl opened
        // "./state.db" relative to the process CWD. A decoy state.db in the
        // CWD (crate root during tests) must NOT be consulted — only the
        // injected workspace DB.
        let dir = tempdir().unwrap();
        let injected_db = dir.path().join("state.db");
        let client = SqliteClient::open(&injected_db).await.unwrap();
        let tier2 = Tier2Search;
        tier2
            .index_note(
                &client,
                "injected",
                "injected",
                "quantum entanglement note",
                "test",
                "notes/injected.md",
                "test",
            )
            .await
            .unwrap();

        let decoy = std::path::PathBuf::from("state.db");
        let decoy_existed = decoy.exists();
        if !decoy_existed {
            let decoy_client = SqliteClient::open(&decoy).await.unwrap();
            tier2
                .index_note(
                    &decoy_client,
                    "decoy",
                    "decoy",
                    "decoy keyword entanglement",
                    "test",
                    "decoy.md",
                    "test",
                )
                .await
                .unwrap();
        }

        let tool = Tier2SearchTool::new(SharedSqliteClient::new(injected_db.clone()));
        let result = tool
            .invoke(json!({ "query": "entanglement", "limit": 10 }))
            .await
            .unwrap();
        let results = result["results"].as_array().unwrap();
        assert!(!results.is_empty(), "must find the injected note");
        assert!(
            results
                .iter()
                .all(|r| r["path"].as_str().unwrap_or("").contains("injected.md")),
            "results must come from the injected db, got: {results:?}"
        );

        if !decoy_existed {
            let _ = std::fs::remove_file(&decoy);
            let _ = std::fs::remove_file("state.db-shm");
            let _ = std::fs::remove_file("state.db-wal");
        }
    }

    #[tokio::test]
    async fn tier2_tool_ignores_legacy_db_path_arg() {
        let dir = tempdir().unwrap();
        let injected_db = dir.path().join("state.db");
        let client = SqliteClient::open(&injected_db).await.unwrap();
        let tier2 = Tier2Search;
        tier2
            .index_note(
                &client,
                "note",
                "note",
                "unique-banana-stand",
                "test",
                "notes/banana.md",
                "test",
            )
            .await
            .unwrap();

        let tool = Tier2SearchTool::new(SharedSqliteClient::new(injected_db));
        // A caller-supplied db_path must be ignored — it was the bug vector.
        let result = tool
            .invoke(json!({ "query": "banana", "db_path": "/nonexistent/wrong.db" }))
            .await
            .unwrap();
        let results = result["results"].as_array().unwrap();
        assert!(
            results
                .iter()
                .any(|r| r["path"].as_str().unwrap_or("").contains("banana.md")),
            "got: {results:?}"
        );
    }
}
