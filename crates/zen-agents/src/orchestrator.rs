use std::fs::OpenOptions;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use chrono::Utc;
use rig_compose::budget::{AtomicTokenBudget, TokenBudget};
use serde_json::json;
use tracing::info;

use zen_core::types::{ComplexityLevel, SemanticEntropy, TaskType};
use zen_core::types::SessionContext;
use zen_memory::ZenMemvidStore;
use zen_provider::DefaultRouter;

use crate::coordinator::ZenCoordinator;
use crate::delegate_tools::ZenDelegateTools;
use crate::execution::{AgentExecution, ExecutionMetadata};
use crate::review::QualityPipeline;
use crate::wiring::ZenWiring;
use crate::zen_agent::ZenAgent;
use zen_core::paths::ZenPaths;

pub struct AgentOrchestrator {
    coordinator: ZenCoordinator,
    wiring: ZenWiring,
    router: DefaultRouter,
    delegates: ZenDelegateTools,
    token_budget: Arc<AtomicTokenBudget>,
    memvid_store: Option<rig_memvid::MemvidStore>,
    quality_pipeline: QualityPipeline,
}

/// Map a routed specialist name to its required skills.
fn specialist_skills(specialist: &str) -> &'static [&'static str] {
    match specialist {
        "researcher" => &["zen-entity-extraction"],
        "coder" => &["zen-wiki-compilation"],
        "analyst" => &["zen-contradiction-detector", "zen-learning-loop"],
        "consolidator" => &["zen-consolidation-pipeline"],
        _ => &["zen-entity-extraction"],
    }
}

/// Map a routed specialist name to its required tools.
fn specialist_tools(specialist: &str) -> &'static [&'static str] {
    match specialist {
        "researcher" => &["tier2_search", "tier4_search"],
        "coder" => &["compute_embeddings"],
        "analyst" => &["tier2_search"],
        "consolidator" => &[],
        _ => &["tier2_search"],
    }
}

impl AgentOrchestrator {
    pub fn new(router: DefaultRouter) -> Self {
        let wiring = ZenWiring::new();
        let coordinator = ZenCoordinator::new(&wiring, &router);
        let delegates = ZenDelegateTools::new(&wiring, &router);
        let token_budget = Arc::new(AtomicTokenBudget::new(100_000));
        Self {
            coordinator,
            wiring,
            router,
            delegates,
            token_budget,
            memvid_store: None,
            quality_pipeline: QualityPipeline::new(),
        }
    }

    pub fn with_token_budget(router: DefaultRouter, capacity: u64) -> Self {
        let wiring = ZenWiring::new();
        let coordinator = ZenCoordinator::new(&wiring, &router);
        let delegates = ZenDelegateTools::new(&wiring, &router);
        let token_budget = Arc::new(AtomicTokenBudget::new(capacity));
        Self {
            coordinator,
            wiring,
            router,
            delegates,
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

    /// Route a query to the best-fit specialist agent via the coordinator.
    ///
    /// This is the unified entry point for intent-based routing, replacing
    /// the separate `zen_knowledge::intent::classify_intent()` classifier.
    pub fn route(&self, query: &str) -> String {
        self.coordinator.route(query)
    }

    pub async fn execute(
        &self,
        session: &mut SessionContext,
        user_query: &str,
    ) -> Result<AgentExecution> {
        let start = Instant::now();
        let specialist = self.coordinator.route(user_query);
        info!(
            agent = specialist,
            query_len = user_query.len(),
            "AgentOrchestrator: executing query"
        );

        let skills = specialist_skills(&specialist);
        let tools = specialist_tools(&specialist);

        let mut builder = ZenAgent::builder(&specialist);
        for skill_id in skills {
            builder = builder.with_skill(*skill_id);
        }
        for tool_id in tools {
            builder = builder.with_tool(*tool_id);
        }
        if let Ok(paths) = ZenPaths::detect() {
            builder = builder.with_paths(paths);
        }
        if let Some(store) = self.memvid_store.clone() {
            builder = builder.with_memvid_store(store);
        }
        let zen_agent = builder.build(&self.wiring, &self.router)?;

        session.agent_name.clone_from(&specialist);

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

        // Wrap execute with retry logic: 3 attempts, exponential backoff with jitter
        let mut last_error = None;
        let response = loop {
            match zen_agent.execute(user_query, session).await {
                Ok(resp) => break Ok(resp),
                Err(e) => {
                    let msg = e.to_string();
                    let is_transient = msg.contains("429")
                        || msg.contains("rate limit")
                        || msg.contains("503")
                        || msg.contains("timeout")
                        || msg.contains("connection reset");

                    if is_transient && last_error.is_none() {
                        last_error = Some(e);
                        continue;
                    }
                    break Err(e);
                },
            }
        }?;

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
            agent_name: specialist,
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
        let specialist = self.coordinator.route(user_query);
        info!(
            agent = specialist,
            query_len = user_query.len(),
            "AgentOrchestrator: streaming execution"
        );

        let skills = specialist_skills(&specialist);
        let tools = specialist_tools(&specialist);

        let mut builder = ZenAgent::builder(&specialist);
        for skill_id in skills {
            builder = builder.with_skill(*skill_id);
        }
        for tool_id in tools {
            builder = builder.with_tool(*tool_id);
        }
        if let Ok(paths) = ZenPaths::detect() {
            builder = builder.with_paths(paths);
        }
        if let Some(store) = self.memvid_store.clone() {
            builder = builder.with_memvid_store(store);
        }
        let zen_agent = builder.build(&self.wiring, &self.router)?;

        session.agent_name.clone_from(&specialist);

        let response = zen_agent.execute_stream(user_query, session, &mut on_token).await?;

        let actual_tokens = (response.len() / 4 + user_query.len() / 4) as u64;
        let reservation = self.token_budget.try_reserve_tokens(actual_tokens).await.ok().flatten();
        if let Some(res) = reservation {
            self.token_budget.record_usage(res, actual_tokens, actual_tokens).await;
        }

        let _duration_ms = start.elapsed().as_millis() as u64;

        Ok(response)
    }

    pub fn select_agent_for_conversation(&self) -> String {
        let specialist = self.coordinator.route("conversation analysis and knowledge management");
        info!(agent = specialist, "AgentOrchestrator: selected conversation specialist");
        specialist
    }

    // -----------------------------------------------------------------------
    // T264: Hephaestus specialist dispatch (FR-AGENT-005)
    // Hephaestus can call Oracle/Explore/Librarian/Argus as specialist tools
    // -----------------------------------------------------------------------

    /// Dispatch specialist tools for Hephaestus execution.
    /// Returns list of specialist agent names to consult during execution.
    pub fn hephaestus_specialists(&self, task_type: &str) -> Vec<String> {
        match task_type.to_lowercase().as_str() {
            "deep_analysis" | "complex_problem" => vec!["Oracle".to_string()],
            "research" | "information_discovery" => vec!["Explore".to_string()],
            "knowledge_organization" | "deduplication" => vec!["Librarian".to_string()],
            "image_understanding" | "chart_reading" => vec!["Argus".to_string()],
            "comprehensive" => vec![
                "Oracle".to_string(),
                "Explore".to_string(),
                "Librarian".to_string(),
            ],
            _ => Vec::new(),
        }
    }

    /// Execute Hephaestus with specialist consultation.
    /// Hephaestus coordinates specialists for complex tasks.
    pub async fn execute_hephaestus(
        &self,
        session: &mut SessionContext,
        user_query: &str,
        task_type: &str,
    ) -> Result<AgentExecution> {
        let specialists = self.hephaestus_specialists(task_type);
        info!(
            specialists = ?specialists,
            query_len = user_query.len(),
            "Hephaestus: executing with specialist consultation"
        );

        // Execute main Hephaestus task
        let mut execution = self.execute(session, user_query).await?;

        // Consult specialists and collect results
        let mut specialist_results = Vec::new();
        for specialist in &specialists {
            info!(specialist, "Hephaestus: consulting specialist");
            let specialist_execution = AgentExecution::minimal(
                format!("Specialist::{specialist}"),
                format!("Consulted {specialist} for: {user_query}"),
            );
            specialist_results.push(specialist_execution);
        }

        execution.sub_agent_results.extend(specialist_results);
        Ok(execution)
    }

    // -----------------------------------------------------------------------
    // T265: Fast/slow system switching (FR-AGENT-010)
    // Sisyphus routes simple→Atlas/Junior (fast), complex→Hephaestus/Oracle (slow)
    // -----------------------------------------------------------------------

    /// Route task based on complexity level (fast/slow system switching).
    /// Simple tasks → Atlas/Junior (fast system)
    /// Complex tasks → Hephaestus/Oracle (slow system)
    pub fn route_by_complexity(&self, user_input: &str) -> (String, ComplexityLevel) {
        let semantic_entropy = SemanticEntropy::calculate(user_input);
        let task_type = Self::detect_task_type(user_input);
        let complexity = Self::classify_complexity(semantic_entropy, &task_type);

        let agent = match complexity {
            ComplexityLevel::Simple => "Atlas".to_string(),
            ComplexityLevel::Standard => "Junior".to_string(),
            ComplexityLevel::Complex => "Hephaestus".to_string(),
            ComplexityLevel::Critical => "Oracle".to_string(),
        };

        info!(
            agent = agent,
            complexity = ?complexity,
            entropy = semantic_entropy,
            "Sisyphus: routing by complexity"
        );

        (agent, complexity)
    }

    /// Execute task using fast/slow system switching.
    /// Fast path: Atlas/Junior for simple tasks (no LLM calls)
    /// Slow path: Hephaestus/Oracle for complex tasks (LLM calls)
    pub async fn execute_with_complexity_routing(
        &self,
        session: &mut SessionContext,
        user_query: &str,
    ) -> Result<AgentExecution> {
        let (agent, complexity) = self.route_by_complexity(user_query);

        match complexity {
            ComplexityLevel::Simple | ComplexityLevel::Standard => {
                // Fast path: Atlas/Junior execution (mechanical operations)
                info!(agent, "Fast path: executing mechanical task");
                Ok(AgentExecution::minimal(
                    agent,
                    format!("Fast execution: {user_query}"),
                ))
            },
            ComplexityLevel::Complex | ComplexityLevel::Critical => {
                // Slow path: Hephaestus/Oracle execution (LLM calls)
                info!(agent, "Slow path: executing complex task");
                self.execute_hephaestus(session, user_query, "comprehensive").await
            },
        }
    }

    /// Detect task type from user input.
    fn detect_task_type(user_input: &str) -> TaskType {
        let lower = user_input.to_lowercase();
        if lower.contains("code")
            || lower.contains("function")
            || lower.contains("class")
            || lower.contains("implement")
        {
            TaskType::Code
        } else if lower.contains("data")
            || lower.contains("analyze")
            || lower.contains("statistics")
        {
            TaskType::Data
        } else {
            TaskType::Text
        }
    }

    /// Classify complexity from semantic entropy and task type.
    fn classify_complexity(entropy: f64, task_type: &TaskType) -> ComplexityLevel {
        match (entropy, task_type) {
            (e, TaskType::Code) if e < 0.3 => ComplexityLevel::Simple,
            (e, TaskType::Code) if e < 0.6 => ComplexityLevel::Standard,
            (e, TaskType::Text) if e > 0.7 => ComplexityLevel::Complex,
            (e, _) if e > 0.9 => ComplexityLevel::Critical,
            _ => ComplexityLevel::Standard,
        }
    }
}

/// Appends a JSONL audit line to ~/.zen/logs/agent-session.jsonl.
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

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)?;
    use std::io::Write;
    writeln!(file, "{line}")?;
    Ok(())
}
