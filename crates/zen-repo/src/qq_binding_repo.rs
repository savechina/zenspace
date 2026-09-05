//! QQBot chat-binding persistence (Phase 13, T061).
//!
//! PURPOSE: Maps a QQ chat identity (group_openid or user_openid) to
//! the hosted gateway session serving it, so the carrier can route
//! each chat to its session across messages and `/new` resets.
//!
//! PERSISTENCE SEMANTICS (honest scope): this table survives daemon
//! restarts, but ONLY the `chat_id → session_id` linkage does. The
//! hosted session context itself lives in gateway memory
//! (`SessionHost.sessions`), so after a restart `session/start`
//! re-creates an EMPTY context under the same id — the binding row
//! preserves routing stability, not conversational memory. Full
//! cross-restart continuity requires durable hosted sessions
//! (Phase 11 memory-durability batch); until then `/new` and a stale
//! binding are behaviorally equivalent for the user.
//!
//! USAGE: Constructed by the qqbot carrier against the daemon's
//! state.db [`crate::SqliteClient`]; upsert on session creation, get
//! on inbound messages, delete on `/new` command reset.

use crate::client::{Result, SqliteClient, SqliteError};

/// Persistent `chat_id → session_id` mapping row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct QqBindingRow {
    /// QQ chat identity (group_openid or user_openid).
    pub chat_id: String,
    /// Hosted gateway sessionId serving this chat.
    pub session_id: String,
    /// Unix-seconds row creation time.
    pub created_at: i64,
    /// Unix-seconds last update time.
    pub updated_at: i64,
}

/// Repository over the `qq_bindings` table (Principle XII: holds
/// `&SqliteClient`, async methods only).
pub struct QqBindingRepo<'a> {
    client: &'a SqliteClient,
}

impl<'a> QqBindingRepo<'a> {
    /// Creates a repo bound to `client`.
    pub fn new(client: &'a SqliteClient) -> Self {
        Self { client }
    }

    /// Inserts or updates the binding for `chat_id`.
    ///
    /// On conflict `session_id`/`updated_at` are replaced;
    /// `created_at` is preserved from the first insert.
    ///
    /// # Errors
    /// SQLite write failure.
    pub async fn upsert(&self, chat_id: &str, session_id: &str) -> Result<()> {
        let chat_id = chat_id.to_string();
        let session_id = session_id.to_string();
        self.client
            .writer()
            .call(move |conn| {
                let now = chrono::Utc::now().timestamp();
                conn.execute(
                    "INSERT INTO qq_bindings (chat_id, session_id, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?3) \
                     ON CONFLICT(chat_id) DO UPDATE SET \
                       session_id = excluded.session_id, \
                       updated_at = excluded.updated_at",
                    rusqlite::params![chat_id, session_id, now],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    /// Loads the binding for `chat_id`, if any.
    ///
    /// # Errors
    /// SQLite read failure.
    pub async fn get(&self, chat_id: &str) -> Result<Option<QqBindingRow>> {
        Ok(sqlx::query_as::<_, QqBindingRow>(
            "SELECT chat_id, session_id, created_at, updated_at \
             FROM qq_bindings WHERE chat_id = ?1",
        )
        .bind(chat_id)
        .fetch_optional(self.client.pool())
        .await?)
    }

    /// Removes the binding for `chat_id` (no-op when absent).
    ///
    /// # Errors
    /// SQLite write failure.
    pub async fn delete(&self, chat_id: &str) -> Result<()> {
        let chat_id = chat_id.to_string();
        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "DELETE FROM qq_bindings WHERE chat_id = ?1",
                    rusqlite::params![chat_id],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    /// All bound chat ids, most-recently-active first (T101: the qqbot
    /// outbox drainer pushes staged briefs to exactly these chats — no
    /// new config surface, no probing of never-seen openids).
    pub async fn list_chat_ids(&self) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar::<_, String>(
            "SELECT chat_id FROM qq_bindings ORDER BY updated_at DESC",
        )
        .fetch_all(self.client.pool())
        .await?)
    }
}
