use zen_core::types::Sensitivity;

/// Structured result from a single agent execution.
///
/// Replaces the previous `Result<String>` return type with full
/// execution metadata, tool call records, and sub-agent results
/// for multi-agent chaining.
#[derive(Debug, Clone)]
pub struct AgentExecution {
    pub agent_name: String,
    pub response: String,
    pub metadata: ExecutionMetadata,
    pub tool_calls: Vec<ToolCall>,
    pub sub_agent_results: Vec<AgentExecution>,
}

/// Execution metadata captured during agent run.
#[derive(Debug, Clone)]
pub struct ExecutionMetadata {
    pub tokens_used: u32,
    pub cost_estimate: f64,
    pub model_used: String,
    pub duration_ms: u64,
    pub sensitivity: Sensitivity,
}

/// A tool call made during agent execution.
#[derive(Debug, Clone)]
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
