use anyhow::{Context, Result};
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use std::path::PathBuf;
use tracing::debug;

pub struct SessionIndex {
    conn: Connection,
}

pub struct IndexedSession {
    pub id: String,
    pub file_path: String,
    pub agent_name: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub workspace: String,
}

impl SessionIndex {
    pub fn open(db_dir: &PathBuf) -> Result<Self> {
        std::fs::create_dir_all(db_dir)?;
        let db_path = db_dir.join("sessions.db");
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=5000;",
        )?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                file_path TEXT NOT NULL,
                agent_name TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                workspace TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_at DESC);",
        )?;
        Ok(Self { conn })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upsert(
        &self,
        id: &str,
        file_path: &str,
        agent_name: &str,
        status: &str,
        created_at: &str,
        updated_at: &str,
        workspace: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO sessions (id, file_path, agent_name, status, created_at, updated_at, workspace) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![id, file_path, agent_name, status, created_at, updated_at, workspace],
        )?;
        debug!(session_id = %id, file_path, "session index upserted");
        Ok(())
    }

    pub fn find(&self, id: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT file_path FROM sessions WHERE id = ?1")?;
        let result: Option<String> = stmt
            .query_row(rusqlite::params![id], |row| row.get(0))
            .optional()?;
        Ok(result)
    }

    pub fn list_all(&self) -> Result<Vec<IndexedSession>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_path, agent_name, status, created_at, updated_at, workspace FROM sessions ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(IndexedSession {
                id: row.get(0)?,
                file_path: row.get(1)?,
                agent_name: row.get(2)?,
                status: row.get(3)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
                workspace: row.get(6)?,
            })
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    pub fn reconcile(&self, id: &str, new_path: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET file_path = ?1 WHERE id = ?2",
            rusqlite::params![new_path, id],
        )?;
        debug!(session_id = %id, new_path, "session index reconciled (read-repair)");
        Ok(())
    }

    pub fn integrity_check(&self) -> Result<String> {
        let result: String = self
            .conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .context("integrity check failed")?;
        debug!(result = %result, "session index integrity check");
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_session_index_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let index = SessionIndex::open(&tmp.path().to_path_buf()).unwrap();

        index
            .upsert(
                "id-1",
                "2025/01/15/id-1.json",
                "agent-a",
                "Active",
                "2025-01-15T10:00:00Z",
                "2025-01-15T12:00:00Z",
                "/workspace",
            )
            .unwrap();
        index
            .upsert(
                "id-2",
                "2025/01/16/id-2.json",
                "agent-b",
                "Completed",
                "2025-01-16T10:00:00Z",
                "2025-01-16T14:00:00Z",
                "/workspace2",
            )
            .unwrap();
        index
            .upsert(
                "id-3",
                "2025/01/17/id-3.json",
                "agent-c",
                "Failed",
                "2025-01-17T10:00:00Z",
                "2025-01-17T08:00:00Z",
                "/workspace3",
            )
            .unwrap();

        let all = index.list_all().unwrap();
        assert_eq!(all.len(), 3);

        let found = index.find("id-2").unwrap();
        assert_eq!(found.as_deref(), Some("2025/01/16/id-2.json"));

        let missing = index.find("no-such-id").unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn test_session_index_reconcile() {
        let tmp = TempDir::new().unwrap();
        let index = SessionIndex::open(&tmp.path().to_path_buf()).unwrap();

        index
            .upsert(
                "id-1",
                "old/path.json",
                "agent",
                "Active",
                "2025-01-01T00:00:00Z",
                "2025-01-01T00:00:00Z",
                "/ws",
            )
            .unwrap();

        index.reconcile("id-1", "2025/06/15/id-1.json").unwrap();

        let path = index.find("id-1").unwrap();
        assert_eq!(path.as_deref(), Some("2025/06/15/id-1.json"));
    }
}
