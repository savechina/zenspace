use serde::{Deserialize, Serialize};
use zen_core::types::Sensitivity;

/// Structured result from a single agent execution.
///
/// Replaces the previous `Result<String>` return type with full
/// execution metadata, tool call records, and sub-agent results
/// for multi-agent chaining.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentExecution {
    pub agent_name: String,
    pub response: String,
    pub metadata: ExecutionMetadata,
    pub tool_calls: Vec<ToolCall>,
    pub sub_agent_results: Vec<AgentExecution>,
}

/// Execution metadata captured during agent run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionMetadata {
    pub tokens_used: u32,
    pub cost_estimate: f64,
    pub model_used: String,
    pub duration_ms: u64,
    pub sensitivity: Sensitivity,
}

/// A tool call made during agent execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub tool_name: String,
    pub arguments: String,
    pub result: String,
}

impl AgentExecution {
    /// Create a minimal execution result (for testing/fallback).
    pub fn minimal(agent_name: impl Into<String>, response: impl Into<String>) -> Self {
        Self {
            agent_name: agent_name.into(),
            response: response.into(),
            metadata: ExecutionMetadata {
                tokens_used: 0,
                cost_estimate: 0.0,
                model_used: String::new(),
                duration_ms: 0,
                sensitivity: Sensitivity::Private,
            },
            tool_calls: Vec::new(),
            sub_agent_results: Vec::new(),
        }
    }

    /// Total tokens used across this execution and all sub-agents.
    #[must_use]
    pub fn total_tokens(&self) -> u32 {
        self.metadata.tokens_used
            + self
                .sub_agent_results
                .iter()
                .map(|r| r.total_tokens())
                .sum::<u32>()
    }

    /// Total cost estimate across this execution and all sub-agents.
    #[must_use]
    pub fn total_cost(&self) -> f64 {
        self.metadata.cost_estimate
            + self
                .sub_agent_results
                .iter()
                .map(|r| r.total_cost())
                .sum::<f64>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_execution_serialization_roundtrip() {
        let sub = AgentExecution::minimal("sub", "sub response");
        let execution = AgentExecution {
            agent_name: "Sisyphus".to_string(),
            response: "Hello, world!".to_string(),
            metadata: ExecutionMetadata {
                tokens_used: 1500,
                cost_estimate: 0.003,
                model_used: "gpt-4o-mini".to_string(),
                duration_ms: 2345,
                sensitivity: Sensitivity::Public,
            },
            tool_calls: vec![
                ToolCall {
                    tool_name: "read_file".to_string(),
                    arguments: r#"{"path":"/test.rs"}"#.to_string(),
                    result: "fn main() {}".to_string(),
                },
                ToolCall {
                    tool_name: "grep".to_string(),
                    arguments: r#"{"pattern":"hello"}"#.to_string(),
                    result: "line 42: hello".to_string(),
                },
            ],
            sub_agent_results: vec![sub],
        };

        let json = serde_json::to_string(&execution).unwrap();
        let decoded: AgentExecution = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.agent_name, "Sisyphus");
        assert_eq!(decoded.response, "Hello, world!");
        assert_eq!(decoded.metadata.tokens_used, 1500);
        assert_eq!(decoded.metadata.cost_estimate, 0.003);
        assert_eq!(decoded.metadata.model_used, "gpt-4o-mini");
        assert_eq!(decoded.metadata.duration_ms, 2345);
        assert_eq!(decoded.metadata.sensitivity, Sensitivity::Public);
        assert_eq!(decoded.tool_calls.len(), 2);
        assert_eq!(decoded.tool_calls[0].tool_name, "read_file");
        assert_eq!(decoded.tool_calls[1].tool_name, "grep");
        assert_eq!(decoded.sub_agent_results.len(), 1);
        assert_eq!(decoded.sub_agent_results[0].agent_name, "sub");
    }

    #[test]
    fn test_agent_execution_minimal_serialization() {
        let execution = AgentExecution::minimal("test", "ok");
        let json = serde_json::to_string(&execution).unwrap();
        let decoded: AgentExecution = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.agent_name, "test");
        assert_eq!(decoded.response, "ok");
        assert_eq!(decoded.metadata.tokens_used, 0);
        assert!(decoded.tool_calls.is_empty());
        assert!(decoded.sub_agent_results.is_empty());
    }
}
