//! In-daemon background task registry (design record
//! `docs/designs/background-tool-execution-2026-10-30.md`, D1/D7/D8).
//!
//! Scope logic (Constitution XV):
//! - Functionality: tracks `shell.exec background:true` children in a
//!   process-global registry — each entry holds the [`tokio::process::Child`]
//!   (strong hold: dropping it closes the pipes and SIGPIPEs the child),
//!   drains stdout/stderr to `logs/tasks/<task_id>/` with an 8MiB
//!   stop-capture cap (drain keeps READING and discarding past the cap so
//!   the child never SIGPIPEs), enforces the wall-clock deadline and the
//!   concurrency ceiling, and exposes snapshot/collect/kill primitives.
//! - User impact: long-running work outlives a model round; output lands on
//!   disk so `shell.collect` (and the meta.json fallback after a daemon
//!   restart) can retrieve it.
//! - Default: registry is empty; `SESSION_SCOPE` unset ⇒ session_id
//!   `"local"`.
//! - Interaction: the orchestrator wraps tool dispatch in `SESSION_SCOPE`
//!   (Lane O); the daemon drains via [`TaskRegistry::kill_all_blocking`]
//!   and sweeps crash orphans via [`sweep_orphans_on_boot`] (Lane G). Boot
//!   sweep is MARK-ONLY — it never signals (pid-reuse risk, D1).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, bail};
use futures_util::FutureExt;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use zen_core::atomic_file::write_atomic;
use zen_core::paths::user_root;

/// Poison-tolerant lock acquisition (review fix o; T264 precedent —
/// zen-gateway `guards::lock_ok`, mirrored locally to avoid a cross-crate
/// dep): a panicked holder must not poison-lock every later
/// spawn/collect/kill in the long-lived daemon. Warn on poison and recover
/// the inner data — registry state is plain snapshots, safe to continue
/// with.
fn lock_ok<'a, T>(lock: &'a Mutex<T>, what: &'static str) -> std::sync::MutexGuard<'a, T> {
    match lock.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!(
                what,
                "mutex poisoned; recovering inner data (T264 precedent)"
            );
            poisoned.into_inner()
        }
    }
}

/// Per-stream stop-capture threshold (D7): the drain task stops WRITING at
/// this many bytes but keeps READING and discarding, so the child can never
/// SIGPIPE. `bytes_*` keep counting past the cap; `truncated` flags it.
pub const OUTPUT_CAP_BYTES: u64 = 8 * 1024 * 1024;

/// `shell.collect` tail length in chars (D5), char-boundary-safe.
pub const COLLECT_TAIL_CHARS: usize = 8_000;

/// `shell.collect` inline threshold (D5): stdout at or below this many chars
/// is returned in full under the `stdout` key instead of a tail.
pub const COLLECT_INLINE_CHARS: usize = 4_000;

/// `shell.collect` default wait in ms (D5, codex-yield precedent).
pub const COLLECT_DEFAULT_WAIT_MS: u64 = 10_000;

/// SIGTERM → SIGKILL escalation window for [`TaskRegistry::kill`].
pub const KILL_ESCALATION_SECS: u64 = 3;

/// SIGTERM grace window inside [`TaskRegistry::kill_all_blocking`] before
/// the group-liveness probe + SIGKILL sweep (review fix d; bounded well
/// inside the daemon's 10s drain budget).
pub const KILL_ALL_GRACE_SECS: u64 = 2;

/// Drain read chunk size — small enough that byte counters update promptly,
/// large enough that an 8MiB stream costs ~1k lock acquisitions.
const DRAIN_CHUNK_BYTES: usize = 8192;

/// 7d retention window in ms — the age past which a `running` meta is a
/// stale relic. Shared by the boot sweep's stale-relic skip (probe/signal
/// work is never spent on relics retention will delete) and the W12.7
/// collect-fallback normalization (a relic can never terminalize — its
/// registry entry and wait task died with the daemon).
const RETENTION_WINDOW_MS: i64 = 7 * 86_400_000;

/// Lifecycle status of one background task.
///
/// Terminal variants are `Completed`/`Failed`/`Timeout`/`Killed`/`Expired`/
/// `Orphaned`; `Running` is the only non-terminal state. Serialized
/// externally-tagged snake_case into `meta.json` (`{"completed": 0}`).
///
/// Path mapping (W5.1, spec 006 D20 §2 — additive, not redefined): the
/// process path (`spawn_background`) keeps every variant it had, including
/// the boot-sweep-only `Orphaned`; the agent-future path (`spawn_async`)
/// reuses `Completed`/`Failed`/`Timeout`-free `Killed` semantics and adds
/// `Expired` for its wall-out and daemon-restart expiry mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Child still running.
    Running,
    /// Child exited with the given exit code. The agent-future path reuses
    /// this with code `0` (a future has no exit code; success is success).
    Completed(i32),
    /// Child died without an exit code (signal), the wait failed, or the
    /// agent future returned `Err`.
    Failed(String),
    /// Wall-clock deadline expired; the process group was SIGKILLed (D7).
    /// Process path only — the agent-future path uses [`TaskStatus::Expired`].
    Timeout,
    /// Killed via `shell.kill` / `kill_all` (SIGTERM → 3s → SIGKILL group;
    /// the agent-future path aborts the handle instead of signalling).
    Killed,
    /// Agent-future task exceeded its wall budget (GC tick) or was found
    /// running after a daemon restart (boot sweep) — marked WITHOUT
    /// aborting a live handle (the future may still finish; its late result
    /// is dropped and the parent is told to re-delegate). Never produced by
    /// the process path (which uses `Timeout`/`Orphaned`).
    Expired,
    /// Boot sweep found a `running` process-path meta with no live registry
    /// entry (daemon crash) — mark-only, never signalled (D1).
    Orphaned,
}

impl TaskStatus {
    /// True when the task has reached a terminal state.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        !matches!(self, TaskStatus::Running)
    }

    /// Lowercase machine/human label used in tool result JSON and audit
    /// lines (`running`, `completed`, `failed`, `timeout`, `killed`,
    /// `expired`, `orphaned`).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Running => "running",
            TaskStatus::Completed(_) => "completed",
            TaskStatus::Failed(_) => "failed",
            TaskStatus::Timeout => "timeout",
            TaskStatus::Killed => "killed",
            TaskStatus::Expired => "expired",
            TaskStatus::Orphaned => "orphaned",
        }
    }
}

/// Cheap-clone view of one background task for collect/push/audit consumers.
///
/// `command_digest` is binary + args joined, truncated to 80 chars
/// (char-boundary-safe) — env is already scrubbed at spawn, but the digest
/// deliberately stays a digest: it is for identification, not replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSnapshot {
    /// Registry task id (uuid v4, hex).
    pub task_id: String,
    /// Owning session (`SESSION_SCOPE` at spawn; `"local"` when unset).
    pub session_id: String,
    /// Child pid (also the process-group id — spawned with `process_group(0)`).
    pub pid: u32,
    /// binary + args joined, ≤80 chars, char-boundary-safe.
    pub command_digest: String,
    /// `~/.zen/logs/tasks/<task_id>/` as a lossy string.
    pub output_dir: String,
    /// Lifecycle status.
    pub status: TaskStatus,
    /// Spawn wall-clock (ms since UNIX_EPOCH).
    pub started_at_ms: i64,
    /// Terminal-transition wall-clock (ms since UNIX_EPOCH), None while running.
    pub ended_at_ms: Option<i64>,
    /// Total stdout bytes seen by the drain (keeps counting past the cap).
    pub bytes_stdout: u64,
    /// Total stderr bytes seen by the drain (keeps counting past the cap).
    pub bytes_stderr: u64,
    /// True once either stream exceeded [`OUTPUT_CAP_BYTES`].
    pub truncated: bool,
}

/// Caps handed to [`TaskRegistry::spawn_background`] (D7).
///
/// Built from `[agentic.background]` by the `shell.exec` background path;
/// `output_cap_bytes` is the code constant [`OUTPUT_CAP_BYTES`] in
/// production but is a field so tests can exercise the cap cheaply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackgroundLimits {
    /// Per-task wall-clock deadline in seconds (GC-tick enforced).
    pub max_wall_secs: u64,
    /// Registry-wide live-task ceiling (checked FIRST on spawn).
    pub max_concurrent: usize,
    /// Per-stream stop-capture threshold in bytes.
    pub output_cap_bytes: u64,
}

tokio::task_local! {
    /// Session scope for tool dispatch (APPROVAL_TURN task-local precedent).
    /// The orchestrator (Lane O) wraps dispatch in `SESSION_SCOPE`; tools
    /// read it via [`current_session_scope`]. Unset ⇒ `"local"`.
    pub static SESSION_SCOPE: String;

    /// MCP-surface marker (W12.28, review 2026-11-17 A2): the
    /// `HookedMcpTool` wrapper scopes `true` around every MCP-surface
    /// `tools/call`. MCP clients have no approval UX and no turn affinity,
    /// so any approval-requiring invocation dispatched under this marker
    /// resolves deny + audit + structured `approval-required` (the W5.2
    /// background-task fail-closed ruling, mirrored for MCP). Unset ⇒ the
    /// pre-W12.28 direct dispatch behavior is unchanged.
    pub static MCP_CALL: bool;
}

/// Real `tokio::process::Command`, re-exported for callers whose own `tokio`
/// is the shuttle-tokio alias (zen-agents/zen-gateway under the workspace
/// `shuttle` feature have no `tokio::process` module — construct background
/// commands through this alias to stay feature-unification-proof).
pub use tokio::process::Command as BackgroundCommand;

/// The `SESSION_SCOPE` value at the call site, or `"local"` when the
/// task-local is not set (direct CLI/test callers outside orchestrator
/// dispatch).
#[must_use]
pub fn current_session_scope() -> String {
    SESSION_SCOPE
        .try_with(|s| s.clone())
        .unwrap_or_else(|_| "local".to_string())
}

/// Whether the current task runs under the MCP-surface marker (W12.28/A2).
/// Unset outside the [`HookedMcpTool`](crate::mcp_hooked_tool::HookedMcpTool)
/// invoke scope ⇒ `false`.
#[must_use]
pub fn is_mcp_call() -> bool {
    MCP_CALL.try_get().unwrap_or(false)
}

/// Daemon-shutdown drain flag (review fix i): set by the gateway daemon at
/// the START of its shutdown drain — BEFORE turn cancellation — so a stuck
/// `block_in_place` turn can no longer spawn a background task that would
/// outlive the daemon (forced-drain late-spawn race).
///
/// Scope logic (Constitution XV):
/// - Functionality: `spawn_background` returns a structured rejection while
///   the flag is set; kill/collect/expire paths are unaffected
/// - User impact: during daemon shutdown no new detached children appear
/// - Default: unset (spawning allowed)
/// - Interaction: set once per daemon process at drain start; [`clear_draining`]
///   exists for tests and for an explicit restart-style reset
static DRAINING: AtomicBool = AtomicBool::new(false);

/// Mark the process as draining: no new background tasks may be spawned.
pub fn set_draining() {
    DRAINING.store(true, Ordering::SeqCst);
}

/// Clear the drain flag (test / explicit reset hook — the daemon never
/// unsets it; it exits instead).
pub fn clear_draining() {
    DRAINING.store(false, Ordering::SeqCst);
}

/// True while the daemon shutdown drain is in progress.
#[must_use]
pub fn is_draining() -> bool {
    DRAINING.load(Ordering::SeqCst)
}

/// Validate a model-supplied `task_id` (review fix a, P1).
///
/// Only a uuid v4 in simple (32 lowercase hex chars) or hyphenated
/// (8-4-4-4-12) form is accepted — exactly the shape
/// `shell.exec background:true` returns. Anything else (path fragments
/// like `..`, absolute paths, mixed separators, wrong length, non-hex) is
/// rejected BEFORE the id reaches [`meta_path`] or the registry, closing
/// the path-traversal hole where `logs/tasks/<task_id>/meta.json` could be
/// aimed at an arbitrary file.
#[must_use]
pub(crate) fn is_valid_task_id(s: &str) -> bool {
    let is_hex = |c: char| c.is_ascii_digit() || ('a'..='f').contains(&c);
    let groups: Vec<&str> = s.split('-').collect();
    match groups.len() {
        1 => s.len() == 32 && s.chars().all(is_hex),
        5 => [8usize, 4, 4, 4, 12]
            .iter()
            .zip(&groups)
            .all(|(len, group)| group.len() == *len && group.chars().all(is_hex)),
        _ => false,
    }
}

/// Defense-in-depth for the `meta.json` fallback (review fix a, P1): the
/// meta's recorded capture paths are EXTERNAL data (a crafted meta.json
/// could point `stdout_path` at `/etc/hosts`). Before the collect fallback
/// opens them, verify each path is absolute, free of `..`/`./` components,
/// names exactly `stdout`/`stderr`, and resolves — canonically, so symlink
/// escapes are caught too — to a file directly inside THIS task's
/// `logs/tasks/<task_id>/` directory.
///
/// A missing capture file is tolerated (the drain may never have created
/// it; there is nothing to read) — the lexical checks above still apply.
#[must_use]
pub(crate) fn meta_capture_paths_are_safe(task_id: &str, meta: &TaskMeta) -> bool {
    let Some(expected_dir) = meta_path(task_id).parent().map(Path::to_path_buf) else {
        return false;
    };
    let Ok(expected_canonical) = std::fs::canonicalize(&expected_dir) else {
        return false;
    };
    for raw in [&meta.stdout_path, &meta.stderr_path] {
        let path = PathBuf::from(raw);
        if !path.is_absolute() {
            return false;
        }
        if path.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        }) {
            return false;
        }
        let Some(file_name) = path.file_name() else {
            return false;
        };
        if file_name != "stdout" && file_name != "stderr" {
            return false;
        }
        let Some(parent) = path.parent() else {
            return false;
        };
        let Ok(parent_canonical) = std::fs::canonicalize(parent) else {
            return false;
        };
        if parent_canonical != expected_canonical {
            return false;
        }
        // The file itself may be a symlink pointing elsewhere — canonicalize
        // it too when it exists (missing file ⇒ nothing to read, tolerated).
        if let Ok(file_canonical) = std::fs::canonicalize(&path)
            && file_canonical != expected_canonical.join(file_name)
        {
            return false;
        }
    }
    true
}

/// On-disk mirror of a task (`logs/tasks/<task_id>/meta.json`).
///
/// Written via [`write_atomic`] at spawn (status `running`) and at every
/// terminal transition, so the boot sweep and the collect fallback work even
/// if the daemon dies between spawn and completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskMeta {
    /// Registry task id.
    pub task_id: String,
    /// Owning session id.
    pub session_id: String,
    /// Child pid (== process-group id).
    pub pid: u32,
    /// binary + args digest (≤80 chars).
    pub command_digest: String,
    /// Task output directory (lossy string).
    pub output_dir: String,
    /// Lifecycle status.
    pub status: TaskStatus,
    /// Spawn wall-clock (ms since UNIX_EPOCH).
    pub started_at_ms: i64,
    /// Terminal-transition wall-clock, None while running.
    pub ended_at_ms: Option<i64>,
    /// Total stdout bytes seen.
    pub bytes_stdout: u64,
    /// Total stderr bytes seen.
    pub bytes_stderr: u64,
    /// True once either stream exceeded the output cap.
    pub truncated: bool,
    /// Absolute path of the stdout capture file.
    pub stdout_path: String,
    /// Absolute path of the stderr capture file.
    pub stderr_path: String,
    /// Owning agent name (agent-future path only; `None` on the process
    /// path so process-path serialization stays byte-identical).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Fan-out slot index (W5.2 batch admissions only; omitted otherwise).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_index: Option<usize>,
    /// Agent-future result payload (spec 006 D20 §2: the result lives in
    /// meta; `None` until the future's own terminal transition).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
}

impl TaskMeta {
    /// Build the meta view from a snapshot plus the concrete capture paths.
    #[must_use]
    pub fn from_snapshot(snapshot: &TaskSnapshot, stdout_path: &Path, stderr_path: &Path) -> Self {
        Self {
            task_id: snapshot.task_id.clone(),
            session_id: snapshot.session_id.clone(),
            pid: snapshot.pid,
            command_digest: snapshot.command_digest.clone(),
            output_dir: snapshot.output_dir.clone(),
            status: snapshot.status.clone(),
            started_at_ms: snapshot.started_at_ms,
            ended_at_ms: snapshot.ended_at_ms,
            bytes_stdout: snapshot.bytes_stdout,
            bytes_stderr: snapshot.bytes_stderr,
            truncated: snapshot.truncated,
            stdout_path: stdout_path.to_string_lossy().into_owned(),
            stderr_path: stderr_path.to_string_lossy().into_owned(),
            // Additive agent-future fields (W5.1): the process path never
            // sets them, so its serialized bytes stay identical.
            agent: None,
            task_index: None,
            result: None,
        }
    }
}

/// One registry entry.
///
/// `child` is the D8 strong hold on the process path: the entry (and the
/// wait task's clone) keep the `tokio::process::Child` alive, which keeps
/// the pipe handles open past the spawning scope. Dropping the Child closes
/// the pipes and SIGPIPEs the child — entries are therefore never removed
/// in v1. Agent-future entries (W5.1) hold `child: None` and an
/// [`tokio::task::AbortHandle`] instead — killing them aborts the future;
/// there is nothing to signal.
#[derive(Clone)]
struct TaskEntry {
    session_id: String,
    output_dir: PathBuf,
    limits: BackgroundLimits,
    snapshot: Arc<Mutex<TaskSnapshot>>,
    // D8 strong hold: never READ, but keeping the Child alive keeps the pipe
    // handles open past the spawning scope — dropping it would close the
    // pipes and SIGPIPE the child. Entries are therefore never removed in v1.
    // `None` on the agent-future path (no process; kill aborts instead).
    #[allow(dead_code)]
    child: Option<Arc<tokio::sync::Mutex<tokio::process::Child>>>,
    /// Set by the wait task once `child.wait()` returned — the kill
    /// escalation thread checks it before SIGKILLing the (possibly dead)
    /// group. Always `false` on the agent-future path (nothing is reaped).
    reaped: Arc<AtomicBool>,
    /// Per-session push cursor (D4): set when `take_status_changes` has
    /// delivered this task's terminal transition to its owner.
    notified: Arc<AtomicBool>,
    /// Agent-future abort handle (`None` on the process path): kill/drain
    /// abort the future; expiry marks `Expired` WITHOUT aborting (W5.1
    /// acceptance).
    abort_handle: Option<tokio::task::AbortHandle>,
    /// Owning agent name (agent-future path; `None` for process entries) —
    /// preserved into every meta rewrite so the unified schema keeps the
    /// boot-sweep discriminator.
    agent: Option<String>,
    /// Fan-out slot index (agent-future batch admissions; `None` otherwise),
    /// preserved into every meta rewrite.
    task_index: Option<usize>,
    /// Agent-future result stash: the Ok payload written by the completion
    /// wrapper under the SAME snapshot-lock critical section as the status
    /// transition, so a collector that observes a terminal status never
    /// races the meta.json rewrite (W5.3). `None` on failure/late results.
    async_result: Arc<Mutex<Option<serde_json::Value>>>,
}

/// W12.5 (review P1-perf/N11): does this entry still hold a LIVE handle —
/// the unit the `[agentic.background] max_concurrent` union cap actually
/// bounds?
///
/// A `Running` status is live by definition. A TERMINAL status is still
/// live while its handle has not terminated/been reaped:
/// - `Expired` (agent-future path, W5.1): expiry marks the status WITHOUT
///   aborting the handle — the future keeps polling and holds its token
///   reserve, so it consumes a slot until the handle finishes.
/// - `Killed`: the async kill's abort lands at the future's next await
///   point (live until `is_finished()`); the process kill's SIGTERM grace
///   window leaves the child alive until the wait task reaps it
///   (`reaped` flag).
/// - `Timeout` (process path): the group was SIGKILLed — dead by
///   construction, only the microsecond reap window remains; not live.
/// - `Completed`/`Failed`/`Orphaned`: terminal with nothing running.
///
/// A missing `abort_handle` on the async path (the pre-wiring window
/// between entry insert and handle wiring inside `spawn_async`) is
/// treated as LIVE — conservative in the safe direction: the cap may
/// briefly over-count, never under-count.
fn entry_holds_live_handle(entry: &TaskEntry) -> bool {
    let status = lock_ok(&entry.snapshot, "task snapshot").status.clone();
    match status {
        TaskStatus::Running => true,
        TaskStatus::Expired => entry.abort_handle.as_ref().is_none_or(|h| !h.is_finished()),
        TaskStatus::Killed => {
            if entry.child.is_none() {
                // Agent-future path: live until the abort lands.
                entry.abort_handle.as_ref().is_none_or(|h| !h.is_finished())
            } else {
                // Process path: live until the wait task reaps the child.
                !entry.reaped.load(Ordering::SeqCst)
            }
        }
        _ => false,
    }
}

/// Process-global background task registry (D8: de facto daemon-scoped —
/// all hosted turns run in the daemon per 004).
pub struct TaskRegistry {
    entries: Mutex<HashMap<String, TaskEntry>>,
}

impl TaskRegistry {
    fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Register and launch one background child.
    ///
    /// GIVEN a spawned-ready tokio Command (stdout/stderr piped, process
    /// group 0, kill_on_drop false — the caller's job),
    /// WHEN the concurrency gate passes,
    /// THEN the child is spawned, its pipes are drained to
    /// `logs/tasks/<task_id>/{stdout,stderr}` (stop-writing at the cap,
    /// keep-reading past it), an initial `meta.json` lands on disk BEFORE
    /// the first await point of any consumer, and a wait task records the
    /// exit and rewrites the meta at the terminal transition.
    ///
    /// The concurrency slot is reserved ATOMICALLY (review fix g): the
    /// live-count check and the entry insert happen under ONE entries-lock
    /// guard, so `max_concurrent` cannot be overshot by buffered concurrent
    /// spawns racing through separate check/insert critical sections.
    ///
    /// # Errors
    /// - Drain flag set ([`is_draining`]): the daemon is shutting down.
    /// - Concurrency gate: `live_count() >= limits.max_concurrent` (the
    ///   error message names the limit — the tool layer turns it into a
    ///   structured JSON rejection).
    /// - Spawn failure or a vanished pid.
    /// - Output-dir creation failure.
    ///
    /// Must be called from within a tokio runtime (spawns drain/wait tasks).
    pub fn spawn_background(
        &self,
        mut cmd: tokio::process::Command,
        session_id: &str,
        command_digest: &str,
        limits: &BackgroundLimits,
    ) -> anyhow::Result<TaskSnapshot> {
        if is_draining() {
            bail!(
                "background task rejected: the daemon is shutting down (drain \
                 in progress); no new background tasks may be spawned"
            );
        }

        let (
            task_id,
            output_dir,
            child,
            snapshot,
            reaped,
            stdout_path,
            stderr_path,
            stdout_pipe,
            stderr_pipe,
        ) = {
            // Atomic reservation (review fix g): ONE critical section covers
            // the live-count check AND the entry insert. Everything between
            // is sync (dir create, fork/exec, meta build), so holding the
            // entries lock across it is safe and bounds the overshoot race.
            let mut map = lock_ok(&self.entries, "registry entries");
            // W12.6: re-check inside the critical section. A spawn whose
            // outer check passed can still race the drain (flag set +
            // kill_all landing between the outer check and this lock
            // acquisition) — an entry inserted here would be a live pump
            // the drain already decided it had killed. Same structured
            // rejection as the outer check.
            if is_draining() {
                bail!(
                    "background task rejected: the daemon is shutting down (drain \
                     in progress); no new background tasks may be spawned"
                );
            }
            // W12.5: the union cap counts LIVE handles, not merely `Running`
            // statuses — an expiry-zombie (terminal status, handle still
            // alive; W5.1 expiry never aborts) consumes a slot until its
            // handle terminates or is reaped.
            let live = map.values().filter(|e| entry_holds_live_handle(e)).count();
            if live >= limits.max_concurrent {
                bail!(
                    "background task rejected: {live} live tasks already at the \
                     [agentic.background] max_concurrent limit of {}; collect or \
                     kill existing tasks first (shell.collect / shell.kill)",
                    limits.max_concurrent
                );
            }

            let task_id = uuid::Uuid::new_v4().simple().to_string();
            let output_dir = user_root()
                .join(zen_core::constants::LOGS_DIR)
                .join("tasks")
                .join(&task_id);
            std::fs::create_dir_all(&output_dir).map_err(|e| {
                anyhow!(
                    "failed to create task output dir {}: {e}",
                    output_dir.display()
                )
            })?;

            let mut child = cmd
                .spawn()
                .map_err(|e| anyhow!("failed to spawn background task: {e}"))?;
            let pid = child
                .id()
                .ok_or_else(|| anyhow!("background child pid vanished immediately after spawn"))?;

            let stdout_pipe = child.stdout.take();
            let stderr_pipe = child.stderr.take();

            let started_at_ms = now_ms();
            let snapshot = Arc::new(Mutex::new(TaskSnapshot {
                task_id: task_id.clone(),
                session_id: session_id.to_string(),
                pid,
                command_digest: command_digest.to_string(),
                output_dir: output_dir.to_string_lossy().into_owned(),
                status: TaskStatus::Running,
                started_at_ms,
                ended_at_ms: None,
                bytes_stdout: 0,
                bytes_stderr: 0,
                truncated: false,
            }));

            let stdout_path = output_dir.join("stdout");
            let stderr_path = output_dir.join("stderr");

            let child = Arc::new(tokio::sync::Mutex::new(child));
            let reaped = Arc::new(AtomicBool::new(false));
            let entry = TaskEntry {
                session_id: session_id.to_string(),
                output_dir: output_dir.clone(),
                limits: *limits,
                snapshot: Arc::clone(&snapshot),
                child: Some(Arc::clone(&child)),
                reaped: Arc::clone(&reaped),
                notified: Arc::new(AtomicBool::new(false)),
                abort_handle: None,
                agent: None,
                task_index: None,
                async_result: Arc::new(Mutex::new(None)),
            };
            map.insert(task_id.clone(), entry);
            (
                task_id,
                output_dir,
                child,
                snapshot,
                reaped,
                stdout_path,
                stderr_path,
                stdout_pipe,
                stderr_pipe,
            )
        };

        // Initial meta BEFORE returning: the boot sweep and the collect
        // fallback must see the task even if the daemon dies right after
        // spawn (design Section 5 spawn contract).
        write_meta(
            &output_dir,
            &lock_ok(&snapshot, "task snapshot").clone(),
            &stdout_path,
            &stderr_path,
        );

        // Drain tasks own their pipe handles (taken above, never the Child):
        // they write to disk up to the cap, then keep reading + discarding
        // while counting bytes (D7 — the child must never SIGPIPE).
        let mut handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
        if let Some(pipe) = stdout_pipe {
            handles.push(tokio::spawn(drain_pipe(
                pipe,
                stdout_path.clone(),
                limits.output_cap_bytes,
                Arc::clone(&snapshot),
                true,
            )));
        }
        if let Some(pipe) = stderr_pipe {
            handles.push(tokio::spawn(drain_pipe(
                pipe,
                stderr_path.clone(),
                limits.output_cap_bytes,
                Arc::clone(&snapshot),
                false,
            )));
        }

        // Wait task: the second strong hold on the Child. It reaps the
        // child, records the exit, transitions the status (unless a kill
        // already marked it terminal) and rewrites meta.json.
        let wait_snapshot = Arc::clone(&snapshot);
        let wait_child = Arc::clone(&child);
        let wait_reaped = Arc::clone(&reaped);
        let wait_dir = output_dir.clone();
        let wait_stdout_path = stdout_path.clone();
        let wait_stderr_path = stderr_path.clone();
        handles.push(tokio::spawn(async move {
            let status = {
                let mut guarded = wait_child.lock().await;
                guarded.wait().await
            };
            wait_reaped.store(true, Ordering::SeqCst);
            let terminal_snapshot = {
                let mut guard = lock_ok(&wait_snapshot, "task snapshot");
                if guard.status.is_terminal() {
                    // kill/expire already recorded the terminal status —
                    // never overwrite it with the raw exit.
                } else {
                    guard.status = match &status {
                        Ok(exit) => match exit.code() {
                            Some(code) => TaskStatus::Completed(code),
                            None => TaskStatus::Failed(format!(
                                "child terminated without an exit code: {exit}"
                            )),
                        },
                        Err(e) => TaskStatus::Failed(format!("wait failed: {e}")),
                    };
                    guard.ended_at_ms = Some(now_ms());
                }
                guard.clone()
            };
            // Blocking small-file I/O off the async worker (house
            // spawn_blocking discipline).
            let _ = tokio::task::spawn_blocking(move || {
                write_meta(
                    &wait_dir,
                    &terminal_snapshot,
                    &wait_stdout_path,
                    &wait_stderr_path,
                )
            })
            .await;
        }));

        // Terminal meta rewrite AFTER the drains settle (review fix f): the
        // wait task's terminal write can race ahead of the drain tasks'
        // last flushes, so a collect right after completion could see short
        // files / stale byte counts. This finalizer awaits BOTH drain tasks
        // AND the wait task (5s timeout each — a descendant can hold a pipe
        // open indefinitely, so the await must stay bounded), then rewrites
        // meta.json once more with the final byte counts (only when the
        // snapshot is terminal; a later kill's own write always follows).
        let fin_snapshot = Arc::clone(&snapshot);
        let fin_dir = output_dir.clone();
        let fin_stdout_path = stdout_path.clone();
        let fin_stderr_path = stderr_path.clone();
        tokio::spawn(async move {
            for handle in handles {
                let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
            }
            let final_snapshot = lock_ok(&fin_snapshot, "task snapshot").clone();
            if final_snapshot.status.is_terminal() {
                let _ = tokio::task::spawn_blocking(move || {
                    write_meta(
                        &fin_dir,
                        &final_snapshot,
                        &fin_stdout_path,
                        &fin_stderr_path,
                    )
                })
                .await;
            }
        });

        let initial = lock_ok(&snapshot, "task snapshot").clone();
        tracing::info!(
            target: "zen_background",
            task_id = %task_id,
            pid = lock_ok(&snapshot, "task snapshot").pid,
            session = %session_id,
            digest = %command_digest,
            "background task spawned"
        );
        Ok(initial)
    }

    /// Register and launch one background agent future (W5.1, spec 006 D20
    /// §2): the agent-future sibling of [`TaskRegistry::spawn_background`].
    ///
    /// GIVEN a caller-supplied uuid-shaped `task_id`, the owning
    /// `session_id`, the delegating `agent` name, the same
    /// [`BackgroundLimits`] caps, and a future producing the task result,
    /// WHEN the drain flag is clear, the concurrency gate passes, the id is
    /// valid and unused, and the output dir is creatable,
    /// THEN the entry is registered, an initial `meta.json` (unified schema,
    /// `agent` set, `result` absent) lands BEFORE the future's first await,
    /// and the returned [`tokio::task::JoinHandle`] yields the future's
    /// `Ok` payload. On completion the status transitions
    /// `Ok → Completed(0)` / `Err → Failed` (unless a kill/expire already
    /// recorded a terminal status — never overwritten) and the meta is
    /// rewritten with the result payload. There is NO process: nothing is
    /// ever signalled, `stdout`/`stderr` capture files are unused (the
    /// conventional paths are still recorded so one schema serves both
    /// paths), and the GC marks wall-out expiry as [`TaskStatus::Expired`]
    /// WITHOUT aborting the live handle.
    ///
    /// The concurrency slot is reserved under the same single
    /// entries-lock critical section as the process path, and `task_id`
    /// must pass the same uuid-shape validation (it reaches
    /// [`meta_path`]).
    ///
    /// # Errors
    /// - Drain flag set ([`is_draining`]).
    /// - Invalid or duplicate `task_id`.
    /// - Concurrency gate: `live_count() >= limits.max_concurrent`.
    /// - Output-dir creation failure.
    ///
    /// Must be called from within a tokio runtime.
    pub fn spawn_async<F>(
        &self,
        task_id: &str,
        session_id: &str,
        agent: &str,
        task_index: Option<usize>,
        limits: &BackgroundLimits,
        future: F,
    ) -> anyhow::Result<tokio::task::JoinHandle<serde_json::Value>>
    where
        F: std::future::Future<Output = anyhow::Result<serde_json::Value>> + Send + 'static,
    {
        if is_draining() {
            bail!(
                "background task rejected: the daemon is shutting down (drain \
                 in progress); no new background tasks may be spawned"
            );
        }
        if !is_valid_task_id(task_id) {
            bail!(
                "invalid background task id `{task_id}`: expected a uuid v4 \
                 (simple or hyphenated)"
            );
        }

        let (snapshot, output_dir, reaped, async_result) = {
            // Atomic reservation (review fix g parity): ONE critical
            // section covers the live-count check, the duplicate-id check,
            // and the entry insert.
            let mut map = lock_ok(&self.entries, "registry entries");
            // W12.6 parity: the drain re-check must sit inside the same
            // critical section as the insert — the outer check alone
            // leaves the flag-set + kill_all vs lock-acquisition window
            // open (leaked live future the drain never saw).
            if is_draining() {
                bail!(
                    "background task rejected: the daemon is shutting down (drain \
                     in progress); no new background tasks may be spawned"
                );
            }
            // W12.5 parity: same live-handle union cap as the process path
            // (expiry-zombie futures consume a slot until they finish).
            let live = map.values().filter(|e| entry_holds_live_handle(e)).count();
            if live >= limits.max_concurrent {
                bail!(
                    "background task rejected: {live} live tasks already at the \
                     max_concurrent limit of {}; collect or kill existing tasks \
                     first",
                    limits.max_concurrent
                );
            }
            if map.contains_key(task_id) {
                bail!("background task id {task_id} is already registered");
            }

            let output_dir = user_root()
                .join(zen_core::constants::LOGS_DIR)
                .join("tasks")
                .join(task_id);
            std::fs::create_dir_all(&output_dir).map_err(|e| {
                anyhow!(
                    "failed to create task output dir {}: {e}",
                    output_dir.display()
                )
            })?;

            let started_at_ms = now_ms();
            let snapshot = Arc::new(Mutex::new(TaskSnapshot {
                task_id: task_id.to_string(),
                session_id: session_id.to_string(),
                // No child process on the agent-future path.
                pid: 0,
                command_digest: truncate_char_safe(&format!("delegate:{agent}"), 80),
                output_dir: output_dir.to_string_lossy().into_owned(),
                status: TaskStatus::Running,
                started_at_ms,
                ended_at_ms: None,
                bytes_stdout: 0,
                bytes_stderr: 0,
                truncated: false,
            }));

            let reaped = Arc::new(AtomicBool::new(false));
            let async_result = Arc::new(Mutex::new(None));
            let async_result_for_wrapper = Arc::clone(&async_result);
            let entry = TaskEntry {
                session_id: session_id.to_string(),
                output_dir: output_dir.clone(),
                limits: *limits,
                snapshot: Arc::clone(&snapshot),
                child: None,
                reaped: Arc::clone(&reaped),
                notified: Arc::new(AtomicBool::new(false)),
                abort_handle: None,
                agent: Some(agent.to_string()),
                task_index,
                async_result,
            };
            map.insert(task_id.to_string(), entry);
            (snapshot, output_dir, reaped, async_result_for_wrapper)
        };

        // Initial meta BEFORE returning (design Section 5 spawn contract
        // parity): the boot sweep and the collect fallback must see the
        // task even if the daemon dies right after admission. The
        // conventional stdout/stderr capture paths are recorded (files
        // never created on this path) so `meta_capture_paths_are_safe`
        // keeps validating.
        let stdout_path = meta_stdout_path(&output_dir);
        let stderr_path = meta_stderr_path(&output_dir);
        let initial_snapshot = lock_ok(&snapshot, "task snapshot").clone();
        write_async_meta(
            &output_dir,
            &initial_snapshot,
            agent,
            task_index,
            None,
            &stdout_path,
            &stderr_path,
        );

        // Agent task: drives the future to completion, performs the
        // terminal transition (unless a kill/expire already recorded one —
        // an Expired task's late result is dropped; the parent re-delegates,
        // spec D20 §8) and rewrites meta.json with the payload. Nothing is
        // signalled; the returned JoinHandle is the only cancellation path.
        let wait_dir = output_dir.clone();
        let wait_agent = agent.to_string();
        let wait_snapshot = Arc::clone(&snapshot);
        let wait_async_result = Arc::clone(&async_result);
        let join = tokio::spawn(async move {
            // F6 (W15): a panic inside the agent future would unwind this
            // wrapper task and leave the status Running until expiry. Catch
            // it and map to the terminal Failed state (the same shape
            // kill/expiry write). The future is dropped here, so its token
            // reservation Drop refund is unchanged.
            let outcome = match std::panic::AssertUnwindSafe(future).catch_unwind().await {
                Ok(outcome) => outcome,
                Err(panic) => {
                    let detail = panic
                        .downcast_ref::<&str>()
                        .map(|s| (*s).to_string())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown panic".to_string());
                    Err(anyhow!(
                        "delegate task panicked: {}",
                        truncate_char_safe(&detail, 200)
                    ))
                }
            };
            let (result, terminal_snapshot) = {
                let mut guard = lock_ok(&wait_snapshot, "task snapshot");
                let result = match outcome {
                    Ok(value) => {
                        if !guard.status.is_terminal() {
                            guard.status = TaskStatus::Completed(0);
                            guard.ended_at_ms = Some(now_ms());
                            // Same-lock stash (W5.3): a collector observing
                            // the Completed status sees the payload without
                            // racing the async meta.json rewrite.
                            *lock_ok(&wait_async_result, "task async result") = Some(value.clone());
                            Some(value)
                        } else {
                            // Expired/killed earlier: keep the recorded
                            // terminal status, drop the late result.
                            None
                        }
                    }
                    Err(e) => {
                        if !guard.status.is_terminal() {
                            guard.status = TaskStatus::Failed(format!("delegate task failed: {e}"));
                            guard.ended_at_ms = Some(now_ms());
                        }
                        None
                    }
                };
                (result, guard.clone())
            };
            // The handle's yield: the Ok payload when this completion drove
            // the transition; Null on failure or when a kill/expire already
            // owned the terminal status (collectors read meta, not this).
            let ret = result.clone().unwrap_or(serde_json::Value::Null);
            // Blocking small-file I/O off the async worker (house
            // spawn_blocking discipline).
            let _ = tokio::task::spawn_blocking(move || {
                write_async_meta(
                    &wait_dir,
                    &terminal_snapshot,
                    &wait_agent,
                    task_index,
                    result,
                    &meta_stdout_path(&wait_dir),
                    &meta_stderr_path(&wait_dir),
                )
            })
            .await;
            ret
        });

        // Wire the abort handle (kill/drain paths) — a kill racing BEFORE
        // this line marks `Killed` without aborting; the future's own
        // completion then never overwrites the terminal status, so the
        // observable contract is unchanged.
        if let Some(entry) = lock_ok(&self.entries, "registry entries").get_mut(task_id) {
            entry.abort_handle = Some(join.abort_handle());
        }
        let _ = reaped; // process-path parity: never set on the async path

        tracing::info!(
            target: "zen_background",
            task_id = %task_id,
            session = %session_id,
            agent = %agent,
            "background agent task spawned"
        );
        Ok(join)
    }

    /// Cheap clone of one task's snapshot, or `None` for an unknown id.
    #[must_use]
    pub fn snapshot(&self, task_id: &str) -> Option<TaskSnapshot> {
        let map = lock_ok(&self.entries, "registry entries");
        map.get(task_id)
            .map(|e| lock_ok(&e.snapshot, "task snapshot").clone())
    }

    /// Per-session push cursor (D4, at-least-once per the 2026-10-31 review):
    /// every terminal transition of this session's tasks that has NOT been
    /// marked consumed yet. Does NOT mark — the orchestrator confirms via
    /// [`TaskRegistry::mark_notified`] only after the model round that
    /// carried the notice returned Ok, so a failed round re-offers the
    /// notice (advisory data tolerates duplicates, never loss). Cross-session
    /// tasks are NEVER returned (Confidential leak guard — security pin,
    /// design Section 7 item 8).
    #[must_use]
    pub fn take_status_changes(&self, session_id: &str) -> Vec<TaskSnapshot> {
        let mut out = Vec::new();
        let map = lock_ok(&self.entries, "registry entries");
        for entry in map.values() {
            if entry.session_id != session_id || entry.notified.load(Ordering::SeqCst) {
                continue;
            }
            let guard = lock_ok(&entry.snapshot, "task snapshot");
            if guard.status.is_terminal() {
                out.push(guard.clone());
            }
        }
        out
    }

    /// Consume the per-session push cursor for exactly these task ids (D4
    /// at-least-once). Session-guarded: ids belonging to another session are
    /// never marked, so a stale caller cannot silence another session's
    /// notices.
    pub fn mark_notified(&self, session_id: &str, task_ids: &[String]) {
        let map = lock_ok(&self.entries, "registry entries");
        for id in task_ids {
            if let Some(entry) = map.get(id)
                && entry.session_id == session_id
            {
                entry.notified.store(true, Ordering::SeqCst);
            }
        }
    }

    /// Number of live (Running) tasks across all sessions.
    #[must_use]
    pub fn live_count(&self) -> usize {
        lock_ok(&self.entries, "registry entries")
            .values()
            .filter(|e| lock_ok(&e.snapshot, "task snapshot").status == TaskStatus::Running)
            .count()
    }

    /// Live (Running) snapshots owned by `session_id`.
    #[must_use]
    pub fn live_for_session(&self, session_id: &str) -> Vec<TaskSnapshot> {
        lock_ok(&self.entries, "registry entries")
            .values()
            .filter(|e| e.session_id == session_id)
            .filter(|e| lock_ok(&e.snapshot, "task snapshot").status == TaskStatus::Running)
            .map(|e| lock_ok(&e.snapshot, "task snapshot").clone())
            .collect()
    }

    /// Delegate-ness discriminator (W5.4): true when the task was admitted
    /// through the agent-future path (no child process, delegate digest).
    /// Registry miss falls back to the on-disk meta (daemon-restart case).
    #[must_use]
    pub fn is_delegate_task(&self, task_id: &str) -> bool {
        let map = lock_ok(&self.entries, "registry entries");
        if let Some(entry) = map.get(task_id) {
            let guard = lock_ok(&entry.snapshot, "task snapshot");
            return entry.child.is_none()
                && (guard.command_digest.starts_with("delegate:") || entry.agent.is_some());
        }
        read_meta(task_id).is_some_and(|m| m.pid == 0 && m.agent.is_some())
    }

    /// Unified D25 A5 notice payload for a terminal delegate task (W5.4):
    /// `{task_index, agent, status, response|error}` — `model_source` is
    /// injected by the caller (it lives in the admission book, not the
    /// registry). Registry miss falls back to `meta.json`. `None` when the
    /// id is unknown everywhere.
    #[must_use]
    pub fn delegate_notice_payload(&self, task_id: &str) -> Option<serde_json::Value> {
        let mut payload = serde_json::Map::new();
        let mut have = false;
        {
            let map = lock_ok(&self.entries, "registry entries");
            if let Some(entry) = map.get(task_id) {
                have = true;
                let guard = lock_ok(&entry.snapshot, "task snapshot");
                payload.insert(
                    "task_index".into(),
                    entry
                        .task_index
                        .map(serde_json::Value::from)
                        .unwrap_or(serde_json::Value::Null),
                );
                payload.insert(
                    "agent".into(),
                    entry
                        .agent
                        .clone()
                        .map(serde_json::Value::from)
                        .unwrap_or(serde_json::Value::Null),
                );
                payload.insert(
                    "status".into(),
                    serde_json::Value::from(guard.status.as_str()),
                );
                if guard.status.is_terminal() {
                    let stash = lock_ok(&entry.async_result, "task async result").clone();
                    let (response, error) = outcome_parts(&guard.status, stash);
                    if let Some(v) = response {
                        payload.insert("response".into(), v);
                    }
                    if let Some(e) = error {
                        payload.insert("error".into(), serde_json::Value::from(e));
                    }
                }
            }
        }
        if !have {
            let meta = read_meta(task_id)?;
            payload.insert(
                "task_index".into(),
                meta.task_index
                    .map(serde_json::Value::from)
                    .unwrap_or(serde_json::Value::Null),
            );
            payload.insert(
                "agent".into(),
                meta.agent
                    .clone()
                    .map(serde_json::Value::from)
                    .unwrap_or(serde_json::Value::Null),
            );
            payload.insert(
                "status".into(),
                serde_json::Value::from(meta.status.as_str()),
            );
            let (response, error) = outcome_parts(&meta.status, meta.result.clone());
            if let Some(v) = response {
                payload.insert("response".into(), v);
            }
            if let Some(e) = error {
                payload.insert("error".into(), serde_json::Value::from(e));
            }
        }
        Some(serde_json::Value::Object(payload))
    }

    /// Kill a task: SIGTERM the process group, escalate to SIGKILL after
    /// [`KILL_ESCALATION_SECS`] if the group is still alive, mark the
    /// status `Killed` and rewrite meta.json.
    ///
    /// Non-blocking: the 3s escalation runs on a detached thread, so the
    /// caller (async tool invoke / SIGTERM drain) returns immediately with
    /// the `Killed` snapshot — matching the `shell.kill` tool contract.
    /// Idempotent on already-terminal tasks (returns the current snapshot
    /// without signalling).
    ///
    /// Pid-reuse guard (review fix b): when the wait task has already
    /// reaped the child (`reaped`), NO signal is sent — the pid may have
    /// been recycled — but the status/meta still transition.
    ///
    /// # Errors
    /// Unknown `task_id` (the tool layer falls back to meta.json).
    pub fn kill(&self, task_id: &str) -> anyhow::Result<TaskSnapshot> {
        let entry = lock_ok(&self.entries, "registry entries")
            .get(task_id)
            .cloned()
            .ok_or_else(|| anyhow!("unknown task_id {task_id}"))?;

        let (pid, snapshot, signal) = {
            let mut guard = lock_ok(&entry.snapshot, "task snapshot");
            if guard.status.is_terminal() {
                return Ok(guard.clone());
            }
            let reaped = entry.reaped.load(Ordering::SeqCst);
            guard.status = TaskStatus::Killed;
            guard.ended_at_ms = Some(now_ms());
            (guard.pid, guard.clone(), !reaped)
        };

        if entry.child.is_none() {
            // Agent-future path (W5.1): abort the handle — there is no
            // process group, and pid 0 would target OUR OWN group. The
            // future's own completion can no longer overwrite the terminal
            // status.
            if let Some(handle) = &entry.abort_handle {
                handle.abort();
            }
            write_async_meta(
                &entry.output_dir,
                &snapshot,
                entry.agent.as_deref().unwrap_or("unknown"),
                entry.task_index,
                None,
                &meta_stdout_path(&entry.output_dir),
                &meta_stderr_path(&entry.output_dir),
            );
            tracing::info!(target: "zen_background", task_id = %task_id, "background agent task killed (future aborted)");
            return Ok(snapshot);
        }
        if signal {
            kill_group(pid, libc::SIGTERM);
            spawn_kill_escalation(pid);
        } else {
            tracing::info!(target: "zen_background", task_id = %task_id, pid, "background task marked killed without signalling (already reaped — pid-reuse guard)");
        }
        write_meta(
            &entry.output_dir,
            &snapshot,
            &meta_stdout_path(&entry.output_dir),
            &meta_stderr_path(&entry.output_dir),
        );
        tracing::info!(target: "zen_background", task_id = %task_id, pid, "background task killed (SIGTERM, escalation armed)");
        Ok(snapshot)
    }

    /// Yield-wait collect for an agent-future task (W5.3 `delegate.collect`).
    ///
    /// Polls the task snapshot on a fixed 25ms tick until a terminal state
    /// or `wait_ms` elapses — never a busy-spin, never aborts. The Ok
    /// payload comes from the same-lock stash (no meta rewrite race); a
    /// registry-miss falls back to the on-disk `meta.json` (daemon-restart
    /// case), where a stale `running` meta older than the 7d retention
    /// window is normalized to `Expired` for the response and on disk
    /// (W12.7 — never running-forever). Idempotent: terminal states are
    /// stable and repeated collects observe the same status.
    ///
    /// # Errors
    /// Unknown id with no `meta.json` fallback.
    pub async fn collect_delegate(
        &self,
        task_id: &str,
        wait_ms: u64,
    ) -> anyhow::Result<DelegateCollect> {
        const POLL_TICK_MS: u64 = 25;
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(wait_ms);
        loop {
            let entry = lock_ok(&self.entries, "registry entries")
                .get(task_id)
                .cloned();
            if let Some(entry) = entry {
                let (status, result, session_id) = {
                    let guard = lock_ok(&entry.snapshot, "task snapshot");
                    let result = if guard.status.is_terminal() {
                        lock_ok(&entry.async_result, "task async result").clone()
                    } else {
                        None
                    };
                    (guard.status.clone(), result, entry.session_id.clone())
                };
                if status.is_terminal() {
                    let (response, error) = outcome_parts(&status, result);
                    return Ok(DelegateCollect {
                        task_id: task_id.to_string(),
                        status: status.as_str().to_string(),
                        response,
                        error,
                        session_id: Some(session_id),
                        agent: entry.agent.clone(),
                        task_index: entry.task_index,
                        from_meta_fallback: false,
                    });
                }
            } else {
                // Registry miss → meta.json fallback (daemon restart).
                if let Some(mut meta) = read_meta(task_id) {
                    // W12.7: a stale (>7d) running meta can never
                    // terminalize — normalize the response to Expired and
                    // persist the rewrite (warn on failure) instead of
                    // reporting running-forever.
                    normalize_stale_running_meta(&mut meta);
                    let (response, error) = outcome_parts(&meta.status, meta.result.clone());
                    return Ok(DelegateCollect {
                        task_id: task_id.to_string(),
                        status: meta.status.as_str().to_string(),
                        response,
                        error,
                        session_id: Some(meta.session_id.clone()),
                        agent: meta.agent.clone(),
                        task_index: meta.task_index,
                        from_meta_fallback: true,
                    });
                }
                return Err(anyhow!(
                    "unknown task_id {task_id}: no registry entry and no meta.json under logs/tasks/"
                ));
            }
            if std::time::Instant::now() >= deadline {
                let agent_task_index = lock_ok(&self.entries, "registry entries")
                    .get(task_id)
                    .map(|e| (e.agent.clone(), e.task_index));
                return Ok(DelegateCollect {
                    task_id: task_id.to_string(),
                    status: "running".to_string(),
                    response: None,
                    error: None,
                    session_id: None,
                    agent: agent_task_index.as_ref().and_then(|(a, _)| a.clone()),
                    task_index: agent_task_index.and_then(|(_, t)| t),
                    from_meta_fallback: false,
                });
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            tokio::time::timeout(
                std::cmp::min(std::time::Duration::from_millis(POLL_TICK_MS), remaining),
                std::future::pending::<()>(),
            )
            .await
            .unwrap_or(());
        }
    }

    /// GC-tick deadline hook (D7): SIGKILL the group of every Running task
    /// whose `started_at_ms + max_wall_secs*1000 <= now_ms`, mark it
    /// `Timeout` and rewrite meta.json. Returns the expired task ids.
    ///
    /// `now_ms` is injected so tests (and the scheduler) are deterministic.
    #[must_use]
    pub fn expire_due(&self, now_ms: i64) -> Vec<String> {
        let expired: Vec<TaskEntry> = lock_ok(&self.entries, "registry entries")
            .values()
            .filter(|e| {
                let guard = lock_ok(&e.snapshot, "task snapshot");
                guard.status == TaskStatus::Running
                    && guard.started_at_ms + (e.limits.max_wall_secs as i64) * 1000 <= now_ms
            })
            .cloned()
            .collect();
        let mut ids = Vec::with_capacity(expired.len());
        for entry in expired {
            let is_async = entry.child.is_none();
            let (pid, snapshot, signal) = {
                let mut guard = lock_ok(&entry.snapshot, "task snapshot");
                if guard.status != TaskStatus::Running {
                    continue;
                }
                // Pid-reuse guard (review fix b): an already-reaped child's
                // pid may be recycled — never signal it.
                let reaped = entry.reaped.load(Ordering::SeqCst);
                // W5.1 expiry mapping: the agent-future path marks
                // `Expired` (spec 006 D20 §2) WITHOUT aborting the live
                // handle (acceptance) — the future may still finish; its
                // late result is dropped and the parent re-delegates. The
                // process path keeps `Timeout` + SIGKILL (D7).
                guard.status = if is_async {
                    TaskStatus::Expired
                } else {
                    TaskStatus::Timeout
                };
                guard.ended_at_ms = Some(now_ms);
                (guard.pid, guard.clone(), !reaped)
            };
            if is_async {
                write_async_meta(
                    &entry.output_dir,
                    &snapshot,
                    entry.agent.as_deref().unwrap_or("unknown"),
                    entry.task_index,
                    None,
                    &meta_stdout_path(&entry.output_dir),
                    &meta_stderr_path(&entry.output_dir),
                );
                tracing::warn!(target: "zen_background", task_id = %snapshot.task_id, "background agent task expired (deadline) — marked expired, handle NOT aborted");
                ids.push(snapshot.task_id.clone());
                continue;
            }
            // Deadline is a hard kill (D7): no SIGTERM grace window.
            if signal {
                kill_group(pid, libc::SIGKILL);
            }
            write_meta(
                &entry.output_dir,
                &snapshot,
                &meta_stdout_path(&entry.output_dir),
                &meta_stderr_path(&entry.output_dir),
            );
            tracing::warn!(target: "zen_background", task_id = %snapshot.task_id, pid, "background task expired (deadline), group SIGKILLed");
            ids.push(snapshot.task_id.clone());
        }
        ids
    }

    /// SIGTERM drain (D1): kill every live task, marking each `Killed`.
    /// `reason` is logged (daemon shutdown / restart drain). Returns the
    /// killed task ids.
    ///
    /// Non-blocking variant: escalation to SIGKILL runs on detached threads
    /// (see [`spawn_kill_escalation`]). The daemon's shutdown drain uses
    /// [`TaskRegistry::kill_all_blocking`] instead — a detached escalation
    /// thread dies with the exiting daemon, so its SIGKILL would never fire
    /// (review fix d).
    #[must_use]
    pub fn kill_all(&self, reason: &str) -> Vec<String> {
        let live: Vec<TaskEntry> = lock_ok(&self.entries, "registry entries")
            .values()
            .filter(|e| lock_ok(&e.snapshot, "task snapshot").status == TaskStatus::Running)
            .cloned()
            .collect();
        let mut ids = Vec::with_capacity(live.len());
        for entry in live {
            let is_async = entry.child.is_none();
            let (pid, snapshot) = {
                let mut guard = lock_ok(&entry.snapshot, "task snapshot");
                if guard.status != TaskStatus::Running {
                    continue;
                }
                guard.status = TaskStatus::Killed;
                guard.ended_at_ms = Some(now_ms());
                (guard.pid, guard.clone())
            };
            if is_async {
                // Agent-future path (W5.1): abort — nothing to signal.
                if let Some(handle) = &entry.abort_handle {
                    handle.abort();
                }
                write_async_meta(
                    &entry.output_dir,
                    &snapshot,
                    entry.agent.as_deref().unwrap_or("unknown"),
                    entry.task_index,
                    None,
                    &meta_stdout_path(&entry.output_dir),
                    &meta_stderr_path(&entry.output_dir),
                );
                tracing::info!(target: "zen_background", task_id = %snapshot.task_id, reason = %reason, "background agent task killed by drain (future aborted)");
                ids.push(snapshot.task_id.clone());
                continue;
            }
            kill_group(pid, libc::SIGTERM);
            spawn_kill_escalation(pid);
            write_meta(
                &entry.output_dir,
                &snapshot,
                &meta_stdout_path(&entry.output_dir),
                &meta_stderr_path(&entry.output_dir),
            );
            tracing::info!(target: "zen_background", task_id = %snapshot.task_id, pid, reason = %reason, "background task killed by drain");
            ids.push(snapshot.task_id.clone());
        }
        ids
    }

    /// Blocking variant of [`TaskRegistry::kill_all`] for the daemon
    /// shutdown drain (review fix d, D1): SIGTERM every live group, wait
    /// [`KILL_ALL_GRACE_SECS`], then SIGKILL every group STILL alive —
    /// SIGTERM-immune descendants survive a graceful leader exit and must
    /// not be left behind when the daemon exits. The whole body is bounded
    /// (~2s + probes) well inside the 10s drain budget, and it runs on the
    /// caller's thread so the escalation cannot die with an exiting daemon.
    ///
    /// Returns the killed task ids (same shape as [`TaskRegistry::kill_all`]).
    #[must_use]
    pub fn kill_all_blocking(&self, reason: &str) -> Vec<String> {
        let live: Vec<(TaskEntry, u32)> = self
            .entries
            .lock()
            .unwrap()
            .values()
            .filter(|e| lock_ok(&e.snapshot, "task snapshot").status == TaskStatus::Running)
            .cloned()
            .map(|e| {
                let pid = lock_ok(&e.snapshot, "task snapshot").pid;
                (e, pid)
            })
            .collect();
        let mut ids = Vec::with_capacity(live.len());
        for (entry, pid) in &live {
            let is_async = entry.child.is_none();
            let snapshot = {
                let mut guard = lock_ok(&entry.snapshot, "task snapshot");
                if guard.status != TaskStatus::Running {
                    continue;
                }
                guard.status = TaskStatus::Killed;
                guard.ended_at_ms = Some(now_ms());
                guard.clone()
            };
            if is_async {
                // Agent-future path (W5.1): abort — nothing to signal, and
                // the blocking drain's thread-based escalation has no
                // meaning for futures.
                if let Some(handle) = &entry.abort_handle {
                    handle.abort();
                }
                write_async_meta(
                    &entry.output_dir,
                    &snapshot,
                    entry.agent.as_deref().unwrap_or("unknown"),
                    entry.task_index,
                    None,
                    &meta_stdout_path(&entry.output_dir),
                    &meta_stderr_path(&entry.output_dir),
                );
                tracing::info!(target: "zen_background", task_id = %snapshot.task_id, reason = %reason, "background agent task killed by blocking drain (future aborted)");
                ids.push(snapshot.task_id.clone());
                continue;
            }
            kill_group(*pid, libc::SIGTERM);
            write_meta(
                &entry.output_dir,
                &snapshot,
                &meta_stdout_path(&entry.output_dir),
                &meta_stderr_path(&entry.output_dir),
            );
            tracing::info!(target: "zen_background", task_id = %snapshot.task_id, pid, reason = %reason, "background task killed by blocking drain (SIGTERM, escalation inline)");
            ids.push(snapshot.task_id.clone());
        }
        if ids.is_empty() {
            return ids;
        }
        std::thread::sleep(Duration::from_secs(KILL_ALL_GRACE_SECS));
        for (entry, pid) in &live {
            // G-40 (FR-029 drain e2e, 2026-11-21): agent-future entries carry
            // `pid: 0` and were already aborted + marked above. POSIX pid-0
            // kill semantics target the CALLER's own process group, so
            // probing or escalating them here would SIGKILL the daemon's
            // whole group mid-drain — the process dies inside
            // `kill_all_blocking` and the kill audit lines are never
            // written. No child process ⇒ nothing to escalate (the same
            // pid-0 hazard class the delegate.kill tool path documents).
            if entry.child.is_none() {
                continue;
            }
            if !group_alive(*pid) {
                continue;
            }
            kill_group(*pid, libc::SIGKILL);
            tracing::warn!(target: "zen_background", task_id = %lock_ok(&entry.snapshot, "task snapshot").task_id, pid, reason = %reason, "background task group survived SIGTERM; SIGKILLed by blocking drain");
        }
        ids
    }
}

/// The process-global registry (D8: OnceLock, de facto daemon-scoped).
#[must_use]
pub fn registry() -> &'static TaskRegistry {
    static REGISTRY: OnceLock<TaskRegistry> = OnceLock::new();
    REGISTRY.get_or_init(TaskRegistry::new)
}

/// Wall-clock age (seconds since start) of process `pid`, for orphan
/// identity verification. `None` when unreadable — callers MUST treat that
/// as unverifiable (mark-only), never as verification failure to kill.
///
/// macOS has NO `/proc`; libc 0.2 dropped BSD structs (`kinfo_proc`
/// unavailable), and a `sysinfo`-class crate for one boot-time probe would
/// violate the reuse/min-dep discipline. `/bin/ps` is a fixed absolute
/// binary (no shell, no PATH dependence), `etime=` prints age in
/// `[[DD-]HH:]MM:SS`, and the sweep runs once at daemon boot — never a hot
/// path.
#[cfg(target_os = "macos")]
fn leader_age_secs(pid: u32) -> Option<i64> {
    let out = std::process::Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "etime="])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // `etime` is [[DD-]HH:]MM:SS — zero-padded, so every "\d\d" chunk parses.
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if raw.is_empty() {
        return None;
    }
    let (days, rest) = match raw.split_once('-') {
        Some((d, r)) => (d.parse::<i64>().ok()?, r),
        None => (0_i64, raw.as_str()),
    };
    let parts: Vec<i64> = rest
        .split(':')
        .map(|p| p.parse().ok())
        .collect::<Option<Vec<i64>>>()?;
    let tail = match parts.len() {
        2 => parts[0] * 60 + parts[1],
        3 => parts[0] * 3600 + parts[1] * 60 + parts[2],
        _ => return None,
    };
    Some(days * 86_400 + tail)
}

#[cfg(target_os = "linux")]
fn leader_age_secs(pid: u32) -> Option<i64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm (field 2) may contain spaces/parens — anchor on the LAST ") ".
    let after_comm = stat.rsplit_once(") ")?.1;
    // Fields here start at state (field 3) ⇒ starttime (field 22) = index 19.
    let starttime_ticks: i64 = after_comm.split_whitespace().nth(19)?.parse().ok()?;
    // SAFETY: sysconf(_SC_CLK_TCK) is a pure query, no side effects.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if hz <= 0 {
        return None;
    }
    let boot_sec: i64 = std::fs::read_to_string("/proc/stat")
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("btime ").and_then(|v| v.trim().parse().ok()))?;
    let started = boot_sec * 1000 + starttime_ticks * 1000 / hz;
    Some((now_ms() - started) / 1000)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn leader_age_secs(_pid: u32) -> Option<i64> {
    None
}

/// Oracle ITEM 2 (2026-10-31 arbitration): a probed-live orphan group is OURS
/// only when the group leader's observed age matches the spawn age in our
/// meta. Pid numbers get recycled; a live pgid whose leader started at a
/// different time is someone else's process — never signal it. ±10s
/// tolerance covers scheduling/granularity skew between spawn bookkeeping
/// and the kernel/ps observation. Unreadable age ⇒ unverifiable (callers
/// must mark-only).
#[cfg(unix)]
#[must_use]
fn verify_leader_identity(pid: u32, started_at_ms: i64) -> bool {
    match leader_age_secs(pid) {
        Some(age) => (age - (now_ms() - started_at_ms) / 1000).abs() <= 10,
        None => false,
    }
}

/// Boot-time crash-orphan sweep (D1, amended 2026-10-31: two-tier).
///
/// GIVEN `logs/tasks/*/meta.json` files left in status `running` by a dead
/// daemon, WHEN this runs at daemon boot (Lane G wiring), THEN:
/// - group dead (kill(-pgid,0) ⇒ ESRCH) ⇒ mark `orphaned`;
/// - group live AND leader identity verified (start time ≈ meta spawn time)
///   ⇒ SIGKILL the group + mark `orphaned` (crash-loop accumulation bound:
///   each reboot reaps the previous generation's strays);
/// - group live but unverifiable (start-time mismatch / unreadable) ⇒
///   mark-only — never signal what we cannot prove is ours (pid-reuse
///   fail-safe, the original D1 invariant, kept for exactly this case).
///
/// Corrupt meta files are skipped with a warn (structured fallback, never a
/// panic on external data). Returns the number of metas marked.
#[must_use]
pub fn sweep_orphans_on_boot() -> usize {
    let root = user_root()
        .join(zen_core::constants::LOGS_DIR)
        .join("tasks");
    let mut marked = 0usize;
    let Ok(dirs) = std::fs::read_dir(&root) else {
        return 0;
    };
    for dir in dirs.flatten() {
        let meta_path = dir.path().join("meta.json");
        let Ok(text) = std::fs::read_to_string(&meta_path) else {
            continue;
        };
        match serde_json::from_str::<TaskMeta>(&text) {
            Ok(mut meta) if meta.status == TaskStatus::Running => {
                // Boot-sweep race guard (review fix h): a task spawned by
                // THIS process after boot has a live registry entry — its
                // meta is authoritative in memory, never touch it.
                if registry().snapshot(&meta.task_id).is_some() {
                    continue;
                }
                // Sweep-cost bound: retention deletes these dirs at 7d
                // anyway; older-than-7d running metas are stale relics.
                if now_ms().saturating_sub(meta.started_at_ms) > RETENTION_WINDOW_MS {
                    continue;
                }
                // Two-tier decision (D1 amended): dead group ⇒ mark-only;
                // verified-ours live group ⇒ SIGKILL; unverifiable live
                // group ⇒ mark-only (pid-reuse fail-safe). W5.1: the
                // agent-future path has NO process group at all — its
                // running metas are marked `Expired` (spec D20 §8 restart
                // expiry; the parent re-delegates) with no probing and no
                // signalling; `Orphaned` stays the process-path value.
                let is_async = meta.agent.is_some();
                #[cfg(unix)]
                let (group_alive, verified_ours) = if is_async {
                    (false, false)
                } else {
                    // SAFETY: kill(pid, 0) is a pure permission/existence
                    // probe — no signal is delivered. Negative pid targets
                    // the process group led by the recorded child pid.
                    let alive = unsafe { libc::kill(-(meta.pid as libc::pid_t), 0) == 0 };
                    (
                        alive,
                        alive && verify_leader_identity(meta.pid, meta.started_at_ms),
                    )
                };
                #[cfg(not(unix))]
                let (group_alive, verified_ours) = (false, false);

                meta.status = if is_async {
                    TaskStatus::Expired
                } else {
                    TaskStatus::Orphaned
                };
                meta.ended_at_ms = Some(now_ms());
                match serde_json::to_vec(&meta) {
                    Ok(bytes) => {
                        if let Err(e) = write_atomic(&meta_path, &bytes) {
                            tracing::warn!(
                                path = %meta_path.display(),
                                error = %e,
                                "boot sweep: failed to rewrite expired/orphaned meta (kept running)"
                            );
                            continue;
                        }
                        if is_async {
                            tracing::warn!(
                                target: "zen_background",
                                task_id = %meta.task_id,
                                agent = meta.agent.as_deref().unwrap_or("unknown"),
                                "boot sweep: running agent-future meta marked expired (daemon restart — parent must re-delegate)"
                            );
                        } else if verified_ours {
                            // SAFETY: the group is probed live AND its
                            // leader's kernel-recorded start time matches our
                            // spawn record within tolerance — identity is
                            // verified ours; a pgid cannot be recycled while
                            // any member exists, so this cannot hit an
                            // unrelated group.
                            unsafe {
                                libc::kill(-(meta.pid as libc::pid_t), libc::SIGKILL);
                            }
                            tracing::warn!(
                                target: "zen_background",
                                task_id = %meta.task_id,
                                pid = meta.pid,
                                "boot sweep: verified orphan group SIGKILLed (leader start-time matched)"
                            );
                        } else if group_alive {
                            tracing::warn!(
                                target: "zen_background",
                                task_id = %meta.task_id,
                                pid = meta.pid,
                                "boot sweep: live group could not be verified as ours — marked orphaned, no signal sent (pid-reuse fail-safe)"
                            );
                        } else {
                            tracing::warn!(
                                target: "zen_background",
                                task_id = %meta.task_id,
                                pid = meta.pid,
                                "boot sweep: crash-orphaned background task marked orphaned (group gone)"
                            );
                        }
                        marked += 1;
                    }
                    Err(e) => tracing::warn!(
                        path = %meta_path.display(),
                        error = %e,
                        "boot sweep: orphaned meta re-serialization failed (skipped)"
                    ),
                }
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(
                path = %meta_path.display(),
                error = %e,
                "boot sweep: corrupt meta.json skipped (structured fallback)"
            ),
        }
    }
    marked
}

/// Absolute path of a task's stdout capture file.
#[must_use]
pub fn meta_stdout_path(output_dir: &Path) -> PathBuf {
    output_dir.join("stdout")
}

/// Absolute path of a task's stderr capture file.
#[must_use]
pub fn meta_stderr_path(output_dir: &Path) -> PathBuf {
    output_dir.join("stderr")
}

/// Absolute path of a task's meta.json.
#[must_use]
pub fn meta_path(task_id: &str) -> PathBuf {
    user_root()
        .join(zen_core::constants::LOGS_DIR)
        .join("tasks")
        .join(task_id)
        .join("meta.json")
}

/// Read and parse a task's meta.json, or `None` when absent/corrupt
/// (structured fallback for the collect/kill tools — never a panic on
/// One task's collect snapshot for `delegate.collect` (W5.3) — the
/// registry-side half of the unified D25 A5 payload shape. `status`
/// mirrors [`TaskStatus::as_str`] labels; `response` carries the Ok
/// payload only for terminal-success collects; `error` carries the
/// failure reason otherwise.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DelegateCollect {
    /// Registry task id.
    pub task_id: String,
    /// Status label (`running`/`completed`/`failed`/`killed`/`timeout`/`expired`).
    pub status: String,
    /// Ok payload (terminal success only; `None` otherwise).
    pub response: Option<serde_json::Value>,
    /// Failure reason (terminal non-success or `None`).
    pub error: Option<String>,
    /// Owning session id (`None` on a still-running collect timeout).
    pub session_id: Option<String>,
    /// Owning agent name (agent-future path).
    pub agent: Option<String>,
    /// Fan-out slot index (batch admissions).
    pub task_index: Option<usize>,
    /// True when read from `meta.json` after a registry miss
    /// (daemon-restart case) — surfaced for observability.
    pub from_meta_fallback: bool,
}

/// Split a terminal task outcome into the unified `(response, error)`
/// collect shape: Completed + non-null payload ⇒ response; Completed
/// without payload ⇒ both empty; Failed ⇒ error message; any other
/// terminal label (killed/timeout/expired/orphaned) ⇒ explanatory error.
fn outcome_parts(
    status: &TaskStatus,
    result: Option<serde_json::Value>,
) -> (Option<serde_json::Value>, Option<String>) {
    match status {
        TaskStatus::Completed(_) => {
            if result.as_ref().is_some_and(|v| !v.is_null()) {
                (result, None)
            } else {
                (None, None)
            }
        }
        TaskStatus::Failed(msg) => (None, Some(msg.clone())),
        other => (None, Some(format!("task ended as {}", other.as_str()))),
    }
}

/// external data).
#[must_use]
pub fn read_meta(task_id: &str) -> Option<TaskMeta> {
    let path = meta_path(task_id);
    let text = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<TaskMeta>(&text) {
        Ok(meta) => Some(meta),
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "task meta.json corrupt (collect/kill fallback unavailable)"
            );
            None
        }
    }
}

/// Rewrite a task's meta.json with `status` — the `shell.kill` fallback for a
/// running-but-lost task (registry entry gone after a daemon restart).
/// Check-then-set: an already-terminal meta is returned unchanged, never
/// overwritten. Returns the updated meta, or `None` when the meta is
/// absent/corrupt/unwritable (structured fallback — never a panic).
pub fn mark_meta_status(task_id: &str, status: TaskStatus) -> Option<TaskMeta> {
    let mut meta = read_meta(task_id)?;
    if meta.status.is_terminal() {
        return Some(meta);
    }
    meta.status = status;
    meta.ended_at_ms = Some(now_ms());
    let bytes = serde_json::to_vec_pretty(&meta).ok()?;
    let path = meta_path(task_id);
    write_atomic(&path, &bytes).ok()?;
    Some(meta)
}

/// W12.7 (review Q7): normalize a stale `running` meta at collect-fallback
/// READ time. A meta stuck `running` with `started_at_ms` older than the
/// 7d retention window ([`RETENTION_WINDOW_MS`] — the same age rule the
/// boot sweep uses for stale relics) can never terminalize: its registry
/// entry and wait task died with the daemon, so `collect` would report
/// `running` forever.
///
/// GIVEN such a meta, WHEN this runs, THEN the meta is normalized to
/// [`TaskStatus::Expired`] (in place, for the caller's response) and the
/// rewrite is persisted via [`write_atomic`]. The response normalization
/// happens regardless; a persist failure warns (never a silent `let _ =`)
/// and leaves the disk meta for the next retention pass. Fresh (<7d)
/// running metas and already-terminal metas are returned untouched.
///
/// Returns `true` when the meta was normalized.
fn normalize_stale_running_meta(meta: &mut TaskMeta) -> bool {
    if meta.status != TaskStatus::Running {
        return false;
    }
    if now_ms().saturating_sub(meta.started_at_ms) <= RETENTION_WINDOW_MS {
        return false;
    }
    meta.status = TaskStatus::Expired;
    meta.ended_at_ms = Some(now_ms());
    let path = meta_path(&meta.task_id);
    match serde_json::to_vec_pretty(&meta) {
        Ok(bytes) => {
            if let Err(e) = write_atomic(&path, &bytes) {
                tracing::warn!(
                    path = %path.display(),
                    task_id = %meta.task_id,
                    error = %e,
                    "collect fallback: stale running meta normalized to expired, but the terminal-state meta rewrite failed (disk meta left for retention)"
                );
            }
        }
        Err(e) => tracing::warn!(
            task_id = %meta.task_id,
            error = %e,
            "collect fallback: stale running meta re-serialization failed (response still expired, disk meta untouched)"
        ),
    }
    true
}

/// Persist a task's meta view atomically (tmp + fsync + rename). Failures
/// warn and never abort the caller — meta is a durability aid, not a gate.
fn write_meta(output_dir: &Path, snapshot: &TaskSnapshot, stdout_path: &Path, stderr_path: &Path) {
    let meta = TaskMeta::from_snapshot(snapshot, stdout_path, stderr_path);
    let bytes = match serde_json::to_vec_pretty(&meta) {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::warn!(task_id = %snapshot.task_id, error = %e, "meta serialization failed");
            return;
        }
    };
    let path = output_dir.join("meta.json");
    if let Err(e) = write_atomic(&path, &bytes) {
        tracing::warn!(path = %path.display(), error = %e, "meta.json write failed");
    }
}

/// Persist the agent-future meta view (W5.1): the unified schema with
/// `agent`/`task_index`/`result` set — atomic (tmp + fsync + rename),
/// failures warn and never abort. `agent` is the boot sweep's
/// async-vs-process discriminator, so EVERY rewrite on this path must
/// preserve it (never route an async entry through [`write_meta`]).
fn write_async_meta(
    output_dir: &Path,
    snapshot: &TaskSnapshot,
    agent: &str,
    task_index: Option<usize>,
    result: Option<serde_json::Value>,
    stdout_path: &Path,
    stderr_path: &Path,
) {
    let mut meta = TaskMeta::from_snapshot(snapshot, stdout_path, stderr_path);
    meta.agent = Some(agent.to_string());
    meta.task_index = task_index;
    meta.result = result;
    let bytes = match serde_json::to_vec_pretty(&meta) {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::warn!(task_id = %snapshot.task_id, error = %e, "async meta serialization failed");
            return;
        }
    };
    let path = output_dir.join("meta.json");
    if let Err(e) = write_atomic(&path, &bytes) {
        tracing::warn!(path = %path.display(), error = %e, "async meta.json write failed");
    }
}

/// Drain one pipe into its capture file with the D7 contract: stop WRITING
/// at `cap` bytes, keep READING and discarding past it (the child must never
/// SIGPIPE), count every byte into the snapshot, flag `truncated`.
async fn drain_pipe<R>(
    mut pipe: R,
    path: PathBuf,
    cap: u64,
    snapshot: Arc<Mutex<TaskSnapshot>>,
    is_stdout: bool,
) where
    R: tokio::io::AsyncRead + Unpin + Send,
{
    let mut file = tokio::fs::File::create(&path).await.ok();
    // Bytes actually captured to disk. On a write failure the file is
    // abandoned (`file = None`) and `captured` STOPS advancing — meta must
    // never claim bytes the disk does not hold (review fix p).
    let mut captured: u64 = 0;
    let mut buf = vec![0u8; DRAIN_CHUNK_BYTES];
    loop {
        let n = match pipe.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        {
            let mut guard = lock_ok(&snapshot, "task snapshot");
            let total_seen = if is_stdout {
                guard.bytes_stdout += n as u64;
                guard.bytes_stdout
            } else {
                guard.bytes_stderr += n as u64;
                guard.bytes_stderr
            };
            // truncated iff bytes were actually DISCARDED (total seen past
            // the capture cap) — an exactly-cap output is NOT truncated.
            if total_seen > cap {
                guard.truncated = true;
            }
        }
        if let Some(f) = file.as_mut()
            && captured < cap
        {
            let to_write = usize::try_from(cap - captured).unwrap_or(usize::MAX).min(n);
            if f.write_all(&buf[..to_write]).await.is_ok() {
                captured += to_write as u64;
            } else {
                // Disk full / dir removed: keep draining (child must never
                // SIGPIPE), stop writing, stop counting.
                file = None;
            }
        }
    }
}

/// Signal a child's whole process group.
fn kill_group(pid: u32, sig: i32) {
    #[cfg(unix)]
    {
        // SAFETY: negative pid targets the group led by our own spawned
        // child (spawned with process_group(0), so pgid == child pid);
        // permission is guaranteed because the child is our descendant.
        // This mirrors the shell_exec.rs timeout path (FR-038): a
        // pid-reuse race is bounded because we only signal while our
        // registry entry tracks the child and the wait task reaps it
        // promptly after death.
        unsafe {
            libc::kill(-(pid as i32), sig);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, sig);
    }
}

/// Probe whether the process GROUP led by `pid` still has any member
/// (signal 0 — nothing is delivered). Used by the kill escalation and the
/// blocking drain to decide whether SIGKILL is still needed: a leader that
/// exits promptly on SIGTERM can leave SIGTERM-immune DESCENDANTS alive,
/// so the child's `reaped` flag alone is not a group-liveness signal.
#[cfg(unix)]
fn group_alive(pid: u32) -> bool {
    // SAFETY: signal 0 performs a permission/liveness check only — no
    // signal is delivered. The negative pid targets the group led by our
    // own spawned child (pgid == child pid). A pgid cannot be recycled
    // while any group member exists, so a successful probe means the
    // probed group is still populated; ESRCH is the only "gone" answer we
    // trust (any other error conservatively reports alive — a redundant
    // SIGKILL attempt on a foreign group fails harmlessly with EPERM).
    let ret = unsafe { libc::kill(-(pid as i32), 0) };
    if ret == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(not(unix))]
fn group_alive(pid: u32) -> bool {
    let _ = pid;
    false
}

/// Detached 3s escalation thread for [`TaskRegistry::kill`] /
/// [`TaskRegistry::kill_all`]: after the grace window, probe GROUP
/// liveness — if any group member still exists, SIGKILL the whole group
/// (review fix c: a leader that exits promptly on SIGTERM leaves
/// SIGTERM-immune descendants alive; the child's `reaped` flag says
/// nothing about them). The probe (not `reaped`) is the decision, so the
/// escalation no longer skips SIGKILL for a reaped leader with live
/// descendants.
fn spawn_kill_escalation(pid: u32) {
    std::thread::Builder::new()
        .name("zen-bg-kill-escalation".to_string())
        .spawn(move || {
            std::thread::sleep(Duration::from_secs(KILL_ESCALATION_SECS));
            if group_alive(pid) {
                kill_group(pid, libc::SIGKILL);
            }
        })
        .map(|_| ())
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "kill escalation thread spawn failed (SIGTERM only)");
        });
}

/// Current wall-clock in ms since the UNIX epoch.
#[must_use]
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Truncate to the FIRST `max_chars` characters (char-boundary-safe by
/// construction — `chars()` iteration never splits a code point).
#[must_use]
pub fn truncate_char_safe(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

/// Take the LAST `max_chars` characters (char-boundary-safe by construction).
/// Used by `shell.collect` for the ≤8k stdout/stderr tails (D5).
#[must_use]
pub fn tail_chars(s: &str, max_chars: usize) -> String {
    let total = s.chars().count();
    if total <= max_chars {
        return s.to_string();
    }
    s.chars().skip(total - max_chars).collect()
}

/// Build the ≤80-char command digest (binary + args joined,
/// char-boundary-safe). Identification only — never replayed.
#[must_use]
pub fn make_command_digest(binary: &str, args: &[String]) -> String {
    let mut digest = String::from(binary);
    for arg in args {
        digest.push(' ');
        digest.push_str(arg);
    }
    truncate_char_safe(&digest, 80)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_chars_is_char_boundary_safe() {
        // CJK multibyte: naive byte slicing at 8000 would panic or split a
        // code point; chars() never does (design Section 7 item 9).
        let s = "漢".repeat(9000);
        let tail = tail_chars(&s, COLLECT_TAIL_CHARS);
        assert_eq!(tail.chars().count(), COLLECT_TAIL_CHARS);
        assert!(tail.ends_with("漢"));
        assert_eq!(tail_chars("short", 8000), "short");
    }

    #[test]
    fn truncate_char_safe_is_char_boundary_safe() {
        let s = "漢".repeat(100);
        assert_eq!(truncate_char_safe(&s, 80).chars().count(), 80);
    }

    #[test]
    fn command_digest_truncates_at_80_chars() {
        let args: Vec<String> = vec!["x".repeat(200)];
        let digest = make_command_digest("/bin/echo", &args);
        assert_eq!(digest.chars().count(), 80);
        assert!(digest.starts_with("/bin/echo"));
    }

    // F15 (W15): a poisoned entries lock must not wedge the registry — the
    // kill/expire/kill_all paths recover the inner map (lock_ok, T264).
    #[test]
    fn poisoned_entries_lock_is_recovered_by_kill_expire_and_kill_all() {
        let reg = TaskRegistry::new();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = reg.entries.lock().unwrap();
            panic!("poison the registry lock");
        }));
        assert!(
            reg.kill("missing").is_err(),
            "kill recovers and reports the unknown id"
        );
        assert!(reg.expire_due(i64::MAX).is_empty(), "expire_due recovers");
        assert!(reg.kill_all("test").is_empty(), "kill_all recovers");
    }

    #[test]
    fn status_labels_and_terminality() {
        assert!(!TaskStatus::Running.is_terminal());
        assert!(TaskStatus::Completed(0).is_terminal());
        assert!(TaskStatus::Failed("x".into()).is_terminal());
        assert!(TaskStatus::Timeout.is_terminal());
        assert!(TaskStatus::Killed.is_terminal());
        assert!(TaskStatus::Orphaned.is_terminal());
        assert_eq!(TaskStatus::Running.as_str(), "running");
        assert_eq!(TaskStatus::Timeout.as_str(), "timeout");
    }

    #[test]
    fn meta_status_round_trips_through_json() {
        let meta = TaskMeta {
            task_id: "t".into(),
            session_id: "s".into(),
            pid: 1,
            command_digest: "d".into(),
            output_dir: "/o".into(),
            status: TaskStatus::Completed(3),
            started_at_ms: 1,
            ended_at_ms: Some(2),
            bytes_stdout: 0,
            bytes_stderr: 0,
            truncated: false,
            stdout_path: "/o/stdout".into(),
            stderr_path: "/o/stderr".into(),
            agent: None,
            task_index: None,
            result: None,
        };
        let json = serde_json::to_string(&meta).unwrap();
        let back: TaskMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(back.status, TaskStatus::Completed(3));
    }

    #[test]
    fn session_scope_defaults_to_local() {
        // Outside any SESSION_SCOPE scope ⇒ "local" (frozen API default).
        assert_eq!(current_session_scope(), "local");
    }

    #[test]
    fn is_valid_task_id_accepts_only_uuid_v4_shapes() {
        assert!(is_valid_task_id("0123456789abcdef0123456789abcdef"));
        assert!(is_valid_task_id("01234567-89ab-cdef-0123-456789abcdef"));
        // Traversal fragments, absolute paths, wrong shapes: all rejected
        // BEFORE the id reaches meta_path() (review fix a, P1).
        for bad in [
            "",
            "../../x",
            "/etc/x",
            "../../..//etc",
            "0123456789ABCDEF0123456789ABCDEF",
            "short",
            "g123456789abcdef0123456789abcdef",
            "0123456789abcdef0123456789abcde",
            "01234567-89ab-cdef-0123-456789abcde",
            "a-b-c-d-e",
            "0123456789abcdef-0123456789abcdef",
        ] {
            assert!(!is_valid_task_id(bad), "must reject {bad:?}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn group_alive_probes_group_liveness() {
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("5")
            .process_group(0)
            .spawn()
            .expect("sleep spawns");
        let pid = child.id();
        assert!(group_alive(pid), "live child's group must probe alive");
        child.kill().expect("SIGKILL");
        let _ = child.wait().expect("reap");
        assert!(
            !group_alive(pid),
            "reaped child's group must probe gone (ESRCH)"
        );
    }

    /// W12.6 tight-window pin (review Q6): the drain decision landing
    /// BETWEEN a spawn's outer `is_draining` check and its entries-lock
    /// acquisition must be caught by the IN-lock re-check — the spawn is
    /// refused and NO entry is inserted behind the drain's back.
    ///
    /// The interleaving is driven explicitly through the lock, not by
    /// timing: the test thread HOLDS the entries lock while the spawn
    /// thread runs (its outer check passes — the flag is still clear — and
    /// it parks at the lock acquisition), sets the drain flag while the
    /// spawn is parked, then releases the lock so the spawn enters the
    /// critical section with the flag already set. Deterministic under
    /// nextest (process-per-test). No shuttle DST sweep: zen-plugin has no
    /// `shuttle` feature (the shuttle-tokio feature chain lives in
    /// zen-agents/zen-gateway/zen-cli only), so a deterministic pin is the
    /// available and sufficient proof here.
    #[test]
    fn spawn_parked_on_the_entries_lock_is_refused_when_drain_lands_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        // SAFETY: single-threaded test process under nextest
        // (process-per-test); std::env::set_var is process-global.
        unsafe {
            std::env::set_var("ZEN_HOME", dir.path());
        }
        zen_core::config::invalidate_config_cache();
        clear_draining();

        let reg = registry();
        {
            // Hold the entries lock: any concurrent spawn passes its outer
            // check (flag clear) and parks at the critical-section entry.
            let _map_guard = lock_ok(&reg.entries, "test entries");

            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let handle = std::thread::spawn(move || {
                let mut cmd = tokio::process::Command::new("/bin/sleep");
                cmd.arg("5");
                cmd.process_group(0);
                let limits = BackgroundLimits {
                    max_wall_secs: 60,
                    max_concurrent: 4,
                    output_cap_bytes: OUTPUT_CAP_BYTES,
                };
                // spawn_background needs a runtime context for its spawned
                // drain/wait tasks; a current-thread runtime suffices. The
                // child is never forked on the expected (refusal) path —
                // the in-lock bail fires BEFORE cmd.spawn().
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("test runtime");
                let res =
                    rt.block_on(async { reg.spawn_background(cmd, "local", "sleep 5", &limits) });
                done_tx.send(res).expect("receiver alive");
            });

            // Bounded window for the spawn thread to pass its outer check
            // and park on the lock (the flag is still clear at that point).
            // Either interleaving is safe when the fix is present — both
            // checks refuse — so the pin is stable; the held lock makes the
            // discriminating interleaving (outer passed, flag set, then
            // lock entered) the overwhelmingly likely one.
            std::thread::sleep(Duration::from_millis(200));

            // The drain decision: flag set FIRST (daemon drain-start order),
            // while we still hold the lock the spawn is parked on.
            set_draining();
            drop(_map_guard);

            let res = done_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("spawn thread must finish");
            let err = res.expect_err("spawn parked across the drain decision must be refused");
            assert!(
                err.to_string().contains("shutting down"),
                "structured drain rejection, got: {err}"
            );
            handle.join().expect("spawn thread joins");
        }

        // Nothing was inserted behind the drain's back.
        assert_eq!(
            reg.live_count(),
            0,
            "a drain-window refusal must not leave a live entry behind"
        );
        clear_draining();
    }
}
