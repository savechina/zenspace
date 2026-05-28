use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};
use uuid::Uuid;

use zen_core::paths::ZenPaths;
use zen_core::types::{Sensitivity, SessionStatus};

/// Session entity persisted to `~/.zen/sessions/<id>.json`.
///
/// Per data-model.md §3.9: JSON file is primary storage (Tier 2 derived cache).
/// SQLite table is derived from these files for fast queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntity {
    /// Unique session identifier (UUID v7).
    pub id: String,
    /// Agent name from zen-agents registry.
    pub agent_name: String,
    /// Computed max sensitivity across retrieved notes.
    pub sensitivity_policy: Sensitivity,
    /// Session creation time (ISO 8601).
    pub created_at: DateTime<Utc>,
    /// Last session activity (ISO 8601).
    pub updated_at: DateTime<Utc>,
    /// Session lifecycle state.
    pub status: SessionStatus,
}

impl SessionEntity {
    /// Create a new session entity with the given agent name.
    pub fn new(agent_name: &str) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::now_v7().to_string(),
            agent_name: agent_name.to_string(),
            sensitivity_policy: Sensitivity::Private, // Safe default (FR-071)
            created_at: now,
            updated_at: now,
            status: SessionStatus::Active,
        }
    }

    /// Save this session to `~/.zen/sessions/<id>.json`.
    pub fn save(&self) -> Result<PathBuf> {
        let dir = Self::sessions_dir()?;
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create sessions directory: {}", dir.display()))?;

        let file_path = dir.join(format!("{}.json", self.id));
        let json =
            serde_json::to_string_pretty(self).context("failed to serialize session entity")?;
        std::fs::write(&file_path, json)
            .with_context(|| format!("failed to write session file: {}", file_path.display()))?;

        debug!("saved session {} to {}", self.id, file_path.display());
        Ok(file_path)
    }

    /// Load a session from `~/.zen/sessions/<id>.json`.
    pub fn load(id: &str) -> Result<SessionEntity> {
        let dir = Self::sessions_dir()?;
        let file_path = dir.join(format!("{}.json", id));

        let json = std::fs::read_to_string(&file_path)
            .with_context(|| format!("session not found: {id}"))?;
        let session: SessionEntity = serde_json::from_str(&json)
            .with_context(|| format!("failed to parse session file: {}", file_path.display()))?;

        Ok(session)
    }

    /// List all sessions, sorted by updated_at descending.
    pub fn list() -> Result<Vec<SessionEntity>> {
        let dir = Self::sessions_dir()?;

        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut sessions = Vec::new();
        let entries = std::fs::read_dir(&dir)
            .with_context(|| format!("failed to read sessions directory: {}", dir.display()))?;

        for entry in entries {
            let entry = entry.context("failed to read directory entry")?;
            let path = entry.path();

            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                match Self::load(&path.file_stem().expect("valid filename").to_string_lossy()) {
                    Ok(session) => sessions.push(session),
                    Err(e) => debug!("skipping invalid session file {}: {}", path.display(), e),
                }
            }
        }

        // Sort by updated_at descending (most recent first)
        sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_at));

        debug!("listed {} sessions from {}", sessions.len(), dir.display());
        Ok(sessions)
    }

    /// List only active sessions.
    pub fn list_active() -> Result<Vec<SessionEntity>> {
        Ok(Self::list()?
            .into_iter()
            .filter(|s| s.status == SessionStatus::Active)
            .collect())
    }

    /// Transition session to Archived state (terminal).
    pub fn archive(&mut self) -> Result<()> {
        self.status = SessionStatus::Archived;
        self.updated_at = Utc::now();
        self.save()?;
        info!(session_id = %self.id, "session archived");
        Ok(())
    }

    /// Transition session to Compacted state (context was truncated).
    pub fn compact(&mut self) -> Result<()> {
        self.status = SessionStatus::Compacted;
        self.updated_at = Utc::now();
        self.save()?;
        info!(session_id = %self.id, "session compacted");
        Ok(())
    }

    /// Reactivate a compacted session.
    pub fn reactivate(&mut self) -> Result<()> {
        if self.status == SessionStatus::Archived {
            anyhow::bail!("cannot reactivate archived session");
        }
        self.status = SessionStatus::Active;
        self.updated_at = Utc::now();
        self.save()?;
        info!(session_id = %self.id, "session reactivated");
        Ok(())
    }

    fn sessions_dir() -> Result<PathBuf> {
        let paths = ZenPaths::detect().context("failed to resolve zen paths")?;
        Ok(paths.sessions())
    }
}

/// Session lifecycle manager — creates, persists, resumes, and archives sessions.
///
/// Lives in zen-memory because memory owns all session data (FR-078).
pub struct SessionManager;

impl SessionManager {
    pub fn new() -> Self {
        Self
    }

    /// Create a new session with the specified agent.
    ///
    /// Returns the created SessionEntity, already persisted to disk.
    pub fn create_session(&self, agent_name: &str) -> Result<SessionEntity> {
        let session = SessionEntity::new(agent_name);
        session.save()?;
        info!(
            session_id = %session.id,
            agent = agent_name,
            "session created"
        );
        Ok(session)
    }

    /// Resume an existing session by ID.
    ///
    /// Loads the session from disk and reactivates it if compacted.
    /// Returns error if session is archived.
    pub fn resume_session(&self, session_id: &str) -> Result<SessionEntity> {
        let mut session = SessionEntity::load(session_id)?;

        match session.status {
            SessionStatus::Active => {
                debug!(session_id = %session_id, "resuming active session");
            },
            SessionStatus::Compacted => {
                session.reactivate()?;
                info!(session_id = %session_id, "resumed compacted session");
            },
            SessionStatus::Archived => {
                anyhow::bail!("cannot resume archived session '{}'", session_id);
            },
            SessionStatus::Completed | SessionStatus::Failed => {
                anyhow::bail!("cannot resume {} session '{}'", session.status, session_id);
            },
        }

        Ok(session)
    }

    /// Archive a session (terminal state).
    pub fn archive_session(&self, session_id: &str) -> Result<()> {
        let mut session = SessionEntity::load(session_id)?;
        session.archive()
    }

    /// Get the current status of a session.
    pub fn get_status(&self, session_id: &str) -> Result<SessionStatus> {
        let session = SessionEntity::load(session_id)?;
        Ok(session.status)
    }

    /// List all sessions.
    pub fn list_sessions(&self) -> Result<Vec<SessionEntity>> {
        SessionEntity::list()
    }

    /// List only active sessions.
    pub fn list_active_sessions(&self) -> Result<Vec<SessionEntity>> {
        SessionEntity::list_active()
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_status_display() {
        assert_eq!(SessionStatus::Active.to_string(), "Active");
        assert_eq!(SessionStatus::Compacted.to_string(), "Compacted");
        assert_eq!(SessionStatus::Archived.to_string(), "Archived");
    }

    #[test]
    fn session_entity_new_has_correct_defaults() {
        let session = SessionEntity::new("test-agent");
        assert_eq!(session.agent_name, "test-agent");
        assert_eq!(session.sensitivity_policy, Sensitivity::Private);
        assert_eq!(session.status, SessionStatus::Active);
        assert!(!session.id.is_empty());
    }

    #[test]
    fn session_entity_serialization_roundtrip() {
        let session = SessionEntity::new("Sisyphus-Junior");
        let json = serde_json::to_string(&session).unwrap();
        let loaded: SessionEntity = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.agent_name, "Sisyphus-Junior");
        assert_eq!(loaded.status, SessionStatus::Active);
    }

    #[test]
    fn session_state_transitions() {
        let mut session = SessionEntity::new("test");
        assert_eq!(session.status, SessionStatus::Active);

        session.compact().unwrap();
        assert_eq!(session.status, SessionStatus::Compacted);

        session.reactivate().unwrap();
        assert_eq!(session.status, SessionStatus::Active);

        session.archive().unwrap();
        assert_eq!(session.status, SessionStatus::Archived);

        // Archived cannot be reactivated
        assert!(session.reactivate().is_err());
    }
}
