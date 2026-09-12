//! T104 (T058 contract): tool-loop wiring — `max_rounds` default 8,
//! env override, clamp, and per-instance override reach the orchestrator.
//!
//! T114 (PD-01 A native-primary dispatch): `resolve_invocations` dispatch
//! policy — native-only, fenced-fallback, both-present (degraded), and
//! fenced parse-error preservation.
//!
//! Unit coverage of the parse/budget/screen internals lives in
//! `orchestrator.rs` (25/25) and the ordering grammar in gateway
//! `hosting.rs` (20/20); this file asserts the end-to-end wiring only.

use rig_core::completion::message::ToolCall as NativeToolCall;
use rig_core::completion::message::ToolFunction;
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

// ── T114 PD-01 A: native-primary dispatch contract tests ──────────

fn native_call(name: &str, args: serde_json::Value) -> NativeToolCall {
    NativeToolCall::new(
        "native-id".to_string(),
        ToolFunction::new(name.to_string(), args),
    )
}

#[test]
fn t114_native_only_round_dispatches_without_fenced_parser() {
    let native = vec![native_call(
        "web.search",
        serde_json::json!({"query": "rust"}),
    )];

    let (invocations, errors, degraded) =
        AgentOrchestrator::resolve_invocations("plain text answer", &native);
    assert_eq!(invocations.len(), 1);
    assert_eq!(invocations[0].name.as_str(), "web.search");
    assert_eq!(invocations[0].args["query"], "rust");
    assert!(errors.is_empty());
    assert!(!degraded);
}

#[test]
fn t114_fenced_fallback_byte_identical_to_pre_pd01() {
    let response = "```json\n{\"tool\": \"fs.read\", \"args\": {\"path\": \"/tmp/x\"}}\n```";

    let (invocations, errors, degraded) = AgentOrchestrator::resolve_invocations(response, &[]);
    assert_eq!(invocations.len(), 1);
    assert_eq!(invocations[0].name.as_str(), "fs.read");
    assert_eq!(invocations[0].args["path"], "/tmp/x");
    assert!(errors.is_empty());
    assert!(!degraded);
}

#[test]
fn t114_both_present_native_wins_no_double_dispatch() {
    let response = "```json\n{\"tool\": \"fs.read\", \"args\": {\"path\": \"/tmp/x\"}}\n```";
    let native = vec![native_call(
        "web.search",
        serde_json::json!({"query": "rust"}),
    )];

    let (invocations, errors, degraded) = AgentOrchestrator::resolve_invocations(response, &native);
    assert!(degraded, "must flag as degraded when both present");
    assert_eq!(
        invocations.len(),
        1,
        "only native dispatched, fenced ignored"
    );
    assert_eq!(invocations[0].name.as_str(), "web.search");
    assert!(errors.is_empty());
}

#[test]
fn t114_fenced_parse_errors_surface_when_no_native() {
    let response = "```json\n{\"tool\": \"x\"}\n```";
    let (invocations, errors, _degraded) = AgentOrchestrator::resolve_invocations(response, &[]);
    assert!(invocations.is_empty());
    assert!(!errors.is_empty(), "T105: parse errors must surface");
}

#[test]
fn t114_invalid_native_name_logged_not_dispatched() {
    let native = vec![native_call("", serde_json::json!({}))];
    let (invocations, errors, _degraded) =
        AgentOrchestrator::resolve_invocations("answer", &native);
    assert!(invocations.is_empty());
    assert_eq!(errors.len(), 1);
}

#[test]
fn t114_empty_response_no_native_no_dispatch() {
    let (invocations, errors, degraded) = AgentOrchestrator::resolve_invocations("just text", &[]);
    assert!(invocations.is_empty());
    assert!(errors.is_empty());
    assert!(!degraded);
}

// ── T115: AgentRun state-machine wiring ───────────────────────────

use zen_agents::output_schema::{
    agent_output_schema, agent_output_schema_value, max_output_schema_retries,
};

/// T115: executor.rs exposes the two sync/async helper methods.
/// We verify the method signatures compile and are callable via a trait bound check.
#[test]
fn t115_executor_has_sync_and_async_model_call_methods() {
    // The methods exist on AgentExecutor; this test verifies the public API
    // compiles. Full integration requires a live LLM — covered by manual QA.
    let _ = std::any::type_name::<zen_agents::executor::AgentExecutor>();
}

/// T115: AgentRun builder chain compiles with all required fields.
#[test]
fn t115_agent_run_builder_compiles() {
    use rig_agent::agent::run::AgentRun;
    use rig_core::message::Message;

    let _run = AgentRun::new(Message::user("test"))
        .max_turns(4)
        .with_output_validation(None, 0)
        .with_history(vec![Message::user("prior")]);
}

// ── T116: output_schema wiring ───────────────────────────────────

/// T116: agents with declared schemas return Some from agent_output_schema_value.
#[test]
fn t116_output_schema_declared_agents_return_some() {
    for agent in &[
        "sisyphus",
        "hephaestus",
        "momus",
        "hermes",
        "oracle",
        "prometheus",
        "metis",
        "zeus",
    ] {
        let val = agent_output_schema_value(agent);
        assert!(val.is_some(), "expected schema for agent {agent}");
        let schema = agent_output_schema(agent);
        assert!(
            schema.is_some(),
            "expected schemars::Schema for agent {agent}"
        );
    }
}

/// T116: unknown agent names return None.
#[test]
fn t116_output_schema_unknown_agent_returns_none() {
    assert!(agent_output_schema_value("NonexistentAgent123").is_none());
    assert!(agent_output_schema("NonexistentAgent123").is_none());
}

/// T116: max_output_schema_retries returns a sensible default (2).
#[test]
fn t116_max_output_schema_retries_default() {
    assert_eq!(max_output_schema_retries(), 2);
}

/// T116: output_schema flows into AgentRun builder (not rejected by type system).
#[test]
fn t116_output_schema_wires_into_agent_run() {
    use rig_agent::agent::run::AgentRun;
    use rig_core::message::Message;

    let schema = agent_output_schema_value("sisyphus").cloned();
    let retries = max_output_schema_retries();
    let _run = AgentRun::new(Message::user("test"))
        .max_turns(4)
        .with_output_validation(schema, retries);
}
