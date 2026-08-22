use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use rig_compose::budget::{AtomicTokenBudget, TokenBudget};
use rig_compose::normalizer::{
    ToolInvocation, ToolInvocationResult, dispatch_tool_invocations_with_hooks,
};
use rig_memvid::MemvidPersistHook;
use tracing::{debug, info, instrument, warn};

use zen_core::types::SessionContext;
use zen_memory::{ZenMemvidStore, create_persist_hook, default_memory_config};
use zen_provider::DefaultRouter;

use crate::delegate_tools;
use crate::delegate_tools::ZenDelegateTools;
use crate::execution::{AgentExecution, ExecutionMetadata, ToolCall};
use crate::registry::AgentRegistry;
use crate::review::QualityPipeline;
use crate::wiring::ZenWiring;
use crate::zen_agent::ZenAgent;
use zen_core::paths::ZenPaths;

/// Maximum tool dispatch rounds per user query before giving the final answer.
const MAX_TOOL_ROUNDS: usize = 4;

/// Orchestrator manages agent lifecycle, registry, and execution flow.
///
/// Architecture (ADR-011 + FR-TUI-012):
/// - Registry: Manages AgentProfile by role/name
/// - Executor: Executes with AgentContext (routing) + ZenAgent (instance)
/// - FR-TUI-012: Agent preferences influence provider selection
pub struct AgentOrchestrator {
    registry: crate::registry::DefaultAgentRegistry,
    wiring: ZenWiring,
    delegates: ZenDelegateTools,
    executor: crate::executor::AgentExecutor,
    token_budget: Arc<AtomicTokenBudget>,
    memvid_store: Option<rig_memvid::MemvidStore>,
    persist_hook: Option<MemvidPersistHook<rig_core::completion::CompletionRequest>>,
    quality_pipeline: QualityPipeline,
    /// FR-046 `[agents] tools` overlay applied on top of the builtin
    /// per-agent grant map when building agents and delegates.
    tool_overlay: Vec<String>,
}

impl AgentOrchestrator {
    pub fn new(router: DefaultRouter) -> Self {
        let registry = crate::registry::DefaultAgentRegistry::new();
        let wiring = ZenWiring::new();
        let memvid_store = wiring.memvid_store.clone();
        let persist_hook = memvid_store.as_ref().map(|store| {
            let config = zen_memory::default_memory_config();
            zen_memory::create_persist_hook(store.clone(), config)
        });
        if memvid_store.is_some() {
            debug!("AgentOrchestrator: auto-wired memvid store from ZenWiring");
        }
        let tool_overlay = delegate_tools::load_tool_grant_overlay();
        let delegates = ZenDelegateTools::with_tool_overlay(&wiring, &router, tool_overlay.clone());
        let executor = crate::executor::AgentExecutor::new(router.clone());
        let token_budget = Arc::new(AtomicTokenBudget::new(100_000));
        Self {
            registry,
            wiring,
            delegates,
            executor,
            token_budget,
            memvid_store,
            persist_hook,
            quality_pipeline: QualityPipeline::new(),
            tool_overlay,
        }
    }

    pub fn with_token_budget(router: DefaultRouter, capacity: u64) -> Self {
        let registry = crate::registry::DefaultAgentRegistry::new();
        let wiring = ZenWiring::new();
        let memvid_store = wiring.memvid_store.clone();
        let persist_hook = memvid_store.as_ref().map(|store| {
            let config = zen_memory::default_memory_config();
            zen_memory::create_persist_hook(store.clone(), config)
        });
        let tool_overlay = delegate_tools::load_tool_grant_overlay();
        let delegates = ZenDelegateTools::with_tool_overlay(&wiring, &router, tool_overlay.clone());
        let executor = crate::executor::AgentExecutor::new(router.clone());
        let token_budget = Arc::new(AtomicTokenBudget::new(capacity));
        Self {
            registry,
            wiring,
            delegates,
            executor,
            token_budget,
            memvid_store,
            persist_hook,
            quality_pipeline: QualityPipeline::new(),
            tool_overlay,
        }
    }

    pub fn with_memory(mut self, memory_path: PathBuf) -> Result<Self> {
        let store = ZenMemvidStore::new(memory_path)?;
        let inner = store.into_inner();
        let config = default_memory_config();
        let hook = create_persist_hook(inner.clone(), config);
        self.memvid_store = Some(inner);
        self.persist_hook = Some(hook);
        debug!("AgentOrchestrator: PersistHook wired for auto-capture (FR-MEM-002 / D2)");
        Ok(self)
    }

    pub fn with_memory_read_only(mut self, memory_path: PathBuf) -> Result<Self> {
        let store = ZenMemvidStore::new_read_only(memory_path)?;
        let inner = store.into_inner();
        self.memvid_store = Some(inner);
        debug!("AgentOrchestrator: read-only memory store (no PersistHook)");
        Ok(self)
    }

    /// Rebuild the wiring with the given sandbox mode.
    ///
    /// The mode drives the fs-tool path validators and the dispatch-time
    /// sandbox hook pipeline (rate limit → seatbelt → audit → approval).
    pub fn with_sandbox_mode(mut self, mode: zen_core::sandbox::SandboxMode) -> Self {
        self.wiring = ZenWiring::with_sandbox_mode(mode, Vec::new(), None);
        self
    }

    /// Register an interactive approval callback for `Ask` sandbox mode.
    pub fn with_approval_callback(mut self, callback: zen_core::sandbox::ApprovalCallback) -> Self {
        self.wiring.set_approval_callback(callback);
        self
    }

    pub fn budget_available(&self) -> u64 {
        self.token_budget.available()
    }

    pub fn budget_consumed(&self) -> u64 {
        self.token_budget
            .capacity()
            .saturating_sub(self.token_budget.available())
    }

    pub fn delegates(&self) -> &ZenDelegateTools {
        &self.delegates
    }

    pub fn quality_pipeline(&self) -> &QualityPipeline {
        &self.quality_pipeline
    }

    pub fn with_quality_pipeline(mut self, pipeline: QualityPipeline) -> Self {
        self.quality_pipeline = pipeline;
        self
    }

    async fn build_agent(&self, agent_name: &str) -> Result<ZenAgent> {
        let skills = delegate_tools::resolve_skill_ids_for_agent(agent_name);
        let tools = delegate_tools::resolve_agent_tool_grants(
            agent_name,
            &self.tool_overlay,
            &self.wiring.tools,
        );
        debug!(
            "building agent: {}",
            delegate_tools::describe_agent(agent_name, &self.registry)
        );

        let mut builder = ZenAgent::builder(agent_name);
        for skill_id in &skills {
            builder = builder.with_skill(skill_id.as_str());
        }
        for tool_id in &tools {
            builder = builder.with_tool(tool_id.as_str());
        }
        if let Ok(paths) = ZenPaths::detect() {
            builder = builder.with_paths(paths);
        }
        if let Some(store) = self.memvid_store.clone() {
            builder = builder.with_memvid_store(store);
        }
        builder.build(&self.wiring, self.executor.router())
    }

    fn classify_agent(&self, query: &str) -> String {
        let lower = query.to_lowercase();

        if lower.contains("implement")
            || lower.contains("code")
            || lower.contains("function")
            || lower.contains("class")
            || lower.contains("refactor")
            || lower.contains("debug")
        {
            return "Hephaestus".to_string();
        }

        if lower.contains("research")
            || lower.contains("explore")
            || lower.contains("discover")
            || lower.contains("find information")
            || lower.contains("investigate")
        {
            return "Explore".to_string();
        }

        if lower.contains("analyze")
            || lower.contains("analysis")
            || lower.contains("deep")
            || lower.contains("architecture")
            || lower.contains("design")
        {
            return "Oracle".to_string();
        }

        if lower.contains("organize")
            || lower.contains("knowledge")
            || lower.contains("wiki")
            || lower.contains("notes")
            || lower.contains("catalog")
            || lower.contains("dedup")
        {
            return "Librarian".to_string();
        }

        if lower.contains("consolidate")
            || lower.contains("pipeline")
            || lower.contains("merge")
            || lower.contains("compile wiki")
        {
            return "Hermes".to_string();
        }

        if lower.contains("review")
            || lower.contains("audit")
            || lower.contains("check quality")
            || lower.contains("security")
        {
            return "Momus".to_string();
        }

        if lower.contains("plan")
            || lower.contains("strategy")
            || lower.contains("roadmap")
            || lower.contains("spec")
            || lower.contains("milestone")
        {
            return "Prometheus".to_string();
        }

        if lower.contains("gap")
            || lower.contains("tactical")
            || lower.contains("assumption")
            || lower.contains("feasibility")
        {
            return "Metis".to_string();
        }

        if lower.contains("batch")
            || lower.contains("automate")
            || lower.contains("routine")
            || lower.contains("schedule")
        {
            return "Atlas".to_string();
        }

        if lower.contains("format")
            || lower.contains("convert")
            || lower.contains("download")
            || lower.contains("clean")
        {
            return "Junior".to_string();
        }

        if lower.contains("value")
            || lower.contains("align")
            || lower.contains("priority")
            || lower.contains("should we")
        {
            return "Zeus".to_string();
        }

        if lower.contains("image")
            || lower.contains("chart")
            || lower.contains("visual")
            || lower.contains("diagram")
            || lower.contains("screenshot")
        {
            return "Argus".to_string();
        }

        "Sisyphus".to_string()
    }

    #[instrument(skip(self, session), fields(session_id = %session.session_id))]
    pub async fn execute(
        &self,
        session: &mut SessionContext,
        user_query: &str,
    ) -> Result<AgentExecution> {
        let start = Instant::now();
        let agent_name = self.classify_agent(user_query);
        info!(
            agent = agent_name,
            query_len = user_query.len(),
            "AgentOrchestrator: executing query"
        );

        let zen_agent = self.build_agent(&agent_name).await?;

        session.agent_name.clone_from(&agent_name);

        // Architecture: Orchestrator → Registry → AgentProfile by name
        let profile = self
            .registry
            .find_by_name(&agent_name)
            .map_err(|e| anyhow::anyhow!("Agent not found: {}", e))?
            .clone();

        // FR-TUI-012: AgentContext with preferences from profile
        let context =
            crate::AgentContext::new(profile.clone(), user_query.to_string(), session.clone())
                .with_preferences(profile.llm_preferences.clone());

        let estimated_tokens = user_query.len() / 4 + 512;
        let reservation = self
            .token_budget
            .try_reserve_tokens(estimated_tokens as u64)
            .await?;
        if reservation.is_none() {
            return Err(anyhow::anyhow!(
                "Token budget exhausted ({} consumed, {} capacity)",
                self.token_budget.tokens_consumed().await,
                self.token_budget.capacity()
            ));
        }
        let reservation = reservation.unwrap();

        // Execution: AgentContext (routing) + ZenAgent (instance) → Executor.
        // The first round advertises the agent-scoped tool manifest (honours
        // the per-agent whitelist from AGENT_TOOLS) so the model can emit
        // fenced-JSON tool calls for tools the agent actually holds.
        let tool_manifest = zen_agent.tool_manifest();
        let mut execution =
            self.executor
                .execute_round(&context, &zen_agent, &tool_manifest, "")?;

        // Connect stdio MCP servers once per process (idempotent, non-fatal).
        self.wiring.connect_mcp_servers().await;

        // Update the confidentiality gate for this session so cloud tools
        // are blocked when the session is Confidential (FR-009).
        self.wiring.set_sensitivity(session.sensitivity_policy);

        // Agentic tool loop: while the model requests tools, dispatch them
        // through the sandbox hook pipeline and feed results back, up to
        // MAX_TOOL_ROUNDS iterations.
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut round = 0;
        while round < MAX_TOOL_ROUNDS {
            let invocations = Self::parse_tool_invocations(&execution.response);
            if invocations.is_empty() {
                break;
            }
            round += 1;

            let hooks = self.wiring.dispatch_hooks();
            match dispatch_tool_invocations_with_hooks(
                zen_agent.generic.tools(),
                &invocations,
                &hooks,
            )
            .await
            {
                Ok(results) => {
                    for result in &results {
                        tool_calls.push(ToolCall {
                            tool_name: result.invocation.name.to_string(),
                            arguments: result.invocation.args.to_string(),
                            result: result.output.to_string(),
                        });
                    }
                    let results_json = Self::results_to_prompt(&results);
                    execution = self.executor.execute_round(
                        &context,
                        &zen_agent,
                        &tool_manifest,
                        &results_json,
                    )?;
                }
                Err(e) => {
                    warn!(error = %e, round, "tool dispatch terminated by sandbox hook");
                    tool_calls.push(ToolCall {
                        tool_name: "<dispatch>".to_string(),
                        arguments: String::new(),
                        result: format!("blocked by sandbox: {e}"),
                    });
                    break;
                }
            }
        }

        let actual_tokens = (execution.response.len() / 4 + user_query.len() / 4) as u64;
        self.token_budget
            .record_usage(reservation, actual_tokens, actual_tokens)
            .await;

        // Check if the query suggests needing sub-agent help
        let mut sub_agent_results = Vec::new();
        let lower = user_query.to_lowercase();
        if lower.contains("research") {
            sub_agent_results.push(AgentExecution::minimal(
                "Delegate::Oracle",
                format!("Delegated research on: {user_query}"),
            ));
        }
        if lower.contains("analyze deeply") {
            sub_agent_results.push(AgentExecution::minimal(
                "Delegate::Metis",
                format!("Deep analysis on: {user_query}"),
            ));
        }
        if lower.contains("compare") {
            sub_agent_results.push(AgentExecution::minimal(
                "Delegate::Momus",
                format!("Comparison analysis on: {user_query}"),
            ));
        }

        let duration_ms = start.elapsed().as_millis() as u64;

        // Build execution with sub-agent results
        let final_execution = AgentExecution {
            agent_name: execution.agent_name,
            response: execution.response,
            metadata: ExecutionMetadata {
                tokens_used: execution.metadata.tokens_used,
                cost_estimate: execution.metadata.cost_estimate,
                model_used: execution.metadata.model_used,
                duration_ms,
                sensitivity: execution.metadata.sensitivity,
            },
            tool_calls: if tool_calls.is_empty() {
                execution.tool_calls
            } else {
                tool_calls
            },
            sub_agent_results,
        };

        crate::observability::emit_prompt_completed(
            &final_execution.metadata.model_used,
            &session.session_id.to_string(),
            Some(final_execution.metadata.tokens_used as u64),
            None,
            Some(final_execution.metadata.duration_ms),
        );

        session.add_turn("user", user_query);
        session.add_turn("assistant", &final_execution.response);
        zen_agent.persist_turn(
            &session.session_id.to_string(),
            user_query,
            &final_execution.response,
        );

        Ok(final_execution)
    }

    /// Parse fenced-JSON tool invocations from model output.
    ///
    /// The model announces tool usage inside ```` ```json ```` blocks with the
    /// shape `{"tool": "<name>", "args": { ... }}` (or an array of such
    /// objects). Anything else is treated as a plain answer and yields an
    /// empty result, terminating the tool loop.
    fn parse_tool_invocations(response: &str) -> Vec<ToolInvocation> {
        let mut invocations = Vec::new();
        let mut rest = response;
        while let Some(start) = rest.find("```json") {
            let after_marker = &rest[start + "```json".len()..];
            let Some(end) = after_marker.find("```") else {
                break;
            };
            let block = &after_marker[..end];
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(block) {
                let items: Vec<&serde_json::Value> = match &value {
                    serde_json::Value::Array(items) => items.iter().collect(),
                    serde_json::Value::Object(_) => vec![&value],
                    _ => Vec::new(),
                };
                for item in items {
                    let (Some(name), Some(args)) =
                        (item.get("tool").and_then(|v| v.as_str()), item.get("args"))
                    else {
                        continue;
                    };
                    if let Ok(invocation) = ToolInvocation::new(name, args.clone()) {
                        invocations.push(invocation);
                    }
                }
            }
            rest = &after_marker[end + 3..];
        }
        invocations
    }

    /// Render dispatch results as a compact prompt section for the next round.
    fn results_to_prompt(results: &[ToolInvocationResult]) -> String {
        let entries: Vec<String> = results
            .iter()
            .map(|r| {
                format!(
                    "tool: {}\nargs: {}\nresult: {}",
                    r.invocation.name,
                    r.invocation.args,
                    serde_json::to_string(&r.output).unwrap_or_default()
                )
            })
            .collect();
        entries.join("\n---\n")
    }

    /// Execute with backward compatibility — returns String for existing callers.
    pub async fn execute_string(
        &self,
        session: &mut SessionContext,
        user_query: &str,
    ) -> Result<String> {
        let execution = self.execute(session, user_query).await?;
        Ok(execution.response)
    }

    /// Stream tokens to a callback while building the response.
    ///
    /// Drives a fenced-JSON tool loop identical in shape to `execute()`:
    /// round 1 streams the model's answer; if it contains tool calls, we
    /// dispatch them through the sandbox hook pipeline, feed results back,
    /// and stream another round, up to `MAX_TOOL_ROUNDS`.
    #[instrument(skip(self, session, on_token), fields(session_id = %session.session_id))]
    pub async fn execute_stream(
        &self,
        session: &mut SessionContext,
        user_query: &str,
        mut on_token: impl FnMut(&str),
    ) -> Result<String> {
        let _start = Instant::now();
        let agent_name = self.classify_agent(user_query);
        info!(
            agent = agent_name,
            query_len = user_query.len(),
            "AgentOrchestrator: streaming execution"
        );

        let zen_agent = self.build_agent(&agent_name).await?;

        session.agent_name.clone_from(&agent_name);

        self.wiring.connect_mcp_servers().await;

        let mut response = zen_agent
            .execute_stream_round(user_query, session, None, &mut on_token)
            .await?;

        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut interaction_turns: Vec<(&str, String)> = Vec::new();
        let mut round = 0;
        while round < MAX_TOOL_ROUNDS {
            let invocations = Self::parse_tool_invocations(&response);
            if invocations.is_empty() {
                break;
            }
            round += 1;

            let hooks = self.wiring.dispatch_hooks();
            match dispatch_tool_invocations_with_hooks(
                zen_agent.generic.tools(),
                &invocations,
                &hooks,
            )
            .await
            {
                Ok(results) => {
                    for result in &results {
                        tool_calls.push(ToolCall {
                            tool_name: result.invocation.name.to_string(),
                            arguments: result.invocation.args.to_string(),
                            result: result.output.to_string(),
                        });
                    }
                    let results_json = Self::results_to_prompt(&results);
                    info!(
                        round,
                        tool_count = results.len(),
                        "streaming tool dispatch succeeded, re-streaming"
                    );
                    interaction_turns.push(("assistant", response.clone()));
                    interaction_turns.push(("tool", results_json.clone()));
                    response = zen_agent
                        .execute_stream_round(
                            user_query,
                            session,
                            Some(&results_json),
                            &mut on_token,
                        )
                        .await?;
                }
                Err(e) => {
                    warn!(error = %e, round, "streaming tool dispatch terminated by sandbox hook");
                    tool_calls.push(ToolCall {
                        tool_name: "<dispatch>".to_string(),
                        arguments: String::new(),
                        result: format!("blocked by sandbox: {e}"),
                    });
                    break;
                }
            }
        }

        session.add_turn("user", user_query);
        for (role, content) in &interaction_turns {
            session.add_turn(role, content);
        }
        session.add_turn("assistant", &response);

        let actual_tokens = (response.len() / 4 + user_query.len() / 4) as u64;
        let reservation = self
            .token_budget
            .try_reserve_tokens(actual_tokens)
            .await
            .ok()
            .flatten();
        if let Some(res) = reservation {
            self.token_budget
                .record_usage(res, actual_tokens, actual_tokens)
                .await;
        }

        zen_agent.persist_turn(&session.session_id.to_string(), user_query, &response);

        Ok(response)
    }

    /// Route (keyword classification) — backward compatible public facade.
    /// Internally delegates to classify_agent.
    pub fn route(&self, query: &str) -> String {
        self.classify_agent(query)
    }

    #[must_use]
    pub fn select_agent_for_conversation(&self) -> String {
        "Sisyphus".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_compose::normalizer::ToolDispatchHook;

    #[test]
    fn test_select_agent_returns_sisyphus() {
        let config = zen_core::config::LlmConfig::default();
        let router = zen_provider::DefaultRouter::new(config);
        let orchestrator = AgentOrchestrator::new(router);
        assert_eq!(orchestrator.select_agent_for_conversation(), "Sisyphus");
    }

    #[test]
    fn test_parse_tool_invocations_single_block() {
        let response = "Let me check that file.\n```json\n{\"tool\": \"fs.read\", \"args\": {\"path\": \"/tmp/x\"}}\n```\nHere it is.";
        let invocations = AgentOrchestrator::parse_tool_invocations(response);
        assert_eq!(invocations.len(), 1);
        assert_eq!(invocations[0].name.as_str(), "fs.read");
        assert_eq!(invocations[0].args["path"], "/tmp/x");
    }

    #[test]
    fn test_parse_tool_invocations_array() {
        let response = "```json\n[{\"tool\": \"fs.read\", \"args\": {\"path\": \"/a\"}}, {\"tool\": \"web.fetch\", \"args\": {\"url\": \"https://x\"}}]\n```";
        let invocations = AgentOrchestrator::parse_tool_invocations(response);
        assert_eq!(invocations.len(), 2);
        assert_eq!(invocations[1].name.as_str(), "web.fetch");
    }

    #[test]
    fn test_parse_tool_invocations_plain_answer() {
        let response = "The answer is 42. No tools needed.";
        assert!(AgentOrchestrator::parse_tool_invocations(response).is_empty());
    }

    #[test]
    fn test_parse_tool_invocations_ignores_bad_shape() {
        let response = "```json\n{\"not_a_tool\": true}\n```";
        assert!(AgentOrchestrator::parse_tool_invocations(response).is_empty());
    }

    #[test]
    fn test_results_to_prompt_renders_entries() {
        let invocation =
            ToolInvocation::new("fs.read", serde_json::json!({"path": "/tmp/x"})).unwrap();
        let results = vec![ToolInvocationResult {
            invocation,
            output: serde_json::json!({"content": "hello"}),
        }];
        let prompt = AgentOrchestrator::results_to_prompt(&results);
        assert!(prompt.contains("tool: fs.read"));
        assert!(prompt.contains("hello"));
    }

    #[test]
    fn test_tool_manifest_lists_registered_tools() {
        let wiring = ZenWiring::new();
        let manifest = wiring.tool_manifest();
        assert!(manifest.contains("fs.read"));
        assert!(manifest.contains("web.fetch"));
        assert!(manifest.contains("web.search"));
    }

    #[test]
    fn test_agent_scoped_manifest_honours_whitelist() {
        let wiring = ZenWiring::new();
        let router = zen_provider::DefaultRouter::new(zen_provider::LlmConfig::default());
        // Build an agent with a narrow whitelist: read-only fs + web tools,
        // deliberately excluding mutating fs tools.
        let agent = crate::ZenAgent::builder("Hermes")
            .with_tool("fs.read")
            .with_tool("fs.list")
            .with_tool("web.fetch")
            .with_tool("web.search")
            .build(&wiring, &router)
            .expect("agent build");

        let manifest = agent.tool_manifest();
        assert!(manifest.contains("fs.read"), "granted tool missing");
        assert!(manifest.contains("web.search"), "granted tool missing");
        assert!(
            !manifest.contains("fs.write"),
            "whitelist bypassed: fs.write advertised"
        );
        assert!(
            !manifest.contains("fs.delete"),
            "whitelist bypassed: fs.delete advertised"
        );
        // Dispatch against the scoped registry must reject un-granted tools.
        let invocations = vec![
            ToolInvocation::new(
                "fs.write",
                serde_json::json!({"path": "/tmp/x", "content": "boom"}),
            )
            .expect("invocation"),
        ];
        let hooks: Vec<&dyn ToolDispatchHook> = Vec::new();
        let result = tokio::runtime::Runtime::new().expect("rt").block_on(
            dispatch_tool_invocations_with_hooks(agent.generic.tools(), &invocations, &hooks),
        );
        assert!(result.is_err(), "un-granted tool dispatched");
    }

    #[test]
    fn test_all_agents_resolve_web_and_fs_tools() {
        const ALL_AGENTS: &[&str] = &[
            "Sisyphus",
            "Junior",
            "Hermes",
            "Metis",
            "Momus",
            "Oracle",
            "Prometheus",
            "Explore",
            "Librarian",
            "Argus",
            "Hephaestus",
            "Atlas",
            "Zeus",
        ];
        for name in ALL_AGENTS {
            let tools = crate::delegate_tools::resolve_tool_ids_for_agent(name);
            let joined = tools.join(",");
            assert!(
                tools.iter().any(|t| t == "web.search"),
                "{name} missing web.search: {joined}"
            );
            assert!(
                tools.iter().any(|t| t == "fs.read"),
                "{name} missing fs.read: {joined}"
            );
        }
    }

    #[test]
    fn test_dispatch_hooks_pipeline_order() {
        let wiring = ZenWiring::new();
        let hooks = wiring.dispatch_hooks();
        assert_eq!(hooks.len(), 5);
    }
}
