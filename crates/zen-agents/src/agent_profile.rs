use serde::{Deserialize, Serialize};

use zen_core::AgentDefinition;

/// Roles that agents can fulfill in the system.
///
/// Maps to the 3-tier agent taxonomy per ADR-007:
/// Orchestrator (Sisyphus only — single entry point), Planner (thinking only),
/// Specialist (consulting only), Worker (execution only).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Role {
    Orchestrator,
    Planner,
    Specialist,
    Worker,
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Role::Orchestrator => write!(f, "Orchestrator"),
            Role::Planner => write!(f, "Planner"),
            Role::Specialist => write!(f, "Specialist"),
            Role::Worker => write!(f, "Worker"),
        }
    }
}

/// Capabilities that agents advertise for matching.
///
/// Used by agent registry to select agents whose capability set
/// covers all requested items.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Capability {
    CodeReview,
    DesignReview,
    Research,
    Testing,
    Documentation,
    Deployment,
    Debugging,
    Refactoring,
    Architecture,
    SecurityAudit,
    PerformanceOptimization,
    TaskExecution,
    SessionManagement,
    SpecificationWriting,
    CodeGeneration,
    KnowledgeManagement,
    MemoryManagement,
    Analysis,
    Automation,
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Capability::CodeReview => write!(f, "code-review"),
            Capability::DesignReview => write!(f, "design-review"),
            Capability::Research => write!(f, "research"),
            Capability::Testing => write!(f, "testing"),
            Capability::Documentation => write!(f, "documentation"),
            Capability::Deployment => write!(f, "deployment"),
            Capability::Debugging => write!(f, "debugging"),
            Capability::Refactoring => write!(f, "refactoring"),
            Capability::Architecture => write!(f, "architecture"),
            Capability::SecurityAudit => write!(f, "security-audit"),
            Capability::PerformanceOptimization => write!(f, "performance-optimization"),
            Capability::TaskExecution => write!(f, "task-execution"),
            Capability::SessionManagement => write!(f, "session-management"),
            Capability::SpecificationWriting => write!(f, "specification-writing"),
            Capability::CodeGeneration => write!(f, "code-generation"),
            Capability::KnowledgeManagement => write!(f, "knowledge-management"),
            Capability::MemoryManagement => write!(f, "memory-management"),
            Capability::Analysis => write!(f, "analysis"),
            Capability::Automation => write!(f, "automation"),
        }
    }
}

/// Sensitivity level an agent is cleared to handle.
///
/// Agents are filtered by comparing this against the session's
/// effective sensitivity to ensure data stays within clearance bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SensitivityLevel {
    Low,
    Medium,
    High,
}

impl std::fmt::Display for SensitivityLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SensitivityLevel::Low => write!(f, "Low"),
            SensitivityLevel::Medium => write!(f, "Medium"),
            SensitivityLevel::High => write!(f, "High"),
        }
    }
}

/// Structured cost representation for per-token pricing.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CostPerToken {
    pub input_cents_per_million: f64,
    pub output_cents_per_million: f64,
}

impl Default for CostPerToken {
    fn default() -> Self {
        Self {
            input_cents_per_million: 0.0,
            output_cents_per_million: 0.0,
        }
    }
}

impl std::fmt::Display for CostPerToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "in={:.0} out={:.0}",
            self.input_cents_per_million, self.output_cents_per_million
        )
    }
}

/// Agent preference for LLM provider selection.
///
/// Guides session assembly when choosing which model backend
/// to route an agent's requests through.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LlmPreference {
    Any,
    LocalOnly,
    CloudOnly,
    Provider(String),
}

impl std::fmt::Display for LlmPreference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmPreference::Any => write!(f, "any"),
            LlmPreference::LocalOnly => write!(f, "local-only"),
            LlmPreference::CloudOnly => write!(f, "cloud-only"),
            LlmPreference::Provider(name) => write!(f, "{name}"),
        }
    }
}

/// Profile describing an agent's identity, capabilities, and constraints.
///
/// This is the core entity managed by agent registry implementations.
/// Each profile captures:
/// - **name**: unique identifier (e.g. "Junior", "Sisyphus", "Hermes")
/// - **role**: the agent's functional role in the system
/// - **capabilities**: what the agent can do (matched via `has_all_capabilities`)
/// - **llm_preferences**: which model backends are acceptable
/// - **max_sensitivity**: highest data sensitivity the agent may access
/// - **cost_per_token**: structured cost info for budgeting
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProfile {
    pub name: String,
    pub role: Role,
    pub capabilities: Vec<Capability>,
    pub llm_preferences: Vec<LlmPreference>,
    pub max_sensitivity: SensitivityLevel,
    pub cost_per_token: CostPerToken,
    pub definition: Option<AgentDefinition>,
}

impl AgentProfile {
    /// Returns true when this agent possesses all requested capabilities.
    ///
    /// Used by agent registry for intersection matching.
    #[must_use]
    pub fn has_all_capabilities(&self, required: &[Capability]) -> bool {
        required.iter().all(|cap| self.capabilities.contains(cap))
    }

    /// Returns true when this agent can handle the given sensitivity level.
    #[must_use]
    pub fn can_handle_sensitivity(&self, level: SensitivityLevel) -> bool {
        self.max_sensitivity >= level
    }

    /// Build a profile builder for fluent construction.
    #[must_use]
    pub fn builder(name: impl Into<String>) -> AgentProfileBuilder {
        AgentProfileBuilder::new(name)
    }
}

/// Builder for [`AgentProfile`] with ergonomic defaults.
pub struct AgentProfileBuilder {
    name: String,
    role: Role,
    capabilities: Vec<Capability>,
    llm_preferences: Vec<LlmPreference>,
    max_sensitivity: SensitivityLevel,
    cost_per_token: CostPerToken,
    definition: Option<AgentDefinition>,
}

impl AgentProfileBuilder {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            role: Role::Worker,
            capabilities: Vec::new(),
            llm_preferences: vec![LlmPreference::Any],
            max_sensitivity: SensitivityLevel::Low,
            cost_per_token: CostPerToken::default(),
            definition: None,
        }
    }

    #[must_use]
    pub fn role(mut self, role: Role) -> Self {
        self.role = role;
        self
    }

    #[must_use]
    pub fn capabilities(mut self, capabilities: Vec<Capability>) -> Self {
        self.capabilities = capabilities;
        self
    }

    #[must_use]
    pub fn llm_preferences(mut self, prefs: Vec<LlmPreference>) -> Self {
        self.llm_preferences = prefs;
        self
    }

    #[must_use]
    pub fn max_sensitivity(mut self, level: SensitivityLevel) -> Self {
        self.max_sensitivity = level;
        self
    }

    #[must_use]
    pub fn cost_per_token(mut self, cost: CostPerToken) -> Self {
        self.cost_per_token = cost;
        self
    }

    #[must_use]
    pub fn definition(mut self, def: AgentDefinition) -> Self {
        self.definition = Some(def);
        self
    }

    #[must_use]
    pub fn build(self) -> AgentProfile {
        AgentProfile {
            name: self.name,
            role: self.role,
            capabilities: self.capabilities,
            llm_preferences: self.llm_preferences,
            max_sensitivity: self.max_sensitivity,
            cost_per_token: self.cost_per_token,
            definition: self.definition,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_creates_profile() {
        let profile = AgentProfile::builder("test-agent")
            .role(Role::Specialist)
            .capabilities(vec![Capability::CodeReview, Capability::Testing])
            .max_sensitivity(SensitivityLevel::High)
            .build();

        assert_eq!(profile.name, "test-agent");
        assert_eq!(profile.role, Role::Specialist);
        assert_eq!(profile.capabilities.len(), 2);
        assert_eq!(profile.max_sensitivity, SensitivityLevel::High);
    }

    #[test]
    fn has_all_capabilities_returns_true_when_subset() {
        let profile = AgentProfile::builder("a")
            .capabilities(vec![
                Capability::CodeReview,
                Capability::Testing,
                Capability::Debugging,
            ])
            .build();

        assert!(profile.has_all_capabilities(&[Capability::CodeReview]));
        assert!(profile.has_all_capabilities(&[Capability::CodeReview, Capability::Testing]));
        assert!(!profile.has_all_capabilities(&[Capability::Research]));
    }

    #[test]
    fn can_handle_sensitivity_respects_ordering() {
        let profile = AgentProfile::builder("a")
            .max_sensitivity(SensitivityLevel::Medium)
            .build();

        assert!(profile.can_handle_sensitivity(SensitivityLevel::Low));
        assert!(profile.can_handle_sensitivity(SensitivityLevel::Medium));
        assert!(!profile.can_handle_sensitivity(SensitivityLevel::High));
    }

    #[test]
    fn display_implementations() {
        assert_eq!(Role::Planner.to_string(), "Planner");
        assert_eq!(Capability::CodeReview.to_string(), "code-review");
        assert_eq!(SensitivityLevel::High.to_string(), "High");
        assert_eq!(LlmPreference::LocalOnly.to_string(), "local-only");
    }
}
