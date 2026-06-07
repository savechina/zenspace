//! Unit tests for fallback chain construction.

use std::collections::HashMap;

use zen_core::config::{AgentConfig, ZenConfig, FallbackStep};
use zen_provider::{DefaultRouter, Provider};

#[test]
fn test_build_fallback_chain_with_fallbacks() {
    let config = ZenConfig {
        default_provider: Some("ollama".into()),
        default_model: Some("qwen3.6:35b-mlx".into()),
        agents: {
            let mut map = HashMap::new();
            map.insert(
                "entity_extraction".into(),
                AgentConfig {
                    provider: Some("ollama".into()),
                    model: Some("qwen3.6:35b-mlx".into()),
                    fallbacks: vec![
                        FallbackStep {
                            provider: "deepseek".into(),
                            model: Some("deepseek-v4-flash".into()),
                            timeout_secs: None,
                        },
                        FallbackStep {
                            provider: "openai".into(),
                            model: Some("gpt-4o-mini".into()),
                            timeout_secs: None,
                        },
                    ],
                    retry_policy: None,
                    sensitivity: None,
                },
            );
            map
        },
        ..Default::default()
    };

    let router = DefaultRouter::from_agentic(&config);
    let chain = router.build_fallback_chain("entity_extraction");

    assert_eq!(chain.len(), 4);
    assert_eq!(chain[0].0, Provider::Ollama);
    assert_eq!(chain[1].0, Provider::DeepSeek);
    assert_eq!(chain[2].0, Provider::OpenAI);
    assert_eq!(chain[3].0, Provider::Mock);
}

#[test]
fn test_build_fallback_chain_without_fallbacks() {
    let config = ZenConfig {
        default_provider: Some("ollama".into()),
        agents: {
            let mut map = HashMap::new();
            map.insert(
                "test_agent".into(),
                AgentConfig {
                    provider: Some("ollama".into()),
                    model: Some("qwen3.6:35b-mlx".into()),
                    fallbacks: vec![],
                    retry_policy: None,
                    sensitivity: None,
                },
            );
            map
        },
        ..Default::default()
    };

    let router = DefaultRouter::from_agentic(&config);
    let chain = router.build_fallback_chain("test_agent");

    assert_eq!(chain.len(), 2);
    assert_eq!(chain[0].0, Provider::Ollama);
    assert_eq!(chain[1].0, Provider::Mock);
}

#[test]
fn test_build_fallback_chain_unknown_agent() {
    let config = ZenConfig {
        default_provider: Some("openai".into()),
        default_model: Some("gpt-4o-mini".into()),
        ..Default::default()
    };

    let router = DefaultRouter::from_agentic(&config);
    let chain = router.build_fallback_chain("unknown_agent");

    assert_eq!(chain.len(), 2);
    assert_eq!(chain[0].0, Provider::OpenAI);
    assert_eq!(chain[1].0, Provider::Mock);
}
