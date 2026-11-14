//! W9.3 `plan.control` — cancel/append control surface for live plans
//! (006-agentic-orchestration D24).
//!
//! Cancel flips the `workflow_plans` row to the terminal `cancelled` value
//! and signals the in-process executor through a watch channel; the
//! executor's layer walk observes the signal at batch boundaries (and via
//! `select!` mid-batch) and finalizes with the W6.2b vocabulary: in-flight
//! tasks `failed`, never-started tasks `skipped`. Append extends a
//! `running` plan's task ledger between batches — refused while a batch is
//! in flight, never editing an in-flight task row.
//!
//! Mutating for the quality gate (the orchestrator whitelist excludes this
//! tool name — see the W9.5-F1 pin). Sisyphus-only grant, mirroring
//! `plan.execute`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};

use crate::delegate_task::{DELEGATE_ROOT_PARENT, DelegateRequest, DelegateTaskTool};

pub const PLAN_CONTROL_TOOL_NAME: &str = "plan.control";

/// Per-live-plan executor state: the cancel watch sender and the batch
/// gate. Shared between the executor's registration guard and the control
/// tool's signals.
pub struct PlanLiveState {
    cancel_tx: tokio::sync::watch::Sender<bool>,
    in_batch: AtomicBool,
    live_guards: AtomicUsize,
}

static LIVE_PLANS: OnceLock<Mutex<HashMap<String, std::sync::Arc<PlanLiveState>>>> =
    OnceLock::new();

fn registry() -> &'static Mutex<HashMap<String, std::sync::Arc<PlanLiveState>>> {
    LIVE_PLANS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// RAII executor registration: the plan stays "live" in the registry until
/// the last guard for its id drops. A second registration for the same id
/// JOINS the existing state (same watch channel) so a pre-signal cancels
/// the joiner immediately.
pub struct LivePlanGuard {
    plan_id: String,
    state: std::sync::Arc<PlanLiveState>,
}

impl Drop for LivePlanGuard {
    fn drop(&mut self) {
        if self.state.live_guards.fetch_sub(1, Ordering::SeqCst) == 1
            && let Ok(mut map) = registry().lock()
        {
            map.remove(&self.plan_id);
        }
    }
}

/// Register a live plan (create-or-join) and return the subscription
/// receiver for the executor's cancel observations plus the owning guard.
pub fn register_live_plan(plan_id: &str) -> (LivePlanGuard, tokio::sync::watch::Receiver<bool>) {
    let mut map = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let state = map
        .entry(plan_id.to_string())
        .or_insert_with(|| {
            let (tx, _rx) = tokio::sync::watch::channel(false);
            std::sync::Arc::new(PlanLiveState {
                cancel_tx: tx,
                in_batch: AtomicBool::new(false),
                live_guards: AtomicUsize::new(0),
            })
        })
        .clone();
    state.live_guards.fetch_add(1, Ordering::SeqCst);
    let rx = state.cancel_tx.subscribe();
    (
        LivePlanGuard {
            plan_id: plan_id.to_string(),
            state,
        },
        rx,
    )
}

/// Batch gate: the executor sets `true` while a layer batch is in flight,
/// `false` between batches. Append consults this to refuse mid-batch.
pub fn set_in_batch(plan_id: &str, in_batch: bool) {
    let map = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(state) = map.get(plan_id) {
        state.in_batch.store(in_batch, Ordering::SeqCst);
    }
}

/// Signal cancellation to a live executor. `false` when the plan is not
/// live in this process (the DB row flip is the source of truth then).
pub fn signal_cancel(plan_id: &str) -> bool {
    let map = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match map.get(plan_id) {
        Some(state) => {
            let _ = state.cancel_tx.send(true);
            true
        }
        None => false,
    }
}

/// Whether a live executor currently holds a batch in flight for the plan.
pub fn is_live_between_batches(plan_id: &str) -> Option<bool> {
    let map = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    map.get(plan_id).map(|s| !s.in_batch.load(Ordering::SeqCst))
}

/// Emit the one additive audit line for a control cancellation.
fn append_cancel_audit(plan_id: &str) {
    let user_root = zen_core::paths::user_root();
    let line = serde_json::json!({
        "kind": "loop.plan.cancelled",
        "plan_id": plan_id,
        "ts": chrono::Utc::now().timestamp(),
    });
    let path = user_root.join("logs").join("audit.jsonl");
    let _ = zen_core::jsonl::append_jsonl_line(&path, &line);
}

/// W9.3 control tool: `{op: "cancel"|"append", plan_id, tasks?}`.
pub struct PlanControlTool {
    delegate: std::sync::Arc<DelegateTaskTool>,
    state_db: Option<PathBuf>,
    db: OnceLock<Option<std::sync::Arc<zen_repo::SqliteClient>>>,
}

impl PlanControlTool {
    pub fn new(delegate: std::sync::Arc<DelegateTaskTool>, state_db: Option<PathBuf>) -> Self {
        Self {
            delegate,
            state_db,
            db: OnceLock::new(),
        }
    }

    async fn db(&self) -> Option<&std::sync::Arc<zen_repo::SqliteClient>> {
        if let Some(client) = self.db.get() {
            return client.as_ref();
        }
        let path = self.state_db.as_ref()?;
        let client = zen_repo::SqliteClient::open_lazy(path).await.ok()?;
        let _ = self.db.set(Some(std::sync::Arc::new(client)));
        self.db.get().and_then(|c| c.as_ref())
    }
}

#[async_trait::async_trait]
impl Tool for PlanControlTool {
    fn schema(&self) -> ToolSchema {
        rig_compose::ToolSchema {
            name: PLAN_CONTROL_TOOL_NAME.to_string(),
            description: "Control a running multi-step plan. op=cancel stops a \
                running plan (terminal `cancelled`; in-flight tasks fail, pending \
                tasks skip). op=append adds new draft tasks to a running plan's \
                ledger — only between batches; a batch in flight refuses the \
                append. Never edits an in-flight task."
                .to_string(),
            args_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "op": {
                        "type": "string",
                        "enum": ["cancel", "append"],
                        "description": "cancel: stop the plan. append: extend its task ledger"
                    },
                    "plan_id": {
                        "type": "string",
                        "description": "The plan id (from plan.execute's result or plan.todos op=get)"
                    },
                    "tasks": {
                        "type": "array",
                        "description": "append only: new draft tasks with stable ids",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string" },
                                "agent": { "type": "string" },
                                "prompt": { "type": "string" },
                                "depends_on": {
                                    "type": "array",
                                    "items": { "type": "string" }
                                }
                            },
                            "required": ["id", "agent", "prompt"]
                        }
                    }
                },
                "required": ["op", "plan_id"]
            }),
            result_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "cancelled": { "type": "boolean" },
                    "appended": { "type": "integer" },
                    "plan_id": { "type": "string" },
                    "error": { "type": "string" }
                }
            }),
        }
    }

    async fn invoke(&self, args: serde_json::Value) -> Result<serde_json::Value, KernelError> {
        let op = args.get("op").and_then(|v| v.as_str()).unwrap_or("");
        let Some(plan_id) = args.get("plan_id").and_then(|v| v.as_str()) else {
            return Ok(serde_json::json!({
                "error": "plan.control requires `plan_id`"
            }));
        };
        let Some(db) = self.db().await else {
            return Ok(serde_json::json!({
                "error": "plan store unavailable; plan.control needs state.db"
            }));
        };
        let repo = zen_repo::WorkflowRepo::new(db);
        let now = chrono::Utc::now().timestamp();
        match op {
            "cancel" => {
                let row = match repo.load_plan(plan_id).await {
                    Ok(Some(row)) => row,
                    Ok(None) => {
                        return Ok(serde_json::json!({
                            "error": format!("unknown plan {plan_id}")
                        }));
                    }
                    Err(e) => {
                        return Ok(serde_json::json!({
                            "error": format!("plan store read failed: {e}")
                        }));
                    }
                };
                if row.status != "running" {
                    return Ok(serde_json::json!({
                        "error": format!(
                            "plan {plan_id} already terminal ({}) — cancel refused",
                            row.status
                        )
                    }));
                }
                let flipped = repo
                    .cancel_running_plan(plan_id, now)
                    .await
                    .unwrap_or(false);
                let _signalled = signal_cancel(plan_id);
                append_cancel_audit(plan_id);
                Ok(serde_json::json!({
                    "cancelled": true,
                    "plan_id": plan_id,
                    "row_flipped": flipped,
                }))
            }
            "append" => {
                // Between-batches gate only — a live executor holding a
                // batch refuses; registry-miss (rounds between invokes)
                // falls through to the row-status gate.
                if let Some(false) = is_live_between_batches(plan_id) {
                    return Ok(serde_json::json!({
                        "error": format!(
                            "plan {plan_id} has a batch in flight — append only between batches"
                        )
                    }));
                }
                let row = match repo.load_plan(plan_id).await {
                    Ok(Some(row)) => row,
                    Ok(None) => {
                        return Ok(serde_json::json!({
                            "error": format!("unknown plan {plan_id}")
                        }));
                    }
                    Err(e) => {
                        return Ok(serde_json::json!({
                            "error": format!("plan store read failed: {e}")
                        }));
                    }
                };
                if row.status != "running" {
                    return Ok(serde_json::json!({
                        "error": format!(
                            "plan {plan_id} already terminal ({}) — append refused",
                            row.status
                        )
                    }));
                }
                let Some(drafts) = args.get("tasks").and_then(|v| v.as_array()) else {
                    return Ok(serde_json::json!({
                        "error": "append requires a `tasks` array"
                    }));
                };
                let existing = repo.load_task_rows(plan_id).await.unwrap_or_default();
                let mut validated: Vec<zen_repo::TaskDraft> = Vec::new();
                for d in drafts {
                    let Some(id) = d.get("id").and_then(|v| v.as_str()) else {
                        return Ok(serde_json::json!({
                            "error": "appended task requires `id`"
                        }));
                    };
                    let Some(agent) = d.get("agent").and_then(|v| v.as_str()) else {
                        return Ok(serde_json::json!({
                            "error": format!("appended task `{id}` requires `agent`")
                        }));
                    };
                    let Some(prompt) = d.get("prompt").and_then(|v| v.as_str()) else {
                        return Ok(serde_json::json!({
                            "error": format!("appended task `{id}` requires `prompt`")
                        }));
                    };
                    let depends_on: Vec<String> = d
                        .get("depends_on")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    // Known agent — the delegate validator is the single
                    // source of truth (same check a dispatch would hit).
                    let req = DelegateRequest {
                        agent: agent.to_string(),
                        prompt: prompt.to_string(),
                        description: None,
                        independent: true,
                        artifact: None,
                        fork_mode: None,
                        model_hint: None,
                    };
                    if let Some(error) = self.delegate.validate_task(DELEGATE_ROOT_PARENT, 0, &req)
                    {
                        return Ok(serde_json::json!({
                            "error": format!("appended task `{id}` rejected: {error}")
                        }));
                    }
                    // Dependencies must reference existing ledger rows or
                    // sibling drafts in THIS append — an unknown dep would
                    // strand the task forever.
                    for dep in &depends_on {
                        let known = existing.iter().any(|r| &r.task_id == dep)
                            || validated.iter().any(|v| &v.id == dep);
                        if !known {
                            return Ok(serde_json::json!({
                                "error": format!(
                                    "appended task `{id}` depends on unknown task `{dep}`"
                                )
                            }));
                        }
                    }
                    if existing.iter().any(|r| r.task_id == id)
                        || validated.iter().any(|v| v.id == id)
                    {
                        return Ok(serde_json::json!({
                            "error": format!("appended task `{id}` duplicates an existing ledger row")
                        }));
                    }
                    validated.push(zen_repo::TaskDraft {
                        id: id.to_string(),
                        status: "pending".to_string(),
                    });
                }
                if validated.is_empty() {
                    return Ok(serde_json::json!({
                        "error": "append requires at least one task"
                    }));
                }
                match repo.append_tasks(plan_id, &validated, now).await {
                    Ok(n) => Ok(serde_json::json!({
                        "appended": n,
                        "plan_id": plan_id,
                    })),
                    Err(e) => Ok(serde_json::json!({
                        "error": format!("append failed: {e}")
                    })),
                }
            }
            other => Ok(serde_json::json!({
                "error": format!("unknown op `{other}` — cancel or append")
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn control_tool(state_db: PathBuf) -> PlanControlTool {
        use std::sync::Mutex;
        use std::time::Duration;
        let delegate = crate::delegate_task::DelegateTaskTool::new(
            std::sync::Arc::new(crate::wiring::ZenWiring::new()),
            zen_provider::DefaultRouter::new(zen_provider::LlmConfig {
                default_provider: Some("mock".to_string()),
                ..Default::default()
            }),
            Vec::new(),
            None,
            std::sync::Arc::new(Mutex::new(zen_core::types::Sensitivity::Public)),
            std::sync::Arc::new(Mutex::new(Vec::new())),
            std::sync::Arc::new(Mutex::new(Vec::new())),
            std::sync::Arc::new(rig_compose::budget::AtomicTokenBudget::new(100_000)),
            Duration::from_secs(30),
            1,
            4,
        );
        PlanControlTool::new(std::sync::Arc::new(delegate), Some(state_db))
    }

    async fn running_plan(state_db: &std::path::Path, plan_id: &str) {
        let client = zen_repo::SqliteClient::open(state_db).await.unwrap();
        let repo = zen_repo::WorkflowRepo::new(&client);
        repo.create_plan(plan_id, None, "{}", 1_000).await.unwrap();
    }

    // W9.3: append while a live executor holds a batch is refused —
    // the between-batches gate is the acceptance contract.
    #[tokio::test]
    async fn append_during_live_batch_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ZEN_HOME", dir.path()) };
        let state_db = dir.path().join("state.db");
        running_plan(&state_db, "p1").await;
        let (_guard, _rx) = register_live_plan("p1");
        set_in_batch("p1", true);
        let tool = control_tool(state_db);
        let out = tool
            .invoke(serde_json::json!({
                "op": "append",
                "plan_id": "p1",
                "tasks": [{ "id": "x", "agent": "Explore", "prompt": "more" }]
            }))
            .await
            .unwrap();
        assert!(
            out["error"]
                .as_str()
                .is_some_and(|e| e.contains("batch in flight")),
            "{out}"
        );
        unsafe { std::env::remove_var("ZEN_HOME") };
    }

    // W9.3: between batches (or with no live executor in-process) the
    // append lands as pending ledger rows.
    #[tokio::test]
    async fn append_between_batches_extends_the_ledger() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ZEN_HOME", dir.path()) };
        let state_db = dir.path().join("state.db");
        running_plan(&state_db, "p1").await;
        let (_guard, _rx) = register_live_plan("p1");
        set_in_batch("p1", false);
        let tool = control_tool(state_db.clone());
        let out = tool
            .invoke(serde_json::json!({
                "op": "append",
                "plan_id": "p1",
                "tasks": [{ "id": "x", "agent": "Explore", "prompt": "more" }]
            }))
            .await
            .unwrap();
        assert_eq!(out["appended"], serde_json::json!(1), "{out}");
        let client = zen_repo::SqliteClient::open(&state_db).await.unwrap();
        let repo = zen_repo::WorkflowRepo::new(&client);
        let rows = repo.load_task_rows("p1").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "pending");
        // W6.2: draft rows carry no agent until dispatch (agent-drift
        // replay rule) — append lands pending scaffolding only.
        assert_eq!(rows[0].agent, "");
        unsafe { std::env::remove_var("ZEN_HOME") };
    }

    // W9.3: append validates against the same task contract as dispatch —
    // unknown agents and unknown dependencies are structured refusals.
    #[tokio::test]
    async fn append_rejects_unknown_agent_and_unknown_dependency() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ZEN_HOME", dir.path()) };
        let state_db = dir.path().join("state.db");
        running_plan(&state_db, "p1").await;
        let tool = control_tool(state_db);
        let out = tool
            .invoke(serde_json::json!({
                "op": "append",
                "plan_id": "p1",
                "tasks": [{ "id": "x", "agent": "NoSuchAgent", "prompt": "?" }]
            }))
            .await
            .unwrap();
        assert!(
            out["error"]
                .as_str()
                .is_some_and(|e| e.contains("rejected")),
            "unknown agent refused: {out}"
        );
        let tool = control_tool(dir.path().join("state.db"));
        let out = tool
            .invoke(serde_json::json!({
                "op": "append",
                "plan_id": "p1",
                "tasks": [{
                    "id": "x",
                    "agent": "Explore",
                    "prompt": "?",
                    "depends_on": ["ghost"]
                }]
            }))
            .await
            .unwrap();
        assert!(
            out["error"]
                .as_str()
                .is_some_and(|e| e.contains("unknown task `ghost`")),
            "unknown dep refused: {out}"
        );
        unsafe { std::env::remove_var("ZEN_HOME") };
    }

    // W9.3: cancel flips a running row to the terminal `cancelled` value
    // and signals the live executor's watch.
    #[tokio::test]
    async fn cancel_flips_running_row_and_signals_live_executor() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ZEN_HOME", dir.path()) };
        let state_db = dir.path().join("state.db");
        running_plan(&state_db, "p1").await;
        let (_guard, rx) = register_live_plan("p1");
        let tool = control_tool(state_db.clone());
        let out = tool
            .invoke(serde_json::json!({ "op": "cancel", "plan_id": "p1" }))
            .await
            .unwrap();
        assert_eq!(out["cancelled"], serde_json::json!(true), "{out}");
        let client = zen_repo::SqliteClient::open(&state_db).await.unwrap();
        let repo = zen_repo::WorkflowRepo::new(&client);
        let row = repo.load_plan("p1").await.unwrap().unwrap();
        assert_eq!(row.status, "cancelled");
        assert!(*rx.borrow(), "live executor observed the signal");
        unsafe { std::env::remove_var("ZEN_HOME") };
    }

    // W9.3: cancel of a terminal or unknown plan is a structured error —
    // never a silent overwrite of a recorded outcome.
    #[tokio::test]
    async fn cancel_of_terminal_or_unknown_plan_is_structured_error() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ZEN_HOME", dir.path()) };
        let state_db = dir.path().join("state.db");
        running_plan(&state_db, "p1").await;
        let client = zen_repo::SqliteClient::open(&state_db).await.unwrap();
        let repo = zen_repo::WorkflowRepo::new(&client);
        repo.cancel_running_plan("p1", 2_000).await.unwrap();
        let tool = control_tool(state_db);
        let out = tool
            .invoke(serde_json::json!({ "op": "cancel", "plan_id": "p1" }))
            .await
            .unwrap();
        assert!(
            out["error"]
                .as_str()
                .is_some_and(|e| e.contains("already terminal")),
            "{out}"
        );
        let out = tool
            .invoke(serde_json::json!({ "op": "cancel", "plan_id": "ghost" }))
            .await
            .unwrap();
        assert!(
            out["error"]
                .as_str()
                .is_some_and(|e| e.contains("unknown plan")),
            "{out}"
        );
        unsafe { std::env::remove_var("ZEN_HOME") };
    }

    /// W11.12/T13 (G6): a cancel arriving between register_live_plan and the
    /// executor's first watch check LANDS (watch join semantics), and the
    /// last guard drop removes the registry entry — deterministic, no sleeps.
    #[tokio::test]
    async fn cancel_between_register_and_first_watch_check_lands() {
        let (guard, mut rx) = register_live_plan("p-join-cancel");
        // The pre-signal arrives before the executor's first watch check.
        assert!(signal_cancel("p-join-cancel"), "plan is live");
        // watch::Receiver observes the change deterministically.
        rx.changed().await.expect("watch channel open");
        assert!(*rx.borrow_and_update(), "the pre-signal cancel must land");
        drop(guard);
        // Last guard drop removes the entry: a further signal finds nothing.
        assert!(
            !signal_cancel("p-join-cancel"),
            "entry removed after the last guard drops"
        );
    }

    /// W11.12/T13 (G6): a second registration JOINS the same watch channel;
    /// the entry survives until the LAST guard drops.
    #[tokio::test]
    async fn join_shares_channel_and_entry_survives_until_last_guard() {
        let (g1, _rx1) = register_live_plan("p-join-shared");
        let (g2, mut rx2) = register_live_plan("p-join-shared");
        assert!(signal_cancel("p-join-shared"));
        // The joiner observes the same cancel on its own receiver.
        rx2.changed().await.expect("watch channel open");
        assert!(*rx2.borrow_and_update(), "joiner shares the cancel channel");
        drop(g1);
        // One guard remains: the entry is still live.
        assert!(
            signal_cancel("p-join-shared"),
            "entry survives one guard drop"
        );
        drop(g2);
        assert!(
            !signal_cancel("p-join-shared"),
            "entry removed only after the last guard drops"
        );
    }
}
