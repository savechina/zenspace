//! Memvid replay checkpoint persistence (Phase 11, T049).
//!
//! PURPOSE: Stores the applied jsonl byte offset per session file so
//! the SessionReplayer (T048) can resume incremental jsonl-to-mv2
//! replay after restarts instead of re-replaying every session from
//! byte 0. Together with per-turn idempotency keys this keeps mv2 a
//! derived store — never the sole copy of user data (Phase 11
//! baseline invariant).
//!
//! USAGE: Constructed by the gateway replay tick (T052) against the
//! daemon's state.db [`crate::SqliteClient`]; `load` before replaying
//! a session file, `update` after each applied batch (or to reset the
//! checkpoint to 0 when the offset ran past EOF — full re-replay is
//! safe by idempotency).

use crate::client::{Result, SqliteClient, SqliteError};

/// Repository over the `memvid_replay_offsets` table (Principle XII:
/// holds `&SqliteClient`, async methods only).
pub struct MemvidReplayOffsetRepo<'a> {
    client: &'a SqliteClient,
}

impl<'a> MemvidReplayOffsetRepo<'a> {
    /// Creates a repo bound to `client`.
    pub fn new(client: &'a SqliteClient) -> Self {
        Self { client }
    }

    /// Loads the applied replay offset for `session_path`.
    ///
    /// Returns `None` when the session has no checkpoint yet — the
    /// caller must full-replay from byte 0, which is safe because
    /// replay is idempotent by per-turn keys.
    ///
    /// # Errors
    /// SQLite read failure.
    pub async fn load(&self, session_path: &str) -> Result<Option<i64>> {
        Ok(sqlx::query_scalar::<_, i64>(
            "SELECT applied_offset FROM memvid_replay_offsets WHERE session_path = ?1",
        )
        .bind(session_path)
        .fetch_optional(self.client.pool())
        .await?)
    }

    /// Clears every checkpoint so the next replay pass re-reads all
    /// session files from byte 0 (used by `memory/rebuild` after a full
    /// mv2 reindex wipes the derived turn frames and their dedup keys).
    ///
    /// Operational DELETE from application code — permitted by Principle
    /// XIII #3 (the migration-forbidden list constrains migration files,
    /// not runtime maintenance paths); the table holds derived offsets,
    /// never user data.
    ///
    /// # Errors
    /// SQLite write failure.
    pub async fn reset_all(&self) -> Result<()> {
        self.client
            .writer()
            .call(|conn| {
                conn.execute("DELETE FROM memvid_replay_offsets", [])?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    /// Upserts the applied replay offset for `session_path`.
    ///
    /// Inserts the checkpoint row on first write; an existing row's
    /// `applied_offset` is replaced, covering both monotonic advance
    /// and reset-to-0 after offset-beyond-EOF detection.
    ///
    /// # Errors
    /// SQLite write failure.
    pub async fn update(&self, session_path: &str, applied_offset: i64) -> Result<()> {
        let session_path = session_path.to_string();
        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO memvid_replay_offsets (session_path, applied_offset) \
                     VALUES (?1, ?2) \
                     ON CONFLICT(session_path) DO UPDATE SET \
                       applied_offset = excluded.applied_offset",
                    rusqlite::params![session_path, applied_offset],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }
}
