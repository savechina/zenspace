//! `delegate.collect` + `delegate.kill` (W5.3, spec 006 D20 §4/§8).
//!
//! Surface for background sub-agent tasks admitted through
//! `delegate.task { background: true }`:
//!
//! - **collect**: `{task_id | task_ids[], wait_ms?}` — yield-wait poll of a
//!   task's terminal state (default 10s, clamp 0..=30000), idempotent,
//!   `meta.json` fallback after a daemon restart. Releasing an observed
//!   terminal hold refunds the admission token reservation (armed refund
//!   on drop). Confidential sensitivity (collect returns sub-agent shell
//!   output = `shell.exec`-grade data) but READ-ONLY for the quality gate
//!   (orchestrator `is_mutating_tool` whitelist).
//! - **kill**: `{task_id}` — aborts the live handle and marks `killed`;
//!   releases the hold exactly like collect. MUTATING (fails the
//!   fail-safe default in `is_mutating_tool`).
//!
//! Payload parity (D25 A5): per-task payloads use the unified shape
//! `{task_id, task_index, agent, status, response|error, model_source}` —
//! the same key names the admission pass and the completion notice (W5.4)
//! use for the same meanings. Registry-only fields (agent/task_index) fall
//! back to `None` → JSON `null` when unknown.

use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::json;
use tracing::info;

use crate::delegate_task::BackgroundBook;

/// `delegate.collect` tool name.
pub const DELEGATE_COLLECT_TOOL_NAME: &str = "delegate.collect";
/// `delegate.kill` tool name.
pub const DELEGATE_KILL_TOOL_NAME: &str = "delegate.kill";

/// Default yield-wait for collect (10s; clamp 0..=30000).
const COLLECT_WAIT_DEFAULT_MS: u64 = 10_000;
const COLLECT_WAIT_MAX_MS: u64 = 30_000;

/// One per-task payload in the unified D25 A5 shape.
#[derive(serde::Serialize)]
struct TaskReceipt {
    task_id: String,
    task_index: Option<usize>,
    agent: Option<String>,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    response: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    model_source: Option<String>,
}

/// Build the per-task receipt from a registry collect outcome plus the
/// (possibly released) book hold. `model_source` comes from the hold only
/// — after a restart fallback it is unknown (`null`).
fn receipt_from(
    collect: zen_plugin::task_registry::DelegateCollect,
    hold_model: Option<String>,
) -> TaskReceipt {
    TaskReceipt {
        task_id: collect.task_id,
        task_index: collect.task_index,
        agent: collect.agent,
        status: collect.status,
        response: collect.response,
        error: collect.error,
        model_source: hold_model,
    }
}

// PURPOSE: Yield-wait collect for background delegate tasks — the parent
//          agent polls task outcomes without busy-spin and refunds the
//          admission token reservation on terminal observation.
// USAGE: Parent of `delegate.task { background: true }` admissions.
// EXPECTED: `{"background": true, "results": [<unified receipts>]}`.
// ERRORS: Unknown id without meta fallback → structured Ok-error payload
//         (never panics the round).
#[derive(Clone)]
pub struct DelegateCollectTool {
    book: BackgroundBook,
}

impl DelegateCollectTool {
    /// Create with a book shared with the admitting `DelegateTaskTool`.
    pub fn new(book: BackgroundBook) -> Self {
        Self { book }
    }

    /// Clamp + parse `wait_ms` (0..=30000, default 10s).
    fn clamp_wait(raw: Option<u64>) -> u64 {
        raw.unwrap_or(COLLECT_WAIT_DEFAULT_MS)
            .min(COLLECT_WAIT_MAX_MS)
    }
}

#[async_trait::async_trait]
impl Tool for DelegateCollectTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: DELEGATE_COLLECT_TOOL_NAME.to_string(),
            description: "Collect the result(s) of background delegate task(s) started with \
              delegate.task background:true. Pass task_id (single) or task_ids[] (batch, \
              results are slot-ordered to the input). Waits up to wait_ms (default 10000, \
              max 30000) for completion; returns early on terminal state. Idempotent — \
              repeat collects return the same status. Works across daemon restarts via \
              the on-disk task meta."
                .to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "Single background task id to collect"
                    },
                    "task_ids": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Batch collect; results[i] corresponds to task_ids[i]"
                    },
                    "wait_ms": {
                        "type": "integer",
                        "description": "Max wait in milliseconds (default 10000, clamp 0..=30000)"
                    }
                }
            }),
            result_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "background": {"type": "boolean"},
                    "results": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "task_id": {"type": "string"},
                                "task_index": {"type": ["integer", "null"]},
                                "agent": {"type": ["string", "null"]},
                                "status": {"type": "string"},
                                "response": {},
                                "error": {"type": ["string", "null"]},
                                "model_source": {"type": ["string", "null"]}
                            }
                        }
                    }
                }
            }),
        }
    }

    async fn invoke(&self, args: serde_json::Value) -> Result<serde_json::Value, KernelError> {
        let single = args.get("task_id").and_then(|v| v.as_str());
        let batch = args.get("task_ids").and_then(|v| v.as_array()).map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect::<Vec<_>>()
        });
        let ids: Vec<String> = match (single, batch) {
            (Some(id), _) => vec![id.to_string()],
            (None, Some(ids)) if !ids.is_empty() => ids,
            _ => {
                return Ok(json!({
                    "error": "provide task_id or a non-empty task_ids[] batch",
                    "background": true
                }));
            }
        };
        if ids.len() > crate::delegate_task::FANOUT_MAX_TASKS {
            return Ok(json!({
                "error": format!(
                    "task_ids[] batch is capped at {} (got {})",
                    crate::delegate_task::FANOUT_MAX_TASKS,
                    ids.len()
                ),
                "background": true
            }));
        }
        let wait_ms = Self::clamp_wait(args.get("wait_ms").and_then(|v| v.as_u64()));
        let registry = zen_plugin::task_registry::registry();
        let mut results = Vec::with_capacity(ids.len());
        for id in &ids {
            let hold_model = self
                .book
                .release(id)
                .and_then(|h| h.model_source.map(|m| m.as_str().to_string()));
            match registry.collect_delegate(id, wait_ms).await {
                Ok(collect) => {
                    let receipt =
                        serde_json::to_value(receipt_from(collect, hold_model)).map_err(|e| {
                            KernelError::ToolFailed(format!("receipt serialize failed: {e}"))
                        })?;
                    results.push(receipt);
                }
                Err(e) => results.push(json!({
                    "task_id": id,
                    "status": "unknown",
                    "error": e.to_string(),
                })),
            }
        }
        Ok(json!({"background": true, "results": results}))
    }
}

// PURPOSE: Abort a live background delegate task — the parent-facing
//          cancel path; marks `killed` with no partial result.
// USAGE: Parent of a running `delegate.task { background: true }` task.
// EXPECTED: `{"task_id", "status": "killed"}` (or the already-terminal
//           status for idempotent repeats).
// ERRORS: Unknown id without meta fallback → structured Ok-error.
#[derive(Clone)]
pub struct DelegateKillTool {
    book: BackgroundBook,
}

impl DelegateKillTool {
    /// Create with a book shared with the admitting `DelegateTaskTool`.
    pub fn new(book: BackgroundBook) -> Self {
        Self { book }
    }
}

#[async_trait::async_trait]
impl Tool for DelegateKillTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: DELEGATE_KILL_TOOL_NAME.to_string(),
            description: "Kill (abort) a running background delegate task started with \
              delegate.task background:true. The task is marked killed with no partial \
              result; its token reservation is released. Idempotent — killing an \
              already-terminal task returns its current status unchanged."
                .to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "The background task id to kill"
                    }
                },
                "required": ["task_id"]
            }),
            result_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "task_id": {"type": "string"},
                    "status": {"type": "string"},
                    "background": {"type": "boolean"}
                }
            }),
        }
    }

    async fn invoke(&self, args: serde_json::Value) -> Result<serde_json::Value, KernelError> {
        let Some(id) = args.get("task_id").and_then(|v| v.as_str()) else {
            return Ok(json!({"error": "task_id is required"}));
        };
        let id = id.to_string();
        // Small-file meta rewrite inside kill() runs on the blocking pool
        // (house spawn_blocking discipline; shell.kill precedent).
        let registry = zen_plugin::task_registry::registry();
        let (id_for_task, book) = (id.clone(), self.book.clone());
        let joined = tokio::task::spawn_blocking(move || {
            let snapshot = registry
                .kill(&id_for_task)
                .map_err(|e| KernelError::ToolFailed(format!("kill failed: {e}")))?;
            // Release AFTER the registry marked the terminal state so the
            // refund mirrors an observed terminal transition.
            let _released = book.release(&id_for_task);
            Ok::<_, KernelError>(snapshot)
        })
        .await
        .map_err(|e| KernelError::ToolFailed(format!("kill task join failed: {e}")))?;
        let outcome = joined?;
        info!(
            target: "zen_delegate",
            task_id = %id,
            status = %outcome.status.as_str(),
            "delegate.kill completed"
        );
        Ok(json!({
            "task_id": id,
            "status": outcome.status.as_str(),
            "background": true
        }))
    }
}

/// Self-test: the wait clamp is total (no panic on absurd input).
#[cfg(test)]
mod clamp_tests {
    use super::*;

    #[test]
    fn collect_wait_clamps_to_bounds() {
        assert_eq!(DelegateCollectTool::clamp_wait(None), 10_000);
        assert_eq!(DelegateCollectTool::clamp_wait(Some(0)), 0);
        assert_eq!(DelegateCollectTool::clamp_wait(Some(30_000)), 30_000);
        assert_eq!(DelegateCollectTool::clamp_wait(Some(1_000_000)), 30_000);
    }

    /// Isolate ZEN_HOME so spawn_async's meta writes land in a tempdir
    /// (nextest process-per-test makes the env mutation safe, T118).
    fn isolate_zen_home() -> tempfile::TempDir {
        let home = tempfile::tempdir().expect("temp home");
        // SAFETY: single-threaded test process (nextest discipline).
        unsafe {
            std::env::set_var("ZEN_HOME", home.path());
        }
        zen_core::config::invalidate_config_cache();
        home
    }

    /// W5.3 acceptance: a terminal task is observed as `completed` with the
    /// Ok payload — before the full wait window elapses (yield-wait, not a
    /// fixed sleep), and a zero-wait collect of a still-running task reads
    /// `running` without error.
    #[tokio::test]
    async fn collect_running_then_ok_without_busy_spin() {
        let _home = isolate_zen_home();
        let registry = zen_plugin::task_registry::registry();
        let limits = zen_plugin::task_registry::BackgroundLimits {
            max_wall_secs: 60,
            max_concurrent: 4,
            output_cap_bytes: zen_plugin::task_registry::OUTPUT_CAP_BYTES,
        };
        registry
            .spawn_async(
                "a5f30000-0000-4000-8000-000000000001",
                "sess-c1",
                "Explore",
                Some(0),
                &limits,
                async {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    Ok(json!({"answer": "done"}))
                },
            )
            .expect("spawn");
        let tool = DelegateCollectTool::new(BackgroundBook::default());
        let started = std::time::Instant::now();
        let running = tool
            .invoke(json!({"task_id": "a5f30000-0000-4000-8000-000000000001", "wait_ms": 0}))
            .await
            .expect("invoke");
        assert_eq!(
            running["results"][0]["status"],
            json!("running"),
            "zero-wait collect of a still-sleeping task reads running: {running}"
        );
        let done = tool
            .invoke(json!({"task_id": "a5f30000-0000-4000-8000-000000000001", "wait_ms": 5000}))
            .await
            .expect("invoke");
        assert_eq!(done["results"][0]["status"], json!("completed"), "{done}");
        assert_eq!(done["results"][0]["response"], json!({"answer": "done"}));
        assert_eq!(done["results"][0]["agent"], json!("Explore"));
        assert_eq!(done["results"][0]["task_index"], json!(0));
        assert!(
            started.elapsed() < std::time::Duration::from_millis(5000),
            "yield-wait returns on terminal, not on full window"
        );
    }

    /// W5.3 acceptance: collect is idempotent — a second collect after the
    /// terminal observation returns the same status and payload.
    #[tokio::test]
    async fn collect_is_idempotent_after_terminal() {
        let _home = isolate_zen_home();
        let registry = zen_plugin::task_registry::registry();
        let limits = zen_plugin::task_registry::BackgroundLimits {
            max_wall_secs: 60,
            max_concurrent: 4,
            output_cap_bytes: zen_plugin::task_registry::OUTPUT_CAP_BYTES,
        };
        registry
            .spawn_async(
                "a5f30000-0000-4000-8000-000000000002",
                "sess-c2",
                "Explore",
                None,
                &limits,
                async { Ok(json!({"v": 1})) },
            )
            .expect("spawn");
        let tool = DelegateCollectTool::new(BackgroundBook::default());
        let first = tool
            .invoke(json!({"task_id": "a5f30000-0000-4000-8000-000000000002", "wait_ms": 5000}))
            .await
            .expect("invoke");
        let second = tool
            .invoke(json!({"task_id": "a5f30000-0000-4000-8000-000000000002", "wait_ms": 0}))
            .await
            .expect("invoke");
        assert_eq!(
            first["results"][0], second["results"][0],
            "idempotent collect"
        );
        assert_eq!(first["results"][0]["status"], json!("completed"));
    }

    /// W5.3 acceptance: an unknown registry id falls back to the on-disk
    /// meta.json (daemon-restart case) and surfaces the stored result.
    #[tokio::test]
    async fn collect_unknown_id_falls_back_to_meta() {
        let home = isolate_zen_home();
        let id = "a5f30000-0000-4000-8000-000000000003";
        let dir = home.path().join("logs").join("tasks").join(id);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let meta = json!({
            "task_id": id,
            "session_id": "sess-old",
            "pid": 0,
            "command_digest": "delegate:Explore",
            "output_dir": dir.to_string_lossy(),
            "status": {"completed": 0},
            "started_at_ms": 1,
            "ended_at_ms": 2,
            "bytes_stdout": 0,
            "bytes_stderr": 0,
            "truncated": false,
            "stdout_path": "stdout",
            "stderr_path": "stderr",
            "agent": "Explore",
            "task_index": 3,
            "result": {"answer": "from-meta"}
        });
        std::fs::write(
            dir.join("meta.json"),
            serde_json::to_vec(&meta).expect("ser"),
        )
        .expect("write meta");
        let tool = DelegateCollectTool::new(BackgroundBook::default());
        let out = tool
            .invoke(json!({"task_id": id, "wait_ms": 0}))
            .await
            .expect("invoke");
        assert_eq!(out["results"][0]["status"], json!("completed"), "{out}");
        assert_eq!(
            out["results"][0]["response"],
            json!({"answer": "from-meta"})
        );
        assert_eq!(out["results"][0]["task_index"], json!(3));
    }

    /// W5.3 acceptance: kill aborts the live handle and marks `killed`; a
    /// following collect observes `killed` with no partial result.
    #[tokio::test]
    async fn kill_yields_killed_with_no_partial_result() {
        let _home = isolate_zen_home();
        let registry = zen_plugin::task_registry::registry();
        let limits = zen_plugin::task_registry::BackgroundLimits {
            max_wall_secs: 60,
            max_concurrent: 4,
            output_cap_bytes: zen_plugin::task_registry::OUTPUT_CAP_BYTES,
        };
        registry
            .spawn_async(
                "a5f30000-0000-4000-8000-000000000004",
                "sess-k1",
                "Explore",
                None,
                &limits,
                async {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    Ok(json!({"never": true}))
                },
            )
            .expect("spawn");
        let kill = DelegateKillTool::new(BackgroundBook::default());
        let out = kill
            .invoke(json!({"task_id": "a5f30000-0000-4000-8000-000000000004"}))
            .await
            .expect("invoke");
        assert_eq!(out["status"], json!("killed"), "{out}");
        let collect = DelegateCollectTool::new(BackgroundBook::default());
        let collected = collect
            .invoke(json!({"task_id": "a5f30000-0000-4000-8000-000000000004", "wait_ms": 0}))
            .await
            .expect("invoke");
        assert_eq!(collected["results"][0]["status"], json!("killed"));
        assert!(
            collected["results"][0].get("response").is_none(),
            "no partial result survives a kill: {collected}"
        );
    }

    /// D25 A5 (SC-023): the per-task receipt uses the unified shape —
    /// exactly `{task_id, task_index, agent, status, response,
    /// model_source}` for a success; the same key names the admission
    /// result and the W5.4 completion notice use for the same meanings.
    #[test]
    fn receipt_payload_uses_unified_d25_a5_shape() {
        let receipt = TaskReceipt {
            task_id: "t".to_string(),
            task_index: Some(1),
            agent: Some("Explore".to_string()),
            status: "completed".to_string(),
            response: Some(json!({"a": 1})),
            error: None,
            model_source: Some("hint".to_string()),
        };
        let value = serde_json::to_value(&receipt).expect("ser");
        let keys: std::collections::BTreeSet<&str> = value
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        let expected: std::collections::BTreeSet<&str> = [
            "task_id",
            "task_index",
            "agent",
            "status",
            "response",
            "model_source",
        ]
        .into_iter()
        .collect();
        assert_eq!(keys, expected, "unified receipt shape: {value}");
    }

    /// Compile-time guard: ModelSource label surface is reachable from this
    /// module (payload parity) and stays exhaustive.
    #[test]
    fn model_source_labels_are_stable() {
        use crate::delegate_task::ModelSource;
        let expected: std::collections::BTreeSet<&str> =
            ["profile", "hint", "default"].into_iter().collect();
        let actual: std::collections::BTreeSet<&str> = [
            ModelSource::Profile.as_str(),
            ModelSource::Hint.as_str(),
            ModelSource::Default.as_str(),
        ]
        .into_iter()
        .collect();
        assert_eq!(expected, actual);
    }
}
