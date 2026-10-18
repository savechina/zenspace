//! W4.5e/W4.6: tool-batch isolation + multi-round context regression tests.
//!
//! PURPOSE: pin the audited fixes on the real orchestrator loop — a fenced
//! ```json tool call is parsed, dispatched, and fed back across rounds, so
//! these drive production code (not helpers).
//!
//! USAGE: `cargo nextest run -p zen-agents -E 'test(tool_batch_isolation)'`.
//!
//! DRIVER: `DefaultRouter::with_mock_response` embeds a canned reply; the
//! mock's `prompt_len=N` echo is the observable that proves per-round
//! accumulation reached prompt assembly.
//!
//! EXPECTED: an invalid tool name never suppresses a valid sibling, ≥2
//! invalid names survive the round, round-3 prompts carry round-1 results,
//! and mid-loop budget exhaustion breaks with a structured note.

mod common;

use zen_agents::execution::AgentExecution;
use zen_agents::orchestrator::AgentOrchestrator;
use zen_core::types::SessionContext;

fn scripted_router(reply: &str) -> zen_provider::DefaultRouter {
    zen_provider::DefaultRouter::new(zen_provider::LlmConfig {
        default_provider: Some("mock".to_string()),
        ..Default::default()
    })
    .with_mock_response(reply)
}

fn run(reply: &str, rounds: usize, budget: u64) -> AgentExecution {
    let orchestrator = AgentOrchestrator::with_token_budget(scripted_router(reply), budget)
        .with_tool_loop_config(rounds);
    let mut session = SessionContext::new("Sisyphus".to_string(), String::new());
    tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(orchestrator.execute(&mut session, "guard turn"))
        .expect("turn must complete")
}

fn fenced(tool: &str, args: serde_json::Value) -> String {
    let payload =
        serde_json::to_string(&serde_json::json!({ "tool": tool, "args": args })).expect("json");
    format!("```json\n{payload}\n```")
}

fn fenced_array(calls: &[(&str, serde_json::Value)]) -> String {
    let items: Vec<serde_json::Value> = calls
        .iter()
        .map(|(tool, args)| serde_json::json!({ "tool": tool, "args": args }))
        .collect();
    format!(
        "```json\n{}\n```",
        serde_json::to_string(&items).expect("json")
    )
}

fn prompt_len(execution: &AgentExecution) -> usize {
    const MARKER: &str = "prompt_len=";
    let start = execution
        .response
        .find(MARKER)
        .expect("mock reply carries prompt_len")
        + MARKER.len();
    execution.response[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .expect("prompt_len parses")
}

#[test]
fn invalid_name_does_not_suppress_valid_sibling() {
    let _guard = common::begin();
    let reply = fenced_array(&[
        ("tier1_search", serde_json::json!({ "query": "x" })),
        ("fs.read", serde_json::json!({})),
    ]);
    let execution = run(&reply, 1, 10_000_000);

    let rejected = execution
        .tool_calls
        .iter()
        .find(|c| c.tool_name == "tier1_search")
        .expect("invalid name recorded");
    assert!(
        rejected.result.contains("tool not available"),
        "invalid name carries a per-call reason: {rejected:?}"
    );
    let dispatched = execution
        .tool_calls
        .iter()
        .find(|c| c.tool_name == "fs.read")
        .expect("valid sibling dispatched");
    assert!(
        !dispatched.result.is_empty(),
        "valid sibling's result must be preserved: {:?}",
        execution.tool_calls
    );
}

#[test]
fn two_invalid_names_round_survives() {
    let _guard = common::begin();
    let reply = fenced_array(&[
        ("tier1_search", serde_json::json!({})),
        ("bogus.missing.tool", serde_json::json!({})),
    ]);
    let execution = run(&reply, 1, 10_000_000);

    let invalid: Vec<&str> = execution
        .tool_calls
        .iter()
        .filter(|c| c.result.contains("tool not available"))
        .map(|c| c.tool_name.as_str())
        .collect();
    assert!(
        invalid.contains(&"tier1_search") && invalid.contains(&"bogus.missing.tool"),
        "both invalid names answered per-call (no protocol violation): {invalid:?}"
    );
}

#[test]
fn multi_round_prompt_carries_prior_results() {
    let _guard = common::begin();
    let reply = fenced("fs.read", serde_json::json!({}));
    let one = run(&reply, 1, 10_000_000);
    let three = run(&reply, 3, 10_000_000);
    assert!(
        prompt_len(&three) > prompt_len(&one),
        "round-3 prompt must include round-1 results: one={} three={}",
        prompt_len(&one),
        prompt_len(&three)
    );
}

#[test]
fn budget_exhaustion_breaks_with_note() {
    let _guard = common::begin();
    let reply = fenced("fs.read", serde_json::json!({}));
    let execution = run(&reply, 3, 1200);
    assert!(
        execution
            .tool_calls
            .iter()
            .any(|c| c.tool_name == "<budget>" && c.result.contains("budget_exhausted")),
        "mid-loop budget exhaustion must break with a structured note: {:?}",
        execution.tool_calls
    );
}
