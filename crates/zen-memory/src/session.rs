// Re-export SessionContext, RetrievedNote, ConversationTurn from zen-core::types (FR-081)
pub use zen_core::types::{ConversationTurn, RetrievedNote, SessionContext};

use anyhow::Result;

use zen_core::types::{SessionEntity, SessionStatus};

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
    pub fn create_session(&self, agent_name: &str, workspace: &str) -> Result<SessionEntity> {
        let session = SessionEntity::new(agent_name, workspace);
        session.save()?;
        tracing::info!(
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
                tracing::debug!(session_id = %session_id, "resuming active session");
            }
            SessionStatus::Compacted => {
                session.reactivate()?;
                tracing::info!(session_id = %session_id, "resumed compacted session");
            }
            SessionStatus::Archived => {
                anyhow::bail!("cannot resume archived session '{}'", session_id);
            }
            SessionStatus::Completed | SessionStatus::Failed => {
                anyhow::bail!("cannot resume {} session '{}'", session.status, session_id);
            }
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

    /// Fork an existing session (deep copy messages).
    pub fn fork_session(&self, source_id: &str, title: Option<String>) -> Result<SessionEntity> {
        let source = SessionEntity::load(source_id)?;
        let forked = source.fork(title);
        forked.save()?;

        tracing::info!(
            session_id = %forked.id,
            parent_id = %source_id,
            "session forked"
        );

        Ok(forked)
    }

    /// Rename an existing session.
    pub fn rename_session(&self, session_id: &str, title: String) -> Result<()> {
        let mut session = SessionEntity::load(session_id)?;
        session.rename(title)
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}
