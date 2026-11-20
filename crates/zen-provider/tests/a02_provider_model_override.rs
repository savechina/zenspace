// PURPOSE: A-02 provider-side pins for the per-task runtime model override:
//          the shared `effective_model_name` resolver, the ollama delegating
//          helper, and the non-streaming router seam
//          `DefaultRouter::call_with_usage_with_model`.
// USAGE: cargo nextest run -p zen-provider --test a02_provider_model_override
// EXPECTED: PASS — None keeps the configured model and is byte-identical to
//           `call_with_usage`; Some(x) lands in `ModelOptions::model`.
// ERRORS: a red pin means the override stopped flowing to the model
//         construction layer, or the None path diverged from the plain call.

use zen_provider::providers::OllamaProvider;
use zen_provider::providers::effective_model_name;
use zen_provider::{DefaultRouter, LlmConfig, LlmRouter as _, Provider};

fn options_with(model: Option<&str>) -> zen_core::config::ModelOptions {
    zen_core::config::ModelOptions {
        model: model.map(str::to_string),
        ..Default::default()
    }
}

#[test]
fn effective_model_name_matrix() {
    let configured = "configured-model";
    assert_eq!(
        effective_model_name(configured, &options_with(None)),
        configured,
        "absent override keeps the configured default"
    );
    assert_eq!(
        effective_model_name(configured, &options_with(Some("override-model"))),
        "override-model",
        "present override wins"
    );
}

#[test]
fn ollama_effective_model_delegates_to_shared_helper() {
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434".to_string(),
        "qwen3.6:35b-mlx".to_string(),
    );
    let plain = options_with(None);
    assert_eq!(provider.effective_model(&plain), "qwen3.6:35b-mlx");
    assert_eq!(
        provider.effective_model(&plain),
        effective_model_name(&provider.model, &plain),
        "ollama must delegate to the shared helper"
    );

    let overridden = options_with(Some("llama3.2:2GB"));
    assert_eq!(provider.effective_model(&overridden), "llama3.2:2GB");
    assert_eq!(
        provider.effective_model(&overridden),
        effective_model_name(&provider.model, &overridden),
        "override path must match the shared helper too"
    );
}

#[test]
fn call_with_usage_with_model_none_is_byte_identical() {
    let router = DefaultRouter::new(LlmConfig::default()).with_mock_response("canned reply");
    let plain = router
        .call_with_usage(Provider::Mock, "hello model seam")
        .expect("mock provider needs no network");
    let via_none = router
        .call_with_usage_with_model(Provider::Mock, "hello model seam", None)
        .expect("mock provider needs no network");

    assert_eq!(via_none.text, plain.text);
    assert_eq!(via_none.usage, plain.usage);
}

#[test]
fn call_with_usage_with_model_some_carries_model_name() {
    // Options-layer pin: `call_with_usage_with_model` builds its request
    // options through `DefaultRouter::stream_options`, so Some(x) lands in
    // `ModelOptions::model` — the field every provider reads through
    // `effective_model_name`. Residual: this crate has no request-echo
    // fixture, so the wire-level "model" field is not observed here; the
    // provider honour is pinned by the matrix/delegation pins above plus the
    // per-provider call sites (`client.chat(effective_model_name(..))`).
    let router = DefaultRouter::new(LlmConfig::default());

    let overridden = router.stream_options("ollama", Some("override-model".to_string()));
    assert_eq!(overridden.model.as_deref(), Some("override-model"));

    let plain = router.stream_options("ollama", None);
    assert_eq!(plain.model, None, "None leaves options untouched");
}
