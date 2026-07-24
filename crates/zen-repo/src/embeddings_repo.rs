use crate::client::{Result, SqliteClient, SqliteError};
use crate::types::{InsertNotionEmbeddingRequest, InsertNoteEmbeddingRequest, VecSearchResult};

pub struct EmbeddingsRepo<'a> {
    client: &'a SqliteClient,
}

impl<'a> EmbeddingsRepo<'a> {
    pub fn new(client: &'a SqliteClient) -> Self {
        Self { client }
    }

    /// Ensure vec0 virtual tables exist at the system embedding dimension.
    ///
    /// Creates the tables at 4096-dim if they don't exist. If the tables
    /// already exist from a prior migration (e.g. at 384-dim), they are
    /// kept as-is for backward compatibility — the application layer
    /// handles padding shorter vectors to 4096-dim.
    pub async fn ensure_embedding_tables(&self) -> Result<()> {
        self.client
            .writer()
            .call(|conn| {
                conn.execute_batch(
                    "CREATE VIRTUAL TABLE IF NOT EXISTS note_embeddings USING vec0(
                         note_id TEXT PRIMARY KEY,
                         embedding FLOAT[4096]
                     );
                     CREATE VIRTUAL TABLE IF NOT EXISTS notion_embeddings USING vec0(
                         notion_id TEXT PRIMARY KEY,
                         embedding FLOAT[4096]
                     );"
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)
    }

    pub async fn search(&self, query_embedding: &[f32], top_k: usize) -> Result<Vec<VecSearchResult>> {
        if query_embedding.is_empty() {
            return Ok(Vec::new());
        }
        let top_k = top_k.max(1) as i64;
        let blob: Vec<u8> = query_embedding
            .iter()
            .flat_map(|f| f.to_le_bytes().to_vec())
            .collect();

        let results = sqlx::query_as::<_, VecSearchResult>(
            "SELECT nm.file_path, 0 as line, nf.content \
             FROM note_embeddings ne \
             JOIN notes_meta nm ON ne.note_id = nm.id \
             JOIN notes_fts nf ON nm.rowid = nf.rowid \
             WHERE ne.embedding MATCH ?1 AND k = ?2 \
             ORDER BY distance",
        )
        .bind(blob)
        .bind(top_k)
        .fetch_all(self.client.pool())
        .await?;
        Ok(results)
    }

    /// Fetch the stored embedding for a given note ID, if present.
    pub async fn get_note_embedding(&self, note_id: &str) -> Result<Option<Vec<f32>>> {
        let note_id = note_id.to_string();
        let row: Option<(Vec<u8>,)> = sqlx::query_as("SELECT embedding FROM note_embeddings WHERE note_id = ?1")
            .bind(note_id)
            .fetch_optional(self.client.pool())
            .await?;

        match row {
            None => Ok(None),
            Some((blob,)) => {
                if blob.is_empty() {
                    return Ok(Some(Vec::new()));
                }
                let floats: Vec<f32> = blob
                    .chunks_exact(4)
                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();
                Ok(Some(floats))
            }
        }
    }

    pub async fn insert_note_embedding(&self, req: InsertNoteEmbeddingRequest<'_>) -> Result<()> {
        let note_id = req.note_id.to_string();
        let blob: Vec<u8> = req
            .embedding
            .iter()
            .flat_map(|f| f.to_le_bytes().to_vec())
            .collect();

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO note_embeddings (note_id, embedding) VALUES (?1, ?2)",
                    rusqlite::params![note_id, blob],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    pub async fn insert_entity_embedding(&self, req: InsertNotionEmbeddingRequest<'_>) -> Result<()> {
        let notion_id = req.notion_id.to_string();
        let blob: Vec<u8> = req
            .embedding
            .iter()
            .flat_map(|f| f.to_le_bytes().to_vec())
            .collect();

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO notion_embeddings (notion_id, embedding) VALUES (?1, ?2)",
                    rusqlite::params![notion_id, blob],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }
}
