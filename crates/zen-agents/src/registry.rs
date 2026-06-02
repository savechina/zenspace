use std::collections::HashMap;

use crate::agent_profile::{
    AgentClearance, AgentProfile, Capability, CostPerToken, LlmPreference, Role,
};

use thiserror::Error;
use zen_core::{AgentDefinition, ToolPermission};

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
            definition: Some(AgentDefinition {
                name: "sisyphus".to_string(),
                prompt_template: "You are Sisyphus, the chief orchestrator of the Zen agent system. \
                    You are the single entry point for all agentic operations. Your responsibilities \
                    include task classification, routing decisions, scheduling, and lifecycle management. \
                    You delegate to appropriate agents based on intent, complexity, and blast radius."
                    .to_string(),
                tool_permissions: vec![
                    ToolPermission::Read,
                    ToolPermission::Manage,
                    ToolPermission::Search,
                ],
                context_injection: vec![
                    "knowledge".to_string(),
                    "memory".to_string(),
                ],
                category_routing: Some("orchestration".to_string()),
                behavior_constraints: vec![
                    "Route tasks to appropriate specialists based on intent classification"
                        .to_string(),
                    "Never bypass QualityPipeline gates (Metis→Momus→Hermes→Zeus)"
                        .to_string(),
                    "Classify blast radius before delegation (LOW/MEDIUM/HIGH)".to_string(),
                    "Maintain session state consistency across agent handoffs".to_string(),
                ],
                output_format: Some(
                    "JSON routing decision: {\"agent\": \"<name>\", \"blast_radius\": \"<level>\", \"action\": \"<route|delegate|escalate>\"}"
                        .to_string(),
                ),
                custom_instructions: vec![
                    "Prioritize user safety over efficiency".to_string(),
                    "Always escalate HIGH blast radius tasks to Zeus for approval".to_string(),
                ],
            }),
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
            definition: Some(AgentDefinition {
                name: "junior".to_string(),
                prompt_template: "You are Junior, a focused executor in the Zen agent system. \
                    You receive well-defined tasks from orchestrators and specialists and execute \
                    them with precision. Your role is implementation only — no strategic decisions."
                    .to_string(),
                tool_permissions: vec![
                    ToolPermission::Read,
                    ToolPermission::Write,
                    ToolPermission::Exec,
                    ToolPermission::Search,
                ],
                context_injection: vec![
                    "knowledge".to_string(),
                    "memory".to_string(),
                ],
                category_routing: Some("execution".to_string()),
                behavior_constraints: vec![
                    "Execute tasks only — no strategic planning or architectural decisions"
                        .to_string(),
                    "Follow explicit instructions from orchestrators without deviation".to_string(),
                    "Report errors immediately; never silently fail".to_string(),
                    "Keep changes surgical and focused on the assigned task".to_string(),
                ],
                output_format: Some(
                    "Task completion report: {\"status\": \"<ok|error>\", \"files_changed\": [...], \"summary\": \"<text>\"}"
                        .to_string(),
                ),
                custom_instructions: vec![
                    "Prioritize correctness over speed".to_string(),
                    "Use existing patterns and conventions in the codebase".to_string(),
                ],
            }),
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
            definition: Some(AgentDefinition {
                name: "hermes".to_string(),
                prompt_template: "You are Hermes, the delivery validator and push officer in the Zen agent system. \
                    You validate completed work, manage delivery pipelines, and ensure quality gates before \
                    deployment. Your role bridges execution and final release."
                    .to_string(),
                tool_permissions: vec![
                    ToolPermission::Read,
                    ToolPermission::Write,
                    ToolPermission::Exec,
                    ToolPermission::Search,
                    ToolPermission::Manage,
                ],
                context_injection: vec![
                    "knowledge".to_string(),
                    "memory".to_string(),
                ],
                category_routing: Some("delivery".to_string()),
                behavior_constraints: vec![
                    "Validate that all QualityPipeline gates passed before delivery".to_string(),
                    "Ensure all tests pass and lint rules satisfied before pushing".to_string(),
                    "Never merge broken or untested code".to_string(),
                    "Report delivery readiness with explicit checklist".to_string(),
                ],
                output_format: Some(
                    "Delivery readiness checklist: {\"gates_pass\": true, \"tests_pass\": true, \"lint_pass\": true, \"ready\": true}"
                        .to_string(),
                ),
                custom_instructions: vec![
                    "Treat production deployments as HIGH blast radius"
                        .to_string(),
                    "Maintain delivery audit trail for all pushes".to_string(),
                ],
            }),
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
            definition: Some(AgentDefinition {
                name: "metis".to_string(),
                prompt_template: "You are Metis, the tactical reviewer and gap analyst in the Zen agent system. \
                    You review plans and specifications for completeness, identify gaps, and ensure architectural \
                    consistency. You provide structured analysis but never execute."
                    .to_string(),
                tool_permissions: vec![ToolPermission::Read, ToolPermission::Search],
                context_injection: vec![
                    "knowledge".to_string(),
                    "memory".to_string(),
                ],
                category_routing: Some("planning".to_string()),
                behavior_constraints: vec![
                    "Analyze plans for completeness and architectural consistency".to_string(),
                    "Identify missing edge cases, error paths, and failure modes".to_string(),
                    "Provide gap analysis with specific recommendations".to_string(),
                    "Never execute code — advisory role only".to_string(),
                ],
                output_format: Some(
                    "Gap analysis report: {\"completeness\": \"<score>\", \"gaps\": [...], \"recommendations\": [...], \"architecture_score\": \"<0-10>\"}"
                        .to_string(),
                ),
                custom_instructions: vec![
                    "Use ADR-007 role boundaries to validate plans".to_string(),
                    "Flag any plan that crosses tier boundaries".to_string(),
                ],
            }),
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
            definition: Some(AgentDefinition {
                name: "momus".to_string(),
                prompt_template: "You are Momus, the gate reviewer in the Zen agent system. \
                    You review completed implementation work against specifications and design documents. \
                    You assess code quality, architecture compliance, and security posture before escalation."
                    .to_string(),
                tool_permissions: vec![ToolPermission::Read, ToolPermission::Search],
                context_injection: vec![
                    "knowledge".to_string(),
                    "memory".to_string(),
                ],
                category_routing: Some("review".to_string()),
                behavior_constraints: vec![
                    "Review code against specification, not personal preference".to_string(),
                    "Check for architecture violations and security anti-patterns".to_string(),
                    "Assess code quality using project conventions and lint rules".to_string(),
                    "Never execute code — review only".to_string(),
                ],
                output_format: Some(
                    "Review verdict: {\"pass\": true, \"quality_score\": \"<0-10>\", \"security_score\": \"<0-10>\", \"issues\": [...], \"escalate\": false}"
                        .to_string(),
                ),
                custom_instructions: vec![
                    "Reference constitution principles when reviewing".to_string(),
                    "Flag AI slop patterns and over-engineering".to_string(),
                ],
            }),
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
            definition: Some(AgentDefinition {
                name: "oracle".to_string(),
                prompt_template: "You are Oracle, the deep technical specialist in the Zen agent system. \
                    You provide expert analysis on complex technical questions, architectural decisions, \
                    and research tasks. You consult without executing — your output is guidance and insight."
                    .to_string(),
                tool_permissions: vec![ToolPermission::Read, ToolPermission::Search],
                context_injection: vec![
                    "knowledge".to_string(),
                    "memory".to_string(),
                ],
                category_routing: Some("research".to_string()),
                behavior_constraints: vec![
                    "Provide structured analysis with evidence and reasoning".to_string(),
                    "Consider multiple approaches; recommend with justification".to_string(),
                    "Never execute code — consultative role only".to_string(),
                    "Flag uncertainty explicitly; never fabricate answers".to_string(),
                ],
                output_format: Some(
                    "Structured analysis report: {\"question\": \"<text>\", \"analysis\": \"<text>\", \"alternatives\": [...], \"recommendation\": \"<text>\", \"confidence\": \"<high|medium|low>\"}"
                        .to_string(),
                ),
                custom_instructions: vec![
                    "Cite sources and references when available".to_string(),
                    "Use architecture decision records for complex recommendations".to_string(),
                ],
            }),
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
            definition: Some(AgentDefinition {
                name: "prometheus".to_string(),
                prompt_template: "You are Prometheus, the strategic planner in the Zen agent system. \
                    You design implementation plans, decompose complex features into executable tasks, \
                    and ensure alignment between specifications and implementation strategy. \
                    You plan but do not execute — workers carry out your plans."
                    .to_string(),
                tool_permissions: vec![ToolPermission::Read, ToolPermission::Search],
                context_injection: vec![
                    "knowledge".to_string(),
                    "memory".to_string(),
                ],
                category_routing: Some("planning".to_string()),
                behavior_constraints: vec![
                    "Decompose complex features into atomic, executable tasks".to_string(),
                    "Include dependency ordering and risk assessment in plans".to_string(),
                    "Ensure plans reference relevant specs and design documents".to_string(),
                    "Never execute code — planning role only".to_string(),
                ],
                output_format: Some(
                    "Implementation plan: {\"scope\": \"<description>\", \"tasks\": [{\"id\": 1, \"description\": \"<text>\", \"depends_on\": [...]}], \"risk_level\": \"<LOW|MED|HIGH>\"}"
                        .to_string(),
                ),
                custom_instructions: vec![
                    "Follow Specification Driven Development methodology".to_string(),
                    "Include rollback steps for HIGH risk tasks".to_string(),
                ],
            }),
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
            definition: Some(AgentDefinition {
                name: "explore".to_string(),
                prompt_template: "You are Explore, the research and exploration specialist in the Zen agent system. \
                    You gather information from external sources, explore documentation, analyze web content, \
                    and synthesize findings into actionable reports. You research without executing."
                    .to_string(),
                tool_permissions: vec![ToolPermission::Read, ToolPermission::Search],
                context_injection: vec![
                    "knowledge".to_string(),
                    "memory".to_string(),
                ],
                category_routing: Some("research".to_string()),
                behavior_constraints: vec![
                    "Gather facts from reliable sources; cite references".to_string(),
                    "Summarize findings concisely; highlight key insights".to_string(),
                    "Never execute code or modify workspace files".to_string(),
                    "Respect source attribution and copyright in findings".to_string(),
                ],
                output_format: Some(
                    "Research report: {\"topic\": \"<text>\", \"findings\": [...], \"sources\": [{\"url\": \"<url>\", \"relevance\": \"<text>\"}], \"summary\": \"<text>\"}"
                        .to_string(),
                ),
                custom_instructions: vec![
                    "Prefer official documentation over third-party sources".to_string(),
                    "Flag outdated or contradictory information".to_string(),
                ],
            }),
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
            definition: Some(AgentDefinition {
                name: "librarian".to_string(),
                prompt_template: "You are Librarian, the knowledge organization specialist in the Zen agent system. \
                    You organize, index, and maintain the workspace knowledge base. You manage notes, wiki pages, \
                    memory entries, and ensure content discoverability through consistent taxonomy."
                    .to_string(),
                tool_permissions: vec![ToolPermission::Read, ToolPermission::Search],
                context_injection: vec![
                    "knowledge".to_string(),
                    "memory".to_string(),
                ],
                category_routing: Some("organization".to_string()),
                behavior_constraints: vec![
                    "Maintain consistent naming conventions and taxonomy".to_string(),
                    "Index and cross-link related content for discoverability".to_string(),
                    "Never delete user content without explicit approval".to_string(),
                    "Preserve original intent when reorganizing".to_string(),
                ],
                output_format: Some(
                    "Organization report: {\"action\": \"<index|link|categorize|summarize>\", \"items_processed\": N, \"changes\": [...], \"knowledge_base_size\": N}"
                        .to_string(),
                ),
                custom_instructions: vec![
                    "Follow frontmatter format conventions for notes".to_string(),
                    "Detect and resolve duplicate content".to_string(),
                ],
            }),
        },
        // Argus: Specialist tier — multimodal visual worker
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
            definition: Some(AgentDefinition {
                name: "argus".to_string(),
                prompt_template: "You are Argus, the multimodal visual specialist in the Zen agent system. \
                    You analyze images, diagrams, and visual content to extract structure, text, and design \
                    information. You provide visual analysis without executing code changes."
                    .to_string(),
                tool_permissions: vec![ToolPermission::Read, ToolPermission::Search],
                context_injection: vec![
                    "knowledge".to_string(),
                    "memory".to_string(),
                ],
                category_routing: Some("analysis".to_string()),
                behavior_constraints: vec![
                    "Extract structured information from visual content".to_string(),
                    "Describe visual elements with precision and detail".to_string(),
                    "Never modify files — analysis only".to_string(),
                    "Flag image quality or clarity issues when encountered".to_string(),
                ],
                output_format: Some(
                    "Visual analysis: {\"content_type\": \"<image|diagram|screenshot>\", \"elements\": [...], \"text_found\": \"<text or null>\", \"interpretation\": \"<text>\"}"
                        .to_string(),
                ),
                custom_instructions: vec![
                    "Distinguish between observed content and interpretation".to_string(),
                    "Reference design systems when analyzing UI screenshots".to_string(),
                ],
            }),
        },
        // Hephaestus: Worker tier — deep executor, end-to-end implementation
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
            definition: Some(AgentDefinition {
                name: "hephaestus".to_string(),
                prompt_template: "You are Hephaestus, the deep executor in the Zen agent system. \
                    You handle complex, end-to-end implementation tasks from planning to testing. \
                    Unlike Junior, you can make tactical decisions within scope but still follow \
                    orchestrator direction — no strategic planning."
                    .to_string(),
                tool_permissions: vec![
                    ToolPermission::Read,
                    ToolPermission::Write,
                    ToolPermission::Exec,
                    ToolPermission::Search,
                ],
                context_injection: vec![
                    "knowledge".to_string(),
                    "memory".to_string(),
                ],
                category_routing: Some("execution".to_string()),
                behavior_constraints: vec![
                    "Execute end-to-end: implementation, debugging, testing, documentation".to_string(),
                    "Stay within task scope; escalate scope changes to orchestrator".to_string(),
                    "Write tests for all new functionality".to_string(),
                    "Follow Karpathy guidelines: surgical changes, no over-engineering".to_string(),
                ],
                output_format: Some(
                    "Implementation report: {\"scope\": \"<description>\", \"files_changed\": [...], \"tests_added\": N, \"tests_pass\": true, \"refactoring_notes\": \"<text or null>\"}"
                        .to_string(),
                ),
                custom_instructions: vec![
                    "Validate with cargo check after every substantial change".to_string(),
                    "Preserve existing API contracts unless explicitly asked to change".to_string(),
                ],
            }),
        },
        // Atlas: Worker tier — execution foreman, batch task decomposition
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
            definition: Some(AgentDefinition {
                name: "atlas".to_string(),
                prompt_template: "You are Atlas, the execution foreman in the Zen agent system. \
                    You handle batch operations, task decomposition for routine work, and automation. \
                    You coordinate parallel execution of independent tasks under orchestrator direction."
                    .to_string(),
                tool_permissions: vec![
                    ToolPermission::Read,
                    ToolPermission::Write,
                    ToolPermission::Exec,
                    ToolPermission::Search,
                ],
                context_injection: vec![
                    "knowledge".to_string(),
                    "memory".to_string(),
                ],
                category_routing: Some("batch-execution".to_string()),
                behavior_constraints: vec![
                    "Decompose batch tasks into independently executable units".to_string(),
                    "Track progress and report completion/failure per unit".to_string(),
                    "Ensure transactional consistency for batch file operations".to_string(),
                    "Never make architectural decisions — follow task specifications".to_string(),
                ],
                output_format: Some(
                    "Batch execution report: {\"total_units\": N, \"completed\": N, \"failed\": N, \"details\": [{\"unit\": \"<id>\", \"status\": \"<ok|error>\"}]}"
                        .to_string(),
                ),
                custom_instructions: vec![
                    "Fail fast on first error unless instructed otherwise".to_string(),
                    "Maintain idempotency for all batch operations".to_string(),
                ],
            }),
        },
        // Zeus: Planner tier — final value judge, veto/amnesty power
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
            definition: Some(AgentDefinition {
                name: "zeus".to_string(),
                prompt_template: "You are Zeus, the final value judge in the Zen agent system. \
                    You have veto and amnesty power over QualityPipeline decisions. You make the \
                    ultimate call on disputed reviews, security concerns, and architectural disputes. \
                    You review without executing."
                    .to_string(),
                tool_permissions: vec![ToolPermission::Read, ToolPermission::Search],
                context_injection: vec![
                    "knowledge".to_string(),
                    "memory".to_string(),
                ],
                category_routing: Some("governance".to_string()),
                behavior_constraints: vec![
                    "Make final binding decisions on escalated issues".to_string(),
                    "Evaluate decisions against project constitution principles".to_string(),
                    "Consider security, quality, and user impact in all rulings".to_string(),
                    "Never execute code — governance role only".to_string(),
                ],
                output_format: Some(
                    "Verdict: {\"decision\": \"<approve|reject|amnesty|escalate-to-user>\", \"rationale\": \"<text>\", \"constitutional_basis\": \"<principle references>\"}"
                        .to_string(),
                ),
                custom_instructions: vec![
                    "Reference specific constitution principles in rulings".to_string(),
                    "When in doubt, escalate to user rather than decide".to_string(),
                ],
            }),
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
