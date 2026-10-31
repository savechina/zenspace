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

/// Lifecycle status of one background task.
///
/// Terminal variants are `Completed`/`Failed`/`Timeout`/`Killed`/`Orphaned`;
/// `Running` is the only non-terminal state. Serialized externally-tagged
/// snake_case into `meta.json` (`{"completed": 0}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Child still running.
    Running,
    /// Child exited with the given exit code.
    Completed(i32),
    /// Child died without an exit code (signal) or the wait failed.
    Failed(String),
    /// Wall-clock deadline expired; the group was SIGKILLed (D7).
    Timeout,
    /// Killed via `shell.kill` / `kill_all` (SIGTERM → 3s → SIGKILL group).
    Killed,
    /// Boot sweep found a `running` meta.json with no live registry entry
    /// (daemon crash) — mark-only, never signalled (D1).
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
    /// `orphaned`).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Running => "running",
            TaskStatus::Completed(_) => "completed",
            TaskStatus::Failed(_) => "failed",
            TaskStatus::Timeout => "timeout",
            TaskStatus::Killed => "killed",
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
        }
    }
}

/// One registry entry.
///
/// `child` is the D8 strong hold: the entry (and the wait task's clone) keep
/// the `tokio::process::Child` alive, which keeps the pipe handles open past
/// the spawning scope. Dropping the Child closes the pipes and SIGPIPEs the
/// child — entries are therefore never removed in v1.
#[derive(Clone)]
struct TaskEntry {
    session_id: String,
    output_dir: PathBuf,
    limits: BackgroundLimits,
    snapshot: Arc<Mutex<TaskSnapshot>>,
    // D8 strong hold: never READ, but keeping the Child alive keeps the pipe
    // handles open past the spawning scope — dropping it would close the
    // pipes and SIGPIPE the child. Entries are therefore never removed in v1.
    #[allow(dead_code)]
    child: Arc<tokio::sync::Mutex<tokio::process::Child>>,
    /// Set by the wait task once `child.wait()` returned — the kill
    /// escalation thread checks it before SIGKILLing the (possibly dead)
    /// group.
    reaped: Arc<AtomicBool>,
    /// Per-session push cursor (D4): set when `take_status_changes` has
    /// delivered this task's terminal transition to its owner.
    notified: Arc<AtomicBool>,
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
            let live = map
                .values()
                .filter(|e| lock_ok(&e.snapshot, "task snapshot").status == TaskStatus::Running)
                .count();
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
                child: Arc::clone(&child),
                reaped: Arc::clone(&reaped),
                notified: Arc::new(AtomicBool::new(false)),
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
        let entry = self
            .entries
            .lock()
            .unwrap()
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

    /// GC-tick deadline hook (D7): SIGKILL the group of every Running task
    /// whose `started_at_ms + max_wall_secs*1000 <= now_ms`, mark it
    /// `Timeout` and rewrite meta.json. Returns the expired task ids.
    ///
    /// `now_ms` is injected so tests (and the scheduler) are deterministic.
    #[must_use]
    pub fn expire_due(&self, now_ms: i64) -> Vec<String> {
        let expired: Vec<TaskEntry> = self
            .entries
            .lock()
            .unwrap()
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
            let (pid, snapshot, signal) = {
                let mut guard = lock_ok(&entry.snapshot, "task snapshot");
                if guard.status != TaskStatus::Running {
                    continue;
                }
                // Pid-reuse guard (review fix b): an already-reaped child's
                // pid may be recycled — never signal it.
                let reaped = entry.reaped.load(Ordering::SeqCst);
                guard.status = TaskStatus::Timeout;
                guard.ended_at_ms = Some(now_ms);
                (guard.pid, guard.clone(), !reaped)
            };
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
        let live: Vec<TaskEntry> = self
            .entries
            .lock()
            .unwrap()
            .values()
            .filter(|e| lock_ok(&e.snapshot, "task snapshot").status == TaskStatus::Running)
            .cloned()
            .collect();
        let mut ids = Vec::with_capacity(live.len());
        for entry in live {
            let (pid, snapshot) = {
                let mut guard = lock_ok(&entry.snapshot, "task snapshot");
                if guard.status != TaskStatus::Running {
                    continue;
                }
                guard.status = TaskStatus::Killed;
                guard.ended_at_ms = Some(now_ms());
                (guard.pid, guard.clone())
            };
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
            let snapshot = {
                let mut guard = lock_ok(&entry.snapshot, "task snapshot");
                if guard.status != TaskStatus::Running {
                    continue;
                }
                guard.status = TaskStatus::Killed;
                guard.ended_at_ms = Some(now_ms());
                guard.clone()
            };
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
                if now_ms().saturating_sub(meta.started_at_ms) > 7 * 86_400_000 {
                    continue;
                }
                // Two-tier decision (D1 amended): dead group ⇒ mark-only;
                // verified-ours live group ⇒ SIGKILL; unverifiable live
                // group ⇒ mark-only (pid-reuse fail-safe).
                #[cfg(unix)]
                let (group_alive, verified_ours) = {
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

                meta.status = TaskStatus::Orphaned;
                meta.ended_at_ms = Some(now_ms());
                match serde_json::to_vec(&meta) {
                    Ok(bytes) => {
                        if let Err(e) = write_atomic(&meta_path, &bytes) {
                            tracing::warn!(
                                path = %meta_path.display(),
                                error = %e,
                                "boot sweep: failed to rewrite orphaned meta (kept running)"
                            );
                            continue;
                        }
                        if verified_ours {
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
}
