use crate::client::{Result, SqliteClient, SqliteError};
use crate::types::{InsertNoteEmbeddingRequest, InsertNotionEmbeddingRequest, VecSearchResult};

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
                     );",
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)
    }

    /// Drop and recreate the vec0 embedding tables.
    ///
    /// The drop requires the vec0 module in-process (the plain `sqlite3` CLI
    /// fails with "no such module: vec0"), so this only works inside a
    /// `SqliteClient`. Callers must re-embed afterwards: the tables come back
    /// empty.
    ///
    /// `notes_meta.content_hash` and `notes_meta.embedded_hash` are cleared in
    /// the same batch so the reindex change gate cannot skip a file and cannot
    /// claim a vector is fresh: a re-embed interrupted mid-walk would otherwise
    /// leave the unprocessed files with matching hashes and no vector —
    /// silently absent from semantic search forever. Drop and create also
    /// share the batch so the tables are never absent across a crash.
    pub async fn reset_embedding_tables(&self) -> Result<()> {
        self.client
            .writer()
            .call(|conn| {
                conn.execute_batch(
                    "DROP TABLE IF EXISTS note_embeddings;
                     DROP TABLE IF EXISTS notion_embeddings;
                     CREATE VIRTUAL TABLE IF NOT EXISTS note_embeddings USING vec0(
                         note_id TEXT PRIMARY KEY,
                         embedding FLOAT[4096]
                     );
                     CREATE VIRTUAL TABLE IF NOT EXISTS notion_embeddings USING vec0(
                         notion_id TEXT PRIMARY KEY,
                         embedding FLOAT[4096]
                     );
                     UPDATE notes_meta SET content_hash = '';
                     UPDATE notes_meta SET embedded_hash = '';",
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)
    }

    /// KNN over the vec0 note embeddings (tier-3 semantic retrieval).
    ///
    /// Do NOT re-add the FTS join or drop the `AS file` alias. [`VecSearchResult`]
    /// derives `FromRow`, which maps columns by FIELD name, so the projection must
    /// be aliased `file`; and `notes_fts` is contentless (`content=''`, migration
    /// 001), so content comes from the canonical markdown file instead. Both were
    /// wrong until 2026-10-17 (`no column found for name: file` on 5/5 production
    /// requests), which silently dropped the vec list from RRF fusion.
    pub async fn search(
        &self,
        query_embedding: &[f32],
        top_k: usize,
    ) -> Result<Vec<VecSearchResult>> {
        if query_embedding.is_empty() {
            return Ok(Vec::new());
        }
        let top_k = top_k.max(1) as i64;
        let blob: Vec<u8> = query_embedding
            .iter()
            .flat_map(|f| f.to_le_bytes().to_vec())
            .collect();

        let mut results = sqlx::query_as::<_, VecSearchResult>(
            "SELECT nm.file_path AS file, 0 AS line, '' AS content \
             FROM note_embeddings ne \
             JOIN notes_meta nm ON ne.note_id = nm.id \
             WHERE ne.embedding MATCH ?1 AND k = ?2 \
             ORDER BY distance",
        )
        .bind(blob)
        .bind(top_k)
        .fetch_all(self.client.pool())
        .await?;
        for result in &mut results {
            result.content = crate::notes_repo::read_canonical_snippet(&result.file);
        }
        Ok(results)
    }

    /// Fetch the stored embedding for a given note ID, if present.
    pub async fn get_note_embedding(&self, note_id: &str) -> Result<Option<Vec<f32>>> {
        let note_id = note_id.to_string();
        let row: Option<(Vec<u8>,)> =
            sqlx::query_as("SELECT embedding FROM note_embeddings WHERE note_id = ?1")
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

    /// Store (or replace) the vec0 vector for a note.
    ///
    /// vec0 does not honor `INSERT OR REPLACE` on an existing `note_id`: the
    /// insert reaches the shadow rowids table and fails with
    /// `UNIQUE constraint failed on note_embeddings primary key`. Replacement
    /// is therefore delete-then-insert inside one writer call.
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
                    "DELETE FROM note_embeddings WHERE note_id = ?1",
                    rusqlite::params![note_id],
                )?;
                conn.execute(
                    "INSERT INTO note_embeddings (note_id, embedding) VALUES (?1, ?2)",
                    rusqlite::params![note_id, blob],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    /// Store (or replace) the vec0 vector for a notion entity.
    ///
    /// Same shadow-rowid trap as [`Self::insert_note_embedding`]: vec0 does not
    /// honor `INSERT OR REPLACE` for an existing `notion_id` (the insert reaches
    /// the shadow rowids table and fails with a UNIQUE violation), so a re-embed
    /// of a changed entity would fail forever. Delete-then-insert instead.
    pub async fn insert_entity_embedding(
        &self,
        req: InsertNotionEmbeddingRequest<'_>,
    ) -> Result<()> {
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
                    "DELETE FROM notion_embeddings WHERE notion_id = ?1",
                    rusqlite::params![notion_id],
                )?;
                conn.execute(
                    "INSERT INTO notion_embeddings (notion_id, embedding) VALUES (?1, ?2)",
                    rusqlite::params![notion_id, blob],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }
}
