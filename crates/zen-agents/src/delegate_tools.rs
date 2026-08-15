use std::sync::Arc;

use rig_compose::agent::GenericAgent;
use rig_compose::delegate::{DelegateRegistry, InProcessAgentDelegate};
use rig_compose::registry::{SkillRegistry, ToolRegistry};
use tracing::{info, warn};
use zen_plugin::registry::RESERVED_NAMESPACE_PREFIXES;

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
            "zen-notion-extraction",
            "zen-wiki-compilation",
            "zen-consolidation-pipeline",
        ],
    ),
    ("Junior", &["zen-wiki-compilation"]),
    ("Hermes", &["zen-consolidation-pipeline"]),
    (
        "Metis",
        &["zen-notion-extraction", "zen-contradiction-detection"],
    ),
    ("Momus", &["zen-vault-learning-loop"]),
    (
        "Oracle",
        &[
            "zen-notion-extraction",
            "zen-vault-learning-loop",
            "zen-contradiction-detection",
        ],
    ),
    (
        "Prometheus",
        &["zen-wiki-compilation", "zen-notion-extraction"],
    ),
    ("Explore", &["zen-vault-learning-loop"]),
    (
        "Librarian",
        &["zen-wiki-compilation", "zen-vault-learning-loop"],
    ),
    ("Argus", &["zen-vault-learning-loop"]),
    (
        "Hephaestus",
        &[
            "zen-notion-extraction",
            "zen-wiki-compilation",
            "zen-consolidation-pipeline",
            "zen-contradiction-detection",
        ],
    ),
    ("Atlas", &["zen-wiki-compilation"]),
    (
        "Zeus",
        &["zen-notion-extraction", "zen-contradiction-detection"],
    ),
];

const AGENT_TOOLS: &[(&str, &[&str])] = &[
    (
        "Sisyphus",
        &[
            "tier2_search",
            "tier4_search",
            "compute_embeddings",
            "fs.read",
            "fs.list",
            "fs.glob",
            "fs.grep",
            "fs.write",
            "fs.edit",
            "fs.delete",
            "fs.move",
            "fs.copy",
            "web.fetch",
            "web.search",
            "shell.exec",
            "system.health",
            "system.notifications",
            "system.calendar",
            "system.daemon",
            "system.fs_watcher",
        ],
    ),
    (
        "Junior",
        &[
            "tier2_search",
            "compute_embeddings",
            "fs.read",
            "fs.list",
            "fs.glob",
            "fs.grep",
            "fs.write",
            "fs.edit",
            "fs.delete",
            "fs.move",
            "fs.copy",
            "web.fetch",
            "web.search",
        ],
    ),
    (
        "Hermes",
        &[
            "tier2_search",
            "tier4_search",
            "fs.read",
            "fs.list",
            "fs.glob",
            "fs.grep",
            "web.fetch",
            "web.search",
        ],
    ),
    (
        "Metis",
        &[
            "tier2_search",
            "fs.read",
            "fs.list",
            "fs.glob",
            "fs.grep",
            "web.fetch",
            "web.search",
        ],
    ),
    (
        "Momus",
        &[
            "tier2_search",
            "tier4_search",
            "fs.read",
            "fs.list",
            "fs.glob",
            "fs.grep",
            "web.fetch",
            "web.search",
        ],
    ),
    (
        "Oracle",
        &[
            "tier2_search",
            "tier4_search",
            "fs.read",
            "fs.list",
            "fs.glob",
            "fs.grep",
            "web.fetch",
            "web.search",
        ],
    ),
    (
        "Prometheus",
        &[
            "compute_embeddings",
            "fs.read",
            "fs.list",
            "fs.glob",
            "fs.grep",
            "web.fetch",
            "web.search",
        ],
    ),
    (
        "Explore",
        &[
            "tier2_search",
            "fs.read",
            "fs.list",
            "fs.glob",
            "fs.grep",
            "web.fetch",
            "web.search",
        ],
    ),
    (
        "Librarian",
        &[
            "tier2_search",
            "compute_embeddings",
            "fs.read",
            "fs.list",
            "fs.glob",
            "fs.grep",
            "web.fetch",
            "web.search",
        ],
    ),
    (
        "Argus",
        &[
            "tier2_search",
            "fs.read",
            "fs.list",
            "fs.glob",
            "fs.grep",
            "web.fetch",
            "web.search",
        ],
    ),
    (
        "Hephaestus",
        &[
            "tier2_search",
            "tier4_search",
            "compute_embeddings",
            "fs.read",
            "fs.list",
            "fs.glob",
            "fs.grep",
            "fs.write",
            "fs.edit",
            "fs.delete",
            "fs.move",
            "fs.copy",
            "web.fetch",
            "web.search",
            "system.health",
            "system.daemon",
            "system.fs_watcher",
            "plugin.wasm_sandbox",
        ],
    ),
    (
        "Atlas",
        &[
            "tier2_search",
            "compute_embeddings",
            "fs.read",
            "fs.list",
            "fs.glob",
            "fs.grep",
            "fs.write",
            "fs.edit",
            "fs.delete",
            "fs.move",
            "fs.copy",
            "web.fetch",
            "web.search",
            "system.calendar",
        ],
    ),
    (
        "Zeus",
        &[
            "tier2_search",
            "tier4_search",
            "fs.read",
            "fs.list",
            "fs.glob",
            "fs.grep",
            "web.fetch",
            "web.search",
            "system.notifications",
        ],
    ),
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
    pub fn new(wiring: &ZenWiring, router: &DefaultRouter) -> Self {
        Self::with_tool_overlay(wiring, router, load_tool_grant_overlay())
    }

    /// Build delegates with an explicit tool-grant overlay (FR-046).
    ///
    /// `overlay` carries the `[agents] tools` patterns; an empty overlay
    /// reproduces the builtin grant set exactly.
    pub fn with_tool_overlay(
        wiring: &ZenWiring,
        _router: &DefaultRouter,
        overlay: Vec<String>,
    ) -> Self {
        let registry = DelegateRegistry::new();

        for &agent_name in BUILTIN_AGENT_NAMES {
            let agent_skills = resolve_skill_ids(agent_name);
            let agent_tools = resolve_agent_tool_grants(agent_name, &overlay, &wiring.tools);
            let available_tools: Vec<&str> = agent_tools.iter().map(String::as_str).collect();
            let available_skills = filter_registered_skills(&agent_skills, &wiring.skills);

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

// ---------------------------------------------------------------------------
// FR-046: config-driven tool grants (builtin defaults + overlay)
// ---------------------------------------------------------------------------

/// Load the `[agents] tools` overlay from the merged 5-layer config
/// (FR-046a). Best-effort: config load failure logs a warning and yields
/// an empty overlay, leaving the builtin grant set unchanged.
pub fn load_tool_grant_overlay() -> Vec<String> {
    match zen_core::config::load_config() {
        Ok(config) => config.agents_tools.clone(),
        Err(e) => {
            warn!(
                error = %e,
                "ZenDelegateTools: config unavailable, using builtin tool grants only"
            );
            Vec::new()
        }
    }
}

/// FR-046 overlay grant pattern: exact tool name, `prefix.*` wildcard, or
/// `plugin:*` (all plugin-registered tools).
#[derive(Debug, Clone, PartialEq, Eq)]
enum ToolGrantPattern {
    /// Exact tool name (e.g. `"shell.exec"`).
    Exact(String),
    /// Namespace wildcard (e.g. `"fs.*"` matches every `fs.<name>`).
    Prefix(String),
    /// Every plugin-registered tool (e.g. `echo.hello`), never builtins.
    AllPlugins,
}

impl ToolGrantPattern {
    fn parse(pattern: &str) -> Self {
        if pattern == "plugin:*" {
            Self::AllPlugins
        } else if let Some(prefix) = pattern.strip_suffix(".*") {
            Self::Prefix(prefix.to_string())
        } else {
            Self::Exact(pattern.to_string())
        }
    }

    fn matches(&self, tool_name: &str) -> bool {
        match self {
            Self::Exact(name) => tool_name == name,
            Self::Prefix(prefix) => tool_name
                .strip_prefix(prefix.as_str())
                .is_some_and(|rest| rest.starts_with('.')),
            Self::AllPlugins => is_plugin_tool(tool_name),
        }
    }
}

/// A builtin tool name: any tool granted anywhere in the static
/// [`AGENT_TOOLS`] map.
fn is_builtin_tool_name(name: &str) -> bool {
    AGENT_TOOLS
        .iter()
        .flat_map(|(_, tools)| tools.iter())
        .any(|tool| *tool == name)
}

/// A plugin-registered tool (FR-046 `plugin:*` target): namespaced
/// (contains `.`), NOT a builtin tool name, and NOT under a reserved
/// namespace prefix ([`RESERVED_NAMESPACE_PREFIXES`], FR-050) — so a
/// spoofed `fs.evil` plugin tool can never be granted by `plugin:*`.
fn is_plugin_tool(name: &str) -> bool {
    name.contains('.')
        && !is_builtin_tool_name(name)
        && name
            .split('.')
            .next()
            .is_some_and(|namespace| !RESERVED_NAMESPACE_PREFIXES.contains(&namespace))
}

/// Resolve an agent's granted tool ids (FR-046b): the builtin defaults
/// from [`AGENT_TOOLS`] plus the config overlay, filtered down to tools
/// actually present in `registry`. Patterns that match nothing (e.g. a
/// plugin not yet installed) are silently idle. An empty overlay
/// reproduces the builtin grant set exactly.
pub fn resolve_agent_tool_grants(
    agent_name: &str,
    overlay: &[String],
    registry: &ToolRegistry,
) -> Vec<String> {
    let mut granted: Vec<String> = resolve_tool_ids(agent_name)
        .into_iter()
        .map(String::from)
        .collect();

    let registered: Vec<String> = registry
        .schemas()
        .iter()
        .map(|schema| schema.name.clone())
        .collect();

    for pattern in overlay {
        let pattern = ToolGrantPattern::parse(pattern);
        for name in &registered {
            if pattern.matches(name) && !granted.contains(name) {
                granted.push(name.clone());
            }
        }
    }

    granted
        .into_iter()
        .filter(|name| registry.get(name).is_ok())
        .collect()
}

fn filter_registered_skills<'a>(ids: &[&'a str], registry: &SkillRegistry) -> Vec<&'a str> {
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

/// T310-T313: Public API for builtin tool resolution. Returns the builtin
/// grant list only; grant-aware callers (delegate construction,
/// orchestrator agent building) should use
/// [`resolve_agent_tool_grants`] with the config overlay instead.
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

/// Format a human-readable description of an agent by looking it up in the
/// DefaultAgentRegistry. Used in diagnostics and logging output.
pub(crate) fn describe_agent(agent_name: &str, registry: &DefaultAgentRegistry) -> String {
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

    // ── FR-046 (T106): grant regression tests ────────────────────────────

    /// Frozen snapshot of the pre-FR-046 static grant map (task spec
    /// FR-046 test 1): with an empty overlay the resolved grants must be
    /// byte-for-byte identical to this table.
    const PRIOR_STATIC_AGENT_TOOLS: &[(&str, &[&str])] = &[
        (
            "Sisyphus",
            &[
                "tier2_search",
                "tier4_search",
                "compute_embeddings",
                "fs.read",
                "fs.list",
                "fs.glob",
                "fs.grep",
                "fs.write",
                "fs.edit",
                "fs.delete",
                "fs.move",
                "fs.copy",
                "web.fetch",
                "web.search",
                "shell.exec",
                "system.health",
                "system.notifications",
                "system.calendar",
                "system.daemon",
                "system.fs_watcher",
            ],
        ),
        (
            "Junior",
            &[
                "tier2_search",
                "compute_embeddings",
                "fs.read",
                "fs.list",
                "fs.glob",
                "fs.grep",
                "fs.write",
                "fs.edit",
                "fs.delete",
                "fs.move",
                "fs.copy",
                "web.fetch",
                "web.search",
            ],
        ),
        (
            "Hermes",
            &[
                "tier2_search",
                "tier4_search",
                "fs.read",
                "fs.list",
                "fs.glob",
                "fs.grep",
                "web.fetch",
                "web.search",
            ],
        ),
        (
            "Metis",
            &[
                "tier2_search",
                "fs.read",
                "fs.list",
                "fs.glob",
                "fs.grep",
                "web.fetch",
                "web.search",
            ],
        ),
        (
            "Momus",
            &[
                "tier2_search",
                "tier4_search",
                "fs.read",
                "fs.list",
                "fs.glob",
                "fs.grep",
                "web.fetch",
                "web.search",
            ],
        ),
        (
            "Oracle",
            &[
                "tier2_search",
                "tier4_search",
                "fs.read",
                "fs.list",
                "fs.glob",
                "fs.grep",
                "web.fetch",
                "web.search",
            ],
        ),
        (
            "Prometheus",
            &[
                "compute_embeddings",
                "fs.read",
                "fs.list",
                "fs.glob",
                "fs.grep",
                "web.fetch",
                "web.search",
            ],
        ),
        (
            "Explore",
            &[
                "tier2_search",
                "fs.read",
                "fs.list",
                "fs.glob",
                "fs.grep",
                "web.fetch",
                "web.search",
            ],
        ),
        (
            "Librarian",
            &[
                "tier2_search",
                "compute_embeddings",
                "fs.read",
                "fs.list",
                "fs.glob",
                "fs.grep",
                "web.fetch",
                "web.search",
            ],
        ),
        (
            "Argus",
            &[
                "tier2_search",
                "fs.read",
                "fs.list",
                "fs.glob",
                "fs.grep",
                "web.fetch",
                "web.search",
            ],
        ),
        (
            "Hephaestus",
            &[
                "tier2_search",
                "tier4_search",
                "compute_embeddings",
                "fs.read",
                "fs.list",
                "fs.glob",
                "fs.grep",
                "fs.write",
                "fs.edit",
                "fs.delete",
                "fs.move",
                "fs.copy",
                "web.fetch",
                "web.search",
                "system.health",
                "system.daemon",
                "system.fs_watcher",
                "plugin.wasm_sandbox",
            ],
        ),
        (
            "Atlas",
            &[
                "tier2_search",
                "compute_embeddings",
                "fs.read",
                "fs.list",
                "fs.glob",
                "fs.grep",
                "fs.write",
                "fs.edit",
                "fs.delete",
                "fs.move",
                "fs.copy",
                "web.fetch",
                "web.search",
                "system.calendar",
            ],
        ),
        (
            "Zeus",
            &[
                "tier2_search",
                "tier4_search",
                "fs.read",
                "fs.list",
                "fs.glob",
                "fs.grep",
                "web.fetch",
                "web.search",
                "system.notifications",
            ],
        ),
    ];

    #[test]
    fn fr046_default_config_reproduces_prior_static_map_exactly() {
        let wiring = _ZenWiring::new();
        for (name, expected) in PRIOR_STATIC_AGENT_TOOLS {
            let granted = resolve_agent_tool_grants(name, &[], &wiring.tools);
            let expected: Vec<String> = expected.iter().map(|t| (*t).to_string()).collect();
            assert_eq!(
                granted, expected,
                "default (no overlay) grants drifted from the prior static map for {name}"
            );
        }
    }

    struct ProbeTool(&'static str);

    #[async_trait::async_trait]
    impl rig_compose::tool::Tool for ProbeTool {
        fn schema(&self) -> rig_compose::tool::ToolSchema {
            rig_compose::tool::ToolSchema {
                name: self.0.to_string(),
                description: self.0.to_string(),
                args_schema: serde_json::json!({}),
                result_schema: serde_json::json!({}),
            }
        }

        async fn invoke(
            &self,
            _args: serde_json::Value,
        ) -> Result<serde_json::Value, rig_compose::registry::KernelError> {
            Ok(serde_json::json!({ "ran": self.0 }))
        }
    }

    #[test]
    fn fr046_plugin_wildcard_excludes_reserved_prefixes_and_builtin_names() {
        let tools = ToolRegistry::new();
        tools.register(Arc::new(ProbeTool("echo.hello")));
        tools.register(Arc::new(ProbeTool("fs.evil")));
        tools.register(Arc::new(ProbeTool("web.evil")));
        tools.register(Arc::new(ProbeTool("system.evil")));
        tools.register(Arc::new(ProbeTool("plugin.evil")));
        tools.register(Arc::new(ProbeTool("shell.evil")));
        tools.register(Arc::new(ProbeTool("shell.exec")));

        let overlay = vec!["plugin:*".to_string()];
        let granted = resolve_agent_tool_grants("Metis", &overlay, &tools);

        assert!(
            granted.contains(&"echo.hello".to_string()),
            "plugin:* must reach plugin-namespaced tools"
        );
        for spoof in [
            "fs.evil",
            "web.evil",
            "system.evil",
            "plugin.evil",
            "shell.evil",
        ] {
            assert!(
                !granted.contains(&spoof.to_string()),
                "spoofed reserved-prefix name {spoof} must NOT be granted by plugin:*"
            );
        }
        assert!(
            !granted.contains(&"shell.exec".to_string()),
            "plugin:* must not grant builtin tool names outside the builtin map"
        );
    }

    #[test]
    fn fr046_exact_and_prefix_patterns_grant_registered_tools() {
        let wiring = _ZenWiring::new();

        let exact =
            resolve_agent_tool_grants("Metis", &["system.calendar".to_string()], &wiring.tools);
        assert!(exact.contains(&"system.calendar".to_string()));

        let prefix = resolve_agent_tool_grants("Metis", &["system.*".to_string()], &wiring.tools);
        for tool in [
            "system.health",
            "system.notifications",
            "system.calendar",
            "system.daemon",
            "system.fs_watcher",
        ] {
            assert!(
                prefix.contains(&tool.to_string()),
                "system.* must grant {tool}"
            );
        }
        assert!(!prefix.contains(&"fs.write".to_string()));
    }

    #[test]
    fn fr046_patterns_matching_nothing_are_silently_idle() {
        let wiring = _ZenWiring::new();
        let overlay = vec![
            "notinstalled.*".to_string(),
            "plugin:*".to_string(),
            "no.such.tool".to_string(),
        ];
        let granted = resolve_agent_tool_grants("Metis", &overlay, &wiring.tools);
        let expected: Vec<String> = resolve_tool_ids("Metis")
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(
            granted, expected,
            "idle patterns must not alter the builtin grant set"
        );
    }

    fn write_echo_plugin(dir: &std::path::Path) {
        use sha2::{Digest, Sha256};

        let echo_dir = dir.join("echo");
        std::fs::create_dir_all(&echo_dir).unwrap();
        let wasm = wat::parse_str(r#"(module (func (export "hello")))"#).unwrap();
        std::fs::write(echo_dir.join("echo.wasm"), &wasm).unwrap();

        let mut hasher = Sha256::new();
        hasher.update(&wasm);
        let sha256: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        std::fs::write(
            echo_dir.join("manifest.toml"),
            format!(
                "id = \"echo\"\nname = \"Echo\"\nversion = \"0.1.0\"\ntype = \"tool\"\n\
                 permissions = []\nentry = \"echo.wasm\"\nsha256 = \"{sha256}\"\n"
            ),
        )
        .unwrap();
    }

    fn wiring_with_echo_plugin(dir: &std::path::Path) -> _ZenWiring {
        let mut registry = zen_plugin::registry::PluginRegistry::with_plugin_dir(dir.to_path_buf());
        registry.discover().unwrap();
        _ZenWiring::with_sandbox_mode(
            zen_core::sandbox::SandboxMode::WorkspaceWrite,
            vec![dir.to_path_buf()],
            Some(&registry),
        )
    }

    #[test]
    fn fr046_plugin_wildcard_grants_installed_plugin_tool() {
        let dir = tempfile::tempdir().unwrap();
        write_echo_plugin(dir.path());
        let wiring = wiring_with_echo_plugin(dir.path());
        assert!(wiring.tools.get("echo.hello").is_ok());

        let granted = resolve_agent_tool_grants("Metis", &["plugin:*".to_string()], &wiring.tools);
        assert!(
            granted.contains(&"echo.hello".to_string()),
            "overlay [plugin:*] must grant echo.hello"
        );

        let builtin_only = resolve_agent_tool_grants("Metis", &[], &wiring.tools);
        assert!(
            !builtin_only.contains(&"echo.hello".to_string()),
            "without the overlay the plugin tool must stay unreachable"
        );
    }

    // ── T114: D1+D2 interplay (disabled plugin + plugin:* overlay) ───────

    #[test]
    fn t114_disabled_plugin_with_plugin_wildcard_yields_no_phantom_grant() {
        use zen_plugin::registry::PluginState;

        let dir = tempfile::tempdir().unwrap();
        write_echo_plugin(dir.path());

        // D2 (FR-047): disable via persisted state.json BEFORE discovery —
        // takes effect at next wiring construction.
        PluginState {
            disabled: vec!["echo".to_string()],
        }
        .save(dir.path())
        .unwrap();

        let mut registry =
            zen_plugin::registry::PluginRegistry::with_plugin_dir(dir.path().to_path_buf());
        registry.discover().unwrap();
        assert_eq!(
            registry.count(),
            0,
            "disabled plugin must be skipped at discovery"
        );

        let wiring = _ZenWiring::with_sandbox_mode(
            zen_core::sandbox::SandboxMode::WorkspaceWrite,
            vec![dir.path().to_path_buf()],
            Some(&registry),
        );
        assert!(wiring.tools.get("echo.hello").is_err());

        // D1 (FR-046) + D2 interplay: the overlay asks for all plugin tools,
        // but the plugin is disabled — no phantom grant, no error.
        let overlay = vec!["plugin:*".to_string()];
        let granted = resolve_agent_tool_grants("Sisyphus", &overlay, &wiring.tools);
        assert!(
            !granted.contains(&"echo.hello".to_string()),
            "disabled plugin must not be granted via plugin:*"
        );

        let router = mock_router();
        let delegates = ZenDelegateTools::with_tool_overlay(&wiring, &router, overlay);
        assert!(
            delegates.registry.get("Sisyphus").is_some(),
            "delegate construction must stay error-free under the interplay"
        );
    }

    // ── T107 (T-A): agent-path e2e — plugin tool reachable by an agent ───

    #[tokio::test]
    async fn t107_plugin_tool_callable_through_agent_session_path() {
        use rig_compose::normalizer::ToolInvocation;

        let dir = tempfile::tempdir().unwrap();
        write_echo_plugin(dir.path());
        let wiring = wiring_with_echo_plugin(dir.path());

        // The exact grant resolution both agent-side consumers use
        // (ZenDelegateTools::with_tool_overlay / AgentOrchestrator::build_agent).
        let overlay = vec!["plugin:*".to_string()];
        let tools = resolve_agent_tool_grants("Sisyphus", &overlay, &wiring.tools);
        assert!(tools.contains(&"echo.hello".to_string()));

        // Build the agent the way AgentOrchestrator::build_agent does.
        let router = mock_router();
        let mut builder = crate::ZenAgent::builder("Sisyphus");
        for tool_id in &tools {
            builder = builder.with_tool(tool_id.as_str());
        }
        let agent = builder.build(&wiring, &router).expect("agent build");

        // Reachable: advertised in the agent-visible manifest...
        let manifest = agent.tool_manifest();
        assert!(
            manifest.contains("echo.hello"),
            "granted plugin tool missing from agent manifest"
        );

        // ...and invocable through the agent's own tool registry.
        let invocation = ToolInvocation::new("echo.hello", serde_json::json!({})).unwrap();
        let hooks: Vec<&dyn rig_compose::normalizer::ToolDispatchHook> = Vec::new();
        let results = rig_compose::normalizer::dispatch_tool_invocations_with_hooks(
            agent.generic.tools(),
            &[invocation],
            &hooks,
        )
        .await
        .expect("plugin tool dispatch through the agent path");
        assert_eq!(results[0].output["output"]["exit_code"], 0);
        assert_eq!(results[0].output["metrics"]["plugin"], "echo");

        // Delegate path constructs cleanly with the same overlay.
        let delegates = ZenDelegateTools::with_tool_overlay(&wiring, &router, overlay);
        assert!(delegates.registry.get("Sisyphus").is_some());
    }
}
