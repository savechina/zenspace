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
use tracing::{info, instrument, warn};

use zen_core::sanitize::InputSanitizer;
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
    ///
    /// # Architecture (ADR-011 + FR-TUI-012)
    ///
    /// - `context`: Routing context (preferences, session, agent_profile)
    /// - `agent`: Agent instance (identity, skills, tools)
    ///
    /// FR-TUI-012: context.preferences激活route_with_preferences routing
    /// Agent identity: agent.identity注入到system prompt assembly
    #[instrument(skip(self, context, agent), fields(agent_name = %context.agent_profile.name, sensitivity = ?context.sensitivity))]
    pub fn execute(
        &self,
        context: &AgentContext,
        agent: &crate::ZenAgent,
    ) -> Result<AgentExecution> {
        let start = Instant::now();
        let agent_name = context.agent_profile.name.clone();
        let sensitivity = context.sensitivity;

        // FR-TUI-012: Use context.preferences for routing
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

        // Build prompt with agent identity (SOUL.md/MEMORY.md/AGENTS.md)
        let prompt = self.build_prompt_with_identity(context, agent);
        let (response, tokens) = self.execute_with_retry(&provider, &prompt, &agent_name)?;

        let duration_ms = start.elapsed().as_millis() as u64;
        let cost = tokens as f64 * 0.002 / 1000.0; // ~$0.002/1K tokens estimate

        Ok(AgentExecution {
            agent_name,
            response,
            metadata: ExecutionMetadata {
                tokens_used: tokens,
                cost_estimate: (cost * 1000.0).round() / 1000.0,
                model_used: provider.to_string(),
                duration_ms,
                sensitivity,
            },
            tool_calls: Vec::new(),
            sub_agent_results: Vec::new(),
        })
    }

    #[instrument(skip(self, context, agent), fields(query_len = context.user_query.len()))]
    fn build_prompt_with_identity(
        &self,
        context: &AgentContext,
        agent: &crate::ZenAgent,
    ) -> String {
        let system_prompt = self.assemble_system_prompt_with_identity(context, agent);
        let sanitizer = InputSanitizer::new();
        let sanitized_query = sanitizer.strip_dangerous_patterns(&context.user_query);
        format!(
            "{}\n\nUser: [USER_CONTENT_START]{}[USER_CONTENT_END]\n\nAssistant:",
            system_prompt, sanitized_query
        )
    }

    // ADR-013: Using zen_memory::PromptAssembly 18-section tiered system
    // Features: cache boundary, priority chain, section memoization, blast radius taxonomy
    #[instrument(skip(self, context, agent), fields(sensitivity = ?context.sensitivity))]
    fn assemble_system_prompt_with_identity(
        &self,
        context: &AgentContext,
        agent: &crate::ZenAgent,
    ) -> String {
        use zen_memory::PromptAssembly;

        // Build PromptAssembly from AgentContext and ZenAgent identity
        let mut builder = PromptAssembly::builder().sensitivity(context.sensitivity);

        // Priority 3: Agent definition (from AgentProfile)
        if let Some(ref def) = context.agent_profile.definition {
            builder = builder.agent_definition(def.clone());
        }

        // Section 2: Intro (SOUL.md)
        // Section 18: CLAUDE.md (AGENTS.md)
        if let Some(identity) = agent.identity() {
            if !identity.soul_content().is_empty() {
                builder = builder.intro(identity.soul_content().to_string());
            }
            if !identity.agents_content().is_empty() {
                builder = builder.claude_md(identity.agents_content().to_string());
            }
        }

        // Section 17: Memory (retrieved knowledge + conversation history)
        let knowledge: Vec<String> = context
            .session
            .knowledge
            .iter()
            .map(|n| n.content.clone())
            .collect();
        let history: Vec<(String, String)> = context
            .session
            .conversation
            .iter()
            .map(|turn| (turn.role.clone(), turn.content.clone()))
            .collect();
        builder = builder.memory_section(knowledge, history);

        // Section 13: Env info (dynamic)
        builder = builder.env_info(PromptAssembly::build_env_info(&context.session));

        // Self-Learning Signal Sections (corrections, feedback, beliefs, etc.)
        if let Some(signals) = agent.signals() {
            if !signals.corrections.is_empty() {
                builder = builder.corrections(&signals.corrections);
            }
            if !signals.feedback.is_empty() {
                builder = builder.feedback(&signals.feedback);
            }
            if !signals.beliefs.is_empty() {
                builder = builder.beliefs(&signals.beliefs);
            }
            if !signals.virtue_logs.is_empty() {
                builder = builder.virtue_logs(&signals.virtue_logs);
            }
            if !signals.reflections.is_empty() {
                builder = builder.reflections(&signals.reflections);
            }
            if !signals.mental_models.is_empty() {
                builder = builder.mental_models(&signals.mental_models);
            }
            if !signals.decisions.is_empty() {
                builder = builder.decisions(&signals.decisions);
            }
            if !signals.priority_items.is_empty() {
                builder = builder.priority_items(&signals.priority_items);
            }
        }

        // Build and assemble with cache boundary
        builder.build().assemble()
    }

    #[instrument(skip(self), fields(agent_name, prompt_len = prompt.len(), provider = %provider))]
    fn execute_with_retry(
        &self,
        provider: &Provider,
        prompt: &str,
        agent_name: &str,
    ) -> Result<(String, u32)> {
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
                    let estimated_tokens = (prompt.len() + response.len()) as u32 / 4;
                    if attempt > 0 {
                        info!(attempt, agent = agent_name, "Retry succeeded");
                    }
                    return Ok((response, estimated_tokens));
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

        let response = self.router.call(Provider::Mock, prompt).with_context(|| {
            format!(
                "Mock fallback also failed after {} retries. Last error: {:?}",
                self.retry_policy.max_retries, last_error
            )
        })?;
        let estimated_tokens = (prompt.len() + response.len()) as u32 / 4;
        Ok((response, estimated_tokens))
    }

    /// Execute a single agent request with streaming.
    ///
    /// Calls `on_token` for each token chunk received from the streaming
    /// response and returns the complete accumulated response.
    ///
    /// **T296 Note**: Full streaming implementation requires rig-core
    /// `CompletionModel::stream()` + BudgetGuard (T295). Until those are
    /// available, this method falls back gracefully to the non-streaming
    /// execution path — the complete response is delivered as a single
    /// token via `on_token`.
    #[instrument(skip(self, context, agent, on_token))]
    pub fn execute_with_retry_stream(
        &self,
        context: &AgentContext,
        agent: &crate::ZenAgent,
        mut on_token: impl FnMut(&str),
    ) -> Result<AgentExecution> {
        let result = self.execute(context, agent)?;
        on_token(&result.response);
        Ok(result)
    }
}
