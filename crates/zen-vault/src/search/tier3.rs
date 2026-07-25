use anyhow::Result;
use rig_core::Embed;
use rig_sqlite::{Column, ColumnValue, SqliteVectorStoreTable};
use serde::{Deserialize, Serialize};
use tracing::debug;
use zen_repo::{
    EmbeddingsRepo, InsertNoteEmbeddingRequest, InsertNotionEmbeddingRequest, SqliteClient,
};

use super::SearchResult;

// ---------------------------------------------------------------------------
// Tier 3 search: sqlite-vec KNN cosine similarity + rig-sqlite integration
// ---------------------------------------------------------------------------

#[derive(Embed, Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeDocument {
    pub id: String,
    #[embed]
    pub content: String,
    pub source: String,
    pub sensitivity: String,
}

impl SqliteVectorStoreTable for KnowledgeDocument {
    fn name() -> &'static str {
        "knowledge_docs"
    }
    fn schema() -> Vec<Column> {
        vec![
            Column::new("id", "TEXT PRIMARY KEY"),
            Column::new("content", "TEXT"),
            Column::new("source", "TEXT"),
            Column::new("sensitivity", "TEXT"),
        ]
    }
    fn id(&self) -> String {
        self.id.clone()
    }
    fn column_values(&self) -> Vec<(&'static str, Box<dyn ColumnValue>)> {
        vec![
            ("id", Box::new(self.id.clone())),
            ("content", Box::new(self.content.clone())),
            ("source", Box::new(self.source.clone())),
            ("sensitivity", Box::new(self.sensitivity.clone())),
        ]
    }
}

pub struct Tier3Search;

impl Tier3Search {
    pub fn knowledge_doc_schema_name() -> &'static str {
        KnowledgeDocument::name()
    }

    pub fn knowledge_doc_schema_columns() -> usize {
        KnowledgeDocument::schema().len()
    }

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

        debug!(
            "Tier3Search: found {} results for {DIM}-dim query (top_k={top_k})",
            docs.len(),
        );

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

const DIM: usize = 384;

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

    #[test]
    fn test_knowledge_document_schema() {
        let doc = KnowledgeDocument {
            id: "test-1".to_string(),
            content: "Test content".to_string(),
            source: "test".to_string(),
            sensitivity: "private".to_string(),
        };

        assert_eq!(KnowledgeDocument::name(), "knowledge_docs");
        let schema = KnowledgeDocument::schema();
        assert_eq!(schema.len(), 4);
        assert_eq!(doc.id(), "test-1");
    }
}
