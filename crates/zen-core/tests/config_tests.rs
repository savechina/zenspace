// ============================================================================
// 4D Test Suite: zen-core config.rs
//
// Dimensions:
//   NORMAL     — load_embedded_config(), all public helpers on various configs
//   REVERSE    — Missing providers/agents return None/defaults
//   ADVERSARIAL — Empty maps, missing keys, extreme values
//   LOGIC TREE  — resolve_task_provider/resolve_task_model fallback chains
// ============================================================================

use std::collections::HashMap;
use zen_core::config::*;

// ============================================================================
// NORMAL PATH — Default instances, embedded config, standard helpers
// ============================================================================

#[test]
fn load_embedded_config_succeeds() {
    let config = load_embedded_config().expect("Embedded config should load");
    // Should have provider definitions
    assert!(
        !config.providers.is_empty(),
        "Embedded config should have providers"
    );
    // Should have an ollama provider
    let ollama = config.providers.get("ollama");
    assert!(
        ollama.is_some(),
        "Embedded config should have ollama provider"
    );
}

#[test]
fn default_llm_provider_with_defaults() {
    let config = ZenConfig::default();
    assert_eq!(default_llm_provider(&config), "ollama");
}

#[test]
fn default_llm_provider_custom() {
    let config = ZenConfig {
        default_provider: Some("openai".into()),
        ..Default::default()
    };
    assert_eq!(default_llm_provider(&config), "openai");
}

#[test]
fn default_model_with_defaults() {
    let config = ZenConfig::default();
    assert_eq!(default_model(&config), "qwen3-coder");
}

#[test]
fn default_model_custom() {
    let config = ZenConfig {
        default_model: Some("gpt-4".into()),
        ..Default::default()
    };
    assert_eq!(default_model(&config), "gpt-4");
}

#[test]
fn get_provider_returns_some_when_exists() {
    let mut config = ZenConfig::default();
    let mut providers = HashMap::new();
    providers.insert(
        "my-provider".into(),
        ProviderConfig {
            provider_type: Some("openai".into()),
            ..Default::default()
        },
    );
    config.providers = providers;

    let found = get_provider(&config, "my-provider");
    assert!(
        found.is_some(),
        "get_provider should find existing provider"
    );
    assert_eq!(found.unwrap().provider_type.as_deref(), Some("openai"));
}

#[test]
fn get_agent_task_returns_some_when_exists() {
    let mut config = ZenConfig::default();
    let mut agents = HashMap::new();
    agents.insert(
        "research".into(),
        AgentConfig {
            provider: Some("anthropic".into()),
            model: Some("claude-3".into()),
            ..Default::default()
        },
    );
    config.agents = agents;

    let found = get_agent_task(&config, "research");
    assert!(found.is_some(), "get_agent_task should find existing task");
    assert_eq!(found.unwrap().model.as_deref(), Some("claude-3"));
}

#[test]
fn resolve_task_provider_uses_task_provider_first() {
    let mut config = ZenConfig::default();
    let mut agents = HashMap::new();
    agents.insert(
        "research".into(),
        AgentConfig {
            provider: Some("anthropic".into()),
            ..Default::default()
        },
    );
    config.agents = agents;
    config.default_provider = Some("ollama".into());

    assert_eq!(resolve_task_provider(&config, "research"), "anthropic");
}

#[test]
fn resolve_task_provider_falls_back_to_default() {
    let config = ZenConfig {
        default_provider: Some("deepseek".into()),
        ..Default::default()
    };

    assert_eq!(
        resolve_task_provider(&config, "nonexistent-task"),
        "deepseek"
    );
}

#[test]
fn resolve_task_provider_falls_back_to_ollama() {
    let config = ZenConfig::default();
    assert_eq!(resolve_task_provider(&config, "ghost"), "ollama");
}

#[test]
fn resolve_task_model_uses_task_model_first() {
    let mut config = ZenConfig::default();
    let mut agents = HashMap::new();
    agents.insert(
        "research".into(),
        AgentConfig {
            provider: Some("anthropic".into()),
            model: Some("claude-opus".into()),
            ..Default::default()
        },
    );
    config.agents = agents;
    config.default_model = Some("gpt-4".into());

    assert_eq!(resolve_task_model(&config, "research"), "claude-opus");
}

#[test]
fn resolve_task_model_falls_back_to_provider_default_model() {
    let mut config = ZenConfig::default();
    let mut agents = HashMap::new();
    agents.insert(
        "research".into(),
        AgentConfig {
            provider: Some("openai".into()),
            model: None,
            ..Default::default()
        },
    );
    config.agents = agents;
    let mut providers = HashMap::new();
    providers.insert(
        "openai".into(),
        ProviderConfig {
            default_model: Some("gpt-4-turbo".into()),
            ..Default::default()
        },
    );
    config.providers = providers;
    config.default_model = Some("gpt-4".into());

    assert_eq!(resolve_task_model(&config, "research"), "gpt-4-turbo");
}

#[test]
fn resolve_task_model_falls_back_to_global_default_model() {
    let mut config = ZenConfig::default();
    let mut agents = HashMap::new();
    agents.insert(
        "research".into(),
        AgentConfig {
            provider: Some("custom".into()),
            model: None,
            ..Default::default()
        },
    );
    config.agents = agents;
    config.default_model = Some("qwen3-coder".into());

    assert_eq!(resolve_task_model(&config, "research"), "qwen3-coder");
}

#[test]
fn resolve_task_model_falls_back_to_hardcoded_default() {
    let config = ZenConfig::default();
    assert_eq!(resolve_task_model(&config, "ghost"), "qwen3-coder");
}

#[test]
fn consolidation_time_with_value() {
    let mut config = ZenConfig::default();
    config.cron.consolidation_time = Some("03:30".into());
    assert_eq!(consolidation_time(&config), "03:30");
}

#[test]
fn consolidation_time_default() {
    let config = ZenConfig::default();
    assert_eq!(consolidation_time(&config), "02:00");
}

#[test]
fn llm_preference_display_variants() {
    assert_eq!(LlmPreference::Any.to_string(), "any");
    assert_eq!(LlmPreference::LocalOnly.to_string(), "local-only");
    assert_eq!(LlmPreference::CloudOnly.to_string(), "cloud-only");
    assert_eq!(
        LlmPreference::Provider("custom".into()).to_string(),
        "custom"
    );
}

#[test]
fn load_embedded_config_has_providers() {
    let config = load_embedded_config().expect("Embedded config should load");
    assert!(!config.providers.is_empty(), "Should have providers");
    // Known providers that should exist based on the codebase
    for name in &["ollama", "openai", "deepseek"] {
        assert!(
            config.providers.contains_key(*name),
            "Embedded config should have provider '{name}'"
        );
    }
}

// ============================================================================
// REVERSE PATH — Missing entries, empty configs
// ============================================================================

#[test]
fn get_provider_returns_none_when_missing() {
    let config = ZenConfig::default();
    assert!(get_provider(&config, "nonexistent").is_none());
}

#[test]
fn get_agent_task_returns_none_when_missing() {
    let config = ZenConfig::default();
    assert!(get_agent_task(&config, "nonexistent").is_none());
}

#[test]
fn get_provider_returns_none_when_providers_empty() {
    let config = ZenConfig::default();
    assert!(get_provider(&config, "anything").is_none());
}

#[test]
fn resolve_task_provider_chain_all_none() {
    let config = ZenConfig {
        default_provider: None,
        ..Default::default()
    };
    assert_eq!(resolve_task_provider(&config, "anything"), "ollama");
}

#[test]
fn resolve_task_model_chain_all_none() {
    let config = ZenConfig {
        default_model: None,
        ..Default::default()
    };
    assert_eq!(resolve_task_model(&config, "anything"), "qwen3-coder");
}

// ── Embedded config basics ──

#[test]
fn embedded_config_default_provider_is_set() {
    let config = load_embedded_config().expect("Embedded config should load");
    // The embedded config should have a default_provider
    assert!(
        config.default_provider.is_some(),
        "Embedded config should set default_provider"
    );
}

// ============================================================================
// ADVERSARIAL PATH — Edge cases
// ============================================================================

#[test]
fn get_provider_with_empty_string() {
    let config = ZenConfig::default();
    assert!(
        get_provider(&config, "").is_none(),
        "Empty string should get None"
    );
}

#[test]
fn get_agent_task_with_empty_string() {
    let config = ZenConfig::default();
    assert!(
        get_agent_task(&config, "").is_none(),
        "Empty string should get None"
    );
}

#[test]
fn resolve_task_provider_with_empty_task_name() {
    let config = ZenConfig::default();
    // Even with empty task name, should fall through to defaults
    let result = resolve_task_provider(&config, "");
    assert!(
        !result.is_empty(),
        "Should return a non-empty provider name"
    );
}

#[test]
fn get_provider_with_very_long_name() {
    let config = ZenConfig::default();
    let long_name = "a".repeat(10_000);
    assert!(get_provider(&config, &long_name).is_none());
}

#[test]
fn resolve_task_model_with_non_existent_provider_in_chain() {
    let mut config = ZenConfig::default();
    let mut agents = HashMap::new();
    agents.insert(
        "task".into(),
        AgentConfig {
            provider: Some("non-existent-provider".into()),
            model: None,
            ..Default::default()
        },
    );
    config.agents = agents;
    config.default_model = None;

    // The provider lookup will fail, so it should fall back to "qwen3-coder"
    assert_eq!(resolve_task_model(&config, "task"), "qwen3-coder");
}

#[test]
fn load_embedded_config_is_deterministic() {
    let config1 = load_embedded_config().expect("First load should succeed");
    let config2 = load_embedded_config().expect("Second load should succeed");
    assert_eq!(
        config1.default_provider, config2.default_provider,
        "Embedded config should be deterministic"
    );
}

#[test]
fn config_default_feature_flags() {
    let config = ZenConfig::default();
    assert_eq!(config.features.multi_agent, Some(true));
    assert_eq!(config.features.auto_research, Some(true));
}

// ============================================================================
// LOGIC TREE — Fallback chain branches for resolve_* functions
// ============================================================================

#[test]
fn resolve_task_provider_logic_tree_exhaustive() {
    // Branch 1: task has provider → use it
    {
        let mut config = ZenConfig::default();
        let mut agents = HashMap::new();
        agents.insert(
            "t1".into(),
            AgentConfig {
                provider: Some("p1".into()),
                ..Default::default()
            },
        );
        config.agents = agents;
        config.default_provider = Some("default-p".into());
        assert_eq!(
            resolve_task_provider(&config, "t1"),
            "p1",
            "Branch 1: task provider"
        );
    }

    // Branch 2: task has no provider, default_provider set → use default
    {
        let config = ZenConfig {
            default_provider: Some("default-p".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve_task_provider(&config, "t2"),
            "default-p",
            "Branch 2: default provider"
        );
    }

    // Branch 3: neither task nor default → "ollama"
    {
        let config = ZenConfig {
            default_provider: None,
            ..Default::default()
        };
        assert_eq!(
            resolve_task_provider(&config, "t3"),
            "ollama",
            "Branch 3: hardcoded default"
        );
    }
}

#[test]
fn resolve_task_model_logic_tree_exhaustive() {
    // Branch 1: task has model → use it
    {
        let mut config = ZenConfig::default();
        let mut agents = HashMap::new();
        agents.insert(
            "t1".into(),
            AgentConfig {
                provider: Some("p".into()),
                model: Some("m1".into()),
                ..Default::default()
            },
        );
        config.agents = agents;
        config.default_model = Some("global-m".into());
        assert_eq!(
            resolve_task_model(&config, "t1"),
            "m1",
            "Branch 1: task model"
        );
    }

    // Branch 2: task has no model, provider has default_model → use provider default
    {
        let mut config = ZenConfig::default();
        let mut agents = HashMap::new();
        agents.insert(
            "t2".into(),
            AgentConfig {
                provider: Some("p".into()),
                model: None,
                ..Default::default()
            },
        );
        config.agents = agents;
        let mut providers = HashMap::new();
        providers.insert(
            "p".into(),
            ProviderConfig {
                default_model: Some("pm".into()),
                ..Default::default()
            },
        );
        config.providers = providers;
        config.default_model = Some("global-m".into());
        assert_eq!(
            resolve_task_model(&config, "t2"),
            "pm",
            "Branch 2: provider default model"
        );
    }

    // Branch 3: task has no model, provider has no default_model, global default set → use global
    {
        let mut config = ZenConfig::default();
        let mut agents = HashMap::new();
        agents.insert(
            "t3".into(),
            AgentConfig {
                provider: Some("p".into()),
                model: None,
                ..Default::default()
            },
        );
        config.agents = agents;
        config.default_model = Some("global-m".into());
        assert_eq!(
            resolve_task_model(&config, "t3"),
            "global-m",
            "Branch 3: global default model"
        );
    }

    // Branch 4: nothing set → hardcoded "qwen3-coder"
    {
        let config = ZenConfig::default();
        assert_eq!(
            resolve_task_model(&config, "t4"),
            "qwen3-coder",
            "Branch 4: hardcoded default"
        );
    }
}

#[test]
fn consolidation_time_logic_tree() {
    // Branch 1: configured time
    {
        let mut config = ZenConfig::default();
        config.cron.consolidation_time = Some("12:00".into());
        assert_eq!(consolidation_time(&config), "12:00");
    }
    // Branch 2: default
    {
        let config = ZenConfig::default();
        assert_eq!(consolidation_time(&config), "02:00");
    }
}

// ============================================================================
// Struct sanity: Ensure public types have expected bounds
// ============================================================================

#[test]
fn agentic_config_is_send_sync() {
    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}
    assert_send::<ZenConfig>();
    assert_sync::<ZenConfig>();
}

#[test]
fn provider_config_default_is_empty() {
    let pc = ProviderConfig::default();
    assert!(pc.provider_type.is_none());
    assert!(pc.base_url.is_none());
    assert!(pc.api_key.is_none());
    assert!(pc.default_model.is_none());
}

#[test]
fn agent_config_default_is_empty() {
    let ac = AgentConfig::default();
    assert!(ac.provider.is_none());
    assert!(ac.model.is_none());
    assert!(ac.fallbacks.is_empty());
    assert!(ac.retry_policy.is_none());
}

#[test]
fn llm_preference_eq_hash() {
    use std::collections::HashSet;
    let mut set = HashSet::new();
    set.insert(LlmPreference::Any);
    set.insert(LlmPreference::LocalOnly);
    set.insert(LlmPreference::CloudOnly);
    set.insert(LlmPreference::Provider("x".into()));
    assert_eq!(set.len(), 4);
    // Duplicate
    set.insert(LlmPreference::Any);
    assert_eq!(set.len(), 4);
}

// ============================================================================
// FR-046: [agents.tools] grant overlay
// ============================================================================

#[test]
fn embedded_config_agents_tools_overlay_defaults_empty() {
    let config = load_embedded_config().expect("Embedded config should load");
    assert!(
        config.agents_tools.is_empty(),
        "embedded default overlay must be empty (builtin grant set unchanged)"
    );
    assert!(
        config.agents.contains_key("synthesis"),
        "task entries must still parse alongside the overlay block"
    );
}

#[test]
fn agents_tools_overlay_parses_from_toml() {
    let config: ZenConfig = toml::from_str(
        r#"
[agents]
tools = ["plugin:*"]

[agents.research]
provider = "ollama"
"#,
    )
    .expect("overlay TOML must parse");
    assert_eq!(config.agents_tools, vec!["plugin:*"]);
    assert!(config.agents.contains_key("research"));
}

#[test]
fn agents_tools_default_constructs_empty() {
    let config = ZenConfig::default();
    assert!(config.agents_tools.is_empty());
}
