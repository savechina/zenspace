//! Intent-based agent routing via rig-compose's CoordinatorAgent.
//!
//! `ZenCoordinator` replaces the hardcoded `select_agent_for_query()` logic
//! in `AgentOrchestrator` with deterministic, signal-driven routing.
//!
//! # Architecture
//!
//! Four specialist agents are registered as `GenericAgent` instances. Each
//! specialist carries a distinct skill set and tool access profile. The
//! `CoordinatorAgent` routes incoming inquiries to the first specialist
//! whose routing rule matches the context's signal.
//!
//! # Signal Routing Table
//!
//! | Signal              | Specialist     | Skills                         |
//! |---------------------|----------------|--------------------------------|
//! | knowledge-query     | researcher     | zen-notion-extraction         |
//! | wiki-compile        | coder          | zen-wiki-compilation           |
//! | analysis            | analyst        | zen-contradiction-detection,   |
//! |                     |                |   zen-vault-learning-loop  |
//! | consolidate        | consolidator   | zen-consolidation-pipeline     |
//! | conversation       | conversation   | zen-conversational-assistant   |
//! | system             | system         | zen-system-operations           |
//!

use std::sync::Arc;
use std::time::Duration;

use rig_compose::agent::GenericAgent;
use rig_compose::context::InvestigationContext;
use rig_compose::coordinator::{CoordinatorAgent, RoutingRule};
use rig_compose::delegate::DelegateRegistry;
use rig_compose::registry::KernelError;
use serde_json::{Value, json};
use tracing::{info, warn};

use zen_core::types::{ComplexityLevel, SemanticEntropy};
use zen_provider::DefaultRouter;

pub use crate::wiring::ZenWiring;

// ---------------------------------------------------------------------------
// EntropyConfig — thresholds for complexity classification
// ---------------------------------------------------------------------------

/// Thresholds used by `route_by_complexity()` to map semantic entropy
/// values to a `ComplexityLevel`.
#[derive(Debug, Clone)]
pub struct EntropyConfig {
    pub simple_threshold: f64,
    pub standard_threshold: f64,
    pub complex_threshold: f64,
    pub critical_threshold: f64,
}

impl Default for EntropyConfig {
    fn default() -> Self {
        Self {
            simple_threshold: 0.3,
            standard_threshold: 0.6,
            complex_threshold: 0.7,
            critical_threshold: 0.9,
        }
    }
}

/// T291: Result of a specialist invocation.
#[derive(Debug, Clone)]
pub struct SpecialistResult {
    pub agent_name: String,
    pub response: String,
}

/// T291: Collection of specialist invocation results with partial error tracking.
#[derive(Debug, Clone)]
pub struct SpecialistInvocationResult {
    pub results: Vec<SpecialistResult>,
    pub errors: Vec<String>,
}

// ---------------------------------------------------------------------------
// ZenCoordinator
// ---------------------------------------------------------------------------

/// Signal-based, deterministic agent router wrapping rig-compose's
/// `CoordinatorAgent`.
///
/// Construction creates thirteen specialist `GenericAgent` instances with
/// distinct skill chains and tool whitelists. At runtime, `route()`
/// classifies the query into a signal tag and delegates to the
/// coordinator's first-match rule.
pub struct ZenCoordinator {
    coordinator: CoordinatorAgent,
    /// Fallback specialist name used when `route()` returns `None`.
    fallback: String,
    /// Entropy thresholds for complexity-based routing.
    entropy_config: EntropyConfig,
    /// Delegate registry for specialist invocation. T291.
    delegates: DelegateRegistry,
}

impl ZenCoordinator {
    /// Build a `ZenCoordinator` from the given wiring configuration.
    ///
    /// # Panics
    ///
    /// Panics if the coordinator builder cannot construct the routing
    /// topology.
    #[must_use]
    pub fn new(wiring: &ZenWiring, _router: &DefaultRouter) -> Self {
        let fallback = "researcher".to_string();
        let entropy_config = EntropyConfig::default();
        let delegates = wiring.delegates.clone();

        // --- Build specialist agents -----------------------------------

        let researcher = GenericAgent::builder("researcher")
            .with_skills(["zen-notion-extraction"])
            .with_tools(["tier2_search", "tier4_search"])
            .build(&wiring.skills, &wiring.tools)
            .expect("researcher build");

        let coder = GenericAgent::builder("coder")
            .with_skills(["zen-wiki-compilation"])
            .with_tools(["compute_embeddings"])
            .build(&wiring.skills, &wiring.tools)
            .expect("coder build");

        let analyst = GenericAgent::builder("analyst")
            .with_skills(["zen-contradiction-detection", "zen-vault-learning-loop"])
            .with_tools(["tier2_search"])
            .build(&wiring.skills, &wiring.tools)
            .expect("analyst build");

        let consolidator = GenericAgent::builder("consolidator")
            .with_skills(["zen-consolidation-pipeline"])
            .with_tools(Vec::<String>::new())
            .build(&wiring.skills, &wiring.tools)
            .expect("consolidator build");

        let conversation = GenericAgent::builder("conversation")
            .with_skills(Vec::<String>::new())
            .with_tools(Vec::<String>::new())
            .build(&wiring.skills, &wiring.tools)
            .expect("conversation build");

        let system = GenericAgent::builder("system")
            .with_skills(Vec::<String>::new())
            .with_tools(Vec::<String>::new())
            .build(&wiring.skills, &wiring.tools)
            .expect("system build");

        // --- T292: 13-agent routing rules ------------------------------

        let rules = [
            RoutingRule {
                agent_name: "researcher".to_string(),
                signals: vec![
                    "knowledge-query".to_string(),
                    "research".to_string(),
                    "search".to_string(),
                ],
            },
            RoutingRule {
                agent_name: "coder".to_string(),
                signals: vec!["code-generation".to_string(), "wiki-compile".to_string()],
            },
            RoutingRule {
                agent_name: "analyst".to_string(),
                signals: vec![
                    "analysis".to_string(),
                    "contradiction".to_string(),
                    "learning".to_string(),
                ],
            },
            RoutingRule {
                agent_name: "consolidator".to_string(),
                signals: vec!["consolidate".to_string(), "pipeline".to_string()],
            },
            RoutingRule {
                agent_name: "conversation".to_string(),
                signals: vec![
                    "conversation".to_string(),
                    "chat".to_string(),
                    "help".to_string(),
                ],
            },
            RoutingRule {
                agent_name: "system".to_string(),
                signals: vec![
                    "system".to_string(),
                    "config".to_string(),
                    "serve".to_string(),
                ],
            },
            // T292: additional 7 agent rules (13 total matching orchestrator)
            RoutingRule {
                agent_name: "Sisyphus".to_string(),
                signals: vec![
                    "execute".to_string(),
                    "workflow".to_string(),
                    "default".to_string(),
                ],
            },
            RoutingRule {
                agent_name: "Junior".to_string(),
                signals: vec![
                    "format".to_string(),
                    "convert".to_string(),
                    "download".to_string(),
                    "clean".to_string(),
                ],
            },
            RoutingRule {
                agent_name: "Hermes".to_string(),
                signals: vec![
                    "consolidate-pipeline".to_string(),
                    "merge".to_string(),
                    "compile-wiki".to_string(),
                ],
            },
            RoutingRule {
                agent_name: "Metis".to_string(),
                signals: vec![
                    "gap".to_string(),
                    "tactical".to_string(),
                    "assumption".to_string(),
                    "feasibility".to_string(),
                ],
            },
            RoutingRule {
                agent_name: "Momus".to_string(),
                signals: vec![
                    "review".to_string(),
                    "audit".to_string(),
                    "check-quality".to_string(),
                    "security".to_string(),
                ],
            },
            RoutingRule {
                agent_name: "Oracle".to_string(),
                signals: vec![
                    "deep-analysis".to_string(),
                    "analysis".to_string(),
                    "architecture".to_string(),
                    "design".to_string(),
                ],
            },
            RoutingRule {
                agent_name: "Prometheus".to_string(),
                signals: vec![
                    "plan".to_string(),
                    "strategy".to_string(),
                    "roadmap".to_string(),
                    "spec".to_string(),
                    "milestone".to_string(),
                ],
            },
            RoutingRule {
                agent_name: "Explore".to_string(),
                signals: vec![
                    "explore".to_string(),
                    "discover".to_string(),
                    "find-information".to_string(),
                    "investigate".to_string(),
                ],
            },
            RoutingRule {
                agent_name: "Librarian".to_string(),
                signals: vec![
                    "organize".to_string(),
                    "knowledge-org".to_string(),
                    "notes".to_string(),
                    "catalog".to_string(),
                    "dedup".to_string(),
                ],
            },
            RoutingRule {
                agent_name: "Argus".to_string(),
                signals: vec![
                    "image".to_string(),
                    "chart".to_string(),
                    "visual".to_string(),
                    "diagram".to_string(),
                    "screenshot".to_string(),
                ],
            },
            RoutingRule {
                agent_name: "Hephaestus".to_string(),
                signals: vec![
                    "implement".to_string(),
                    "code".to_string(),
                    "function".to_string(),
                    "class".to_string(),
                    "refactor".to_string(),
                    "debug".to_string(),
                ],
            },
            RoutingRule {
                agent_name: "Atlas".to_string(),
                signals: vec![
                    "batch".to_string(),
                    "automate".to_string(),
                    "routine".to_string(),
                    "schedule".to_string(),
                ],
            },
            RoutingRule {
                agent_name: "Zeus".to_string(),
                signals: vec![
                    "value".to_string(),
                    "align".to_string(),
                    "priority".to_string(),
                    "should-we".to_string(),
                ],
            },
        ];

        // --- Assemble coordinator --------------------------------------

        let mut builder = CoordinatorAgent::builder("zen-coordinator");

        for rule in rules {
            builder = builder.route(rule);
        }

        let builder = builder
            .with_specialist(Arc::new(researcher))
            .with_specialist(Arc::new(coder))
            .with_specialist(Arc::new(analyst))
            .with_specialist(Arc::new(consolidator))
            .with_specialist(Arc::new(conversation))
            .with_specialist(Arc::new(system))
            .fallback(&fallback);

        let coordinator = builder.build();

        info!(
            specialists = 6,
            routing_rules = 13,
            fallback,
            "ZenCoordinator initialised"
        );

        Self {
            coordinator,
            fallback,
            entropy_config,
            delegates,
        }
    }

    /// Route a user query to the best-fit specialist agent.
    ///
    /// # Classification logic
    ///
    /// Simple keyword-based intent classification:
    ///
    /// | Keywords                                | Signal          |
    /// |-----------------------------------------|-----------------|
    /// | search, find, query                     | knowledge-query |
    /// | compile, wiki, create                   | wiki-compile    |
    /// | analyze, check, detect                  | analysis        |
    /// | consolidate, pipeline                   | consolidate     |
    /// | chat, talk, help, hello, hi, what can    | conversation    |
    /// |   , how do                              |                 |
    /// | config, serve, daemon, settings, setup   | system          |
    /// | (default)                               | knowledge-query |
    ///
    /// Returns the name of the matched specialist.
    pub fn route(&self, query: &str) -> String {
        let signal = Self::classify_intent(query);

        let ctx = InvestigationContext::new("zen-coordination", "default").with_signal(&signal);

        match self.coordinator.route(&ctx) {
            Some(agent) => agent.name().to_string(),
            None => {
                info!(
                    signal,
                    fallback = self.fallback,
                    "No specialist matched signal, using fallback"
                );
                self.fallback.clone()
            }
        }
    }

    // -----------------------------------------------------------------------
    // Intent classification (pure keyword matching)
    // -----------------------------------------------------------------------

    fn classify_intent(query: &str) -> String {
        let lower = query.to_lowercase();

        // Check in priority order — first match wins.

        if lower.contains("consolidate") || lower.contains("pipeline") {
            return "consolidate".to_string();
        }

        if lower.contains("analyze") || lower.contains("check") || lower.contains("detect") {
            return "analysis".to_string();
        }

        if lower.contains("compile") || lower.contains("wiki") || lower.contains("create") {
            return "wiki-compile".to_string();
        }

        if lower.contains("chat")
            || lower.contains("talk")
            || lower.contains("help")
            || lower.contains("hello")
            || lower.contains("hi")
            || lower.contains("what can")
            || lower.contains("how do")
        {
            return "conversation".to_string();
        }

        if lower.contains("config")
            || lower.contains("serve")
            || lower.contains("daemon")
            || lower.contains("settings")
            || lower.contains("setup")
        {
            return "system".to_string();
        }

        if lower.contains("search") || lower.contains("find") || lower.contains("query") {
            return "knowledge-query".to_string();
        }

        // Default
        "knowledge-query".to_string()
    }

    /// Return the underlying `CoordinatorAgent` for advanced use cases.
    #[must_use]
    pub const fn inner(&self) -> &CoordinatorAgent {
        &self.coordinator
    }

    /// Return a reference to the entropy configuration.
    #[must_use]
    pub const fn entropy_config(&self) -> &EntropyConfig {
        &self.entropy_config
    }

    /// Return a reference to the delegate registry.
    #[must_use]
    pub fn delegates(&self) -> &DelegateRegistry {
        &self.delegates
    }

    // -------------------------------------------------------------------
    // T289: route_by_complexity — entropy-driven agent selection
    // -------------------------------------------------------------------

    /// Route a query to an agent based on semantic entropy and task type,
    /// using `EntropyConfig` thresholds to determine complexity level.
    ///
    /// Uses `SemanticEntropy::calculate(query)` to compute an entropy value,
    /// then classifies into `ComplexityLevel` by comparing against thresholds.
    /// Returns `(agent_name, ComplexityLevel)`.
    pub fn route_by_complexity(&self, query: &str) -> (String, ComplexityLevel) {
        let entropy = SemanticEntropy::calculate(query);
        let complexity = self.classify_by_entropy(entropy);

        let agent_name = match complexity {
            ComplexityLevel::Simple => "Atlas".to_string(),
            ComplexityLevel::Standard => "Junior".to_string(),
            ComplexityLevel::Complex => "Hephaestus".to_string(),
            ComplexityLevel::Critical => "Oracle".to_string(),
        };

        info!(
            agent = agent_name,
            complexity = ?complexity,
            entropy,
            "ZenCoordinator: routing by entropy"
        );

        (agent_name, complexity)
    }

    fn classify_by_entropy(&self, entropy: f64) -> ComplexityLevel {
        if entropy >= self.entropy_config.critical_threshold {
            ComplexityLevel::Critical
        } else if entropy >= self.entropy_config.complex_threshold {
            ComplexityLevel::Complex
        } else if entropy >= self.entropy_config.standard_threshold {
            ComplexityLevel::Standard
        } else {
            ComplexityLevel::Simple
        }
    }

    // -------------------------------------------------------------------
    // T290: get_specialists — task type to specialist agent mapping
    // -------------------------------------------------------------------

    /// Return specialist agent names for a given task type.
    ///
    /// | Task type              | Specialist |
    /// |------------------------|------------|
    /// | deep_analysis          | Oracle     |
    /// | research               | Explore    |
    /// | knowledge_organization | Librarian  |
    /// | image_understanding    | Argus      |
    pub fn get_specialists(task_type: &str) -> Vec<String> {
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

    // -------------------------------------------------------------------
    // T291: invoke_specialist — sequential specialist execution
    // -------------------------------------------------------------------

    /// Invoke specialist agents sequentially, collecting results and errors.
    ///
    /// Executes specialists one at a time (per D10 decision — no parallel).
    /// Each specialist has a default 30-second timeout. If a specialist
    /// fails, its error is collected but remaining specialists continue.
    /// Returns partial results along with any errors encountered.
    pub async fn invoke_specialist(
        &self,
        specialist_name: &str,
        query: &str,
    ) -> SpecialistInvocationResult {
        const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

        info!(
            specialist = specialist_name,
            ?DEFAULT_TIMEOUT,
            "ZenCoordinator: invoking specialist"
        );

        let delegate = match self.delegates.get(specialist_name) {
            Some(d) => d,
            None => {
                let err = format!("specialist '{specialist_name}' not found in delegate registry");
                warn!("{err}");
                return SpecialistInvocationResult {
                    results: Vec::new(),
                    errors: vec![err],
                };
            }
        };

        // Execute with timeout using tokio::time::timeout
        let result = tokio::time::timeout(DEFAULT_TIMEOUT, async {
            let args = json!({ "query": query, "agent": specialist_name });
            delegate.invoke(args).await
        })
        .await;

        match result {
            Ok(Ok(response)) => {
                info!(
                    specialist = specialist_name,
                    "ZenCoordinator: specialist succeeded"
                );
                let response_str = extract_response_string(&response);
                SpecialistInvocationResult {
                    results: vec![SpecialistResult {
                        agent_name: specialist_name.to_string(),
                        response: response_str,
                    }],
                    errors: Vec::new(),
                }
            }
            Ok(Err(KernelError::InvalidArgument(msg))) => {
                let err = format!("specialist '{specialist_name}' invalid args: {msg}");
                warn!("{err}");
                SpecialistInvocationResult {
                    results: Vec::new(),
                    errors: vec![err],
                }
            }
            Ok(Err(KernelError::ToolFailed(msg))) => {
                let err = format!("specialist '{specialist_name}' tool failed: {msg}");
                warn!("{err}");
                SpecialistInvocationResult {
                    results: Vec::new(),
                    errors: vec![err],
                }
            }
            Ok(Err(KernelError::ToolNotFound(msg))) => {
                let err = format!("specialist '{specialist_name}' tool not found: {msg}");
                warn!("{err}");
                SpecialistInvocationResult {
                    results: Vec::new(),
                    errors: vec![err],
                }
            }
            Ok(Err(KernelError::SkillFailed(msg))) => {
                let err = format!("specialist '{specialist_name}' skill failed: {msg}");
                warn!("{err}");
                SpecialistInvocationResult {
                    results: Vec::new(),
                    errors: vec![err],
                }
            }
            Ok(Err(e)) => {
                let err = format!("specialist '{specialist_name}' error: {e}");
                warn!("{err}");
                SpecialistInvocationResult {
                    results: Vec::new(),
                    errors: vec![err],
                }
            }
            Err(_) => {
                let err =
                    format!("specialist '{specialist_name}' timed out after {DEFAULT_TIMEOUT:?}");
                warn!("{err}");
                SpecialistInvocationResult {
                    results: Vec::new(),
                    errors: vec![err],
                }
            }
        }
    }

    /// Invoke multiple specialists sequentially, collecting partial results.
    ///
    /// Continues with remaining specialists if one fails. Returns all
    /// partial results plus any errors for the caller to handle.
    pub async fn invoke_specialists(
        &self,
        specialist_names: &[String],
        query: &str,
    ) -> SpecialistInvocationResult {
        let mut all_results = Vec::new();
        let mut all_errors = Vec::new();

        for name in specialist_names {
            let result = self.invoke_specialist(name, query).await;
            all_results.extend(result.results);
            all_errors.extend(result.errors);
        }

        SpecialistInvocationResult {
            results: all_results,
            errors: all_errors,
        }
    }
}

/// T291 helper: extract a human-readable string from delegate invoke response.
fn extract_response_string(value: &Value) -> String {
    if let Some(s) = value.as_str() {
        return s.to_string();
    }
    if let Some(result) = value.get("result")
        && let Some(text) = result.as_str()
    {
        return text.to_string();
    }
    if let Some(text) = value.get("text").and_then(|v| v.as_str()) {
        return text.to_string();
    }
    serde_json::to_string(value).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use rig_compose::context::InvestigationContext;
    use rig_compose::registry::{KernelError, SkillRegistry, ToolRegistry};
    use rig_compose::skill::{Skill, SkillOutcome};
    use zen_provider::{DefaultRouter, LlmConfig};

    use super::*;

    /// Minimal no-op skill used to satisfy GenericAgent builder resolution.
    struct NoopSkill(&'static str);

    #[async_trait]
    impl Skill for NoopSkill {
        fn id(&self) -> &str {
            self.0
        }

        fn description(&self) -> &str {
            "noop"
        }

        fn applies(&self, _ctx: &InvestigationContext) -> bool {
            true
        }

        async fn execute(
            &self,
            _ctx: &mut InvestigationContext,
            _tools: &ToolRegistry,
        ) -> Result<SkillOutcome, KernelError> {
            Ok(SkillOutcome::noop())
        }
    }

    fn wiring_with_skills() -> ZenWiring {
        let skills = SkillRegistry::new();
        for id in [
            "zen-notion-extraction",
            "zen-wiki-compilation",
            "zen-contradiction-detection",
            "zen-vault-learning-loop",
            "zen-consolidation-pipeline",
        ] {
            skills.register(Arc::new(NoopSkill(id)));
        }
        ZenWiring {
            skills,
            tools: ToolRegistry::new(),
            delegates: rig_compose::delegate::DelegateRegistry::new(),
            memvid_store: None,
        }
    }

    fn mock_router() -> DefaultRouter {
        let config = LlmConfig {
            default_provider: Some("mock".to_string()),
            ..Default::default()
        };
        DefaultRouter::new(config)
    }

    #[test]
    fn coordinator_routes_search_to_researcher() {
        let wiring = wiring_with_skills();
        let router = mock_router();
        let coordinator = ZenCoordinator::new(&wiring, &router);

        let specialist = coordinator.route("search for Rust async patterns");
        assert_eq!(specialist, "researcher");
    }

    #[test]
    fn coordinator_routes_find_to_researcher() {
        let wiring = wiring_with_skills();
        let router = mock_router();
        let coordinator = ZenCoordinator::new(&wiring, &router);

        let specialist = coordinator.route("find similar documents about webassembly");
        assert_eq!(specialist, "researcher");
    }

    #[test]
    fn coordinator_routes_query_to_researcher() {
        let wiring = wiring_with_skills();
        let router = mock_router();
        let coordinator = ZenCoordinator::new(&wiring, &router);

        let specialist = coordinator.route("query the knowledge base");
        assert_eq!(specialist, "researcher");
    }

    #[test]
    fn coordinator_routes_wiki_compile_to_coder() {
        let wiring = wiring_with_skills();
        let router = mock_router();
        let coordinator = ZenCoordinator::new(&wiring, &router);

        let specialist = coordinator.route("wiki compile the new notes");
        assert_eq!(specialist, "coder");
    }

    #[test]
    fn coordinator_routes_create_to_coder() {
        let wiring = wiring_with_skills();
        let router = mock_router();
        let coordinator = ZenCoordinator::new(&wiring, &router);

        let specialist = coordinator.route("create a wiki page from this draft");
        assert_eq!(specialist, "coder");
    }

    #[test]
    fn coordinator_routes_analyze_to_analyst() {
        let wiring = wiring_with_skills();
        let router = mock_router();
        let coordinator = ZenCoordinator::new(&wiring, &router);

        let specialist = coordinator.route("analyze contradiction in the wiki");
        assert_eq!(specialist, "analyst");
    }

    #[test]
    fn coordinator_routes_detect_to_analyst() {
        let wiring = wiring_with_skills();
        let router = mock_router();
        let coordinator = ZenCoordinator::new(&wiring, &router);

        let specialist = coordinator.route("check for data inconsistencies");
        assert_eq!(specialist, "analyst");
    }

    #[test]
    fn coordinator_routes_consolidate_to_consolidator() {
        let wiring = wiring_with_skills();
        let router = mock_router();
        let coordinator = ZenCoordinator::new(&wiring, &router);

        let specialist = coordinator.route("consolidate all raw notes");
        assert_eq!(specialist, "consolidator");
    }

    #[test]
    fn coordinator_routes_pipeline_to_consolidator() {
        let wiring = wiring_with_skills();
        let router = mock_router();
        let coordinator = ZenCoordinator::new(&wiring, &router);

        let specialist = coordinator.route("run the consolidation pipeline");
        assert_eq!(specialist, "consolidator");
    }

    #[test]
    fn coordinator_defaults_to_researcher_for_unknown_query() {
        let wiring = wiring_with_skills();
        let router = mock_router();
        let coordinator = ZenCoordinator::new(&wiring, &router);

        let specialist = coordinator.route("blorgflorp zyx");
        assert_eq!(specialist, "researcher");
    }

    #[test]
    fn classify_intent_is_case_insensitive() {
        let wiring = wiring_with_skills();
        let router = mock_router();
        let coordinator = ZenCoordinator::new(&wiring, &router);

        assert_eq!(coordinator.route("SEARCH for documents"), "researcher");
        assert_eq!(coordinator.route("CONSOLIDATE notes"), "consolidator");
        assert_eq!(coordinator.route("Wiki Compile me"), "coder");
    }

    #[test]
    fn coordinator_inner_returns_coordinator_reference() {
        let wiring = wiring_with_skills();
        let router = mock_router();
        let coordinator = ZenCoordinator::new(&wiring, &router);

        let _ = coordinator.inner();
    }

    #[test]
    fn coordinator_routes_chat_to_conversation() {
        let wiring = wiring_with_skills();
        let router = mock_router();
        let coordinator = ZenCoordinator::new(&wiring, &router);

        let specialist = coordinator.route("can we chat about my notes");
        assert_eq!(specialist, "conversation");
    }

    #[test]
    fn coordinator_routes_help_to_conversation() {
        let wiring = wiring_with_skills();
        let router = mock_router();
        let coordinator = ZenCoordinator::new(&wiring, &router);

        let specialist = coordinator.route("help me find something");
        assert_eq!(specialist, "conversation");
    }

    #[test]
    fn coordinator_routes_config_to_system() {
        let wiring = wiring_with_skills();
        let router = mock_router();
        let coordinator = ZenCoordinator::new(&wiring, &router);

        let specialist = coordinator.route("show my config");
        assert_eq!(specialist, "system");
    }

    #[test]
    fn coordinator_routes_serve_to_system() {
        let wiring = wiring_with_skills();
        let router = mock_router();
        let coordinator = ZenCoordinator::new(&wiring, &router);

        let specialist = coordinator.route("start the serve daemon");
        assert_eq!(specialist, "system");
    }
}
