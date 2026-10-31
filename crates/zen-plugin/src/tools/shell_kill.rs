//! `shell.kill` — stop a background task spawned by `shell.exec
//! background:true` (design record
//! `docs/designs/background-tool-execution-2026-10-30.md`, Section 5).
//!
//! Scope logic (Constitution XV):
//! - Functionality: SIGTERMs the task's process group, escalates to SIGKILL
//!   after 3s if the child has not been reaped, marks the status `killed`
//!   and rewrites `meta.json`. Idempotent on already-terminal tasks.
//! - User impact: the model (or the SIGTERM drain) can stop runaway
//!   background work immediately; the call returns without waiting for the
//!   escalation window.
//! - Default: registry lookup first; a running-but-lost task (registry
//!   entry gone after a daemon restart) falls back to its `meta.json`,
//!   which is marked `killed` WITHOUT signalling — killing by a recorded
//!   pid after a restart risks pid reuse (D1 mark-only guard).
//! - Interaction: pairs with `shell.collect` (retrieve output) and
//!   `TaskRegistry::kill_all_blocking` (daemon SIGTERM drain).

use std::sync::LazyLock;

use async_trait::async_trait;
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::{Value, json};

use crate::task_registry::{self, TaskStatus};

const NAME: &str = "shell.kill";
const DESCRIPTION: &str = "Stop a background task (shell.exec background:true): SIGTERM the group, escalate to SIGKILL after 3s";

static ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "task_id": { "type": "string", "description": "task_id returned by shell.exec background:true" }
        },
        "required": ["task_id"]
    })
});

static RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "task_id": { "type": "string" },
            "status": { "type": "string" },
            "pid": { "type": "integer" },
            "note": { "type": "string" }
        }
    })
});

#[derive(Clone, Default)]
pub struct ShellKillTool;

impl ShellKillTool {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for ShellKillTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: NAME.to_string(),
            description: DESCRIPTION.to_string(),
            args_schema: ARGS_SCHEMA.clone(),
            result_schema: RESULT_SCHEMA.clone(),
        }
    }

    async fn invoke(&self, args: Value) -> Result<Value, KernelError> {
        let task_id = args["task_id"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                KernelError::InvalidArgument("Missing or invalid 'task_id' field".into())
            })?
            .to_string();

        // P1 task_id validation (review fix a): the id reaches meta_path()
        // (mark_meta_status rewrite) — only a uuid v4 shape is accepted,
        // closing the path-traversal hole.
        if !task_registry::is_valid_task_id(&task_id) {
            return Err(KernelError::InvalidArgument(format!(
                "invalid task_id \"{task_id}\": expected a uuid v4 id (32 hex \
                 chars, or 8-4-4-4-12 hyphenated) as returned by shell.exec \
                 background:true"
            )));
        }

        // Registry kill + meta rewrite are sync I/O — off the async worker
        // (review fix e, house spawn_blocking discipline).
        let killed = tokio::task::spawn_blocking({
            let task_id = task_id.clone();
            move || task_registry::registry().kill(&task_id)
        })
        .await
        .unwrap_or_else(|e| Err(anyhow::anyhow!("shell.kill registry task failed: {e}")));

        match killed {
            Ok(snapshot) => Ok(json!({
                "task_id": snapshot.task_id,
                "status": snapshot.status.as_str(),
                "pid": snapshot.pid,
                "note": "SIGTERM sent to the process group; SIGKILL escalates after 3s if the group is still alive"
            })),
            Err(_) => {
                // Registry miss: fall back to the on-disk meta. A
                // running-but-lost task is marked killed WITHOUT signalling —
                // the process may be gone (daemon crash) and killing by the
                // recorded pid risks pid reuse (D1 mark-only guard).
                let marked = tokio::task::spawn_blocking({
                    let task_id = task_id.clone();
                    move || task_registry::mark_meta_status(&task_id, TaskStatus::Killed)
                })
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "shell.kill meta fallback task failed");
                    None
                });
                match marked {
                    Some(meta) => Ok(json!({
                        "task_id": meta.task_id,
                        "status": meta.status.as_str(),
                        "pid": meta.pid,
                        "note": "task not in the registry (daemon restart?); meta.json marked killed, no signal sent (pid-reuse guard)"
                    })),
                    None => Ok(json!({
                        "error": format!("unknown task_id {task_id}: no registry entry and no meta.json under logs/tasks/")
                    })),
                }
            }
        }
    }
}
