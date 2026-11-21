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

use anyhow::Result;
use rig_core::message::ToolCall as NativeToolCall;
use tracing::{info, instrument, warn};

use zen_core::sanitize::InputSanitizer;
use zen_provider::{
    DefaultRouter, LlmRouter, Provider, SyncUsage, TaskRequirements, UsedCompletion,
};

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
        self.execute_round(context, agent, "", "")
    }

    /// Execute one agent round with a tool manifest and prior tool results.
    ///
    /// Used by the orchestrator's agentic tool loop: the manifest advertises
    /// the available tools (fenced-JSON calling convention) on the first
    /// round, and `tool_results` feeds back prior dispatch outputs on
    /// subsequent rounds.
    #[instrument(skip(self, context, agent), fields(agent_name = %context.agent_profile.name, sensitivity = ?context.sensitivity, round_has_results = !tool_results.is_empty()))]
    pub fn execute_round(
        &self,
        context: &AgentContext,
        agent: &crate::ZenAgent,
        tool_manifest: &str,
        tool_results: &str,
    ) -> Result<AgentExecution> {
        let start = Instant::now();
        let agent_name = context.agent_profile.name.clone();
        let sensitivity = context.sensitivity;

        // FR-TUI-012: Use context.preferences for routing
        let requirements = TaskRequirements {
            max_tokens: Some(context.max_tokens as u32),
            sensitivity,
            preferred_model: context.model_override.clone(),
            budget_limit: None,
        };

        let provider = match self
            .router
            .route_with_preferences(&requirements, &context.preferences)
        {
            Ok(provider) => provider,
            Err(primary_err) => match self.router.route(&requirements) {
                Ok(provider) => {
                    info!("Preference routing failed: {primary_err}, falling back");
                    provider
                }
                Err(fallback_err) => {
                    return Err(anyhow::anyhow!(
                        "no provider resolved for {agent_name}: primary={primary_err}, fallback={fallback_err}"
                    ));
                }
            },
        };

        // Build prompt with agent identity (SOUL.md/MEMORY.md/AGENTS.md)
        let prompt = self.build_prompt_with_identity(context, agent, tool_manifest, tool_results);
        let model_override = context.model_override.clone();
        let (response, tokens, usage) =
            self.execute_with_retry_model(&provider, &prompt, &agent_name, model_override.clone())?;

        let duration_ms = start.elapsed().as_millis() as u64;
        let cost = tokens as f64 * 0.002 / 1000.0; // ~$0.002/1K tokens estimate
        let runtime_model = self.resolved_runtime_model(&provider, model_override.as_deref());
        let usage_cost_usd = match (&usage, runtime_model.as_deref()) {
            (Some(echo), Some(model)) => self.usage_cost_usd(&provider, model, echo),
            _ => None,
        };

        Ok(AgentExecution {
            agent_name,
            response,
            metadata: ExecutionMetadata {
                tokens_used: tokens,
                cost_estimate: (cost * 1000.0).round() / 1000.0,
                model_used: provider.to_string(),
                runtime_model,
                usage_cost_usd,
                duration_ms,
                sensitivity,
                quality_notes: None,
                delivery_ready: true,
            },
            tool_calls: Vec::new(),
        })
    }

    /// A-02: the REAL model name the routed provider call used. The
    /// override wins; otherwise the provider's configured default (the
    /// mock stand-in names itself). `None` when unresolvable — honest
    /// degradation, never a guessed model.
    fn resolved_runtime_model(
        &self,
        provider: &Provider,
        model_override: Option<&str>,
    ) -> Option<String> {
        if let Some(model) = model_override {
            return Some(model.to_string());
        }
        let name = provider.to_string();
        if name == "mock" {
            return Some("mock".to_string());
        }
        self.router
            .provider_instance(&name)
            .map(|instance| instance.model_name().to_string())
    }

    /// A-02/row13: meter one round from the provider's REAL usage echo plus
    /// configured pricing. `None` when usage was not reported or the
    /// provider is unmetered (no pricing keys, not local) — cost is never
    /// fabricated from an estimate.
    fn usage_cost_usd(&self, provider: &Provider, model: &str, usage: &SyncUsage) -> Option<f64> {
        let metadata = self.router.model_metadata(&provider.to_string(), model);
        let priced = metadata.is_local
            || metadata.input_cost_per_million > 0.0
            || metadata.output_cost_per_million > 0.0;
        priced.then(|| {
            zen_provider::usage_to_cost_usd(&metadata, usage.input_tokens, usage.output_tokens)
        })
    }

    #[instrument(skip(self, context, agent), fields(query_len = context.user_query.len()))]
    fn build_prompt_with_identity(
        &self,
        context: &AgentContext,
        agent: &crate::ZenAgent,
        tool_manifest: &str,
        tool_results: &str,
    ) -> String {
        let system_prompt = self.assemble_system_prompt_with_identity(context, agent);
        let sanitizer = InputSanitizer::new();
        let sanitized_query = sanitizer.strip_dangerous_patterns(&context.user_query);

        let mut prompt = system_prompt;
        if !tool_manifest.is_empty() {
            prompt.push_str(
                "\n\n## Available tools\n\
                 You can call tools to read/write files, fetch web pages, search the web, and more.\n\
                 When you need a tool, respond with ONLY a fenced JSON block — exactly this shape:\n\
                 ```json\n{\"tool\": \"<tool_name>\", \"args\": { ... }}\n```\n\
                 No XML-style wrappers (e.g. <tool_code>), no bare JSON lines, no extra prose:\n\
                 unfenced or wrapped tool calls are NOT executed.\n\
                 You may include multiple calls as a JSON array. After receiving results,\
                 continue the conversation normally.\n\
                 Respond in the user's language — 除非用户要求，回答的语言与用户提问的语言保持一致。\n\
                 Tools:\n",
            );
            prompt.push_str(tool_manifest);
        }
        prompt.push_str(&format!(
            "\n\nUser: [USER_CONTENT_START]{}[USER_CONTENT_END]",
            sanitized_query
        ));
        if !tool_results.is_empty() {
            prompt.push_str("\n\n## Tool results from previous round\n");
            prompt.push_str(tool_results);
            prompt.push_str("\n\nBased on the results above, continue. If you need more tools, emit another fenced JSON block; otherwise answer directly.");
        }
        prompt.push_str("\n\nAssistant:");
        prompt
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
            if !identity.memory_content().is_empty() {
                builder = builder.identity_memory(identity.memory_content().to_string());
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
            .map(|turn| (turn.role.to_string(), turn.content.clone()))
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
    /// W12.13 (T9, review P3): the main non-stream execute path acquires
    /// the SAME primary-timeout bound the streaming machinery uses (W7.1)
    /// — a hung provider call fails at `first_token_budget_secs` instead
    /// of waiting for the 900s turn watchdog. The router call is sync
    /// (blocking client on this thread), so the bound is a dedicated
    /// thread + `recv_timeout` (`health_check` precedent); a timed-out
    /// call's thread is abandoned to finish on its own (bounded leak,
    /// same acceptance as the dedicated-thread probe). The "timed out"
    /// error text classifies transient ⇒ the retry ladder handles it,
    /// byte-consistent with the stream path's W7.1 behavior.
    ///
    /// W12.12 (T8): the call rides `LlmRouter::call_with_usage` so the
    /// provider's usage echo rides out alongside the text (`usage: None`
    /// when the surface does not report — mock under DL-1 semantics).
    fn call_with_non_stream_bound(
        &self,
        provider: &Provider,
        prompt: &str,
        agent_name: &str,
        model_override: Option<String>,
    ) -> Result<UsedCompletion> {
        let bound =
            std::time::Duration::from_secs(crate::completion_model::first_token_budget_secs());
        let router = self.router.clone();
        let provider = provider.clone();
        let prompt = prompt.to_string();
        let (tx, rx) = std::sync::mpsc::channel::<Result<UsedCompletion>>();
        std::thread::Builder::new()
            .name("zen-nonstream-bound".to_string())
            .spawn(move || {
                let _ = tx.send(
                    router
                        .call_with_usage_with_model(provider, &prompt, model_override)
                        .map_err(anyhow::Error::from),
                );
            })?;
        match rx.recv_timeout(bound) {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!(
                "non-streaming completion timed out after {bound:?} (agent {agent_name})"
            )),
        }
    }

    #[cfg(test)]
    fn execute_with_retry(
        &self,
        provider: &Provider,
        prompt: &str,
        agent_name: &str,
    ) -> Result<(String, u32, Option<SyncUsage>)> {
        self.execute_with_retry_model(provider, prompt, agent_name, None)
    }

    fn execute_with_retry_model(
        &self,
        provider: &Provider,
        prompt: &str,
        agent_name: &str,
        model_override: Option<String>,
    ) -> Result<(String, u32, Option<SyncUsage>)> {
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
                // Blocking by contract: async callers reach this only via
                // run_blocking_llm (block_in_place marks this thread blocking);
                // sync callers run it directly.
                std::thread::sleep(std::time::Duration::from_millis(delay));
            }

            match self.call_with_non_stream_bound(
                provider,
                prompt,
                agent_name,
                model_override.clone(),
            ) {
                Ok(completion) => {
                    // W12.12: real echo per counter when the provider
                    // reports one (per-counter estimate fallback,
                    // W12.20); the char/4 sum stays the fallback when
                    // nothing is reported (byte-identical).
                    let (tokens, usage) = match completion.usage {
                        Some(echo) => {
                            let (input, output) = zen_provider::reconcile_usage(
                                Some(echo),
                                (prompt.len() / 4) as u64,
                                (completion.text.len() / 4) as u64,
                            );
                            ((input + output) as u32, Some(echo))
                        }
                        None => ((prompt.len() + completion.text.len()) as u32 / 4, None),
                    };
                    if attempt > 0 {
                        info!(attempt, agent = agent_name, "Retry succeeded");
                    }
                    return Ok((completion.text, tokens, usage));
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

        Err(anyhow::anyhow!(
            "no provider call succeeded for {agent_name} after {} retries: {last_error:?}",
            self.retry_policy.max_retries
        ))
    }

    /// Execute a single agent request with streaming.
    ///
    /// Calls `callback` for each token chunk received from the streaming
    /// response and returns the complete accumulated response.
    ///
    /// **T296 Note**: Full streaming implementation requires rig-core
    /// `CompletionModel::stream()` + BudgetGuard (T295). Until those are
    /// available, this method falls back gracefully to the non-streaming
    /// execution path — the complete response is delivered as a single
    /// token via `callback`.
    #[instrument(skip(self, context, agent, callback))]
    pub fn execute_with_retry_stream(
        &self,
        context: &AgentContext,
        agent: &crate::ZenAgent,
        mut callback: impl FnMut(&str),
    ) -> Result<AgentExecution> {
        let result = self.execute(context, agent)?;
        callback(&result.response);
        Ok(result)
    }

    /// T115: Single-model-call helper for the AgentRun driver loop (sync).
    ///
    /// Builds the full prompt (system identity + tool manifest + user
    /// content + tool results), routes to a provider, calls the LLM with
    /// retry, and returns the response text.
    ///
    /// **Fenced-only contract (W4.6a/G1-01)**: the router `complete()` path
    /// returns plain text and cannot carry native provider tool calls, so
    /// `native_calls` is always empty on this surface and the orchestrator
    /// parses fenced ```json blocks instead. Native tool capture lives on the
    /// streaming surface (`ZenAgent::execute_stream_round`). Do not "fix"
    /// this by inventing a native path here without rewriting the executor's
    /// provider plumbing — the contract is pinned by
    /// `execute_model_call_is_fenced_only`.
    ///
    /// W12.12 (T8): the third slot carries the provider's usage echo when
    /// its surface reports one (`None` = fall back to estimates — mock
    /// under DL-1 semantics never reports).
    #[instrument(skip(self, context, agent), fields(agent_name = %context.agent_profile.name, sensitivity = ?context.sensitivity))]
    pub fn execute_model_call(
        &self,
        context: &AgentContext,
        agent: &crate::ZenAgent,
        tool_manifest: &str,
        tool_results: &str,
    ) -> Result<(String, Vec<NativeToolCall>, Option<SyncUsage>)> {
        let requirements = TaskRequirements {
            max_tokens: Some(context.max_tokens as u32),
            sensitivity: context.sensitivity,
            preferred_model: context.model_override.clone(),
            budget_limit: None,
        };

        let provider = match self
            .router
            .route_with_preferences(&requirements, &context.preferences)
        {
            Ok(provider) => provider,
            Err(primary_err) => match self.router.route(&requirements) {
                Ok(provider) => {
                    info!("Preference routing failed: {primary_err}, falling back");
                    provider
                }
                Err(fallback_err) => {
                    return Err(anyhow::anyhow!(
                        "no provider resolved for {}: primary={primary_err}, fallback={fallback_err}",
                        context.agent_profile.name
                    ));
                }
            },
        };

        let prompt = self.build_prompt_with_identity(context, agent, tool_manifest, tool_results);
        let (response, _tokens, usage) = self.execute_with_retry_model(
            &provider,
            &prompt,
            &context.agent_profile.name,
            context.model_override.clone(),
        )?;

        let native_calls: Vec<NativeToolCall> = Vec::new();

        Ok((response, native_calls, usage))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zen_provider::LlmConfig;

    fn test_executor_no_retries() -> AgentExecutor {
        AgentExecutor::new(DefaultRouter::new(LlmConfig::default())).with_retry_policy(
            RetryPolicy {
                max_retries: 0,
                base_delay_ms: 0,
                max_delay_ms: 0,
                jitter: false,
            },
        )
    }

    fn test_executor_with_router(router: DefaultRouter) -> AgentExecutor {
        AgentExecutor::new(router).with_retry_policy(RetryPolicy {
            max_retries: 0,
            base_delay_ms: 0,
            max_delay_ms: 0,
            jitter: false,
        })
    }

    #[test]
    fn retry_exhaustion_errors_instead_of_mock_fallback() {
        // Unknown provider: route-independent deterministic failure (no
        // instance exists), offline-safe. A single attempt must error.
        let executor = test_executor_no_retries();
        let err = executor
            .execute_with_retry(
                &Provider::Unknown("definitely-not-a-provider".to_string()),
                "hi",
                "test",
            )
            .expect_err("retry exhaustion must not silently fall back to mock");
        let text = format!("{err:#}");
        assert!(text.contains("test"), "error must name the agent: {text}");
    }

    #[test]
    fn mock_provider_direct_succeeds() {
        let executor = test_executor_no_retries();
        let (response, _, _) = executor
            .execute_with_retry(&Provider::Mock, "hi", "test")
            .expect("mock direct succeeds");
        assert!(!response.is_empty());
    }

    #[test]
    fn routing_failure_errors_instead_of_mock_for_both_execution_paths() {
        use crate::context::AgentContext;
        use crate::registry::{AgentRegistry, DefaultAgentRegistry};
        use crate::wiring::ZenWiring;
        use crate::zen_agent::ZenAgent;
        use zen_core::types::SessionContext;

        let router = DefaultRouter::new(LlmConfig {
            default_provider: None,
            ..Default::default()
        });
        let wiring = ZenWiring::new();
        let agent = ZenAgent::builder("Sisyphus")
            .build(&wiring, &router)
            .expect("agent builds");
        let profile = DefaultAgentRegistry::new()
            .find_by_name("Sisyphus")
            .expect("profile")
            .clone();
        let context = AgentContext::new(
            profile,
            "read a file".to_string(),
            SessionContext::new("Sisyphus".to_string(), String::new()),
        );

        let executor = AgentExecutor::new(router).with_retry_policy(RetryPolicy {
            max_retries: 0,
            base_delay_ms: 0,
            max_delay_ms: 0,
            jitter: false,
        });

        let round_err = executor
            .execute_round(&context, &agent, "", "")
            .expect_err("routing failure must not fall back to mock");
        let round_text = format!("{round_err:#}");
        assert!(
            round_text.contains("Sisyphus")
                && round_text.contains("primary=")
                && round_text.contains("fallback="),
            "execute_round must name the agent and both routes: {round_text}"
        );

        let call_err = executor
            .execute_model_call(&context, &agent, "", "")
            .expect_err("routing failure must not fall back to mock");
        let call_text = format!("{call_err:#}");
        assert!(
            call_text.contains("Sisyphus")
                && call_text.contains("primary=")
                && call_text.contains("fallback="),
            "execute_model_call must name the agent and both routes: {call_text}"
        );
    }

    #[test]
    fn execute_model_call_is_fenced_only() {
        use crate::context::AgentContext;
        use crate::registry::{AgentRegistry, DefaultAgentRegistry};
        use crate::wiring::ZenWiring;
        use crate::zen_agent::ZenAgent;
        use zen_core::types::SessionContext;

        let router = DefaultRouter::new(LlmConfig {
            default_provider: Some("mock".to_string()),
            ..Default::default()
        })
        .with_mock_response("```json\n{\"tool\": \"fs.read\", \"args\": {\"path\": \"/x\"}}\n```");
        let wiring = ZenWiring::new();
        let agent = ZenAgent::builder("Sisyphus")
            .build(&wiring, &router)
            .expect("agent builds");
        let profile = DefaultAgentRegistry::new()
            .find_by_name("Sisyphus")
            .expect("profile")
            .clone();
        let context = AgentContext::new(
            profile,
            "read a file".to_string(),
            SessionContext::new("Sisyphus".to_string(), String::new()),
        );

        let executor = AgentExecutor::new(router).with_retry_policy(RetryPolicy {
            max_retries: 0,
            base_delay_ms: 0,
            max_delay_ms: 0,
            jitter: false,
        });
        let (response, native_calls, _usage) = executor
            .execute_model_call(&context, &agent, "", "")
            .expect("model call");

        assert!(
            native_calls.is_empty(),
            "W4.6a/G1-01: the non-stream surface is fenced-only; native capture lives on the stream path"
        );
        assert!(
            response.contains("```json"),
            "the fenced block is the only tool-call channel here: {response}"
        );
    }

    #[test]
    fn build_prompt_with_identity_carries_the_language_rule() {
        use crate::context::AgentContext;
        use crate::registry::{AgentRegistry, DefaultAgentRegistry};
        use crate::wiring::ZenWiring;
        use crate::zen_agent::ZenAgent;
        use zen_core::types::SessionContext;

        let router = DefaultRouter::new(LlmConfig {
            default_provider: Some("mock".to_string()),
            ..Default::default()
        });
        let wiring = ZenWiring::new();
        let agent = ZenAgent::builder("Sisyphus")
            .build(&wiring, &router)
            .expect("agent builds");
        let profile = DefaultAgentRegistry::new()
            .find_by_name("Sisyphus")
            .expect("profile")
            .clone();
        let context = AgentContext::new(
            profile,
            "你好".to_string(),
            SessionContext::new("Sisyphus".to_string(), String::new()),
        );
        let executor = AgentExecutor::new(router);

        let prompt =
            executor.build_prompt_with_identity(&context, &agent, "fs.read: read files", "");
        assert!(
            prompt.contains("除非用户要求"),
            "executor protocol must carry the language-matching rule"
        );
        assert!(
            prompt.contains("## Available tools"),
            "executor protocol header present with a manifest"
        );
    }

    #[test]
    fn characterization_path_b_renders_agent_definition_sections() {
        use crate::context::AgentContext;
        use crate::registry::{AgentRegistry, DefaultAgentRegistry};
        use crate::wiring::ZenWiring;
        use crate::zen_agent::ZenAgent;
        use zen_core::definition::AgentSpec;
        use zen_core::types::SessionContext;

        let router = DefaultRouter::new(LlmConfig {
            default_provider: Some("mock".to_string()),
            ..Default::default()
        });
        let wiring = ZenWiring::new();
        let agent = ZenAgent::builder("Sisyphus")
            .build(&wiring, &router)
            .expect("agent builds");

        // Mirror of the zen_agent.rs Path A pin: Path B renders the agent
        // definition sections (prompt template / behavior constraints /
        // output format) that Path A structurally cannot. Locked pair —
        // spec.md C9 / F13; a convergence wave flips BOTH pins or neither.
        let mut spec = AgentSpec::default_agent();
        spec.prompt_template = "PATHB_TEMPLATE_MARKER_9f2".to_string();
        spec.behavior_constraints = vec!["PATHB_CONSTRAINT_MARKER_7c1".to_string()];
        spec.output_format = Some("PATHB_FORMAT_MARKER_3ee".to_string());
        let mut profile = DefaultAgentRegistry::new()
            .find_by_name("Sisyphus")
            .expect("profile")
            .clone();
        profile.definition = Some(spec);

        let context = AgentContext::new(
            profile,
            "hello".to_string(),
            SessionContext::new("Sisyphus".to_string(), String::new()),
        );
        let executor = AgentExecutor::new(router);
        let prompt = executor.build_prompt_with_identity(&context, &agent, "", "");

        assert!(
            prompt.contains("PATHB_TEMPLATE_MARKER_9f2"),
            "Path B renders the agent prompt template"
        );
        assert!(
            prompt.contains("## Behavior Constraints")
                && prompt.contains("PATHB_CONSTRAINT_MARKER_7c1"),
            "Path B renders behavior constraints + the blast-radius block"
        );
        assert!(
            prompt.contains("## Output Format") && prompt.contains("PATHB_FORMAT_MARKER_3ee"),
            "Path B renders the agent output format"
        );
    }

    #[test]
    fn characterization_path_b_manifest_lands_after_dynamic_boundary() {
        use crate::context::AgentContext;
        use crate::registry::{AgentRegistry, DefaultAgentRegistry};
        use crate::wiring::ZenWiring;
        use crate::zen_agent::ZenAgent;
        use zen_core::types::SessionContext;

        let router = DefaultRouter::new(LlmConfig {
            default_provider: Some("mock".to_string()),
            ..Default::default()
        });
        let wiring = ZenWiring::new();
        let agent = ZenAgent::builder("Sisyphus")
            .build(&wiring, &router)
            .expect("agent builds");
        let profile = DefaultAgentRegistry::new()
            .find_by_name("Sisyphus")
            .expect("profile")
            .clone();
        let context = AgentContext::new(
            profile,
            "hello".to_string(),
            SessionContext::new("Sisyphus".to_string(), String::new()),
        );
        let executor = AgentExecutor::new(router);
        let prompt =
            executor.build_prompt_with_identity(&context, &agent, "fs.read: read files", "");

        let boundary = prompt
            .find("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__")
            .expect("boundary present (env_info always renders a dynamic zone)");
        let manifest = prompt
            .find("## Available tools")
            .expect("executor manifest appended after the system prompt");

        // Twin of the zen_agent.rs Path A pin (spec.md C9 / F13): the
        // manifest lands after the dynamic zone on BOTH paths — a
        // convergence wave hoisting it above the boundary flips both.
        assert!(
            manifest > boundary,
            "Path B manifest lands after the dynamic boundary (cache-prefix break, locked)"
        );
    }

    /// W12.12 test harness: a one-shot OpenAI-compatible
    /// `/chat/completions` responder on a loopback port whose reply
    /// carries a real usage echo (prompt_tokens=111 / completion_tokens=22).
    /// Reads the full request (headers + Content-Length body) before
    /// answering so the connection closes cleanly instead of RST-ing the
    /// client mid-response-read.
    fn spawn_usage_echo_server() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let body = r#"{"id":"chatcmpl-w1212","object":"chat.completion","created":0,"model":"test-model","choices":[{"index":0,"message":{"role":"assistant","content":"echo reply"},"finish_reason":"stop"}],"usage":{"prompt_tokens":111,"completion_tokens":22,"total_tokens":133}}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let head_end = loop {
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos;
                }
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break buf.len(),
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            };
            let head = String::from_utf8_lossy(&buf[..head_end]).to_lowercase();
            let content_length: usize = head
                .lines()
                .find_map(|l| {
                    l.strip_prefix("content-length:")
                        .map(|v| v.trim().parse().unwrap_or(0))
                })
                .unwrap_or(0);
            let target = head_end + 4 + content_length;
            while buf.len() < target {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        });
        port
    }

    /// W12.12 (T8) usage-echo pin: when the provider surface reports
    /// usage, the executor seam threads the REAL echo out (not char/4)
    /// and the token ledger bills the echoed total. The char/4 estimate
    /// for this fixture would be (2 + 10) / 4 = 3 — the pin
    /// discriminates precisely because 133 ≠ 3.
    #[test]
    fn w1212_provider_usage_echo_reaches_the_executor_seam() {
        let port = spawn_usage_echo_server();
        // SAFETY: nextest runs each test in its own process; valid UTF-8.
        unsafe { std::env::set_var("ZEN_TEST_USAGE_ECHO_KEY", "test-key") };
        let router = DefaultRouter::from_agentic(&zen_core::config::ZenConfig {
            default_provider: Some("echo-test".to_string()),
            providers: std::collections::HashMap::from([(
                "echo-test".to_string(),
                zen_core::config::ProviderConfig {
                    provider_type: Some("openai".to_string()),
                    base_url: Some(format!("http://127.0.0.1:{port}/v1")),
                    default_model: Some("test-model".to_string()),
                    api_key_env: Some("ZEN_TEST_USAGE_ECHO_KEY".to_string()),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        });
        let executor = test_executor_with_router(router);
        let prompt = "hi";
        let (response, tokens, usage) = executor
            .execute_with_retry(&Provider::Unknown("echo-test".to_string()), prompt, "test")
            .expect("echo provider call succeeds");
        assert_eq!(response, "echo reply");
        let echo = usage.expect("the reporting provider must surface its echo");
        assert_eq!(echo.input_tokens, 111, "real prompt tokens, not char/4");
        assert_eq!(echo.output_tokens, 22, "real completion tokens, not char/4");
        assert_eq!(tokens, 133, "the ledger bills the echoed total");
        assert_ne!(
            tokens,
            (prompt.len() + response.len()) as u32 / 4,
            "the real echo must not collapse into the char/4 estimate"
        );
    }

    /// W12.12 (T8) fallback pin: when the provider does NOT report usage
    /// (mock under DL-1 semantics never reports), the echo slot stays
    /// None and the char/4 estimate fallback stays byte-identical.
    #[test]
    fn w1212_usage_absent_falls_back_to_char4_estimate() {
        let executor = test_executor_no_retries();
        let prompt = "hello there";
        let (response, tokens, usage) = executor
            .execute_with_retry(&Provider::Mock, prompt, "test")
            .expect("mock direct succeeds");
        assert!(usage.is_none(), "mock never reports usage (DL-1)");
        assert_eq!(
            tokens,
            (prompt.len() + response.len()) as u32 / 4,
            "the char/4 estimate fallback must stay byte-identical"
        );
    }

    /// W12.13 (T9) tightening pin: the EXECUTOR seam the orchestrator
    /// actually drives cuts a hanging provider call at the primary-timeout
    /// bound and surfaces the "non-streaming completion timed out" error
    /// through the retry ladder instead of hanging on the provider's own
    /// (120s+) timeout. Mirrors the completion_model W7.1 pin harness
    /// (hanging TCP listener + tiny explicit env override).
    #[test]
    fn w1213_hanging_provider_call_is_cut_by_the_nonstream_bound() {
        // SAFETY: process-per-test (nextest); valid UTF-8 value.
        unsafe { std::env::set_var("ZEN_STREAM_FIRST_TOKEN_TIMEOUT_SECS", "2") };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            // Accept and hold connections open forever — a deterministic
            // HTTP hang without writing a byte. Leak the accepted sockets:
            // dropping them would close the connection (RST) and turn the
            // deterministic hang into a fast error, voiding the bound proof.
            for stream in listener.incoming().flatten() {
                std::mem::forget(stream);
            }
        });
        let router = DefaultRouter::from_agentic(&zen_core::config::ZenConfig {
            default_provider: Some("ollama".to_string()),
            providers: std::collections::HashMap::from([(
                "ollama".to_string(),
                zen_core::config::ProviderConfig {
                    provider_type: Some("ollama".to_string()),
                    base_url: Some(format!("http://127.0.0.1:{port}")),
                    default_model: Some("test-model".to_string()),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        });
        let executor = test_executor_with_router(router);
        let started = std::time::Instant::now();
        let err = executor
            .execute_with_retry(&Provider::Unknown("ollama".to_string()), "hi", "test")
            .expect_err("the hanging provider must be cut by the bound");
        let elapsed = started.elapsed();
        unsafe { std::env::remove_var("ZEN_STREAM_FIRST_TOKEN_TIMEOUT_SECS") };
        let text = format!("{err:#}");
        assert!(
            text.contains("non-streaming completion timed out"),
            "the failure must be the executor's primary-timeout bound: {text}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(30),
            "the bound must fire at ~2s, not the provider's own 120s: {elapsed:?}"
        );
    }
}
