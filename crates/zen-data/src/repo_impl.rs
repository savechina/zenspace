use sqlx::Row;
use sqlx::sqlite::SqlitePool;
use zen_core::types::Sensitivity;

use crate::DataError;
use crate::models::{AgentProfile, AuditLog, Note};
use crate::repositories::{AgentProfileRepository, AuditLogRepository, NoteRepository};

use async_trait::async_trait;

pub struct SqliteNoteRepository {
    pool: SqlitePool,
}

impl SqliteNoteRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl NoteRepository for SqliteNoteRepository {
    async fn insert(&self, note: &Note) -> Result<Note, DataError> {
        sqlx::query(
            "INSERT INTO notes (id, session_id, content, sensitivity, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(&note.id)
        .bind(&note.session_id)
        .bind(&note.content)
        .bind(note.sensitivity.to_string())
        .bind(note.created_at.to_rfc3339())
        .bind(note.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;

        Ok(note.clone())
    }

    async fn find_by_session(&self, session_id: &str) -> Result<Vec<Note>, DataError> {
        let rows = sqlx::query(
            "SELECT id, session_id, content, sensitivity, created_at, updated_at FROM notes WHERE session_id = ?1 ORDER BY created_at ASC",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                let sensitivity_str: String = row.get("sensitivity");
                let sensitivity = match sensitivity_str.as_str() {
                    "Public" => Sensitivity::Public,
                    "Private" => Sensitivity::Private,
                    "Confidential" => Sensitivity::Confidential,
                    _ => Sensitivity::Private,
                };

                Ok(Note {
                    id: row.get("id"),
                    session_id: row.get("session_id"),
                    content: row.get("content"),
                    sensitivity,
                    created_at: row
                        .get::<String, _>("created_at")
                        .parse()
                        .unwrap_or_default(),
                    updated_at: row
                        .get::<String, _>("updated_at")
                        .parse()
                        .unwrap_or_default(),
                })
            })
            .collect()
    }

    async fn update_sensitivity(
        &self,
        note_id: &str,
        sensitivity: Sensitivity,
    ) -> Result<Note, DataError> {
        let now = chrono::Utc::now();

        sqlx::query("UPDATE notes SET sensitivity = ?1, updated_at = ?2 WHERE id = ?3")
            .bind(sensitivity.to_string())
            .bind(now.to_rfc3339())
            .bind(note_id)
            .execute(&self.pool)
            .await?;

        self.find_by_session("")
            .await
            .ok()
            .and_then(|notes| notes.into_iter().find(|n| n.id == note_id))
            .ok_or_else(|| DataError::NotFound(format!("note {note_id}")))
    }

    async fn delete(&self, note_id: &str) -> Result<bool, DataError> {
        let result = sqlx::query("DELETE FROM notes WHERE id = ?1")
            .bind(note_id)
            .execute(&self.pool)
            .await?;

        Ok(result.rows_affected() > 0)
    }
}

pub struct SqliteAgentProfileRepository {
    pool: SqlitePool,
}

impl SqliteAgentProfileRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl AgentProfileRepository for SqliteAgentProfileRepository {
    async fn insert(&self, profile: &AgentProfile) -> Result<AgentProfile, DataError> {
        sqlx::query(
            "INSERT OR REPLACE INTO agent_profiles (name, role, config_json, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(&profile.name)
        .bind(&profile.role)
        .bind(&profile.config_json)
        .bind(profile.created_at.to_rfc3339())
        .bind(profile.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;

        Ok(profile.clone())
    }

    async fn find_all(&self) -> Result<Vec<AgentProfile>, DataError> {
        let rows = sqlx::query(
            "SELECT name, role, config_json, created_at, updated_at FROM agent_profiles ORDER BY name ASC",
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                Ok(AgentProfile {
                    name: row.get("name"),
                    role: row.get("role"),
                    config_json: row.get("config_json"),
                    created_at: row
                        .get::<String, _>("created_at")
                        .parse()
                        .unwrap_or_default(),
                    updated_at: row
                        .get::<String, _>("updated_at")
                        .parse()
                        .unwrap_or_default(),
                })
            })
            .collect()
    }

    async fn find_by_name(&self, name: &str) -> Result<Option<AgentProfile>, DataError> {
        let row = sqlx::query(
            "SELECT name, role, config_json, created_at, updated_at FROM agent_profiles WHERE name = ?1",
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => Ok(Some(AgentProfile {
                name: row.get("name"),
                role: row.get("role"),
                config_json: row.get("config_json"),
                created_at: row
                    .get::<String, _>("created_at")
                    .parse()
                    .unwrap_or_default(),
                updated_at: row
                    .get::<String, _>("updated_at")
                    .parse()
                    .unwrap_or_default(),
            })),
            None => Ok(None),
        }
    }

    async fn update(&self, profile: &AgentProfile) -> Result<AgentProfile, DataError> {
        let result = sqlx::query(
            "UPDATE agent_profiles SET role = ?1, config_json = ?2, updated_at = ?3 WHERE name = ?4",
        )
        .bind(&profile.role)
        .bind(&profile.config_json)
        .bind(profile.updated_at.to_rfc3339())
        .bind(&profile.name)
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            return Err(DataError::NotFound(format!(
                "agent profile '{}'",
                profile.name
            )));
        }

        Ok(profile.clone())
    }
}

pub struct SqliteAuditLogRepository {
    pool: SqlitePool,
}

impl SqliteAuditLogRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl AuditLogRepository for SqliteAuditLogRepository {
    async fn insert(&self, log: &AuditLog) -> Result<AuditLog, DataError> {
        sqlx::query(
            "INSERT INTO audit_logs (id, session_id, prompt_hash, timestamp) VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(&log.id)
        .bind(&log.session_id)
        .bind(&log.prompt_hash)
        .bind(log.timestamp.to_rfc3339())
        .execute(&self.pool)
        .await?;

        Ok(log.clone())
    }

    async fn find_by_session(&self, session_id: &str) -> Result<Vec<AuditLog>, DataError> {
        let rows = sqlx::query(
            "SELECT id, session_id, prompt_hash, timestamp FROM audit_logs WHERE session_id = ?1 ORDER BY timestamp ASC",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                Ok(AuditLog {
                    id: row.get("id"),
                    session_id: row.get("session_id"),
                    prompt_hash: row.get("prompt_hash"),
                    timestamp: row
                        .get::<String, _>("timestamp")
                        .parse()
                        .unwrap_or_default(),
                })
            })
            .collect()
    }

    async fn stream_to_file(
        &self,
        session_id: &str,
        output_path: &std::path::Path,
    ) -> Result<usize, DataError> {
        let logs = self.find_by_session(session_id).await?;

        let content: Vec<String> = logs
            .iter()
            .map(|log| serde_json::to_string(log).unwrap_or_default())
            .collect();

        let total = content.len();

        if total > 0 {
            let joined = content.join("\n");
            std::fs::write(output_path, joined)
                .map_err(|e| DataError::NotFound(format!("failed to write audit file: {e}")))?;
        }

        Ok(total)
    }
}
