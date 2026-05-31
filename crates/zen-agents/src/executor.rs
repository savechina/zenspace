//! Agent execution with retry, fallback, and error categorization.
//!
//! `AgentExecutor` handles LLM call execution with configurable retry
//! policies and error classification for transient vs client errors.
//!
//! # Architecture (ADR-011)
//!
//! The orchestrator delegates execution lifecycle to this component.
//! Budget enforcement and telemetry hooks are wired in the orchestrator
//! thin-wrapper.
//!
//! # Future Work
//! - T294: Replace custom retry loop with `rig_compose::dispatch_tool_invocations`
//! - T295: Wire BudgetGuard for pre-execution reservation (via orchestrator)
//! - T297: Replace `append_audit_log()` with rig-tap observability hooks

use std::time::Instant;

use anyhow::{Context, Result};
use tracing::{info, warn};

use zen_provider::{DefaultRouter, LlmRouter, Provider, TaskRequirements};

use crate::AgentContext;
use crate::execution::{AgentExecution, ExecutionMetadata};

/// Default retry configuration for agent execution.
const DEFAULT_MAX_RETRIES: u32 = 3;
const DEFAULT_RETRY_BASE_MS: u64 = 500;

/// Categorizes LLM errors for retry decision making.
#[derive(Debug, Clone, PartialEq)]
pub enum ErrorCategory {
    /// Transient error — safe to retry (429, 500, 502, 503, timeout)
    Transient,
    /// Client error — retry will not help (400, 401, 403, 404)
    ClientError,
    /// Unknown — retry with caution
    Unknown,
}

impl ErrorCategory {
    pub fn from_status_code(status: u16) -> Self {
        match status {
            429 | 500 | 502 | 503 | 504 => Self::Transient,
            400 | 401 | 403 | 404 | 422 => Self::ClientError,
            _ => Self::Unknown,
        }
    }

    pub fn from_error_message(msg: &str) -> Self {
        let lower = msg.to_lowercase();
        if lower.contains("429") || lower.contains("rate limit") || lower.contains("too many") {
            return Self::Transient;
        }
        if lower.contains("500")
            || lower.contains("502")
            || lower.contains("503")
            || lower.contains("504")
            || lower.contains("timeout")
            || lower.contains("connection reset")
        {
            return Self::Transient;
        }
        if lower.contains("400") || lower.contains("401") || lower.contains("403") {
            return Self::ClientError;
        }
        Self::Unknown
    }

    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Transient | Self::Unknown)
    }
}

/// Retry policy for agent execution.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
    pub jitter: bool,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: DEFAULT_MAX_RETRIES,
            base_delay_ms: DEFAULT_RETRY_BASE_MS,
            max_delay_ms: 10_000,
            jitter: true,
        }
    }
}

impl RetryPolicy {
    pub fn delay_ms(&self, attempt: u32) -> u64 {
        let exponential = self.base_delay_ms * 2u64.pow(attempt);
        let delay = exponential.min(self.max_delay_ms);
        if self.jitter {
            delay + (fastrand::u64(0..delay / 2))
        } else {
            delay
        }
    }
}

/// Executes agent requests with retry, fallback, and error categorization.
pub struct AgentExecutor {
    router: DefaultRouter,
    retry_policy: RetryPolicy,
}

impl AgentExecutor {
    pub fn new(router: DefaultRouter) -> Self {
        Self {
            router,
            retry_policy: RetryPolicy::default(),
        }
    }

    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = policy;
        self
    }

    /// Accessor for the underlying router — used by orchestrator build_agent.
    #[must_use]
    pub const fn router(&self) -> &DefaultRouter {
        &self.router
    }

    /// Execute a single agent request with retry and fallback.
    pub fn execute(&self, context: &AgentContext) -> Result<AgentExecution> {
        let start = Instant::now();
        let agent_name = context.agent_profile.name.clone();
        let sensitivity = context.sensitivity;

        let requirements = TaskRequirements {
            max_tokens: Some(context.max_tokens as u32),
            sensitivity,
            preferred_model: None,
            budget_limit: None,
        };

        let provider = self
            .router
            .route_with_preferences(&requirements, &context.preferences)
            .or_else(|e| {
                info!("Preference routing failed: {e}, falling back");
                self.router.route(&requirements)
            })
            .unwrap_or_else(|e| {
                warn!("All routing failed: {e}, using mock fallback");
                Provider::Mock
            });

        let prompt = self.build_prompt(context);
        let response = self.execute_with_retry(&provider, &prompt, &agent_name)?;

        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(AgentExecution {
            agent_name,
            response,
            metadata: ExecutionMetadata {
                tokens_used: 0,
                cost_estimate: 0.0,
                model_used: provider.to_string(),
                duration_ms,
                sensitivity,
            },
            tool_calls: Vec::new(),
            sub_agent_results: Vec::new(),
        })
    }

    fn build_prompt(&self, context: &AgentContext) -> String {
        let system_prompt = self.assemble_system_prompt(context);
        format!(
            "{}\n\nUser: {}\n\nAssistant:",
            system_prompt, context.user_query
        )
    }

    fn assemble_system_prompt(&self, context: &AgentContext) -> String {
        let mut parts = Vec::new();

        if let Some(def) = &context.agent_profile.definition
            && !def.prompt_template.is_empty()
        {
            parts.push(def.prompt_template.clone());
        }

        let knowledge: Vec<String> = context
            .session
            .knowledge
            .iter()
            .map(|n| n.content.clone())
            .collect();
        if !knowledge.is_empty() {
            parts.push(format!(
                "## Retrieved Knowledge\n{}",
                knowledge.join("\n\n")
            ));
        }

        let history: Vec<String> = context
            .session
            .conversation
            .iter()
            .map(|t| format!("{}: {}", t.role, t.content))
            .collect();
        if !history.is_empty() {
            parts.push(format!("## Conversation History\n{}", history.join("\n")));
        }

        parts.join("\n\n---\n\n")
    }

    fn execute_with_retry(
        &self,
        provider: &Provider,
        prompt: &str,
        agent_name: &str,
    ) -> Result<String> {
        let mut last_error = None;

        for attempt in 0..=self.retry_policy.max_retries {
            if attempt > 0 {
                let delay = self.retry_policy.delay_ms(attempt - 1);
                info!(
                    attempt,
                    delay_ms = delay,
                    agent = agent_name,
                    "Retrying agent execution"
                );
                std::thread::sleep(std::time::Duration::from_millis(delay));
            }

            match self.router.call(provider.clone(), prompt) {
                Ok(response) => {
                    if attempt > 0 {
                        info!(attempt, agent = agent_name, "Retry succeeded");
                    }
                    return Ok(response);
                }
                Err(e) => {
                    let category = ErrorCategory::from_error_message(&e.to_string());
                    if !category.is_retryable() {
                        warn!(
                            agent = agent_name,
                            error = %e,
                            category = ?category,
                            "Non-retryable error, aborting"
                        );
                        return Err(anyhow::anyhow!("LLM call failed: {e}"));
                    }

                    warn!(
                        attempt,
                        max_retries = self.retry_policy.max_retries,
                        agent = agent_name,
                        error = %e,
                        category = ?category,
                        "Transient error during agent execution"
                    );
                    last_error = Some(e);
                }
            }
        }

        warn!(
            agent = agent_name,
            retries = self.retry_policy.max_retries,
            "All retries exhausted, falling back to mock"
        );

        self.router.call(Provider::Mock, prompt).with_context(|| {
            format!(
                "Mock fallback also failed after {} retries. Last error: {:?}",
                self.retry_policy.max_retries, last_error
            )
        })
    }

    /// Execute a single agent request with streaming.
    ///
    /// Calls `on_token` for each token chunk received from the streaming
    /// response and returns the complete accumulated response.
    pub fn execute_with_retry_stream(
        &self,
        _context: &AgentContext,
        on_token: impl FnMut(&str),
    ) -> Result<()> {
        // T296: Placeholder stub for streaming execution
        // Full implementation requires:
        // 1. Streaming LLM call via rig-core CompletionModel::stream()
        // 2. BudgetGuard pre-reservation (T295)
        // 3. Token-by-token delivery to on_token callback
        let _ = on_token;
        anyhow::bail!(
            "execute_with_retry_stream: streaming not yet implemented in AgentExecutor. \
             Use zen_agent::ZenAgent::execute_stream for streaming support."
        );
    }
}
