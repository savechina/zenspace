use std::sync::Arc;

use rig_compose::agent::GenericAgent;
use rig_compose::delegate::{DelegateRegistry, InProcessAgentDelegate};
use rig_compose::registry::{SkillRegistry, ToolRegistry};
use tracing::info;

use zen_provider::DefaultRouter;

use crate::registry::{AgentRegistry, DefaultAgentRegistry};
use crate::wiring::ZenWiring;

pub struct ZenDelegateTools {
    pub registry: DelegateRegistry,
}

const AGENT_SKILLS: &[(&str, &[&str])] = &[
    (
        "Sisyphus",
        &[
            "zen-entity-extraction",
            "zen-wiki-compilation",
            "zen-consolidation-pipeline",
        ],
    ),
    ("Junior", &["zen-wiki-compilation"]),
    ("Hermes", &["zen-consolidation-pipeline"]),
    (
        "Metis",
        &["zen-entity-extraction", "zen-contradiction-detection"],
    ),
    ("Momus", &["zen-knowledge-learning-loop"]),
    (
        "Oracle",
        &[
            "zen-entity-extraction",
            "zen-knowledge-learning-loop",
            "zen-contradiction-detection",
        ],
    ),
    (
        "Prometheus",
        &["zen-wiki-compilation", "zen-entity-extraction"],
    ),
    ("Explore", &["zen-knowledge-learning-loop"]),
    (
        "Librarian",
        &["zen-wiki-compilation", "zen-knowledge-learning-loop"],
    ),
    ("Argus", &["zen-knowledge-learning-loop"]),
    (
        "Hephaestus",
        &[
            "zen-entity-extraction",
            "zen-wiki-compilation",
            "zen-consolidation-pipeline",
            "zen-contradiction-detection",
        ],
    ),
    ("Atlas", &["zen-wiki-compilation"]),
    (
        "Zeus",
        &["zen-entity-extraction", "zen-contradiction-detection"],
    ),
];

const AGENT_TOOLS: &[(&str, &[&str])] = &[
    (
        "Sisyphus",
        &["tier2_search", "tier4_search", "compute_embeddings"],
    ),
    ("Junior", &["tier2_search", "compute_embeddings"]),
    ("Hermes", &["tier2_search", "tier4_search"]),
    ("Metis", &["tier2_search"]),
    ("Momus", &["tier2_search", "tier4_search"]),
    ("Oracle", &["tier2_search", "tier4_search"]),
    ("Prometheus", &["compute_embeddings"]),
    ("Explore", &["tier2_search"]),
    ("Librarian", &["tier2_search", "compute_embeddings"]),
    ("Argus", &["tier2_search"]),
    (
        "Hephaestus",
        &["tier2_search", "tier4_search", "compute_embeddings"],
    ),
    ("Atlas", &["tier2_search", "compute_embeddings"]),
    ("Zeus", &["tier2_search", "tier4_search"]),
];

const BUILTIN_AGENT_NAMES: &[&str] = &[
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

impl ZenDelegateTools {
    pub fn new(wiring: &ZenWiring, _router: &DefaultRouter) -> Self {
        let registry = DelegateRegistry::new();

        for &agent_name in BUILTIN_AGENT_NAMES {
            let agent_skills = resolve_skill_ids(agent_name);
            let agent_tools = resolve_tool_ids(agent_name);

            let available_skills = filter_registered_skills(&agent_skills, &wiring.skills);
            let available_tools = filter_registered_tools(&agent_tools, &wiring.tools);

            let agent = GenericAgent::builder(agent_name)
                .with_skills(available_skills.iter().copied())
                .with_tools(available_tools.iter().copied())
                .build(&wiring.skills, &wiring.tools)
                .expect("agent builder should not fail");

            let executor = InProcessAgentDelegate::arc(Arc::new(agent));
            registry.register(agent_name, executor);

            info!(
                agent = agent_name,
                skills = available_skills.len(),
                tools = available_tools.len(),
                "ZenDelegateTools: registered delegate tool"
            );
        }

        Self { registry }
    }
}

fn filter_registered_skills<'a>(ids: &[&'a str], registry: &SkillRegistry) -> Vec<&'a str> {
    ids.iter()
        .copied()
        .filter(|id| registry.get(id).is_ok())
        .collect()
}

fn filter_registered_tools<'a>(ids: &[&'a str], registry: &ToolRegistry) -> Vec<&'a str> {
    ids.iter()
        .copied()
        .filter(|id| registry.get(id).is_ok())
        .collect()
}

/// T310-T313: Public API for skill resolution — single source of truth.
/// Returns the skill IDs for an agent by name.
pub fn resolve_skill_ids_for_agent(agent_name: &str) -> Vec<String> {
    resolve_skill_ids(agent_name)
        .into_iter()
        .map(String::from)
        .collect()
}

/// T310-T313: Public API for tool resolution — single source of truth.
/// Returns the tool IDs for an agent by name.
pub fn resolve_tool_ids_for_agent(agent_name: &str) -> Vec<String> {
    resolve_tool_ids(agent_name)
        .into_iter()
        .map(String::from)
        .collect()
}

fn resolve_skill_ids(agent_name: &str) -> Vec<&str> {
    AGENT_SKILLS
        .iter()
        .find(|(name, _)| *name == agent_name)
        .map(|(_, ids)| *ids)
        .unwrap_or(&[])
        .to_vec()
}

fn resolve_tool_ids(agent_name: &str) -> Vec<&str> {
    AGENT_TOOLS
        .iter()
        .find(|(name, _)| *name == agent_name)
        .map(|(_, ids)| *ids)
        .unwrap_or(&[])
        .to_vec()
}

#[allow(dead_code)]
fn describe_agent(agent_name: &str, registry: &DefaultAgentRegistry) -> String {
    if let Ok(profile) = registry.find_by_name(agent_name) {
        let caps: Vec<String> = profile.capabilities.iter().map(|c| c.to_string()).collect();
        format!(
            "Agent '{}' [role={}]: handles {} tasks",
            profile.name,
            profile.role,
            caps.join(", "),
        )
    } else {
        format!("Delegate tool for agent '{}'", agent_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wiring::ZenWiring as _ZenWiring;

    fn mock_router() -> DefaultRouter {
        zen_provider::DefaultRouter::new(zen_provider::LlmConfig {
            default_provider: Some("mock".to_string()),
            ..Default::default()
        })
    }

    #[test]
    fn zen_delegate_tools_registers_all_thirteen_agents() {
        let wiring = _ZenWiring::new();
        let router = mock_router();
        let delegate_tools = ZenDelegateTools::new(&wiring, &router);

        for name in BUILTIN_AGENT_NAMES {
            assert!(
                delegate_tools.registry.get(name).is_some(),
                "expected delegate for {name} to be registered"
            );
        }
    }

    #[test]
    fn agent_skills_mapping_has_complete_coverage() {
        for name in BUILTIN_AGENT_NAMES {
            let mapped = AGENT_SKILLS.iter().any(|(n, _)| *n == *name);
            assert!(mapped, "agent {name} missing from AGENT_SKILLS");
        }
    }

    #[test]
    fn agent_tools_mapping_has_complete_coverage() {
        for name in BUILTIN_AGENT_NAMES {
            let mapped = AGENT_TOOLS.iter().any(|(n, _)| *n == *name);
            assert!(mapped, "agent {name} missing from AGENT_TOOLS");
        }
    }

    #[test]
    fn describe_agent_returns_profile_data() {
        let registry = DefaultAgentRegistry::new();
        let desc = describe_agent("Oracle", &registry);
        assert!(desc.contains("Oracle"));
        assert!(desc.contains("Specialist"));
    }

    #[test]
    fn describe_agent_unknown_returns_placeholder() {
        let registry = DefaultAgentRegistry::new();
        let desc = describe_agent("NonExistent", &registry);
        assert!(desc.contains("Delegate tool for agent 'NonExistent'"));
    }

    #[test]
    fn zen_wiring_default_is_initialized() {
        let wiring = _ZenWiring::default();
        assert!(!wiring.skills.is_empty());
        assert!(!wiring.tools.is_empty());
    }
}
