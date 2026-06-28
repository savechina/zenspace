use crate::client::{Result, SqliteClient, SqliteError};
use crate::types::{FtsResult, IndexNoteRequest};

pub struct NotesRepo<'a> {
    client: &'a SqliteClient,
}

impl<'a> NotesRepo<'a> {
    pub fn new(client: &'a SqliteClient) -> Self {
        Self { client }
    }

    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<FtsResult>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit.max(1) as i64;
        let results = sqlx::query_as::<_, FtsResult>(
            "SELECT nm.file_path as path, bm25(notes_fts) as score, \
                    SUBSTR(nf.content, 1, 200) || CASE WHEN LENGTH(nf.content) > 200 THEN '...' ELSE '' END as snippet \
             FROM notes_fts nf \
             JOIN notes_meta nm ON nf.rowid = nm.rowid \
             WHERE notes_fts MATCH ?1 \
             ORDER BY score LIMIT ?2",
        )
        .bind(query)
        .bind(limit)
        .fetch_all(self.client.pool())
        .await?;
        Ok(results)
    }

    pub async fn index_note(&self, req: IndexNoteRequest<'_>) -> Result<()> {
        let id = req.id.to_string();
        let title = req.title.to_string();
        let content = req.content.to_string();
        let tags = req.tags.to_string();
        let file_path = req.file_path.to_string();
        let source = req.source.to_string();

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO notes_meta (id, file_path, source, domain, project, created_at, updated_at, content_hash) \
                     VALUES (?1, ?2, ?3, '', '', datetime('now'), datetime('now'), '')",
                    rusqlite::params![id, file_path, source],
                )?;
                conn.execute(
                    "INSERT OR REPLACE INTO notes_fts (rowid, id, title, content, tags) \
                     VALUES (last_insert_rowid(), ?1, ?2, ?3, ?4)",
                    rusqlite::params![id, title, content, tags],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }
}
