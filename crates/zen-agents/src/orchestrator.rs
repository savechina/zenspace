use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use chrono::Utc;
use rig_compose::budget::{AtomicTokenBudget, TokenBudget};
use serde_json::json;
use tracing::info;

use zen_core::types::SessionContext;
use zen_memory::ZenMemvidStore;
use zen_provider::DefaultRouter;

use crate::delegate_tools::ZenDelegateTools;
use crate::delegate_tools as delegate_tools;
use crate::execution::{AgentExecution, ExecutionMetadata};
use crate::review::QualityPipeline;
use crate::wiring::ZenWiring;
use crate::zen_agent::ZenAgent;
use zen_core::paths::ZenPaths;

/// T314-T316: Slim orchestrator — registry field removed (T314),
/// router replaced by executor (T305).
pub struct AgentOrchestrator {
    wiring: ZenWiring,
    delegates: ZenDelegateTools,
    executor: crate::executor::AgentExecutor,
    token_budget: Arc<AtomicTokenBudget>,
    memvid_store: Option<rig_memvid::MemvidStore>,
    quality_pipeline: QualityPipeline,
}

impl AgentOrchestrator {
    pub fn new(router: DefaultRouter) -> Self {
        let wiring = ZenWiring::new();
        let delegates = ZenDelegateTools::new(&wiring, &router);
        let executor = crate::executor::AgentExecutor::new(router.clone());
        let token_budget = Arc::new(AtomicTokenBudget::new(100_000));
        Self {
            wiring,
            delegates,
            executor,
            token_budget,
            memvid_store: None,
            quality_pipeline: QualityPipeline::new(),
        }
    }

    pub fn with_token_budget(router: DefaultRouter, capacity: u64) -> Self {
        let wiring = ZenWiring::new();
        let delegates = ZenDelegateTools::new(&wiring, &router);
        let executor = crate::executor::AgentExecutor::new(router.clone());
        let token_budget = Arc::new(AtomicTokenBudget::new(capacity));
        Self {
            wiring,
            delegates,
            executor,
            token_budget,
            memvid_store: None,
            quality_pipeline: QualityPipeline::new(),
        }
    }

    pub fn with_memory(mut self, memory_path: PathBuf) -> Result<Self> {
        let store = ZenMemvidStore::new(memory_path)?;
        self.memvid_store = Some(store.into_inner());
        Ok(self)
    }

    pub fn budget_available(&self) -> u64 {
        self.token_budget.available()
    }

    pub fn budget_consumed(&self) -> u64 {
        self.token_budget.capacity().saturating_sub(self.token_budget.available())
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

        if lower.contains("implement") || lower.contains("code") || lower.contains("function")
            || lower.contains("class") || lower.contains("refactor") || lower.contains("debug")
        {
            return "Hephaestus".to_string();
        }

        if lower.contains("research") || lower.contains("explore") || lower.contains("discover")
            || lower.contains("find information") || lower.contains("investigate")
        {
            return "Explore".to_string();
        }

        if lower.contains("analyze") || lower.contains("analysis") || lower.contains("deep")
            || lower.contains("architecture") || lower.contains("design")
        {
            return "Oracle".to_string();
        }

        if lower.contains("organize") || lower.contains("knowledge") || lower.contains("wiki")
            || lower.contains("notes") || lower.contains("catalog") || lower.contains("dedup")
        {
            return "Librarian".to_string();
        }

        if lower.contains("consolidate") || lower.contains("pipeline") || lower.contains("merge")
            || lower.contains("compile wiki")
        {
            return "Hermes".to_string();
        }

        if lower.contains("review") || lower.contains("audit") || lower.contains("check quality")
            || lower.contains("security")
        {
            return "Momus".to_string();
        }

        if lower.contains("plan") || lower.contains("strategy") || lower.contains("roadmap")
            || lower.contains("spec") || lower.contains("milestone")
        {
            return "Prometheus".to_string();
        }

        if lower.contains("gap") || lower.contains("tactical") || lower.contains("assumption")
            || lower.contains("feasibility")
        {
            return "Metis".to_string();
        }

        if lower.contains("batch") || lower.contains("automate") || lower.contains("routine")
            || lower.contains("schedule")
        {
            return "Atlas".to_string();
        }

        if lower.contains("format") || lower.contains("convert") || lower.contains("download")
            || lower.contains("clean")
        {
            return "Junior".to_string();
        }

        if lower.contains("value") || lower.contains("align") || lower.contains("priority")
            || lower.contains("should we")
        {
            return "Zeus".to_string();
        }

        if lower.contains("image") || lower.contains("chart") || lower.contains("visual")
            || lower.contains("diagram") || lower.contains("screenshot")
        {
            return "Argus".to_string();
        }

        "Sisyphus".to_string()
    }

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

        let estimated_tokens = user_query.len() / 4 + 512;
        let reservation = self.token_budget.try_reserve_tokens(estimated_tokens as u64).await?;
        if reservation.is_none() {
            return Err(anyhow::anyhow!(
                "Token budget exhausted ({} consumed, {} capacity)",
                self.token_budget.tokens_consumed().await,
                self.token_budget.capacity()
            ));
        }
        let reservation = reservation.unwrap();

        let response = zen_agent.execute(user_query, session).await?;

        let actual_tokens = (response.len() / 4 + user_query.len() / 4) as u64;
        self.token_budget.record_usage(reservation, actual_tokens, actual_tokens).await;

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

        let execution = AgentExecution {
            agent_name,
            response,
            metadata: ExecutionMetadata {
                tokens_used: self.budget_consumed() as u32,
                cost_estimate: 0.0,
                model_used: "zen".to_string(),
                duration_ms,
                sensitivity: session.sensitivity_policy,
            },
            tool_calls: Vec::new(),
            sub_agent_results,
        };

        // Append JSONL audit log
        if let Err(e) = append_audit_log(&execution, &lower) {
            tracing::warn!(error = %e, "failed to write audit log");
        }

        Ok(execution)
    }

    /// Execute with backward compatibility — returns String for existing callers.
    pub async fn execute_string(&self, session: &mut SessionContext, user_query: &str) -> Result<String> {
        let execution = self.execute(session, user_query).await?;
        Ok(execution.response)
    }

    /// Stream tokens to a callback while building the response.
    ///
    /// Calls `on_token` for each token chunk as it arrives, then returns the
    /// complete response string.
    pub async fn execute_stream(
        &self,
        session: &mut SessionContext,
        user_query: &str,
        mut on_token: impl FnMut(&str),
    ) -> Result<String> {
        let start = Instant::now();
        let agent_name = self.classify_agent(user_query);
        info!(
            agent = agent_name,
            query_len = user_query.len(),
            "AgentOrchestrator: streaming execution"
        );

        let zen_agent = self.build_agent(&agent_name).await?;

        session.agent_name.clone_from(&agent_name);

        let response = zen_agent.execute_stream(user_query, session, &mut on_token).await?;

        let actual_tokens = (response.len() / 4 + user_query.len() / 4) as u64;
        let reservation = self.token_budget.try_reserve_tokens(actual_tokens).await.ok().flatten();
        if let Some(res) = reservation {
            self.token_budget.record_usage(res, actual_tokens, actual_tokens).await;
        }

        let _duration_ms = start.elapsed().as_millis() as u64;

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

/// Appends a JSONL audit line to ~/.zen/logs/agent-session.jsonl.
/// T297: Replace with rig-tap TelemetryHook integration.
fn append_audit_log(execution: &AgentExecution, query: &str) -> Result<()> {
    let home: PathBuf = std::env::var("HOME")
        .map_err(|_| anyhow::anyhow!("HOME not set"))?
        .into();
    let log_dir = home.join(".zen").join("logs");
    std::fs::create_dir_all(&log_dir)?;

    let log_file = log_dir.join("agent-session.jsonl");
    let line = json!({
        "timestamp": Utc::now().to_rfc3339(),
        "agent": execution.agent_name,
        "query_len": query.len(),
        "response_len": execution.response.len(),
        "duration_ms": execution.metadata.duration_ms,
        "sensitivity": execution.metadata.sensitivity.to_string(),
    });

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)?;
    use std::io::Write;
    writeln!(file, "{line}")?;
    Ok(())
}
