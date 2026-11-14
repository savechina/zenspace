//! `plan.todos` — the durable Sisyphus todo ledger + turn-plan lifecycle
//! (W6.2 / W6.2b; spec D21 §2, D24).
//!
//! One tool, two ops, over `workflow_tasks`/`workflow_plans` DIRECTLY (D24 §1
//! — no second ledger struct, no turn-end snapshot). Canonical task
//! vocabulary: `pending|progressing|completed|failed|skipped`.
//!
//! - Default: `set` mints one `workflow_plans` row (owner = session id,
//!   claim-at-create) plus its draft `workflow_tasks` rows (`agent = ''`
//!   until dispatch); `get` joins spec_json titles with row outcomes.
//! - Interaction: model status writes are UPDATE-only (a settled task's
//!   `response`/`error` survive); `set` is mint-once per session — a
//!   second `set` while a plan for this session is still `running` is a
//!   structured rejection (terminal plans allow a fresh mint).
//! - Sensitivity: mirrors the session's [`Sensitivity`] into `spec_json`
//!   (D24 §5 — future surfaces are safe by construction, gates nothing
//!   today). Unset mirror ⇒ `"unknown"`.
//!
//! The turn-plan registry ([`register_turn_plan`]/[`take_turn_plan`]) is
//! the cancel-path seam (D24 §2, design-review F3): the gateway drain
//! finalizes the session's plan rows synchronously because the orchestrator
//! future is dropped on cancel — nothing else runs to write terminal
//! states. The mark-only stale sweep is the backstop, not the primary.

use crate::delegate_task::SharedSensitivity;
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// Tool name.
pub const PLAN_TODOS_TOOL_NAME: &str = "plan.todos";

/// Canonical per-task status vocabulary (owner directive 2026-10-31).
pub const TASK_STATUSES: [&str; 5] = ["pending", "progressing", "completed", "failed", "skipped"];

/// Draft-item cap — the same 12-task ceiling `plan.execute` enforces
/// (`MAX_PLAN_TASKS`), so a minted ledger is always dispatchable.
pub const LEDGER_MAX_ITEMS: usize = crate::plan_task::MAX_PLAN_TASKS;

/// Resolve the state.db path for plan-ledger persistence. `ZenPaths::detect`
/// walks for a workspace marker and returns `None` in the launchd daemon
/// (cwd=/ — the normal hosted form), so the global tree is the fallback:
/// `user_root()/data/state.db`, the same file the daemon's knowledge stack
/// and replay checkpoint store open. `None` only when even the global root
/// is unresolvable (never in practice); callers degrade to a structured
/// error, never a panic.
pub fn resolve_state_db() -> Option<PathBuf> {
    if let Ok(p) = zen_core::paths::ZenPaths::detect() {
        return Some(p.data().join("state.db"));
    }
    let root = zen_core::paths::user_root();
    Some(root.join("data").join("state.db"))
}

// ---------------------------------------------------------------------------
// Turn-plan registry (D24 §2 drain-path finalizer seam)
// ---------------------------------------------------------------------------

/// What the cancel path needs to finalize a session's plan rows.
#[derive(Debug, Clone)]
pub struct TurnPlanRecord {
    pub plan_id: String,
    /// Task ids dispatched in the live batch whose checkpoints have not
    /// settled yet — they become `failed(error = "cancelled")`; every other
    /// non-terminal row becomes `skipped`.
    pub unsettled: Vec<String>,
    /// W6.3: a batch is executing on this plan row right now. The DB claim
    /// fence admits same-owner re-claims (crash recovery), so the
    /// second-batch refusal lives here — in-process, per session.
    batch_in_flight: bool,
}

static TURN_PLANS: OnceLock<Mutex<HashMap<String, TurnPlanRecord>>> = OnceLock::new();

fn turn_plans() -> &'static Mutex<HashMap<String, TurnPlanRecord>> {
    TURN_PLANS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register (or replace) the session's live plan record. Called at mint
/// ([`PlanTodosTool`]) and at `plan.execute`'s own create (claim-at-create
/// path) so a cancel can always find the rows.
pub fn register_turn_plan(session: &str, plan_id: &str, unsettled: Vec<String>) {
    let mut map = turn_plans()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    map.insert(
        session.to_string(),
        TurnPlanRecord {
            plan_id: plan_id.to_string(),
            unsettled,
            batch_in_flight: false,
        },
    );
}

/// W6.3 second-batch guard: marks the session's plan row as executing a
/// batch. `false` when no record matches `plan_id` or a batch is already
/// in flight — the caller refuses with a structured error.
pub fn try_begin_batch(session: &str, plan_id: &str) -> bool {
    let mut map = turn_plans()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match map.get_mut(session) {
        Some(record) if record.plan_id == plan_id && !record.batch_in_flight => {
            record.batch_in_flight = true;
            true
        }
        _ => false,
    }
}

/// Clears the in-flight marker (paired with [`try_begin_batch`]; the
/// [`BatchGuard`] drop path covers every early return in the executor).
pub fn end_batch(session: &str) {
    let mut map = turn_plans()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(record) = map.get_mut(session) {
        record.batch_in_flight = false;
    }
}

/// Releases the batch-in-flight marker on drop — every executor exit path
/// (run completion, ledger-item refusal, structured error) unmarks the
/// session exactly once.
pub struct BatchGuard {
    session: String,
}

impl BatchGuard {
    #[must_use]
    pub fn arm(session: &str) -> Self {
        Self {
            session: session.to_string(),
        }
    }
}

impl Drop for BatchGuard {
    fn drop(&mut self) {
        end_batch(&self.session);
    }
}

/// Update the unsettled-batch view (wave dispatch / settle points). A
/// stale record (different plan_id) is ignored — the newer mint wins.
pub fn update_turn_plan_unsettled(session: &str, plan_id: &str, unsettled: Vec<String>) {
    let mut map = turn_plans()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(record) = map.get_mut(session)
        && record.plan_id == plan_id
    {
        record.unsettled = unsettled;
    }
}

/// Take (remove) the session's record — the finalizer's consume-once read.
pub fn take_turn_plan(session: &str) -> Option<TurnPlanRecord> {
    let mut map = turn_plans()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    map.remove(session)
}

/// Non-consuming read (the gateway cancel path checks before paying for a
/// state.db open — non-plan turns must skip the DB entirely).
pub fn peek_turn_plan(session: &str) -> Option<TurnPlanRecord> {
    turn_plans()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(session)
        .cloned()
}

/// Clear the record after a NATURAL (non-cancel) terminal write — the
/// engine outcome already made the rows terminal; the registry must not
/// outlive the turn.
pub fn clear_turn_plan(session: &str, plan_id: &str) {
    let mut map = turn_plans()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if map
        .get(session)
        .map(|r| r.plan_id == plan_id)
        .unwrap_or(false)
    {
        map.remove(session);
    }
}

/// Drain-path finalizer (D24 §2): flip the session's live plan to terminal
/// `cancelled`, mark the in-flight batch `failed(reason)`, skip the
/// never-dispatched remainder. Consume-once via [`take_turn_plan`]; `false`
/// when the session has no live plan (nothing to finalize — the common case
/// for non-plan turns). Fail-open at the caller: a DB error here must never
/// block the cancel itself.
pub async fn finalize_cancelled_plan(
    client: &zen_repo::SqliteClient,
    session: &str,
    reason: &str,
    now: i64,
) -> anyhow::Result<bool> {
    let Some(record) = take_turn_plan(session) else {
        return Ok(false);
    };
    finalize_taken_plan(client, &record, reason, now).await?;
    Ok(true)
}

/// Finalize an already-taken record (the gateway path: peek → take → open
/// the client only when a record exists).
pub async fn finalize_taken_plan(
    client: &zen_repo::SqliteClient,
    record: &TurnPlanRecord,
    reason: &str,
    now: i64,
) -> anyhow::Result<()> {
    let repo = zen_repo::WorkflowRepo::new(client);
    // Engine-outcome-wins: only still-`running` plans flip; a batch that
    // settled between abort and finalize keeps its checkpoints.
    repo.cancel_running_plan(&record.plan_id, now).await?;
    if !record.unsettled.is_empty() {
        repo.fail_tasks_with_reason(&record.plan_id, &record.unsettled, reason, now)
            .await?;
    }
    repo.skip_remaining_tasks(&record.plan_id, now).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The tool
// ---------------------------------------------------------------------------

/// Sensitivity as its `spec_json` string (D24 §5).
fn sensitivity_tag(sensitivity: &zen_core::types::Sensitivity) -> &'static str {
    match sensitivity {
        zen_core::types::Sensitivity::Public => "public",
        zen_core::types::Sensitivity::Private => "private",
        zen_core::types::Sensitivity::Confidential => "confidential",
    }
}

/// One ledger item as the model supplies it (`set`) and as `get` returns it.
#[derive(Debug, Clone, serde::Serialize)]
struct LedgerItem {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    response: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Open the ledger client, degrading to a structured Ok-error payload —
/// never a `KernelError` (a persistence outage must not panic the round;
/// the model reads the error and adapts).
async fn open_client(
    state_db: Option<&PathBuf>,
) -> Result<zen_repo::SqliteClient, serde_json::Value> {
    let Some(path) = state_db else {
        return Err(json!({
            "error": "plan ledger unavailable: state.db path unresolvable",
            "plan.todos": true,
        }));
    };
    zen_repo::SqliteClient::open(path).await.map_err(|e| {
        json!({
            "error": format!("plan ledger unavailable: {e}"),
            "plan.todos": true,
        })
    })
}

/// The durable Sisyphus todo ledger (W6.2). Read-only for the quality gate
/// (G-21): it mutates only plan rows, not the world — classifying it
/// mutating would fire Metis→Momus→Hermes on every checklist tick.
#[derive(Clone)]
pub struct PlanTodosTool {
    state_db: Option<PathBuf>,
    sensitivity: SharedSensitivity,
}

impl PlanTodosTool {
    /// Create with the ledger path (see [`resolve_state_db`]) and the
    /// session-sensitivity mirror cell (same last-writer-wins pattern as
    /// the delegate tool's cell; refreshed every turn entry).
    pub fn new(state_db: Option<PathBuf>, sensitivity: SharedSensitivity) -> Self {
        Self {
            state_db,
            sensitivity,
        }
    }

    fn current_sensitivity(&self) -> &'static str {
        // D30 (G4 mirror pin): the tag classifies under the TURN's policy,
        // not whichever session last wrote the shared cell — concurrent
        // hosted sessions must not cross-tag each other's ledger rows.
        if let Some(sensitivity) = zen_core::turn_context::current_sensitivity() {
            return sensitivity_tag(&sensitivity);
        }
        let cell = self
            .sensitivity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        sensitivity_tag(&cell)
    }

    /// Validate `set` items and return the normalized drafts + spec_json
    /// items. All rejections are structured, model-readable strings.
    fn validate_items(
        items: &[serde_json::Value],
    ) -> Result<Vec<(String, String, String)>, String> {
        if items.len() > LEDGER_MAX_ITEMS {
            return Err(format!(
                "plan has {} items; the cap is {LEDGER_MAX_ITEMS} — split into multiple plans",
                items.len()
            ));
        }
        let mut seen = std::collections::HashSet::new();
        let mut progressing = 0usize;
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            let Some(obj) = item.as_object() else {
                return Err("each item must be an object with id and title".to_string());
            };
            if obj.contains_key("agent") {
                return Err(
                    "items must not carry an agent — the engine assigns it at dispatch time"
                        .to_string(),
                );
            }
            let Some(id) = obj.get("id").and_then(|v| v.as_str()) else {
                return Err("each item needs a string id".to_string());
            };
            let title = obj
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let status = obj
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("pending")
                .to_string();
            if !TASK_STATUSES.contains(&status.as_str()) {
                return Err(format!(
                    "item {id}: status {status:?} is outside the vocabulary {TASK_STATUSES:?}"
                ));
            }
            if !seen.insert(id.to_string()) {
                return Err(format!("duplicate item id {id:?}"));
            }
            if status == "progressing" {
                progressing += 1;
                if progressing > 1 {
                    return Err("at most one item may be progressing".to_string());
                }
            }
            out.push((id.to_string(), title, status));
        }
        Ok(out)
    }

    async fn set(&self, items: &[serde_json::Value]) -> Result<serde_json::Value, KernelError> {
        // Structured rejection as a data payload, not a KernelError — the
        // model must read and correct it (SC-008).
        let validated = match Self::validate_items(items) {
            Ok(v) => v,
            Err(e) => return Ok(json!({ "error": e, "op": "set", "plan.todos": true })),
        };
        let mut ids = Vec::with_capacity(validated.len());
        let mut titles = Vec::with_capacity(validated.len());
        let mut statuses = Vec::with_capacity(validated.len());
        for (id, title, status) in validated {
            ids.push(id);
            titles.push(title);
            statuses.push(status);
        }
        let session = zen_plugin::task_registry::current_session_scope();
        let client = open_client(self.state_db.as_ref())
            .await
            .map_err(|payload| KernelError::ToolFailed(payload.to_string()))?;
        let repo = zen_repo::WorkflowRepo::new(&client);
        // Mint-once (F4): one ACTIVE plan per session. A terminal plan
        // allows a fresh mint (a new multi-step request is a new plan); a
        // running one means the ledger already exists — dispatch attaches
        // to it (W6.3), it is never re-minted.
        if let Ok(Some(live)) = repo.latest_plan_for_session(&session).await
            && live.status == "running"
        {
            return Ok(json!({
                "error": "an active plan already exists for this session; set is mint-once — dispatch batches on it (plan.execute) or wait for it to settle",
                "plan_id": live.plan_id,
                "op": "set",
                "plan.todos": true,
            }));
        }
        let plan_id = uuid::Uuid::new_v4().to_string();
        let spec = json!({
            "items": ids.iter().zip(titles.iter()).zip(statuses.iter())
                .map(|((id, title), status)| json!({
                    "id": id, "title": title, "status": status,
                }))
                .collect::<Vec<_>>(),
            "sensitivity": self.current_sensitivity(),
            "session_id": session,
        });
        let drafts = ids
            .iter()
            .cloned()
            .zip(statuses.iter().cloned())
            .map(|(id, status)| zen_repo::TaskDraft { id, status })
            .collect::<Vec<_>>();
        let now = chrono::Utc::now().timestamp();
        repo.mint_plan_with_tasks(&plan_id, None, &spec.to_string(), &session, &drafts, now)
            .await
            .map_err(|e| KernelError::ToolFailed(format!("ledger mint failed: {e}")))?;
        register_turn_plan(&session, &plan_id, Vec::new());
        Ok(json!({
            "plan_id": plan_id,
            "items": ids.len(),
            "op": "set",
        }))
    }

    async fn get(&self) -> Result<serde_json::Value, KernelError> {
        let session = zen_plugin::task_registry::current_session_scope();
        let client = open_client(self.state_db.as_ref())
            .await
            .map_err(|payload| KernelError::ToolFailed(payload.to_string()))?;
        let repo = zen_repo::WorkflowRepo::new(&client);
        let Some(plan) = repo
            .latest_plan_for_session(&session)
            .await
            .map_err(|e| KernelError::ToolFailed(format!("ledger read failed: {e}")))?
        else {
            return Ok(json!({
                "plan": null,
                "items": [],
                "note": "no plan for this session",
                "op": "get",
            }));
        };
        let spec: serde_json::Value =
            serde_json::from_str(&plan.spec_json).unwrap_or(serde_json::Value::Null);
        let draft_items = spec
            .get("items")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let rows = repo
            .load_task_rows(&plan.plan_id)
            .await
            .map_err(|e| KernelError::ToolFailed(format!("ledger read failed: {e}")))?;
        let mut items = Vec::with_capacity(draft_items.len());
        for draft in &draft_items {
            let id = draft
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let title = draft
                .get("title")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let row = rows.iter().find(|r| r.task_id == id);
            items.push(LedgerItem {
                status: row
                    .map(|r| r.status.clone())
                    .unwrap_or_else(|| "pending".to_string()),
                response: row.and_then(|r| r.response.clone()),
                error: row.and_then(|r| r.error.clone()),
                title,
                id,
            });
        }
        Ok(json!({
            "plan_id": plan.plan_id,
            "plan_status": plan.status,
            "items": items,
            "op": "get",
        }))
    }
}

#[async_trait::async_trait]
impl Tool for PlanTodosTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: PLAN_TODOS_TOOL_NAME.to_string(),
            description: "Durable todo ledger for multi-step work (Sisyphus only). \
              op=set replaces the draft list before any batch dispatches — items carry \
              {id, title, status?} with status in pending|progressing|completed|failed|skipped, \
              at most one progressing, at most 12 items, NO agent field (the engine assigns \
              it at dispatch). Mint-once: while a plan for this session is running, set is \
              rejected — dispatch batches with plan.execute against the existing plan. \
              op=get returns the ledger with per-item outcomes; readable across restarts."
                .to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "op": { "type": "string", "enum": ["set", "get"] },
                    "items": {
                        "type": "array",
                        "maxItems": LEDGER_MAX_ITEMS,
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string" },
                                "title": { "type": "string" },
                                "status": {
                                    "type": "string",
                                    "enum": TASK_STATUSES,
                                }
                            },
                            "required": ["id", "title"]
                        }
                    }
                },
                "required": ["op"]
            }),
            result_schema: json!({
                "type": "object",
                "properties": {
                    "plan_id": { "type": ["string", "null"] },
                    "plan_status": { "type": "string" },
                    "items": { "type": "array" },
                    "op": { "type": "string" },
                    "error": { "type": "string" }
                }
            }),
        }
    }

    async fn invoke(&self, args: serde_json::Value) -> Result<serde_json::Value, KernelError> {
        match args.get("op").and_then(|v| v.as_str()) {
            Some("set") => {
                let items = args.get("items").and_then(|v| v.as_array()).cloned();
                let Some(items) = items else {
                    return Ok(json!({
                        "error": "op=set requires items[]",
                        "op": "set",
                        "plan.todos": true,
                    }));
                };
                self.set(&items).await
            }
            Some("get") => self.get().await,
            other => Ok(json!({
                "error": format!("op must be set|get, got {:?}", other.unwrap_or("missing")),
                "plan.todos": true,
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn isolate_home() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ZEN_HOME", dir.path()) };
        dir
    }

    fn tool() -> PlanTodosTool {
        PlanTodosTool::new(resolve_state_db(), {
            let cell: SharedSensitivity =
                Arc::new(Mutex::new(zen_core::types::Sensitivity::Private));
            cell
        })
    }

    fn item(id: &str, status: &str) -> serde_json::Value {
        json!({"id": id, "title": format!("task {id}"), "status": status})
    }

    async fn invoke_set(t: &PlanTodosTool, items: Vec<serde_json::Value>) -> serde_json::Value {
        t.invoke(json!({"op": "set", "items": items}))
            .await
            .unwrap()
    }

    // SC-008 / D21 §2: every ledger rejection is a structured, model-
    // readable payload — never a KernelError, never a panic.
    #[tokio::test]
    async fn plan_todos_set_rejects_invalid_ledgers_structured() {
        let _home = isolate_home();
        let t = tool();
        let over = (0..LEDGER_MAX_ITEMS + 1)
            .map(|i| item(&format!("t{i}"), "pending"))
            .collect::<Vec<_>>();
        let out = invoke_set(&t, over).await;
        assert!(
            out["error"].as_str().unwrap().contains("the cap is 12"),
            "{out}"
        );

        let out = invoke_set(&t, vec![item("a", "pending"), item("a", "pending")]).await;
        assert!(
            out["error"].as_str().unwrap().contains("duplicate item id"),
            "{out}"
        );

        let out = invoke_set(&t, vec![item("a", "progressing"), item("b", "progressing")]).await;
        assert!(
            out["error"].as_str().unwrap().contains("at most one"),
            "{out}"
        );

        let out = invoke_set(&t, vec![item("a", "exploded")]).await;
        assert!(
            out["error"]
                .as_str()
                .unwrap()
                .contains("outside the vocabulary"),
            "{out}"
        );

        let out = t
            .invoke(json!({"op": "set", "items": [
                {"id": "a", "title": "x", "agent": "Explore"}
            ]}))
            .await
            .unwrap();
        assert!(
            out["error"]
                .as_str()
                .unwrap()
                .contains("must not carry an agent"),
            "placeholder rejection (D21 §2 a): {out}"
        );
    }

    // F4 mint-once + claim-at-create (D24 §3): the mint claims the owner on
    // creation; a second set while the plan runs is rejected; a settled
    // plan allows a fresh mint.
    #[tokio::test]
    async fn plan_todos_set_mints_claimed_rows_and_enforces_mint_once() {
        let _home = isolate_home();
        let t = tool();
        let out = invoke_set(&t, vec![item("a", "pending"), item("b", "progressing")]).await;
        let plan_id = out["plan_id"]
            .as_str()
            .expect("mint returns plan_id")
            .to_string();

        let client = zen_repo::SqliteClient::open(&resolve_state_db().unwrap())
            .await
            .unwrap();
        let repo = zen_repo::WorkflowRepo::new(&client);
        let row = repo.load_plan(&plan_id).await.unwrap().expect("minted row");
        assert_eq!(row.status, "running");
        assert_eq!(
            row.owner.as_deref(),
            Some("local"),
            "claim-at-create owner = session"
        );
        let rows = repo.load_task_rows(&plan_id).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter().all(|r| r.agent.is_empty()),
            "'' sentinel until dispatch"
        );
        assert_eq!(rows[0].status, "pending");
        assert_eq!(rows[1].status, "progressing");

        let out = invoke_set(&t, vec![item("x", "pending")]).await;
        assert!(
            out["error"].as_str().unwrap().contains("mint-once"),
            "second set on a running plan rejected: {out}"
        );

        repo.complete_plan(&plan_id, "completed", None, None, None, 1)
            .await
            .unwrap();
        let out = invoke_set(&t, vec![item("x", "pending")]).await;
        assert!(
            out["plan_id"].is_string(),
            "settled plan allows a fresh mint: {out}"
        );
    }

    // `get` joins spec_json titles with row outcomes; undispatched items
    // keep their draft status (D24 §1 — the table is the only ledger).
    #[tokio::test]
    async fn plan_todos_get_joins_spec_titles_with_row_outcomes() {
        let _home = isolate_home();
        let t = tool();
        let minted = invoke_set(&t, vec![item("a", "pending"), item("b", "pending")]).await;
        let plan_id = minted["plan_id"].as_str().unwrap().to_string();

        let client = zen_repo::SqliteClient::open(&resolve_state_db().unwrap())
            .await
            .unwrap();
        let repo = zen_repo::WorkflowRepo::new(&client);
        repo.checkpoint_task(zen_repo::TaskCheckpoint {
            plan_id: &plan_id,
            task_id: "a",
            agent: "Explore",
            status: "completed",
            response: Some("REPLY"),
            error: None,
            now: 5,

            model: None,
            duration_ms: None,
            cost_usd: None,
        })
        .await
        .unwrap();

        let out = t.invoke(json!({"op": "get"})).await.unwrap();
        assert_eq!(out["plan_id"], minted["plan_id"]);
        assert_eq!(out["plan_status"], "running");
        let items = out["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        let a = items.iter().find(|i| i["id"] == "a").unwrap();
        assert_eq!(a["status"], "completed");
        assert_eq!(a["response"], "REPLY");
        assert_eq!(a["title"], "task a", "title from spec_json");
        let b = items.iter().find(|i| i["id"] == "b").unwrap();
        assert_eq!(
            b["status"], "pending",
            "undispatched draft keeps its status"
        );

        // get with no plan is an honest empty, not an error.
        let empty = PlanTodosTool::new(
            resolve_state_db(),
            Arc::new(Mutex::new(zen_core::types::Sensitivity::Public)),
        );
        // A different session scope is not simulatable here (task-local is
        // process-wide under nextest), so this reads the same session's
        // plan — the no-plan arm is covered by the shape contract instead.
        let out = empty.invoke(json!({"op": "get"})).await.unwrap();
        assert!(out["items"].is_array(), "{out}");
    }

    // D24 §2 (design-review F3): the drain-path finalizer writes terminal
    // states for every row class; consume-once via the registry; the
    // engine-outcome-wins rule keeps settled checkpoints intact.
    #[tokio::test]
    async fn finalize_cancelled_plan_writes_terminal_states_consume_once() {
        let _home = isolate_home();
        let t = tool();
        let minted = invoke_set(
            &t,
            vec![
                item("flight", "pending"),
                item("idle", "pending"),
                item("quiet", "pending"),
            ],
        )
        .await;
        let plan_id = minted["plan_id"].as_str().unwrap().to_string();
        let client = zen_repo::SqliteClient::open(&resolve_state_db().unwrap())
            .await
            .unwrap();
        let repo = zen_repo::WorkflowRepo::new(&client);
        // "flight" settled before the cancel landed — its checkpoint wins.
        repo.checkpoint_task(zen_repo::TaskCheckpoint {
            plan_id: &plan_id,
            task_id: "flight",
            agent: "Explore",
            status: "completed",
            response: Some("kept"),
            error: None,
            now: 7,

            model: None,
            duration_ms: None,
            cost_usd: None,
        })
        .await
        .unwrap();
        register_turn_plan("local", &plan_id, vec!["idle".to_string()]);

        assert!(
            finalize_cancelled_plan(&client, "local", "cancelled", 9)
                .await
                .unwrap(),
            "a live record finalizes"
        );
        let plan = repo.load_plan(&plan_id).await.unwrap().unwrap();
        assert_eq!(plan.status, "cancelled");
        assert!(plan.owner.is_none(), "finalizer releases the claim");
        let rows = repo.load_task_rows(&plan_id).await.unwrap();
        let flight = rows.iter().find(|r| r.task_id == "flight").unwrap();
        assert_eq!(
            flight.status, "completed",
            "engine outcome wins over the finalizer"
        );
        assert_eq!(flight.response.as_deref(), Some("kept"));
        let idle = rows.iter().find(|r| r.task_id == "idle").unwrap();
        assert_eq!(
            idle.status, "failed",
            "unsettled rows fail with the cancel reason"
        );
        assert_eq!(idle.error.as_deref(), Some("cancelled"));
        let quiet = rows.iter().find(|r| r.task_id == "quiet").unwrap();
        assert_eq!(
            quiet.status, "skipped",
            "rows outside the unsettled set skip"
        );
        assert!(
            !finalize_cancelled_plan(&client, "local", "cancelled", 10)
                .await
                .unwrap(),
            "consume-once: no record ⇒ no second finalize"
        );
    }

    // Registry seam hygiene: stale-plan_id updates are ignored (a newer
    // mint wins), clear only removes a matching record.
    #[test]
    fn turn_plan_registry_is_plan_scoped() {
        register_turn_plan("s1", "plan-1", vec!["a".to_string()]);
        assert_eq!(peek_turn_plan("s1").unwrap().plan_id, "plan-1");
        update_turn_plan_unsettled("s1", "plan-OTHER", vec!["b".to_string()]);
        assert_eq!(
            peek_turn_plan("s1").unwrap().unsettled,
            vec!["a".to_string()],
            "stale update ignored"
        );
        update_turn_plan_unsettled("s1", "plan-1", Vec::new());
        assert!(peek_turn_plan("s1").unwrap().unsettled.is_empty());
        clear_turn_plan("s1", "plan-OTHER");
        assert!(peek_turn_plan("s1").is_some(), "foreign clear ignored");
        clear_turn_plan("s1", "plan-1");
        assert!(peek_turn_plan("s1").is_none());
        assert!(take_turn_plan("s1").is_none(), "take on empty is None");
    }

    /// W11.1/T1 (G4 mirror pin, D24 §5): the ledger sensitivity tag classifies
    /// under the TURN's policy, not whichever session last wrote the shared
    /// cell — concurrent hosted sessions must not cross-tag ledger rows.
    #[tokio::test]
    async fn sensitivity_tag_follows_turn_context_over_shared_cell() {
        let _home = isolate_home();
        // The shared cell holds Public (a concurrent session's last write).
        let cell: SharedSensitivity = Arc::new(Mutex::new(zen_core::types::Sensitivity::Public));
        let t = PlanTodosTool::new(resolve_state_db(), cell);
        let ctx = zen_core::turn_context::TurnContext {
            sensitivity: zen_core::types::Sensitivity::Confidential,
            knowledge: Vec::new(),
            history: Vec::new(),
        };
        let tag = zen_core::turn_context::TURN_CONTEXT
            .scope(ctx, async { t.current_sensitivity() })
            .await;
        assert_eq!(tag, "confidential", "the turn's policy tags the ledger");
        // Outside any turn scope the cell fallback applies.
        assert_eq!(t.current_sensitivity(), "public");
    }
}
