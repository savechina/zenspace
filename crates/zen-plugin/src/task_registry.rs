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
//!   (Lane O); the daemon drains via [`TaskRegistry::kill_all`] and sweeps
//!   crash orphans via [`sweep_orphans_on_boot`] (Lane G). Boot sweep is
//!   MARK-ONLY — it never signals (pid-reuse risk, D1).

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
    /// # Errors
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
        if self.live_count() >= limits.max_concurrent {
            bail!(
                "background task rejected: {} live tasks already at the \
                 [agentic.background] max_concurrent limit of {}; collect or \
                 kill existing tasks first (shell.collect / shell.kill)",
                self.live_count(),
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

        // Initial meta BEFORE returning: the boot sweep and the collect
        // fallback must see the task even if the daemon dies right after
        // spawn (design Section 5 spawn contract).
        write_meta(
            &output_dir,
            &snapshot.lock().unwrap().clone(),
            &stdout_path,
            &stderr_path,
        );

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
        self.entries.lock().unwrap().insert(task_id.clone(), entry);

        // Drain tasks own their pipe handles (taken above, never the Child):
        // they write to disk up to the cap, then keep reading + discarding
        // while counting bytes (D7 — the child must never SIGPIPE).
        if let Some(pipe) = stdout_pipe {
            tokio::spawn(drain_pipe(
                pipe,
                stdout_path.clone(),
                limits.output_cap_bytes,
                Arc::clone(&snapshot),
                true,
            ));
        }
        if let Some(pipe) = stderr_pipe {
            tokio::spawn(drain_pipe(
                pipe,
                stderr_path.clone(),
                limits.output_cap_bytes,
                Arc::clone(&snapshot),
                false,
            ));
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
        tokio::spawn(async move {
            let status = {
                let mut guarded = wait_child.lock().await;
                guarded.wait().await
            };
            wait_reaped.store(true, Ordering::SeqCst);
            let terminal_snapshot = {
                let mut guard = wait_snapshot.lock().unwrap();
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
        });

        let initial = snapshot.lock().unwrap().clone();
        tracing::info!(
            target: "zen_background",
            task_id = %task_id,
            pid,
            session = %session_id,
            digest = %command_digest,
            "background task spawned"
        );
        Ok(initial)
    }

    /// Cheap clone of one task's snapshot, or `None` for an unknown id.
    #[must_use]
    pub fn snapshot(&self, task_id: &str) -> Option<TaskSnapshot> {
        let map = self.entries.lock().unwrap();
        map.get(task_id).map(|e| e.snapshot.lock().unwrap().clone())
    }

    /// Per-session push cursor (D4): every terminal transition of this
    /// session's tasks since the last take, marking each delivered task as
    /// notified. Cross-session tasks are NEVER returned (Confidential leak
    /// guard — security pin, design Section 7 item 8).
    #[must_use]
    pub fn take_status_changes(&self, session_id: &str) -> Vec<TaskSnapshot> {
        let mut out = Vec::new();
        let mut map = self.entries.lock().unwrap();
        for entry in map.values_mut() {
            if entry.session_id != session_id || entry.notified.load(Ordering::SeqCst) {
                continue;
            }
            let guard = entry.snapshot.lock().unwrap();
            if guard.status.is_terminal() {
                out.push(guard.clone());
                entry.notified.store(true, Ordering::SeqCst);
            }
        }
        out
    }

    /// Number of live (Running) tasks across all sessions.
    #[must_use]
    pub fn live_count(&self) -> usize {
        self.entries
            .lock()
            .unwrap()
            .values()
            .filter(|e| e.snapshot.lock().unwrap().status == TaskStatus::Running)
            .count()
    }

    /// Live (Running) snapshots owned by `session_id`.
    #[must_use]
    pub fn live_for_session(&self, session_id: &str) -> Vec<TaskSnapshot> {
        self.entries
            .lock()
            .unwrap()
            .values()
            .filter(|e| e.session_id == session_id)
            .filter(|e| e.snapshot.lock().unwrap().status == TaskStatus::Running)
            .map(|e| e.snapshot.lock().unwrap().clone())
            .collect()
    }

    /// Kill a task: SIGTERM the process group, escalate to SIGKILL after
    /// [`KILL_ESCALATION_SECS`] if the child has not been reaped, mark the
    /// status `Killed` and rewrite meta.json.
    ///
    /// Non-blocking: the 3s escalation runs on a detached thread, so the
    /// caller (async tool invoke / SIGTERM drain) returns immediately with
    /// the `Killed` snapshot — matching the `shell.kill` tool contract.
    /// Idempotent on already-terminal tasks (returns the current snapshot
    /// without signalling).
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

        let (pid, snapshot) = {
            let mut guard = entry.snapshot.lock().unwrap();
            if guard.status.is_terminal() {
                return Ok(guard.clone());
            }
            guard.status = TaskStatus::Killed;
            guard.ended_at_ms = Some(now_ms());
            (guard.pid, guard.clone())
        };

        kill_group(pid, libc::SIGTERM);
        spawn_kill_escalation(pid, Arc::clone(&entry.reaped));
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
                let guard = e.snapshot.lock().unwrap();
                guard.status == TaskStatus::Running
                    && guard.started_at_ms + (e.limits.max_wall_secs as i64) * 1000 <= now_ms
            })
            .cloned()
            .collect();
        let mut ids = Vec::with_capacity(expired.len());
        for entry in expired {
            let (pid, snapshot) = {
                let mut guard = entry.snapshot.lock().unwrap();
                if guard.status != TaskStatus::Running {
                    continue;
                }
                guard.status = TaskStatus::Timeout;
                guard.ended_at_ms = Some(now_ms);
                (guard.pid, guard.clone())
            };
            // Deadline is a hard kill (D7): no SIGTERM grace window.
            kill_group(pid, libc::SIGKILL);
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
    #[must_use]
    pub fn kill_all(&self, reason: &str) -> Vec<String> {
        let live: Vec<TaskEntry> = self
            .entries
            .lock()
            .unwrap()
            .values()
            .filter(|e| e.snapshot.lock().unwrap().status == TaskStatus::Running)
            .cloned()
            .collect();
        let mut ids = Vec::with_capacity(live.len());
        for entry in live {
            let (pid, snapshot) = {
                let mut guard = entry.snapshot.lock().unwrap();
                if guard.status != TaskStatus::Running {
                    continue;
                }
                guard.status = TaskStatus::Killed;
                guard.ended_at_ms = Some(now_ms());
                (guard.pid, guard.clone())
            };
            kill_group(pid, libc::SIGTERM);
            spawn_kill_escalation(pid, Arc::clone(&entry.reaped));
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
}

/// The process-global registry (D8: OnceLock, de facto daemon-scoped).
#[must_use]
pub fn registry() -> &'static TaskRegistry {
    static REGISTRY: OnceLock<TaskRegistry> = OnceLock::new();
    REGISTRY.get_or_init(TaskRegistry::new)
}

/// Boot-time crash-orphan sweep (D1, MARK-ONLY).
///
/// GIVEN `logs/tasks/*/meta.json` files left in status `running` by a dead
/// daemon,
/// WHEN this runs at daemon boot (Lane G wiring),
/// THEN each such meta is rewritten to status `orphaned` (via
/// [`write_atomic`]) with a `tracing::warn` — and NOTHING is signalled:
/// the processes are gone with the crash, and killing by recorded pid would
/// risk pid reuse (D1).
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
                        tracing::warn!(
                            target: "zen_background",
                            task_id = %meta.task_id,
                            pid = meta.pid,
                            "boot sweep: crash-orphaned background task marked orphaned (mark-only, no signal)"
                        );
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
    let mut written: u64 = 0;
    let mut buf = vec![0u8; DRAIN_CHUNK_BYTES];
    loop {
        let n = match pipe.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        {
            let mut guard = snapshot.lock().unwrap();
            if is_stdout {
                guard.bytes_stdout += n as u64;
            } else {
                guard.bytes_stderr += n as u64;
            }
            if written >= cap {
                guard.truncated = true;
            }
        }
        if written < cap {
            let to_write = usize::try_from(cap - written).unwrap_or(usize::MAX).min(n);
            if let Some(f) = file.as_mut()
                && f.write_all(&buf[..to_write]).await.is_err()
            {
                // Capture file unwritable (disk full / dir removed):
                // keep draining, stop retrying writes.
                file = None;
            }
            written += to_write as u64;
            if written >= cap {
                let mut guard = snapshot.lock().unwrap();
                guard.truncated = true;
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

/// Detached 3s escalation thread for [`TaskRegistry::kill`] /
/// [`TaskRegistry::kill_all`]: if the child has not been reaped by then,
/// SIGKILL the whole group (descendants survive a graceful leader exit and
/// must not be left behind).
fn spawn_kill_escalation(pid: u32, reaped: Arc<AtomicBool>) {
    std::thread::Builder::new()
        .name("zen-bg-kill-escalation".to_string())
        .spawn(move || {
            std::thread::sleep(Duration::from_secs(KILL_ESCALATION_SECS));
            if !reaped.load(Ordering::SeqCst) {
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
}
