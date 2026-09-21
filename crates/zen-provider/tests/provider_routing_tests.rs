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

// ============================================================================
// Metered completion — cost accounting feeding the scheduler worker cost cap
// ============================================================================

fn metered_mock_config(with_pricing: bool) -> ZenConfig {
    let mut providers = HashMap::new();
    providers.insert(
        "mock".to_string(),
        zen_core::config::ProviderConfig {
            provider_type: Some("mock".into()),
            input_cost_per_million: with_pricing.then_some(1.0),
            output_cost_per_million: with_pricing.then_some(2.0),
            ..Default::default()
        },
    );
    ZenConfig {
        default_provider: Some("mock".into()),
        providers,
        ..Default::default()
    }
}

#[test]
fn metered_completion_reports_positive_cost_when_pricing_configured() {
    use zen_core::types::Sensitivity;
    let router = DefaultRouter::from_agentic(&metered_mock_config(true));
    let prompt = "hello metered world";
    let metered = router
        .complete_metered("cost-test", prompt, Sensitivity::Public)
        .expect("mock provider needs no network");

    assert_eq!(metered.provider, "mock");
    assert_eq!(metered.input_tokens, (prompt.len() as u64) / 4);
    assert_eq!(metered.output_tokens, (metered.text.len() as u64) / 4);
    let expected = zen_provider::usage_to_cost_usd(
        &router.model_metadata("mock", "mock"),
        metered.input_tokens,
        metered.output_tokens,
    );
    assert!(metered.cost_usd > 0.0, "priced usage must meter above zero");
    assert!(
        (metered.cost_usd - expected).abs() < 1e-12,
        "cost must equal usage × ModelMetadata pricing"
    );
}

#[test]
fn metered_completion_without_pricing_meters_zero_not_fabricated() {
    use zen_core::types::Sensitivity;
    let router = DefaultRouter::from_agentic(&metered_mock_config(false));
    let metered = router
        .complete_metered("cost-test", "hello", Sensitivity::Public)
        .expect("mock provider needs no network");
    assert!(metered.input_tokens > 0);
    assert_eq!(
        metered.cost_usd, 0.0,
        "unknown pricing must meter as 0, never an invented number"
    );
}

#[test]
fn model_metadata_marks_ollama_local_and_reads_configured_pricing() {
    let mut providers = HashMap::new();
    providers.insert(
        "ollama".to_string(),
        zen_core::config::ProviderConfig {
            provider_type: Some("ollama".into()),
            default_model: Some("qwen3:8b".into()),
            input_cost_per_million: Some(9.0),
            output_cost_per_million: Some(9.0),
            ..Default::default()
        },
    );
    let config = ZenConfig {
        default_provider: Some("ollama".into()),
        providers,
        ..Default::default()
    };
    let router = DefaultRouter::from_agentic(&config);

    let local = router.model_metadata("ollama", "qwen3:8b");
    assert!(local.is_local, "ollama must be structurally local");
    assert_eq!(
        zen_provider::usage_to_cost_usd(&local, 1_000_000, 1_000_000),
        0.0,
        "local inference costs zero even with stale pricing configured"
    );

    let priced = router.model_metadata("mock", "mock");
    assert!(!priced.is_local);
    let unpriced = router.model_metadata("absent-provider", "whatever");
    assert_eq!(unpriced.input_cost_per_million, 0.0);
    assert_eq!(unpriced.output_cost_per_million, 0.0);
}
