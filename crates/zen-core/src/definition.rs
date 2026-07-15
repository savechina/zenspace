use serde::{Deserialize, Serialize};
use std::fmt;
use std::hash::Hash;

/// Tool permission enum for agent capability gating.
///
/// Matches FR-028 specification for tool access control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToolPermission {
    /// Read files, search knowledge base
    Read,
    /// Write files, create notes, modify wiki
    Write,
    /// Execute shell commands, run builds
    Exec,
    /// Search knowledge base, query notion graph
    Search,
    /// Delete files, drop tables (HIGH blast radius)
    Delete,
    /// Manage agent sessions, orchestrate tasks
    Manage,
}

impl fmt::Display for ToolPermission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolPermission::Read => write!(f, "read"),
            ToolPermission::Write => write!(f, "write"),
            ToolPermission::Exec => write!(f, "exec"),
            ToolPermission::Search => write!(f, "search"),
            ToolPermission::Delete => write!(f, "delete"),
            ToolPermission::Manage => write!(f, "manage"),
        }
    }
}

/// Agent definition structure matching FR-028 specification and ADR-011.
///
/// Defines an agent's prompt template, tool permissions, behavior constraints,
/// output format, context injection paths, and optional category routing.
///
/// **Current Status (2026-06-02)**:
/// - ✅ `prompt_template`: Active — consumed by executor.rs:200
/// - ⏳ `tool_permissions`: Reserved for FR-SEC-001 permission gating
/// - ⏳ `context_injection`: Reserved for ADR-011 Tier 3 integration
/// - ⏳ `category_routing`: Reserved for FR-ROUTING-002 intent classification
/// - ⏳ `behavior_constraints`: Reserved for ADR-011 Tier 1 → PromptBuilder
/// - ⏳ `output_format`: Reserved for ADR-011 Tier 2 → PromptBuilder
/// - ⏳ `custom_instructions`: Reserved for ADR-011 Tier 6 → PromptBuilder
///
/// **TODO**: Connect reserved fields to PromptBuilder via `from_definition()` method.
/// See: docs/specs/001-agentic-foundation/spec.md ADR-011 lines 1115-1126
///
/// Tier mapping (ADR-011 system prompt assembly):
/// - Tier 0 (Identity): prompt_template
/// - Tier 1 (Behavior): behavior_constraints + tool_permissions
/// - Tier 2 (Output Format): output_format
/// - Tier 3 (Tools): injected from registry (not in definition)
/// - Tier 6 (Business): custom_instructions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDefinition {
    /// Agent name (e.g., "coordinator", "junior", "oracle")
    /// NOTE: Duplicate of AgentProfile.name — consider removal in ADR-013 cleanup
    pub name: String,

    /// Base prompt template (Tier 0 - Identity)
    /// ✅ ACTIVE — consumed by executor.rs:200 and zen-memory/prompt.rs:48
    pub prompt_template: String,

    /// Tool access permissions (Tier 1 - Behavior gating)
    /// ⏳ RESERVED for FR-SEC-001: Permission gating before tool invocation
    /// Intended: ZenSafetyHook checks tool_permissions before DelegateTool execution
    pub tool_permissions: Vec<ToolPermission>,

    /// Context injection paths (e.g., "knowledge", "memory", "identity")
    /// ⏳ RESERVED for ADR-011 Tier 3: Dynamic context assembly
    /// Intended: PromptBuilder reads paths to inject InvestigationContext evidence
    pub context_injection: Vec<String>,

    /// Category routing hint (e.g., "coding", "research", "planning")
    /// ⏳ RESERVED for FR-ROUTING-002: Intent classification routing
    /// Intended: ZenCoordinator.routing() uses hint for specialist selection
    pub category_routing: Option<String>,

    /// Behavior constraints (Tier 1 - Safety rules)
    /// ⏳ RESERVED for ADR-011 Tier 1 → PromptBuilder.behavior_constraints
    /// Intended: Inject into system prompt as blast radius taxonomy rules
    pub behavior_constraints: Vec<String>,

    /// Output format specification (Tier 2 - Response schema)
    /// ⏳ RESERVED for ADR-011 Tier 2 → PromptBuilder.output_format
    /// Intended: Enforce structured output (JSON, Markdown, etc.)
    pub output_format: Option<String>,

    /// Custom business instructions (Tier 6 - Domain-specific prompts)
    /// ⏳ RESERVED for ADR-011 Tier 6 → PromptBuilder.custom_instructions
    /// Intended: Append domain-specific rules (e.g., "Prioritize user safety")
    pub custom_instructions: Vec<String>,
}

impl AgentDefinition {
    /// Returns the built-in default agent definition for general-purpose use.
    pub fn default_agent() -> Self {
        Self {
            name: "default".to_string(),
            prompt_template: DEFAULT_AGENT_PROMPT.to_string(),
            tool_permissions: vec![
                ToolPermission::Read,
                ToolPermission::Write,
                ToolPermission::Exec,
                ToolPermission::Search,
            ],
            context_injection: vec!["knowledge".to_string(), "memory".to_string()],
            category_routing: None,
            behavior_constraints: vec![
                "Follow tool permissions as defined".to_string(),
                "Be concise and actionable".to_string(),
            ],
            output_format: None,
            custom_instructions: vec!["Respect workspace conventions".to_string()],
        }
    }
}

const DEFAULT_AGENT_PROMPT: &str = "\
You are Zen, a general-purpose AI assistant operating within a Zen workspace.

Your role:
- Help manage and organize workspace content
- Execute agent tasks according to their definitions
- Provide intelligent guidance based on context

Guidelines:
- Follow tool permissions as defined in your agent configuration
- Be concise and actionable in your responses
- Respect workspace conventions and existing patterns
- When uncertain, prefer asking for clarification over assuming
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_agent_has_expected_fields() {
        let agent = AgentDefinition::default_agent();
        assert_eq!(agent.name, "default");
        assert!(!agent.context_injection.is_empty());
        assert!(agent.category_routing.is_none());
        assert_eq!(agent.tool_permissions.len(), 4);
        assert!(agent.tool_permissions.contains(&ToolPermission::Read));
        assert!(agent.tool_permissions.contains(&ToolPermission::Write));
        assert!(!agent.behavior_constraints.is_empty());
        assert!(agent.output_format.is_none());
        assert!(!agent.custom_instructions.is_empty());
    }

    #[test]
    fn serialize_deserialize_roundtrip() {
        let agent = AgentDefinition::default_agent();
        let toml_str = toml::to_string(&agent).unwrap();
        let deserialized: AgentDefinition = toml::from_str(&toml_str).unwrap();
        assert_eq!(deserialized.name, agent.name);
        assert_eq!(deserialized.prompt_template, agent.prompt_template);
        assert_eq!(deserialized.tool_permissions, agent.tool_permissions);
        assert_eq!(deserialized.context_injection, agent.context_injection);
        assert_eq!(deserialized.category_routing, agent.category_routing);
    }
}
