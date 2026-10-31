//! `shell.collect` — retrieve the status and output tails of a background
//! task spawned by `shell.exec background:true` (design record
//! `docs/designs/background-tool-execution-2026-10-30.md`, Section 5).
//!
//! Scope logic (Constitution XV):
//! - Functionality: polls the process-global [`task_registry::registry`]
//!   every 200ms until the task reaches a terminal status or `wait_ms`
//!   elapses (default 10s, clamp 0..=30s), then returns the status plus the
//!   stdout/stderr tails (≤8k chars each, char-boundary-safe; streams at or
//!   below 4k chars are returned in full under `stdout`/`stderr`).
//! - User impact: the model can start long work, end its round, and pick up
//!   results in a later round without holding the turn open.
//! - Default: `wait_ms` 10_000; a task unknown to the registry falls back to
//!   its on-disk `meta.json` (daemon-restart case); a fully unknown id is a
//!   structured JSON error, never a panic.
//! - Interaction: pairs with `shell.kill` (stop) and the
//!   `[agentic.background]` wall-clock deadline (Timeout status).

use std::path::Path;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::{Value, json};

use crate::task_registry::{
    self, COLLECT_DEFAULT_WAIT_MS, COLLECT_INLINE_CHARS, COLLECT_TAIL_CHARS, OUTPUT_CAP_BYTES,
    TaskStatus,
};

const NAME: &str = "shell.collect";
const DESCRIPTION: &str =
    "Collect the status and output tails of a background task (shell.exec background:true)";

static ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "task_id": { "type": "string", "description": "task_id returned by shell.exec background:true" },
            "wait_ms": { "type": "integer", "description": "How long to wait for a terminal status before returning the current state (default 10000, clamped 0..=30000)" }
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
            "exit_code": { "type": "integer" },
            "stdout": { "type": "string" },
            "stderr": { "type": "string" },
            "stdout_tail": { "type": "string" },
            "stderr_tail": { "type": "string" },
            "bytes_stdout": { "type": "integer" },
            "bytes_stderr": { "type": "integer" },
            "truncated": { "type": "boolean" },
            "output_path": { "type": "string" },
            "note": { "type": "string" }
        }
    })
});

const POLL_INTERVAL_MS: u64 = 200;
const MAX_WAIT_MS: u64 = 30_000;

/// Read a capture file lossily, capped at the drain cap plus slack (the
/// drain never writes more than [`OUTPUT_CAP_BYTES`], so this bounds the
/// read without truncating real content).
fn read_capture(path: &Path) -> String {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return String::new(),
    };
    let mut bytes = Vec::new();
    let cap = OUTPUT_CAP_BYTES + 1024;
    let _ = file.take(cap).read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

/// One stream's JSON view: inline under `inline_key` when at or below
/// [`COLLECT_INLINE_CHARS`] chars, otherwise the last [`COLLECT_TAIL_CHARS`]
/// chars under `tail_key` (D5 — char-boundary-safe by construction).
fn stream_view(text: &str, inline_key: &str, tail_key: &str) -> Value {
    if text.chars().count() <= COLLECT_INLINE_CHARS {
        json!({ inline_key: text })
    } else {
        json!({ tail_key: task_registry::tail_chars(text, COLLECT_TAIL_CHARS) })
    }
}

/// Everything `render` needs about one task, gathered from either the
/// registry snapshot or the on-disk meta fallback.
struct CollectState<'a> {
    status: &'a TaskStatus,
    bytes_stdout: u64,
    bytes_stderr: u64,
    truncated: bool,
    output_dir: &'a str,
    stdout_path: &'a Path,
    stderr_path: &'a Path,
}

/// Read both capture files off the async worker (review fix e): the reads
/// are bounded (≤8MiB each) but still blocking I/O — house spawn_blocking
/// discipline.
async fn read_captures_blocking(state: &CollectState<'_>) -> (String, String) {
    let stdout_path = state.stdout_path.to_path_buf();
    let stderr_path = state.stderr_path.to_path_buf();
    tokio::task::spawn_blocking(move || (read_capture(&stdout_path), read_capture(&stderr_path)))
        .await
        .unwrap_or_default()
}

/// Render the collect result from a status plus the capture paths.
fn render(task_id: &str, state: &CollectState<'_>, stdout: String, stderr: String) -> Value {
    let mut out = json!({
        "task_id": task_id,
        "status": state.status.as_str(),
        "bytes_stdout": state.bytes_stdout,
        "bytes_stderr": state.bytes_stderr,
        "truncated": state.truncated,
        "output_path": state.output_dir,
    });
    if let TaskStatus::Completed(code) = state.status {
        out["exit_code"] = json!(code);
    }
    let stdout_view = stream_view(&stdout, "stdout", "stdout_tail");
    if let Some(obj) = stdout_view.as_object() {
        for (k, v) in obj {
            out[k.as_str()] = v.clone();
        }
    }
    let stderr_view = stream_view(&stderr, "stderr", "stderr_tail");
    if let Some(obj) = stderr_view.as_object() {
        for (k, v) in obj {
            out[k.as_str()] = v.clone();
        }
    }
    out
}

#[derive(Clone, Default)]
pub struct ShellCollectTool;

impl ShellCollectTool {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for ShellCollectTool {
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
            })?;

        // P1 task_id validation (review fix a): the id reaches meta_path()
        // and the meta fallback below — only a uuid v4 shape is accepted,
        // closing the path-traversal hole.
        if !task_registry::is_valid_task_id(task_id) {
            return Err(KernelError::InvalidArgument(format!(
                "invalid task_id \"{task_id}\": expected a uuid v4 id (32 hex \
                 chars, or 8-4-4-4-12 hyphenated) as returned by shell.exec \
                 background:true"
            )));
        }

        let wait_ms = args
            .get("wait_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(COLLECT_DEFAULT_WAIT_MS)
            .min(MAX_WAIT_MS);

        let deadline = Instant::now() + Duration::from_millis(wait_ms);
        loop {
            if let Some(snapshot) = task_registry::registry().snapshot(task_id) {
                if snapshot.status.is_terminal() {
                    let output_dir = snapshot.output_dir.clone();
                    let state = CollectState {
                        status: &snapshot.status,
                        bytes_stdout: snapshot.bytes_stdout,
                        bytes_stderr: snapshot.bytes_stderr,
                        truncated: snapshot.truncated,
                        output_dir: &output_dir,
                        stdout_path: &task_registry::meta_stdout_path(Path::new(&output_dir)),
                        stderr_path: &task_registry::meta_stderr_path(Path::new(&output_dir)),
                    };
                    let (stdout, stderr) = read_captures_blocking(&state).await;
                    return Ok(render(task_id, &state, stdout, stderr));
                }
            } else if let Some(meta) = task_registry::read_meta(task_id) {
                // Registry miss (daemon restart): the on-disk meta is the
                // fallback source of truth.
                if meta.status.is_terminal() {
                    // Defense-in-depth (review fix a): the meta's recorded
                    // capture paths are external data — refuse to read any
                    // path that escapes this task's output dir.
                    if !task_registry::meta_capture_paths_are_safe(task_id, &meta) {
                        return Ok(json!({
                            "error": format!(
                                "corrupt meta for task {task_id}: recorded capture \
                                 paths escape the task output dir; refusing to read"
                            )
                        }));
                    }
                    let stdout_path = std::path::PathBuf::from(&meta.stdout_path);
                    let stderr_path = std::path::PathBuf::from(&meta.stderr_path);
                    let state = CollectState {
                        status: &meta.status,
                        bytes_stdout: meta.bytes_stdout,
                        bytes_stderr: meta.bytes_stderr,
                        truncated: meta.truncated,
                        output_dir: &meta.output_dir,
                        stdout_path: &stdout_path,
                        stderr_path: &stderr_path,
                    };
                    let (stdout, stderr) = read_captures_blocking(&state).await;
                    return Ok(render(task_id, &state, stdout, stderr));
                }
            } else {
                return Ok(json!({
                    "error": format!("unknown task_id {task_id}: no registry entry and no meta.json under logs/tasks/")
                }));
            }
            if Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
        }

        // Wait elapsed without a terminal status: report the current state.
        if let Some(snapshot) = task_registry::registry().snapshot(task_id) {
            let output_dir = snapshot.output_dir.clone();
            let state = CollectState {
                status: &snapshot.status,
                bytes_stdout: snapshot.bytes_stdout,
                bytes_stderr: snapshot.bytes_stderr,
                truncated: snapshot.truncated,
                output_dir: &output_dir,
                stdout_path: &task_registry::meta_stdout_path(Path::new(&output_dir)),
                stderr_path: &task_registry::meta_stderr_path(Path::new(&output_dir)),
            };
            let (stdout, stderr) = read_captures_blocking(&state).await;
            let mut out = render(task_id, &state, stdout, stderr);
            out["note"] = json!(format!(
                "still {status} after waiting {wait_ms}ms; call shell.collect again later",
                status = snapshot.status.as_str()
            ));
            return Ok(out);
        }
        if let Some(meta) = task_registry::read_meta(task_id) {
            if !task_registry::meta_capture_paths_are_safe(task_id, &meta) {
                return Ok(json!({
                    "error": format!(
                        "corrupt meta for task {task_id}: recorded capture paths \
                         escape the task output dir; refusing to read"
                    )
                }));
            }
            let stdout_path = std::path::PathBuf::from(&meta.stdout_path);
            let stderr_path = std::path::PathBuf::from(&meta.stderr_path);
            let state = CollectState {
                status: &meta.status,
                bytes_stdout: meta.bytes_stdout,
                bytes_stderr: meta.bytes_stderr,
                truncated: meta.truncated,
                output_dir: &meta.output_dir,
                stdout_path: &stdout_path,
                stderr_path: &stderr_path,
            };
            let (stdout, stderr) = read_captures_blocking(&state).await;
            let mut out = render(task_id, &state, stdout, stderr);
            out["note"] = json!(format!(
                "still {status} after waiting {wait_ms}ms (registry entry gone — daemon restart?); call shell.collect again later",
                status = meta.status.as_str()
            ));
            return Ok(out);
        }
        Ok(json!({
            "error": format!("unknown task_id {task_id}: no registry entry and no meta.json under logs/tasks/")
        }))
    }
}
