use crate::client::{Result, SqliteClient, SqliteError};
use crate::types::{InsertEntityEmbeddingRequest, InsertNoteEmbeddingRequest, VecSearchResult};

pub struct EmbeddingsRepo<'a> {
    client: &'a SqliteClient,
}

impl<'a> EmbeddingsRepo<'a> {
    pub fn new(client: &'a SqliteClient) -> Self {
        Self { client }
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

    pub async fn insert_entity_embedding(&self, req: InsertEntityEmbeddingRequest<'_>) -> Result<()> {
        let entity_id = req.entity_id.to_string();
        let blob: Vec<u8> = req
            .embedding
            .iter()
            .flat_map(|f| f.to_le_bytes().to_vec())
            .collect();

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO entity_embeddings (entity_id, embedding) VALUES (?1, ?2)",
                    rusqlite::params![entity_id, blob],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }
}
