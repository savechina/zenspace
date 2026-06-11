// ============================================================================
// 4D Test Suite: zen-provider routing
//
// Dimensions:
//   NORMAL       — DefaultRouter creation, model metadata, provider selection
//   REVERSE      — Unknown tasks, empty providers, missing API keys
//   ADVERSARIAL  — Extreme values in model metadata, invalid provider names
//   LOGIC TREE   — All model tiers, all protocol types are handled
// ============================================================================

use std::collections::HashMap;
use zen_core::config::{AgentConfig, FallbackStep, ZenConfig};
use zen_provider::{DefaultRouter, ModelMetadata, Provider};

// ============================================================================
// NORMAL PATH — Standard routing operations
// ============================================================================

#[test]
fn test_default_router_creation_from_config() {
    let config = ZenConfig {
        default_provider: Some("ollama".into()),
        default_model: Some("qwen3.6:35b-mlx".into()),
        ..Default::default()
    };
    let router = DefaultRouter::from_agentic(&config);
    let chain = router.build_fallback_chain("synthesis");
    assert!(!chain.is_empty(), "fallback chain should have entries");
}

#[test]
fn test_router_selects_provider_for_known_agent() {
    let config = ZenConfig {
        default_provider: Some("openai".into()),
        agents: {
            let mut map = HashMap::new();
            map.insert(
                "test_agent".into(),
                AgentConfig {
                    provider: Some("deepseek".into()),
                    model: Some("deepseek-v4-flash".into()),
                    fallbacks: vec![],
                    retry_policy: None,
                    sensitivity: None,
                    variant: None,
                    temperature: None,
                    max_tokens: None,
                },
            );
            map
        },
        ..Default::default()
    };
    let router = DefaultRouter::from_agentic(&config);
    let chain = router.build_fallback_chain("test_agent");
    assert_eq!(chain[0].0, Provider::DeepSeek);
}

#[test]
fn test_model_metadata_creation() {
    let meta = ModelMetadata {
        name: "gpt-4o-mini".into(),
        provider: "openai".into(),
        context_window: 128_000,
        input_cost_per_million: 0.15,
        output_cost_per_million: 0.60,
        capabilities: vec![],
        is_local: false,
    };
    assert_eq!(meta.name, "gpt-4o-mini");
    assert_eq!(meta.context_window, 128_000);
    assert!(meta.input_cost_per_million > 0.0);
}

// ============================================================================
// REVERSE PATH — Missing/unknown inputs
// ============================================================================

#[test]
fn test_router_handles_unknown_agent() {
    let config = ZenConfig {
        default_provider: Some("ollama".into()),
        ..Default::default()
    };
    let router = DefaultRouter::from_agentic(&config);
    let chain = router.build_fallback_chain("nonexistent_agent_xyz");
    assert!(!chain.is_empty(), "should fall back to default provider");
    assert_eq!(chain[0].0, Provider::Ollama);
}

#[test]
fn test_router_handles_default_only_config() {
    let config = ZenConfig {
        default_provider: Some("ollama".into()),
        ..Default::default()
    };
    let router = DefaultRouter::from_agentic(&config);
    let chain = router.build_fallback_chain("any_agent");
    assert_eq!(chain.len(), 2, "should have primary + mock fallback");
    assert_eq!(chain[0].0, Provider::Ollama);
    assert_eq!(chain[1].0, Provider::Mock);
}

#[test]
fn test_router_handles_agent_without_provider() {
    let config = ZenConfig {
        default_provider: Some("anthropic".into()),
        default_model: Some("claude-3-opus".into()),
        agents: {
            let mut map = HashMap::new();
            map.insert(
                "no_provider_agent".into(),
                AgentConfig {
                    provider: None,
                    model: None,
                    fallbacks: vec![],
                    retry_policy: None,
                    sensitivity: None,
                    variant: None,
                    temperature: None,
                    max_tokens: None,
                },
            );
            map
        },
        ..Default::default()
    };
    let router = DefaultRouter::from_agentic(&config);
    let chain = router.build_fallback_chain("no_provider_agent");
    assert_eq!(chain[0].0, Provider::Anthropic);
}

// ============================================================================
// ADVERSARIAL PATH — Edge cases and extreme values
// ============================================================================

#[test]
fn test_model_metadata_with_zero_context() {
    let meta = ModelMetadata {
        name: "test".into(),
        provider: "test".into(),
        context_window: 0,
        input_cost_per_million: 0.0,
        output_cost_per_million: 0.0,
        capabilities: vec![],
        is_local: true,
    };
    assert_eq!(meta.context_window, 0);
}

#[test]
fn test_model_metadata_with_negative_costs() {
    let meta = ModelMetadata {
        name: "test".into(),
        provider: "test".into(),
        context_window: 4096,
        input_cost_per_million: -1.0,
        output_cost_per_million: -1.0,
        capabilities: vec![],
        is_local: false,
    };
    assert!(meta.input_cost_per_million < 0.0);
    assert!(meta.output_cost_per_million < 0.0);
}

// ============================================================================
// LOGIC TREE — Variant and branch coverage
// ============================================================================

#[test]
fn test_provider_all_variants_present() {
    let providers = vec![
        Provider::Ollama,
        Provider::OpenAI,
        Provider::Anthropic,
        Provider::Gemini,
        Provider::Mistral,
        Provider::DeepSeek,
        Provider::Aliyun,
        Provider::Groq,
        Provider::Perplexity,
        Provider::Moonshot,
        Provider::XAI,
        Provider::QQBot,
        Provider::Mock,
        // Provider::Unknown(String) has data — skip in this variant coverage test
    ];
    assert!(providers.len() >= 12, "should include all enum variants");
}

#[test]
fn test_fallback_step_creation() {
    let step = FallbackStep {
        provider: "openai".to_string(),
        model: Some("gpt-4o-mini".to_string()),
        timeout_secs: None,
        variant: None,
    };
    assert_eq!(step.provider, "openai");
    assert_eq!(step.model.as_deref(), Some("gpt-4o-mini"));
}

#[test]
fn test_fallback_step_without_model() {
    let step = FallbackStep {
        provider: "ollama".to_string(),
        model: None,
        timeout_secs: Some(30),
        variant: None,
    };
    assert_eq!(step.provider, "ollama");
    assert!(step.model.is_none());
    assert_eq!(step.timeout_secs, Some(30));
}
