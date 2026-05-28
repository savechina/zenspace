use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::debug;
use uuid::Uuid;
use zen_agents::{AgentRegistry, DefaultAgentRegistry};
use zen_core::paths::ZenPaths;
use zen_core::types::{Sensitivity, SessionStatus};
use zen_memory::memory_service::IdentityContext;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntity {
    pub id: String,
    pub agent_name: String,
    pub sensitivity_policy: Sensitivity,
    pub created_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
    pub status: SessionStatus,
    pub workspace: String,
}

// ---------------------------------------------------------------------------
// Session entity persistence (T068)
// ---------------------------------------------------------------------------

impl SessionEntity {
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

    /// List all sessions from `~/.zen/sessions/*.json`.
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

        debug!("listed {} sessions from {}", sessions.len(), dir.display());
        Ok(sessions)
    }

    fn sessions_dir() -> Result<PathBuf> {
        let paths = ZenPaths::detect().context("failed to resolve zen paths")?;
        Ok(paths.sessions())
    }
}

// ---------------------------------------------------------------------------
// Session context assembly (T067)
// ---------------------------------------------------------------------------

pub struct SessionContext {
    pub agent_name: String,
    pub memory_context: IdentityContext,
    pub retrieved_notes: Vec<zen_knowledge::Note>,
    pub sensitivity_policy: Sensitivity,
    pub session_id: Uuid,
}

impl SessionContext {
    /// Assemble a [`SessionContext`] by loading agent definition, memory
    /// context, and knowledge notes.
    ///
    /// - `agent_name` is looked up in the default agent registry.
    /// - `zen_paths` is used to load identity context (SOUL.md, MEMORY.md, AGENTS.md).
    /// - `retrieved_notes` are pre-fetched notes from the knowledge service
    ///   (stubbed as empty for now).
    ///
    /// The `sensitivity_policy` is computed as the maximum sensitivity across all
    /// retrieved notes, defaulting to [`Sensitivity::Private`] when none exist.
    pub fn assemble(
        agent_name: &str,
        zen_paths: &ZenPaths,
        retrieved_notes: Vec<zen_knowledge::Note>,
    ) -> Result<Self> {
        // Load agent definition from zen-agents
        let registry = DefaultAgentRegistry::default();
        let _profile = registry
            .find_by_name(agent_name)
            .with_context(|| format!("agent '{agent_name}' not found in default registry"))?;

        // Load memory context from zen-memory
        let memory_context = zen_memory::memory_service::load_all(zen_paths)
            .with_context(|| "failed to load identity context from zen-memory")?;

        // Compute max sensitivity from retrieved notes
        let sensitivity_policy = compute_max_sensitivity(&retrieved_notes);

        Ok(Self {
            agent_name: agent_name.to_string(),
            memory_context,
            retrieved_notes,
            sensitivity_policy,
            session_id: Uuid::now_v7(),
        })
    }
}

// ---------------------------------------------------------------------------
// Session orchestrator
// ---------------------------------------------------------------------------

pub struct SessionOrchestrator;

impl SessionOrchestrator {
    pub fn new() -> Self {
        Self
    }

    #[allow(dead_code)]
    pub fn start_session(&self, workspace: &str) -> Result<SessionEntity> {
        self.start_session_with_agent(workspace, "default")
    }

    pub fn start_session_with_agent(&self, workspace: &str, agent: &str) -> Result<SessionEntity> {
        let id = Uuid::now_v7().to_string();
        let now = Utc::now();

        let session = SessionEntity {
            id: id.clone(),
            agent_name: agent.to_string(),
            sensitivity_policy: Sensitivity::Public,
            created_at: now,
            updated_at: now,
            status: SessionStatus::Active,
            workspace: workspace.to_string(),
        };

        debug!(
            "starting session {} with agent '{}' and workspace '{}'",
            id, agent, workspace
        );
        session.save()?;
        Ok(session)
    }

    #[allow(dead_code)]
    pub fn get_status(&self, session_id: &str) -> Result<SessionStatus> {
        let session = SessionEntity::load(session_id)?;
        Ok(session.status)
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionEntity>> {
        SessionEntity::list()
    }
}

impl Default for SessionOrchestrator {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Sensitivity helpers
// ---------------------------------------------------------------------------

/// Compute the maximum sensitivity level from a list of notes.
///
/// Returns [`Sensitivity::Private`] when the list is empty
/// (per FR-080: unknown/absent notes default to private).
pub fn compute_max_sensitivity(notes: &[zen_knowledge::Note]) -> Sensitivity {
    if notes.is_empty() {
        return Sensitivity::Private;
    }
    Sensitivity::max_of(&notes.iter().map(|n| n.sensitivity).collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_status_display() {
        assert_eq!(SessionStatus::Active.to_string(), "Active");
        assert_eq!(SessionStatus::Compacted.to_string(), "Compacted");
        assert_eq!(SessionStatus::Completed.to_string(), "Completed");
        assert_eq!(SessionStatus::Failed.to_string(), "Failed");
        assert_eq!(SessionStatus::Archived.to_string(), "Archived");
    }

    #[test]
    fn test_compute_max_sensitivity_empty_returns_private() {
        assert_eq!(compute_max_sensitivity(&[]), Sensitivity::Private);
    }

    #[test]
    fn test_compute_max_sensitivity_single_note() {
        let note = zen_knowledge::Note::default();
        assert_eq!(compute_max_sensitivity(&[note]), Sensitivity::Private);
    }

    #[test]
    fn test_session_entity_serialization_roundtrip() {
        let session = SessionEntity {
            id: "test-id".to_string(),
            agent_name: "Sisyphus-Junior".to_string(),
            sensitivity_policy: Sensitivity::Public,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            status: SessionStatus::Active,
            workspace: "/tmp".to_string(),
        };

        let json = serde_json::to_string(&session).unwrap();
        let loaded: SessionEntity = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.id, "test-id");
        assert_eq!(loaded.agent_name, "Sisyphus-Junior");
        assert_eq!(loaded.status, SessionStatus::Active);
    }
}
