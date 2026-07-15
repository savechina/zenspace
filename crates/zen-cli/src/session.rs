use anyhow::{Context, Result};
use chrono::Utc;
use tracing::debug;
use uuid::Uuid;
use zen_core::types::{Sensitivity, SessionRecord, SessionStatus};
use zen_memory::memory_service::IdentityContext;

pub struct SessionContext {
    pub memory_context: IdentityContext,
}

impl SessionContext {
    pub fn assemble(agent_name: &str, zen_paths: &zen_core::paths::ZenPaths) -> Result<Self> {
        let registry = zen_agents::DefaultAgentRegistry::default();
        let _profile = zen_agents::AgentRegistry::find_by_name(&registry, agent_name)
            .with_context(|| format!("agent '{agent_name}' not found in default registry"))?;

        let memory_context = zen_memory::memory_service::load_all(zen_paths)
            .with_context(|| "failed to load identity context from zen-memory")?;

        Ok(Self { memory_context })
    }
}

pub struct SessionOrchestrator;

impl SessionOrchestrator {
    pub fn new() -> Self {
        Self
    }

    pub fn start_session_with_agent(&self, workspace: &str, agent: &str) -> Result<SessionRecord> {
        let id = Uuid::now_v7().to_string();
        let now = Utc::now();

        let session = SessionRecord {
            id: id.clone(),
            agent_name: agent.to_string(),
            title: None,
            parent_id: None,
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

    pub fn list_sessions(&self) -> Result<Vec<SessionRecord>> {
        SessionRecord::list()
    }
}

impl Default for SessionOrchestrator {
    fn default() -> Self {
        Self::new()
    }
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
    fn test_session_entity_serialization_roundtrip() {
        let session = SessionRecord {
            id: "test-id".to_string(),
            agent_name: "Sisyphus-Junior".to_string(),
            title: None,
            parent_id: None,
            sensitivity_policy: Sensitivity::Public,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            status: SessionStatus::Active,
            workspace: "/tmp".to_string(),
        };

        let json = serde_json::to_string(&session).unwrap();
        let loaded: SessionRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.id, "test-id");
        assert_eq!(loaded.agent_name, "Sisyphus-Junior");
        assert_eq!(loaded.status, SessionStatus::Active);
    }
}
