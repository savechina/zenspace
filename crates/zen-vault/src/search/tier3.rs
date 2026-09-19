use anyhow::Result;
use serde_json::{Value, json};
use tracing::debug;
use zen_repo::{
    EmbeddingsRepo, InsertNoteEmbeddingRequest, InsertNotionEmbeddingRequest, SqliteClient,
};

use super::SearchResult;
use crate::tools::{
    SharedSqliteClient, ZenTool, ZenToolError, ZenToolResult, args_schema_string_limit,
    result_schema_array,
};

// ---------------------------------------------------------------------------
// Tier 3 search: sqlite-vec KNN cosine similarity + rig-sqlite integration
// ---------------------------------------------------------------------------

pub struct Tier3Search;

impl Tier3Search {
    pub async fn search(
        &self,
        client: &SqliteClient,
        query_embedding: &[f32],
        top_k: usize,
    ) -> Result<Vec<SearchResult>> {
        if query_embedding.is_empty() {
            return Ok(Vec::new());
        }

        let results = EmbeddingsRepo::new(client)
            .search(query_embedding, top_k)
            .await?;

        let docs: Vec<SearchResult> = results
            .into_iter()
            .map(|r| SearchResult {
                file: std::path::PathBuf::from(r.file),
                line: r.line,
                content: r.content,
            })
            .collect();

        debug!("Tier3Search: found {} results (top_k={top_k})", docs.len(),);

        Ok(docs)
    }

    pub async fn insert_embedding(
        &self,
        client: &SqliteClient,
        note_id: &str,
        embedding: &[f32],
    ) -> Result<()> {
        if embedding.is_empty() {
            anyhow::bail!("Cannot insert empty embedding for note {note_id}");
        }

        EmbeddingsRepo::new(client)
            .insert_note_embedding(InsertNoteEmbeddingRequest { note_id, embedding })
            .await?;

        debug!(
            "Tier3Search: stored embedding for {note_id} ({}-dim)",
            embedding.len()
        );

        Ok(())
    }

    pub async fn insert_entity_embedding(
        &self,
        client: &SqliteClient,
        notion_id: &str,
        embedding: &[f32],
    ) -> Result<()> {
        if embedding.is_empty() {
            anyhow::bail!("Cannot insert empty embedding for notion {notion_id}");
        }

        EmbeddingsRepo::new(client)
            .insert_entity_embedding(InsertNotionEmbeddingRequest {
                notion_id,
                embedding,
            })
            .await?;

        debug!(
            "Tier3Search: stored notion embedding for {notion_id} ({}-dim)",
            embedding.len()
        );

        Ok(())
    }
}

impl std::fmt::Debug for Tier3Search {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tier3Search").finish()
    }
}

/// Agent-facing wrapper exposing vec0 semantic (KNN) search as a tool.
///
/// Complements `tier2_search` (keyword/FTS5) and `tier4_search` (entity
/// graph) with meaning-based retrieval: the query is embedded before the KNN
/// lookup, so prefer it when the user's words may differ from the note text.
pub struct Tier3SearchTool {
    db: SharedSqliteClient,
    inner: Tier3Search,
}

impl Tier3SearchTool {
    pub fn new(db: SharedSqliteClient) -> Self {
        Self {
            db,
            inner: Tier3Search,
        }
    }
}

impl ZenTool for Tier3SearchTool {
    fn schema(&self) -> crate::tools::ToolSchema {
        crate::tools::ToolSchema {
            name: "tier3_search".to_string(),
            description: "Semantic similarity search over notes and entities using sqlite-vec \
                          embeddings (query is embedded first; use for meaning-based lookups \
                          where exact keywords may differ)."
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

        let embedding = crate::tindy::compute_embeddings_for_text(query)
            .map_err(|e| ZenToolError::ExecutionFailed(format!("query embedding failed: {e}")))?;

        let client = self.db.get().await.map_err(ZenToolError::ExecutionFailed)?;

        let results = self
            .inner
            .search(&client, &embedding, limit)
            .await
            .map_err(|e| ZenToolError::ExecutionFailed(e.to_string()))?;

        let formatted: Vec<Value> = results
            .into_iter()
            .map(|r| {
                json!({
                    "path": r.file.to_string_lossy(),
                    "line": r.line,
                    "snippet": r.content,
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
    async fn test_tier3_search_empty_embedding_returns_empty() {
        let (_dir, client) = setup_test_db().await;
        let tier3 = Tier3Search;

        let results = tier3.search(&client, &[], 5).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_tier3_insert_empty_embedding_fails() {
        let (_dir, client) = setup_test_db().await;
        let tier3 = Tier3Search;

        let result = tier3.insert_embedding(&client, "note-x", &[]).await;
        assert!(result.is_err());
    }
}
