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
//! | knowledge-query     | researcher     | zen-entity-extraction         |
//! | wiki-compile        | coder          | zen-wiki-compilation           |
//! | analysis            | analyst        | zen-contradiction-detection,   |
//! |                     |                |   zen-knowledge-learning-loop  |
//! | consolidate        | consolidator   | zen-consolidation-pipeline     |
//! | conversation       | conversation   | zen-conversational-assistant   |
//! | system             | system         | zen-system-operations           |
//!

use std::sync::Arc;

use rig_compose::agent::GenericAgent;
use rig_compose::coordinator::{CoordinatorAgent, RoutingRule};
use rig_compose::context::InvestigationContext;
use tracing::info;

use zen_provider::DefaultRouter;

pub use crate::wiring::ZenWiring;

// ---------------------------------------------------------------------------
// ZenCoordinator
// ---------------------------------------------------------------------------

/// Signal-based, deterministic agent router wrapping rig-compose's
/// `CoordinatorAgent`.
///
/// Construction creates four specialist `GenericAgent` instances with
/// distinct skill chains and tool whitelists. At runtime, `route()`
/// classifies the query into a signal tag and delegates to the
/// coordinator's first-match rule.
pub struct ZenCoordinator {
    coordinator: CoordinatorAgent,
    /// Fallback specialist name used when `route()` returns `None`.
    fallback: String,
}

impl ZenCoordinator {
    /// Build a `ZenCoordinator` from the given wiring configuration.
    ///
    /// The `router` parameter is currently stored for future use (e.g.
    /// embedding-driven intent classification in Phase 4).
    ///
    /// # Panics
    ///
    /// Panics if the coordinator builder cannot construct the routing
    /// topology (should never happen with the hardcoded specialist names).
    #[must_use]
    pub fn new(wiring: &ZenWiring, _router: &DefaultRouter) -> Self {
        let fallback = "researcher".to_string();

        // --- Build specialist agents -----------------------------------

        let researcher = GenericAgent::builder("researcher")
            .with_skills(["zen-entity-extraction"])
            .with_tools(["tier2_search", "tier4_search"])
            .build(&wiring.skills, &wiring.tools)
            .expect("researcher build");

        let coder = GenericAgent::builder("coder")
            .with_skills(["zen-wiki-compilation"])
            .with_tools(["compute_embeddings"])
            .build(&wiring.skills, &wiring.tools)
            .expect("coder build");

        let analyst = GenericAgent::builder("analyst")
            .with_skills(["zen-contradiction-detection", "zen-knowledge-learning-loop"])
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

        // --- Build routing rules ---------------------------------------

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
                signals: vec![
                    "code-generation".to_string(),
                    "wiki-compile".to_string(),
                ],
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
                signals: vec![
                    "consolidate".to_string(),
                    "pipeline".to_string(),
                ],
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
            fallback,
            "ZenCoordinator initialised"
        );

        Self {
            coordinator,
            fallback,
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

        let ctx = InvestigationContext::new("zen-coordination", "default")
            .with_signal(&signal);

        match self.coordinator.route(&ctx) {
            Some(agent) => agent.name().to_string(),
            None => {
                info!(
                    signal,
                    fallback = self.fallback,
                    "No specialist matched signal, using fallback"
                );
                self.fallback.clone()
            },
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

        if lower.contains("analyze")
            || lower.contains("check")
            || lower.contains("detect")
        {
            return "analysis".to_string();
        }

        if lower.contains("compile")
            || lower.contains("wiki")
            || lower.contains("create")
        {
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

        if lower.contains("search")
            || lower.contains("find")
            || lower.contains("query")
        {
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
            "zen-entity-extraction",
            "zen-wiki-compilation",
            "zen-contradiction-detection",
            "zen-knowledge-learning-loop",
            "zen-consolidation-pipeline",
        ] {
            skills.register(Arc::new(NoopSkill(id)));
        }
        ZenWiring {
            skills,
            tools: ToolRegistry::new(),
            delegates: rig_compose::delegate::DelegateRegistry::new(),
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

        assert_eq!(
            coordinator.route("SEARCH for documents"),
            "researcher"
        );
        assert_eq!(
            coordinator.route("CONSOLIDATE notes"),
            "consolidator"
        );
        assert_eq!(
            coordinator.route("Wiki Compile me"),
            "coder"
        );
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
