// PURPOSE: Pin that the `&dyn LlmRouter` probe path actually probes. The
//          free helper `zen_provider::is_local_llm_available(&dyn LlmRouter)`
//          used to hit the trait-default `false` because `DefaultRouter`'s
//          probing inherent method was not in the trait impl — so the L1
//          local rungs (decision/mod.rs) and the de-anchored review's local
//          hop (orchestrator.rs) structurally never probed. `DefaultRouter`
//          now overrides the trait method to delegate to the inherent probe,
//          which reaches ONLY the dedicated-threaded Ollama health check.
// USAGE: `cargo nextest run -p zen-provider --test local_llm_probe_dyn`
// EXPECTED: Both the inherent and the dyn-dispatched call return `false` for
//           an unreachable local provider (deterministic connection-refused).
// ERRORS: If the override is removed, the dyn assertion fails while the
//         inherent assertion still passes — that split is the regression.

use zen_core::config::{ProviderConfig, ZenConfig};
use zen_provider::{DefaultRouter, LlmRouter};

/// Port 1 on loopback is unassigned — connection-refused is immediate and
/// deterministic. The provider pins `no_proxy` for loopback, so a
/// machine-level proxy cannot intercept the probe (T217 lesson).
const UNREACHABLE: &str = "http://127.0.0.1:1";

fn unreachable_router() -> DefaultRouter {
    DefaultRouter::from_agentic(&ZenConfig {
        default_provider: Some("ollama".to_string()),
        providers: std::collections::HashMap::from([(
            "ollama".to_string(),
            ProviderConfig {
                provider_type: Some("ollama".to_string()),
                base_url: Some(UNREACHABLE.to_string()),
                default_model: Some("test-model".to_string()),
                ..Default::default()
            },
        )]),
        ..Default::default()
    })
}

#[test]
fn inherent_probe_reports_unreachable_local_llm_unavailable() {
    let router = unreachable_router();
    assert!(
        !DefaultRouter::is_local_llm_available(&router),
        "unreachable local provider must report unavailable"
    );
}

#[test]
fn dyn_probe_actually_probes_instead_of_trait_default() {
    let router = unreachable_router();
    let dyn_router: &dyn LlmRouter = &router;
    assert!(
        !zen_provider::is_local_llm_available(dyn_router),
        "the dyn path must probe (and see the unreachable provider), not default to false"
    );
}
