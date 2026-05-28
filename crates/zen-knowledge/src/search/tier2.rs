use std::path::Path;

use anyhow::Result;
use serde_json::{Value, json};
use tracing::debug;

use crate::tools::{
    ZenTool, ZenToolError, ZenToolResult, args_schema_string_limit, result_schema_array,
};

/// Tier 2 search: SQLite FTS5 with BM25 ranking.
#[derive(Debug)]
pub struct Tier2Search;

impl Tier2Search {
    fn ensure_fts(db_path: &Path) -> Result<rusqlite::Connection> {
        if !db_path.exists() {
            anyhow::bail!(
                "Database not found at {}. Initialize with init_kb_schema() first.",
                db_path.display()
            );
        }
        let conn = rusqlite::Connection::open(db_path)?;
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS note_fts USING fts5(
                id, title, content, tags, file_path, source
            );",
        )?;
        Ok(conn)
    }

    pub fn search(&self, query: &str, db_path: &Path, limit: usize) -> Result<Vec<FTSResult>> {
        if query.trim().is_empty() || !db_path.exists() {
            return Ok(Vec::new());
        }

        let conn = Self::ensure_fts(db_path)?;
        let limit = limit.max(1);

        let mut stmt = conn.prepare(
            "SELECT title, content, bm25(note_fts) as score, file_path FROM note_fts WHERE note_fts MATCH ?1 ORDER BY score LIMIT ?2",
        )?;

        let results: Vec<FTSResult> = stmt
            .query_map(rusqlite::params![query, limit as i32], |row| {
                Ok(FTSResult {
                    path: row.get::<_, String>(3)?,
                    score: row.get::<_, f64>(2)?,
                    snippet: make_snippet(&row.get::<_, String>(1)?),
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        debug!(
            "Tier2Search: found {} results for query='{}' (limit={})",
            results.len(),
            query,
            limit
        );
        Ok(results)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn index_note(
        &self,
        db_path: &Path,
        id: &str,
        title: &str,
        content: &str,
        tags: &str,
        file_path: &str,
        source: &str,
    ) -> Result<()> {
        if !db_path.exists() {
            anyhow::bail!(
                "Database not found at {}. Initialize with init_kb_schema() first.",
                db_path.display()
            );
        }

        let conn = Self::ensure_fts(db_path)?;
        conn.execute(
            "INSERT OR REPLACE INTO note_fts (id, title, content, tags, file_path, source) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![title, title, content, tags, file_path, source],
        )?;

        debug!("Tier2Search: indexed note '{id}' (title='{title}', tags='{tags}')");
        Ok(())
    }
}

pub struct FTSResult {
    pub path: String,
    pub score: f64,
    pub snippet: String,
}

fn make_snippet(content: &str) -> String {
    if content.len() <= 200 {
        content.to_string()
    } else {
        format!("{}...", &content[..200])
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
            .unwrap_or("kb.db");
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;

        let results = self
            .search(query, Path::new(db_path), limit)
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

    #[test]
    fn test_tier2_search_empty_query_returns_empty() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("kb.db");
        let tier2 = Tier2Search;
        let results = tier2.search("test", &db_path, 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_tier2_index_and_search() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("kb.db");
        let _conn = rusqlite::Connection::open(&db_path).unwrap();
        let tier2 = Tier2Search;

        tier2
            .index_note(
                &db_path,
                "note-1",
                "Hello World",
                "This is a test note about rust programming.",
                "rust,test",
                "notes/hello.md",
                "manual",
            )
            .unwrap();

        let results = tier2.search("rust", &db_path, 10).unwrap();
        assert!(!results.is_empty());
        assert!(results[0].path.contains("hello.md"));
    }

    #[test]
    fn test_tier2_bm25_ranking() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("kb.db");
        let _conn = rusqlite::Connection::open(&db_path).unwrap();
        let tier2 = Tier2Search;

        tier2
            .index_note(
                &db_path,
                "note-a",
                "rust",
                "rust rust rust rust rust",
                "rust",
                "a.md",
                "test",
            )
            .unwrap();
        tier2
            .index_note(
                &db_path,
                "note-b",
                "rust",
                "rust and other things",
                "rust",
                "b.md",
                "test",
            )
            .unwrap();

        let results = tier2.search("rust", &db_path, 10).unwrap();
        assert!(results.len() >= 2);
    }

    #[test]
    fn test_tier2_missing_db_fails() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("nonexistent.db");
        let tier2 = Tier2Search;

        let result = tier2.index_note(&db_path, "x", "t", "c", "", "f", "s");
        assert!(result.is_err());
    }

    #[test]
    fn test_tier2_tool_schema() {
        let tier2 = Tier2Search;
        let schema = tier2.schema();
        assert_eq!(schema.name, "tier2_search");
        assert!(schema.description.contains("BM25"));
    }
}
