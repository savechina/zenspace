use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use rig_compose::budget::{AtomicTokenBudget, TokenBudget};
use rig_memvid::MemvidPersistHook;
use tracing::{debug, info, instrument};

use zen_core::types::SessionContext;
use zen_memory::{ZenMemvidStore, create_persist_hook, default_memory_config};
use zen_provider::DefaultRouter;

use crate::delegate_tools;
use crate::delegate_tools::ZenDelegateTools;
use crate::execution::{AgentExecution, ExecutionMetadata};
use crate::registry::AgentRegistry;
use crate::review::QualityPipeline;
use crate::wiring::ZenWiring;
use crate::zen_agent::ZenAgent;
use zen_core::paths::ZenPaths;

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
        let delegates = ZenDelegateTools::new(&wiring, &router);
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
        let delegates = ZenDelegateTools::new(&wiring, &router);
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
        let tools = delegate_tools::resolve_tool_ids_for_agent(agent_name);
        debug!("building agent: {}", delegate_tools::describe_agent(agent_name, &self.registry));

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

        // Execution: AgentContext (routing) + ZenAgent (instance) → Executor
        let execution = self.executor.execute(&context, &zen_agent)?;

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
            tool_calls: execution.tool_calls,
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
    /// Calls `on_token` for each token chunk as it arrives, then returns the
    /// complete response string.
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

        let response = zen_agent
            .execute_stream(user_query, session, &mut on_token)
            .await?;

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

    #[test]
    fn test_select_agent_returns_sisyphus() {
        let config = zen_core::config::LlmConfig::default();
        let router = zen_provider::DefaultRouter::new(config);
        let orchestrator = AgentOrchestrator::new(router);
        assert_eq!(orchestrator.select_agent_for_conversation(), "Sisyphus");
    }
}
