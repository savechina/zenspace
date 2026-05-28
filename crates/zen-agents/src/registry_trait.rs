use thiserror::Error;

use crate::agent_profile::{AgentProfile, Capability, Role, SensitivityLevel};

/// Errors that can occur when interacting with the agent registry.
#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("agent not found: {name}")]
    AgentNotFound { name: String },

    #[error("no agents match role: {role}")]
    RoleNotFound { role: String },

    #[error("no agents match capabilities: {capabilities}")]
    CapabilityNotFound { capabilities: String },

    #[error("agent already registered: {name}")]
    DuplicateAgent { name: String },
}

impl From<RegistryError> for zen_core::errors::ZenError {
    fn from(err: RegistryError) -> Self {
        zen_core::errors::ZenError::Service(err.to_string())
    }
}

/// Trait defining the agent registry interface.
///
/// Implementations manage the collection of agent profiles and provide
/// lookup operations for session assembly.
pub trait AgentRegistry: std::fmt::Debug + Send + Sync {
    /// Find agents that match the given role.
    ///
    /// Returns all profiles whose role equals the requested role.
    fn find_by_role(&self, role: Role) -> Vec<&AgentProfile>;

    /// Find agents that possess all requested capabilities.
    ///
    /// Returns profiles where `profile.has_all_capabilities(required)` is true.
    fn find_by_capability(&self, required: &[Capability]) -> Vec<&AgentProfile>;

    /// List all registered agent profiles.
    fn list_all(&self) -> Vec<&AgentProfile>;

    /// Register a new agent profile.
    ///
    /// Returns an error if an agent with the same name already exists.
    fn register(&mut self, profile: AgentProfile) -> Result<(), RegistryError>;

    /// Find a specific agent by name.
    ///
    /// Returns an error if the agent is not found.
    fn find_by_name(&self, name: &str) -> Result<&AgentProfile, RegistryError>;

    /// Filter agents by maximum sensitivity level.
    ///
    /// Returns agents where `profile.can_handle_sensitivity(level)` is true.
    fn filter_by_sensitivity(&self, max_level: SensitivityLevel) -> Vec<&AgentProfile>;
}
