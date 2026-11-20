// T225 parity pin: the mock stand-in bypasses the Private/Confidential
// cloud-egress gate in `DefaultRouter::enforce_sensitivity`.
//
// `parse_provider_name("mock")` lands in `Provider::Unknown("mock")` (the
// router-side normalization `plan_synthesis::is_local_provider` already
// mirrors), so the bypass must be name-based — an enum-only check misses
// every offline harness and W12.15 plan/delegate sub-turn routed under a
// recorded Private policy.

use zen_core::config::ZenConfig;
use zen_core::types::Sensitivity;
use zen_provider::{DefaultRouter, LlmRouter, TaskRequirements};

fn mock_router() -> DefaultRouter {
    DefaultRouter::from_agentic(&ZenConfig {
        default_provider: Some("mock".into()),
        ..Default::default()
    })
}

fn requirements(sensitivity: Sensitivity) -> TaskRequirements {
    TaskRequirements {
        max_tokens: None,
        sensitivity,
        preferred_model: None,
        budget_limit: None,
    }
}

#[test]
fn unknown_mock_routes_under_private_without_local_llm() {
    let router = mock_router();
    let routed = router.route(&requirements(Sensitivity::Private)).unwrap();
    assert_eq!(routed.to_string(), "mock", "routed: {routed}");
}

#[test]
fn unknown_mock_routes_under_confidential_without_local_llm() {
    let router = mock_router();
    let routed = router
        .route(&requirements(Sensitivity::Confidential))
        .unwrap();
    assert_eq!(routed.to_string(), "mock", "routed: {routed}");
}

#[test]
fn real_cloud_provider_still_rejected_for_private_without_local_llm() {
    let router = DefaultRouter::from_agentic(&ZenConfig {
        default_provider: Some("openai".into()),
        ..Default::default()
    });
    let err = router
        .route(&requirements(Sensitivity::Private))
        .expect_err("cloud must stay gated for Private turns");
    assert!(
        err.to_string().contains("cannot be routed to cloud"),
        "gate rejection: {err}"
    );
}
