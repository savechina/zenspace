use std::collections::HashMap;

use crate::agent_profile::{
    AgentClearance, AgentProfile, Capability, CostPerToken, LlmPreference, Role,
};

use thiserror::Error;

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
    fn filter_by_sensitivity(&self, max_level: AgentClearance) -> Vec<&AgentProfile>;
}

/// In-memory agent registry with pre-populated default agents.
///
/// Holds built-in agent profiles (Sisyphus, Junior, Hermes, Metis, Momus,
/// Oracle, Prometheus, Explore, Librarian) and supports dynamic
/// registration of additional profiles.
#[derive(Debug)]
pub struct DefaultAgentRegistry {
    agents: HashMap<String, AgentProfile>,
}

impl DefaultAgentRegistry {
    /// Construct a new registry with built-in agent profiles loaded.
    #[must_use]
    pub fn new() -> Self {
        let mut registry = Self {
            agents: HashMap::new(),
        };
        registry.populate_defaults();
        registry
    }

    fn populate_defaults(&mut self) {
        let profiles = builtin_agents();
        for profile in profiles {
            let name = profile.name.clone();
            self.agents.insert(name, profile);
        }
    }
}

impl Default for DefaultAgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentRegistry for DefaultAgentRegistry {
    fn find_by_role(&self, role: Role) -> Vec<&AgentProfile> {
        self.agents.values().filter(|p| p.role == role).collect()
    }

    fn find_by_capability(&self, required: &[Capability]) -> Vec<&AgentProfile> {
        self.agents
            .values()
            .filter(|p| p.has_all_capabilities(required))
            .collect()
    }

    fn list_all(&self) -> Vec<&AgentProfile> {
        self.agents.values().collect()
    }

    fn register(&mut self, profile: AgentProfile) -> Result<(), RegistryError> {
        if self.agents.contains_key(&profile.name) {
            return Err(RegistryError::DuplicateAgent { name: profile.name });
        }
        self.agents.insert(profile.name.clone(), profile);
        Ok(())
    }

    fn find_by_name(&self, name: &str) -> Result<&AgentProfile, RegistryError> {
        self.agents
            .get(name)
            .ok_or_else(|| RegistryError::AgentNotFound {
                name: name.to_string(),
            })
    }

    fn filter_by_sensitivity(&self, max_level: AgentClearance) -> Vec<&AgentProfile> {
        self.agents
            .values()
            .filter(|p| p.can_handle_sensitivity(max_level))
            .collect()
    }
}

fn builtin_agents() -> Vec<AgentProfile> {
    let cheap = CostPerToken {
        input_cents_per_million: 1.0,
        output_cents_per_million: 2.0,
    };
    let moderate = CostPerToken {
        input_cents_per_million: 5.0,
        output_cents_per_million: 10.0,
    };
    let premium = CostPerToken {
        input_cents_per_million: 10.0,
        output_cents_per_million: 20.0,
    };

    vec![
        // Sisyphus: Orchestrator — single entry point, task classification & routing
        AgentProfile {
            name: "Sisyphus".to_string(),
            role: Role::Orchestrator,
            capabilities: vec![
                Capability::TaskExecution,
                Capability::SessionManagement,
                Capability::Architecture,
                Capability::Analysis,
                Capability::Automation,
            ],
            llm_preferences: vec![LlmPreference::Any],
            max_sensitivity: AgentClearance::High,
            cost_per_token: moderate,
            definition: None,
        },
        // Junior: Worker tier — focused executor
        AgentProfile {
            name: "Junior".to_string(),
            role: Role::Worker,
            capabilities: vec![
                Capability::TaskExecution,
                Capability::CodeGeneration,
                Capability::Debugging,
                Capability::Testing,
                Capability::Documentation,
            ],
            llm_preferences: vec![LlmPreference::Any],
            max_sensitivity: AgentClearance::Medium,
            cost_per_token: cheap,
            definition: None,
        },
        // Hermes: Worker tier — delivery validator & push officer
        AgentProfile {
            name: "Hermes".to_string(),
            role: Role::Worker,
            capabilities: vec![
                Capability::TaskExecution,
                Capability::SessionManagement,
                Capability::Automation,
                Capability::Analysis,
            ],
            llm_preferences: vec![LlmPreference::Any],
            max_sensitivity: AgentClearance::High,
            cost_per_token: moderate,
            definition: None,
        },
        // Metis: Planner tier — tactical reviewer & gap analyst
        AgentProfile {
            name: "Metis".to_string(),
            role: Role::Planner,
            capabilities: vec![
                Capability::Architecture,
                Capability::SpecificationWriting,
                Capability::Analysis,
                Capability::Research,
            ],
            llm_preferences: vec![LlmPreference::Any],
            max_sensitivity: AgentClearance::Medium,
            cost_per_token: moderate,
            definition: None,
        },
        // Momus: Planner tier — gate reviewer
        AgentProfile {
            name: "Momus".to_string(),
            role: Role::Planner,
            capabilities: vec![
                Capability::CodeReview,
                Capability::Architecture,
                Capability::SecurityAudit,
            ],
            llm_preferences: vec![LlmPreference::Any],
            max_sensitivity: AgentClearance::Medium,
            cost_per_token: cheap,
            definition: None,
        },
        // Oracle: Specialist tier — deep technical analysis
        AgentProfile {
            name: "Oracle".to_string(),
            role: Role::Specialist,
            capabilities: vec![
                Capability::Analysis,
                Capability::Research,
                Capability::Architecture,
            ],
            llm_preferences: vec![LlmPreference::Any],
            max_sensitivity: AgentClearance::High,
            cost_per_token: premium,
            definition: None,
        },
        // Prometheus: Planner tier — strategic planner
        AgentProfile {
            name: "Prometheus".to_string(),
            role: Role::Planner,
            capabilities: vec![
                Capability::SpecificationWriting,
                Capability::Architecture,
                Capability::Analysis,
                Capability::CodeGeneration,
            ],
            llm_preferences: vec![LlmPreference::Any],
            max_sensitivity: AgentClearance::Medium,
            cost_per_token: moderate,
            definition: None,
        },
        // Explore: Specialist tier — research & exploration
        AgentProfile {
            name: "Explore".to_string(),
            role: Role::Specialist,
            capabilities: vec![
                Capability::Research,
                Capability::Analysis,
                Capability::Documentation,
            ],
            llm_preferences: vec![LlmPreference::CloudOnly],
            max_sensitivity: AgentClearance::Low,
            cost_per_token: cheap,
            definition: None,
        },
        // Librarian: Specialist tier — knowledge organization
        AgentProfile {
            name: "Librarian".to_string(),
            role: Role::Specialist,
            capabilities: vec![
                Capability::KnowledgeManagement,
                Capability::MemoryManagement,
                Capability::Documentation,
                Capability::Research,
            ],
            llm_preferences: vec![LlmPreference::Any],
            max_sensitivity: AgentClearance::High,
            cost_per_token: cheap,
            definition: None,
        },
        // Argus: Specialist tier — multimodal visual worker (FR-AGENT-001)
        AgentProfile {
            name: "Argus".to_string(),
            role: Role::Specialist,
            capabilities: vec![
                Capability::Research,
                Capability::Analysis,
                Capability::Documentation,
            ],
            llm_preferences: vec![LlmPreference::Any],
            max_sensitivity: AgentClearance::Medium,
            cost_per_token: moderate,
            definition: None,
        },
        // Hephaestus: Worker tier — deep executor, end-to-end implementation (FR-AGENT-001)
        AgentProfile {
            name: "Hephaestus".to_string(),
            role: Role::Worker,
            capabilities: vec![
                Capability::TaskExecution,
                Capability::CodeGeneration,
                Capability::Architecture,
                Capability::Debugging,
                Capability::Refactoring,
                Capability::Testing,
            ],
            llm_preferences: vec![LlmPreference::Any],
            max_sensitivity: AgentClearance::High,
            cost_per_token: premium,
            definition: None,
        },
        // Atlas: Worker tier — execution foreman, batch task decomposition (FR-AGENT-001)
        AgentProfile {
            name: "Atlas".to_string(),
            role: Role::Worker,
            capabilities: vec![
                Capability::TaskExecution,
                Capability::Automation,
                Capability::Analysis,
            ],
            llm_preferences: vec![LlmPreference::Any],
            max_sensitivity: AgentClearance::Medium,
            cost_per_token: cheap,
            definition: None,
        },
        // Zeus: Planner tier — final value judge, veto/amnesty power (FR-AGENT-001)
        AgentProfile {
            name: "Zeus".to_string(),
            role: Role::Planner,
            capabilities: vec![
                Capability::Architecture,
                Capability::SecurityAudit,
                Capability::Analysis,
            ],
            llm_preferences: vec![LlmPreference::Any],
            max_sensitivity: AgentClearance::High,
            cost_per_token: premium,
            definition: None,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_registry_has_thirteen_builtin_agents() {
        let registry = DefaultAgentRegistry::new();
        let all = registry.list_all();
        assert_eq!(all.len(), 13);
    }

    #[test]
    fn find_by_role_returns_correct_agents() {
        let registry = DefaultAgentRegistry::new();
        let planners = registry.find_by_role(Role::Planner);
        assert_eq!(planners.len(), 4);
        let names: Vec<&str> = planners.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"Metis"));
        assert!(names.contains(&"Momus"));
        assert!(names.contains(&"Prometheus"));
        assert!(names.contains(&"Zeus"));

        let orchestrators = registry.find_by_role(Role::Orchestrator);
        assert_eq!(orchestrators.len(), 1);
        assert_eq!(orchestrators[0].name, "Sisyphus");

        let specialists = registry.find_by_role(Role::Specialist);
        assert_eq!(specialists.len(), 4);
        let spec_names: Vec<&str> = specialists.iter().map(|p| p.name.as_str()).collect();
        assert!(spec_names.contains(&"Oracle"));
        assert!(spec_names.contains(&"Explore"));
        assert!(spec_names.contains(&"Librarian"));
        assert!(spec_names.contains(&"Argus"));

        let workers = registry.find_by_role(Role::Worker);
        assert_eq!(workers.len(), 4);
        let worker_names: Vec<&str> = workers.iter().map(|p| p.name.as_str()).collect();
        assert!(worker_names.contains(&"Junior"));
        assert!(worker_names.contains(&"Hermes"));
        assert!(worker_names.contains(&"Hephaestus"));
        assert!(worker_names.contains(&"Atlas"));
    }

    #[test]
    fn find_by_capability_returns_agents_with_all_caps() {
        let registry = DefaultAgentRegistry::new();
        let code_reviewers = registry.find_by_capability(&[Capability::CodeReview]);
        assert_eq!(code_reviewers.len(), 1);
        assert_eq!(code_reviewers[0].name, "Momus");
    }

    #[test]
    fn find_by_capability_multiple_required() {
        let registry = DefaultAgentRegistry::new();
        let required = vec![Capability::Research, Capability::Analysis];
        let matches = registry.find_by_capability(&required);
        let names: Vec<&str> = matches.iter().map(|p| p.name.as_str()).collect();
        // Oracle, Explore, Metis all have Research + Analysis
        assert!(names.contains(&"Oracle"));
        assert!(names.contains(&"Explore"));
        assert!(names.contains(&"Metis"));
    }

    #[test]
    fn find_by_name_existing_agent() {
        let registry = DefaultAgentRegistry::new();
        let profile = registry.find_by_name("Junior");
        assert!(profile.is_ok());
        assert_eq!(profile.unwrap().role, Role::Worker);
    }

    #[test]
    fn find_by_name_missing_agent() {
        let registry = DefaultAgentRegistry::new();
        let result = registry.find_by_name("NonExistent");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            RegistryError::AgentNotFound { .. }
        ));
    }

    #[test]
    fn register_new_agent() {
        let mut registry = DefaultAgentRegistry::new();
        let initial = registry.list_all().len();
        let new_agent = AgentProfile {
            name: "TestAgent".to_string(),
            role: Role::Worker,
            capabilities: vec![Capability::Testing],
            llm_preferences: vec![LlmPreference::Any],
            max_sensitivity: AgentClearance::Low,
            cost_per_token: CostPerToken::default(),
            definition: None,
        };
        registry.register(new_agent).unwrap();
        assert_eq!(registry.list_all().len(), initial + 1);
    }

    #[test]
    fn register_duplicate_rejected() {
        let mut registry = DefaultAgentRegistry::new();
        let duplicate = AgentProfile {
            name: "Hermes".to_string(),
            role: Role::Worker,
            capabilities: vec![],
            llm_preferences: vec![LlmPreference::Any],
            max_sensitivity: AgentClearance::Low,
            cost_per_token: CostPerToken::default(),
            definition: None,
        };
        let result = registry.register(duplicate);
        assert!(matches!(
            result.unwrap_err(),
            RegistryError::DuplicateAgent { .. }
        ));
    }

    #[test]
    fn filter_by_sensitivity() {
        let registry = DefaultAgentRegistry::new();
        let high_clearance = registry.filter_by_sensitivity(AgentClearance::High);
        assert_eq!(high_clearance.len(), 6);
        let high_names: Vec<&str> = high_clearance.iter().map(|p| p.name.as_str()).collect();
        assert!(high_names.contains(&"Sisyphus"));
        assert!(high_names.contains(&"Hermes"));
        assert!(high_names.contains(&"Oracle"));
        assert!(high_names.contains(&"Librarian"));
        assert!(high_names.contains(&"Hephaestus"));
        assert!(high_names.contains(&"Zeus"));

        // All 13 agents have max_sensitivity >= Low (Explore=Low, others=Medium+)
        let low_clearance = registry.filter_by_sensitivity(AgentClearance::Low);
        assert_eq!(low_clearance.len(), 13);

        // Filtering by Medium: all except Explore (Low) = 12 agents
        let medium_clearance = registry.filter_by_sensitivity(AgentClearance::Medium);
        assert_eq!(medium_clearance.len(), 12);
        let medium_names: Vec<&str> = medium_clearance.iter().map(|p| p.name.as_str()).collect();
        assert!(medium_names.contains(&"Junior"));
        assert!(!medium_names.contains(&"Explore")); // Explore is Low only
    }

    #[test]
    fn builtin_agents_have_expected_roles() {
        let registry = DefaultAgentRegistry::new();
        let workers = registry.find_by_role(Role::Worker);
        assert!(workers.iter().any(|p| p.name == "Junior"));
        assert!(workers.iter().any(|p| p.name == "Hermes"));
        assert!(workers.iter().any(|p| p.name == "Hephaestus"));
        assert!(workers.iter().any(|p| p.name == "Atlas"));

        let specialists = registry.find_by_role(Role::Specialist);
        let spec_names: Vec<&str> = specialists.iter().map(|p| p.name.as_str()).collect();
        assert!(spec_names.contains(&"Oracle"));
        assert!(spec_names.contains(&"Librarian"));
        assert!(spec_names.contains(&"Explore"));
        assert!(spec_names.contains(&"Argus"));

        let planners = registry.find_by_role(Role::Planner);
        let planner_names: Vec<&str> = planners.iter().map(|p| p.name.as_str()).collect();
        assert!(planner_names.contains(&"Metis"));
        assert!(planner_names.contains(&"Momus"));
        assert!(planner_names.contains(&"Prometheus"));
        assert!(planner_names.contains(&"Zeus"));

        let orchestrators = registry.find_by_role(Role::Orchestrator);
        assert_eq!(orchestrators.len(), 1);
        assert_eq!(orchestrators[0].name, "Sisyphus");
    }
}
