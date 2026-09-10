//! E2E: multi-agent orchestration pipeline.
//!
//! PURPOSE: Prove the full 006 chain over production code paths — intent
//! classification → routing → gated tool loop (delegate.task / plan.execute)
//! → real sub-agent LLM rounds → state.db persistence (workflow_plans /
//! workflow_tasks + claim fence) → quality gate → audit.jsonl.
//!
//! USAGE: `cargo nextest run -p zen-agents -E 'test(e2e_)'`.
//!
//! DRIVER: `DefaultRouter::with_mock_response` injects a static model reply
//! containing a fenced ```json tool call; only the top model is canned —
//! every layer below it (sub-agent build, sub-LLM rounds, DAG execution,
//! checkpointing, gate, audit) is the live implementation.
//!
//! EXPECTED: sub-agent responses carry the `[mock]` marker (proof of a real
//! LLM round-trip), workflow rows close out `completed`, resume is fenced,
//! and `loop.turn.review` / `loop.plan.completed` audit lines land.
//!
//! ERRORS: tests share one process-frozen ZEN_HOME (LazyLock constraint) and
//! serialize on a mutex (SQLite single-writer); a failure here usually means
//! env ordering, not product code.

use std::sync::{Mutex, MutexGuard, Once, OnceLock};
use tempfile::TempDir;
use zen_agents::execution::AgentExecution;
use zen_agents::orchestrator::AgentOrchestrator;
use zen_core::types::SessionContext;

static INIT: Once = Once::new();
static HOME: OnceLock<TempDir> = OnceLock::new();
static LOCK: Mutex<()> = Mutex::new(());

/// Hold for the whole test body: ZEN_HOME is process-frozen and state.db is
/// single-writer, so orchestration E2E tests must not interleave.
fn begin() -> MutexGuard<'static, ()> {
    let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    INIT.call_once(|| {
        let home = HOME.get_or_init(|| TempDir::new().expect("temp ZEN_HOME"));
        // SAFETY: single-threaded at this point (INIT once under LOCK); no
        // other thread reads the env concurrently, and value is valid UTF-8.
        unsafe { std::env::set_var("ZEN_HOME", home.path()) };
        zen_core::config::invalidate_config_cache();
    });
    guard
}

fn scripted_router(reply: &str) -> zen_provider::DefaultRouter {
    zen_provider::DefaultRouter::new(zen_provider::LlmConfig {
        default_provider: Some("mock".to_string()),
        ..Default::default()
    })
    .with_mock_response(reply)
}

/// One-dispatch orchestrator: the static reply emits the tool call once,
/// the loop dispatches it, and the round cap ends the turn deterministically.
fn orchestrator(reply: &str) -> AgentOrchestrator {
    AgentOrchestrator::with_token_budget(scripted_router(reply), 10_000_000)
        .with_tool_loop_config(1)
}

fn run_turn(reply: &str, query: &str) -> AgentExecution {
    let orchestrator = orchestrator(reply);
    let mut session = SessionContext::new("Sisyphus".to_string(), String::new());
    tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(orchestrator.execute(&mut session, query))
        .expect("turn must complete")
}

fn fenced_tool_call(tool: &str, args: serde_json::Value) -> String {
    let payload = serde_json::to_string(&serde_json::json!({ "tool": tool, "args": args }))
        .expect("tool call json");
    format!("On it.\n```json\n{payload}\n```\n")
}

fn delegate_args() -> serde_json::Value {
    serde_json::json!({
        "agent": "Hephaestus",
        "prompt": "Summarize the migration plan",
        "description": "e2e delegate leg"
    })
}

fn audit_jsonl() -> Vec<serde_json::Value> {
    let path = zen_core::paths::ZenPaths::detect()
        .expect("ZEN_HOME paths")
        .logs()
        .join("audit.jsonl");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[test]
fn e2e_delegation_sub_agent_turn_gated_and_audited() {
    let _guard = begin();
    let reply = fenced_tool_call("delegate.task", delegate_args());
    let execution = run_turn(&reply, "delegate the summary");

    let call = execution
        .tool_calls
        .iter()
        .find(|c| c.tool_name == "delegate.task")
        .expect("delegate.task must be dispatched through the gated tool loop");
    let out: serde_json::Value =
        serde_json::from_str(&call.result).expect("delegate output must be structured JSON");
    assert_eq!(out["agent"], "Hephaestus", "out: {out}");
    let sub_response = out["response"].as_str().expect("sub response string");
    assert!(
        sub_response.contains("[mock]"),
        "sub-agent response must come from a real LLM round: {sub_response}"
    );
    assert!(out.get("duration_ms").is_some(), "out: {out}");

    let notes = execution
        .metadata
        .quality_notes
        .expect("quality gate must run post-loop");
    assert!(notes.contains("Momus gate"), "notes: {notes}");

    let review = audit_jsonl()
        .into_iter()
        .find(|v| v["kind"] == "loop.turn.review")
        .expect("loop.turn.review audit line");
    assert!(review.get("intent_category").is_some(), "{review}");
    assert!(review.get("intent_source").is_some(), "{review}");
    assert_eq!(
        review["delivery_ready"].as_bool(),
        Some(execution.metadata.delivery_ready)
    );
}

#[test]
fn e2e_plan_dag_runs_tasks_persists_and_closes_out() {
    let _guard = begin();
    let reply = fenced_tool_call(
        "plan.execute",
        serde_json::json!({
            "name": "e2e-dag",
            "tasks": [
                { "id": "a", "agent": "Hephaestus", "prompt": "step a" },
                { "id": "b", "agent": "Hermes", "prompt": "step b", "depends_on": ["a"] },
                { "id": "c", "agent": "Hephaestus", "prompt": "step c", "depends_on": ["a"] }
            ]
        }),
    );
    let execution = run_turn(&reply, "kick off the dag");

    let call = execution
        .tool_calls
        .iter()
        .find(|c| c.tool_name == "plan.execute")
        .expect("plan.execute must be dispatched");
    let out: serde_json::Value =
        serde_json::from_str(&call.result).expect("plan output must be structured JSON");
    let tasks = out["tasks"].as_array().expect("tasks array");
    assert_eq!(tasks.len(), 3, "out: {out}");
    for task in tasks {
        assert_eq!(task["status"], "ok", "task {}: {}", task["id"], out);
        assert!(
            task["response"]
                .as_str()
                .unwrap_or_default()
                .contains("[mock]"),
            "task {} must reflect a real sub-agent LLM round: {task}",
            task["id"]
        );
    }
    assert!(out["hermes"].is_object(), "hermes verdict must ride output");

    let state_db = zen_core::paths::ZenPaths::detect()
        .expect("paths")
        .data()
        .join("state.db");
    let plan_id = audit_jsonl()
        .iter()
        .find(|v| v["kind"] == "loop.plan.completed")
        .expect("loop.plan.completed audit line")["plan_id"]
        .as_str()
        .expect("plan_id")
        .to_string();
    let (plan_row, task_rows) = tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(async {
            let client = zen_repo::SqliteClient::open_lazy(&state_db).await.unwrap();
            let repo = zen_repo::WorkflowRepo::new(&client);
            let plan = repo.load_plan(&plan_id).await.unwrap().expect("plan row");
            let tasks = repo.load_task_rows(&plan_id).await.unwrap();
            (plan, tasks)
        });
    assert_eq!(plan_row.status, "completed", "plan must close out");
    assert_eq!(task_rows.len(), 3, "all task checkpoints persisted");
    assert!(task_rows.iter().all(|t| t.status == "ok"));

    // Terminal plans refuse resume (mirrors plan_task.rs unit coverage).
    let resume_reply = fenced_tool_call(
        "plan.execute",
        serde_json::json!({
            "resume_plan_id": plan_id,
            "tasks": [ { "id": "a", "agent": "Hephaestus", "prompt": "x" } ]
        }),
    );
    let execution = run_turn(&resume_reply, "resume it");
    let call = execution
        .tool_calls
        .iter()
        .find(|c| c.tool_name == "plan.execute")
        .expect("resume dispatch");
    assert!(
        call.result.contains("already terminal"),
        "terminal plan must refuse resume: {}",
        call.result
    );
}

#[test]
fn e2e_resume_is_claim_fenced_mid_flight() {
    let _guard = begin();
    let plan_id = "e2e-fenced-plan";
    let state_db = zen_core::paths::ZenPaths::detect()
        .expect("paths")
        .data()
        .join("state.db");
    tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(async {
            let client = zen_repo::SqliteClient::open_lazy(&state_db).await.unwrap();
            let repo = zen_repo::WorkflowRepo::new(&client);
            // Real clock: a fake epoch would read as a stale (stealable) claim.
            let now = chrono::Utc::now().timestamp();
            repo.create_plan(plan_id, None, "{}", now).await.unwrap();
            assert!(repo.claim_plan(plan_id, "other-run", now).await.unwrap());
        });

    let resume_reply = fenced_tool_call(
        "plan.execute",
        serde_json::json!({
            "resume_plan_id": plan_id,
            "tasks": [ { "id": "a", "agent": "Hephaestus", "prompt": "x" } ]
        }),
    );
    let execution = run_turn(&resume_reply, "resume it");
    let call = execution
        .tool_calls
        .iter()
        .find(|c| c.tool_name == "plan.execute")
        .expect("resume dispatch");
    assert!(
        call.result.contains("already being resumed"),
        "a foreign live claim must fence the resume: {}",
        call.result
    );
}

#[test]
fn e2e_streaming_turn_emits_tool_frames() {
    let _guard = begin();
    let reply = fenced_tool_call("delegate.task", delegate_args());
    let orchestrator = orchestrator(&reply);
    let mut session = SessionContext::new("Sisyphus".to_string(), String::new());

    let mut streamed = String::new();
    let response = tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(
            orchestrator.execute_stream(&mut session, "delegate the summary", |chunk| {
                streamed.push_str(chunk)
            }),
        )
        .expect("streaming turn must complete");

    assert!(!response.is_empty());
    assert!(
        streamed.contains("🔧 delegate.task"),
        "tool-started frame must stream: {streamed}"
    );
    assert!(
        streamed.contains("✅ delegate.task"),
        "tool-completed frame must stream: {streamed}"
    );
}
