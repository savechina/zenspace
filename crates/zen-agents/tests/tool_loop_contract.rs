//! T104 (T058 contract): tool-loop wiring — `max_rounds` default 8,
//! env override, clamp, and per-instance override reach the orchestrator.
//!
//! Unit coverage of the parse/budget/screen internals lives in
//! `orchestrator.rs` (25/25) and the ordering grammar in gateway
//! `hosting.rs` (20/20); this file asserts the end-to-end wiring only.

use zen_agents::orchestrator::AgentOrchestrator;
use zen_core::config::{invalidate_config_cache, load_config};
use zen_provider::{DefaultRouter, LlmConfig};

fn orchestrator() -> AgentOrchestrator {
    AgentOrchestrator::new(DefaultRouter::new(LlmConfig::default()))
}

/// All env-dependent phases run in ONE test — sibling tests in this
/// binary share the process env, so parallel env mutation would race.
#[test]
fn tool_loop_wiring_default_env_override_and_instance_override() {
    // SAFETY: test-only env mutation; no other test in this binary reads
    // ZEN_TOOL_MAX_ROUNDS, and the var is removed at the end.
    unsafe { std::env::remove_var("ZEN_TOOL_MAX_ROUNDS") };
    invalidate_config_cache();
    assert_eq!(
        load_config()
            .expect("config loads")
            .agentic
            .tool_loop
            .max_rounds_or_default(),
        8
    );
    assert_eq!(orchestrator().max_tool_rounds(), 8);

    unsafe { std::env::set_var("ZEN_TOOL_MAX_ROUNDS", "3") };
    invalidate_config_cache();
    assert_eq!(orchestrator().max_tool_rounds(), 3);

    unsafe { std::env::set_var("ZEN_TOOL_MAX_ROUNDS", "99") };
    invalidate_config_cache();
    assert_eq!(orchestrator().max_tool_rounds(), 16);

    unsafe { std::env::remove_var("ZEN_TOOL_MAX_ROUNDS") };
    invalidate_config_cache();
    assert_eq!(orchestrator().with_tool_loop_config(5).max_tool_rounds(), 5);
    assert_eq!(
        orchestrator().with_tool_loop_config(99).max_tool_rounds(),
        16
    );
}
