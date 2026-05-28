use serde::{Deserialize, Serialize};

/// Agent definition structure matching FR-028 specification.
///
/// Defines an agent's name, prompt template, tool permissions,
/// context injection behavior, and optional category routing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDefinition {
    pub name: String,
    pub prompt_template: String,
    pub tool_permissions: Vec<String>,
    pub context_injection: bool,
    pub category_routing: Option<String>,
}

impl AgentDefinition {
    /// Returns the built-in default agent definition for general-purpose use.
    pub fn default_agent() -> Self {
        Self {
            name: "default".to_string(),
            prompt_template: DEFAULT_AGENT_PROMPT.to_string(),
            tool_permissions: vec![
                "read".to_string(),
                "write".to_string(),
                "exec".to_string(),
                "search".to_string(),
            ],
            context_injection: true,
            category_routing: None,
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
        assert!(agent.context_injection);
        assert!(agent.category_routing.is_none());
        assert_eq!(agent.tool_permissions.len(), 4);
        assert!(agent.tool_permissions.contains(&"read".to_string()));
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
