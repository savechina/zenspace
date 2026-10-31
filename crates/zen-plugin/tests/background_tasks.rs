//! Background-task pins (design record
//! `docs/designs/background-tool-execution-2026-10-30.md`, Section 7 items
//! 2,3,4,5,6,7,9,10 + registry half of 11).
//!
//! Runs under nextest (process-per-test): the process-global
//! [`zen_plugin::task_registry::registry`] and the ZEN_HOME env mutation are
//! safe per test (T118 discipline).

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use rig_compose::tool::Tool;
use serde_json::json;
use zen_plugin::task_registry::{
    self, BackgroundLimits, TaskStatus, clear_draining, registry, set_draining,
    sweep_orphans_on_boot,
};
use zen_plugin::tools::shell_collect::ShellCollectTool;
use zen_plugin::tools::shell_exec::ShellExecTool;
use zen_plugin::tools::shell_kill::ShellKillTool;

fn set_zen_home() -> PathBuf {
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded test process (nextest process-per-test);
    // std::env::set_var is process-global.
    unsafe {
        std::env::set_var("ZEN_HOME", dir.path());
    }
    zen_core::config::invalidate_config_cache();
    dir.path().to_path_buf()
}

fn exec_tool() -> ShellExecTool {
    use zen_core::sandbox::{OsSandboxProfile, SandboxMode, SandboxValidator};
    let validator = SandboxValidator::new(SandboxMode::DangerFullAccess, vec![]);
    let profile = OsSandboxProfile::from_mode(SandboxMode::DangerFullAccess, vec![], false);
    ShellExecTool::new(PathBuf::from("/"), validator, profile)
}

fn limits(max_concurrent: usize) -> BackgroundLimits {
    BackgroundLimits {
        max_wall_secs: 60,
        max_concurrent,
        output_cap_bytes: task_registry::OUTPUT_CAP_BYTES,
    }
}

/// Bounded reap-wait (house pattern, review fix j): poll until the registry
/// snapshot is terminal — the wait task stores `reaped` BEFORE transitioning
/// the status, so terminal ⇒ the child has been waitpid-reaped — then a
/// fixed 100ms settle for escalation/drain threads. Prevents nextest LEAK
/// flags from unreaped zombies at runtime drop.
async fn reap_wait(task_id: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if registry()
            .snapshot(task_id)
            .is_some_and(|s| s.status.is_terminal())
        {
            break;
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
}

/// Signal-0 liveness probe for real-process pins (no signal delivered).
fn pid_alive(pid: u32) -> bool {
    // SAFETY: signal 0 performs a liveness/permission check only.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Bounded poll until the leader pid is gone (reaped ⇒ ESRCH).
async fn wait_pid_gone(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while pid_alive(pid) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    !pid_alive(pid)
}

// ── Item 2: background spawn returns the task handle immediately ─────────

#[tokio::test]
#[cfg(unix)]
async fn background_spawn_returns_task_id_and_paths_without_waiting() {
    let home = set_zen_home();
    let res = exec_tool()
        .invoke(json!({
            "binary": "/bin/sleep",
            "args": ["5"],
            "background": true
        }))
        .await
        .unwrap();
    assert!(res["error"].as_str().is_none(), "{res}");
    let task_id = res["task_id"].as_str().unwrap().to_string();
    assert_eq!(res["status"], "running");
    assert_eq!(task_id.len(), 32, "uuid simple hex: {task_id}");
    let output_dir = res["output_dir"].as_str().unwrap();
    assert!(output_dir.starts_with(home.join("logs").join("tasks").to_str().unwrap()));
    // The capture FILES are created asynchronously by the drain tasks; the
    // output DIR must exist at spawn (meta.json already landed in it).
    assert!(PathBuf::from(output_dir).is_dir());
    assert!(res["stdout_path"].as_str().unwrap().ends_with("stdout"));
    assert!(res["stderr_path"].as_str().unwrap().ends_with("stderr"));
    // Cleanup: kill so the test process does not leak a live sleeper.
    registry().kill(&task_id).unwrap();
    reap_wait(&task_id, Duration::from_secs(3)).await;
}

// ── Item 7: meta.json lands on disk at spawn, BEFORE completion ──────────

#[tokio::test]
#[cfg(unix)]
async fn meta_json_written_at_spawn_before_completion() {
    set_zen_home();
    let res = exec_tool()
        .invoke(json!({
            "binary": "/bin/sleep",
            "args": ["5"],
            "background": true
        }))
        .await
        .unwrap();
    let task_id = res["task_id"].as_str().unwrap().to_string();
    let meta = task_registry::read_meta(&task_id).expect("meta.json must exist at spawn");
    assert_eq!(meta.status, TaskStatus::Running);
    assert_eq!(meta.task_id, task_id);
    assert!(meta.stdout_path.ends_with("stdout"));
    registry().kill(&task_id).unwrap();
    reap_wait(&task_id, Duration::from_secs(3)).await;
}

// ── Item 3: collect returns the completed status and stdout content ──────

#[tokio::test]
#[cfg(unix)]
async fn collect_returns_completed_status_and_stdout() {
    set_zen_home();
    let res = exec_tool()
        .invoke(json!({
            "binary": "/bin/echo",
            "args": ["hello-background"],
            "background": true
        }))
        .await
        .unwrap();
    let task_id = res["task_id"].as_str().unwrap().to_string();

    let collected = ShellCollectTool::new()
        .invoke(json!({ "task_id": task_id, "wait_ms": 5000 }))
        .await
        .unwrap();
    assert_eq!(collected["status"], "completed", "{collected}");
    assert_eq!(collected["exit_code"], 0);
    assert!(
        collected["stdout"]
            .as_str()
            .unwrap()
            .contains("hello-background")
    );
    assert_eq!(collected["bytes_stdout"], 17);
    assert_eq!(collected["truncated"], false);
    reap_wait(&task_id, Duration::from_secs(5)).await;
}

// ── Item 4: kill marks the task killed and collect reports it ────────────

#[tokio::test]
#[cfg(unix)]
async fn kill_marks_task_killed_and_collect_reports_it() {
    set_zen_home();
    let res = exec_tool()
        .invoke(json!({
            "binary": "/bin/sleep",
            "args": ["30"],
            "background": true
        }))
        .await
        .unwrap();
    let task_id = res["task_id"].as_str().unwrap().to_string();

    let killed = ShellKillTool::new()
        .invoke(json!({ "task_id": task_id }))
        .await
        .unwrap();
    assert_eq!(killed["status"], "killed", "{killed}");
    assert!(killed["pid"].as_u64().is_some());

    let collected = ShellCollectTool::new()
        .invoke(json!({ "task_id": task_id, "wait_ms": 500 }))
        .await
        .unwrap();
    assert_eq!(collected["status"], "killed", "{collected}");
    reap_wait(&task_id, Duration::from_secs(3)).await;
}

// ── Item 5: concurrency ceiling rejects with a structured error ──────────

#[tokio::test]
#[cfg(unix)]
async fn concurrency_ceiling_rejects_with_structured_error() {
    set_zen_home();
    let mut cmd = tokio::process::Command::new("/bin/sleep");
    cmd.args(["5"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null());
    #[cfg(unix)]
    cmd.process_group(0);
    cmd.kill_on_drop(false);
    let first = registry()
        .spawn_background(cmd, "local", "sleep 5", &limits(1))
        .unwrap();

    let mut second = tokio::process::Command::new("/bin/sleep");
    second
        .args(["5"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null());
    #[cfg(unix)]
    second.process_group(0);
    second.kill_on_drop(false);
    let err = registry()
        .spawn_background(second, "local", "sleep 5", &limits(1))
        .unwrap_err();
    assert!(
        err.to_string().contains("max_concurrent"),
        "rejection must name the limit: {err}"
    );
    registry().kill(&first.task_id).unwrap();
    reap_wait(&first.task_id, Duration::from_secs(3)).await;
}

// ── Item 6: background + stdin is rejected before spawn ──────────────────

#[tokio::test]
#[cfg(unix)]
async fn background_with_stdin_is_rejected() {
    set_zen_home();
    let res = exec_tool()
        .invoke(json!({
            "binary": "/bin/cat",
            "stdin": "data",
            "background": true
        }))
        .await;
    let err = res.unwrap_err().to_string();
    assert!(err.contains("stdin"), "{err}");
}

// ── Item 9: collect tails are char-boundary-safe on CJK output ───────────

#[tokio::test]
#[cfg(unix)]
async fn collect_cjk_output_tail_is_char_boundary_safe() {
    set_zen_home();
    // 9000 CJK chars: above the 4k inline threshold AND the 8k tail length —
    // a naive byte slice at 8000 would panic or split a code point.
    let payload = "漢".repeat(9000);
    let res = exec_tool()
        .invoke(json!({
            "binary": "/bin/echo",
            "args": [payload],
            "background": true
        }))
        .await
        .unwrap();
    let task_id = res["task_id"].as_str().unwrap().to_string();

    let collected = ShellCollectTool::new()
        .invoke(json!({ "task_id": task_id, "wait_ms": 5000 }))
        .await
        .unwrap();
    assert!(
        collected["stdout"].as_str().is_none(),
        "above the inline threshold the full stdout key must be absent: keys {collected}"
    );
    let tail = collected["stdout_tail"].as_str().unwrap();
    assert_eq!(tail.chars().count(), task_registry::COLLECT_TAIL_CHARS);
    // echo appends a trailing newline — the last payload char before it is
    // a complete 漢 code point (no byte-slice split).
    assert!(tail.trim_end().ends_with('漢'));
    assert_eq!(collected["truncated"], false, "8MiB cap not reached");
    reap_wait(&task_id, Duration::from_secs(5)).await;
}

// ── Item 10 + review fix (a): id-shape validation gates collect/kill ──────

#[tokio::test]
#[cfg(unix)]
async fn invalid_shape_task_id_is_arg_error_and_valid_unknown_id_is_structured_error() {
    set_zen_home();
    // Traversal/absolute shapes are rejected as argument errors BEFORE any
    // registry lookup or meta read (path-traversal defense).
    for bad in ["does-not-exist", "../../x", "/etc/x", "../../..//etc"] {
        let collected = ShellCollectTool::new()
            .invoke(json!({ "task_id": bad, "wait_ms": 0 }))
            .await;
        assert!(
            collected.is_err(),
            "collect must reject invalid shape: {bad}"
        );
        assert!(
            collected.unwrap_err().to_string().contains("task_id"),
            "rejection must name the argument"
        );
        let killed = ShellKillTool::new().invoke(json!({ "task_id": bad })).await;
        assert!(killed.is_err(), "kill must reject invalid shape: {bad}");
    }

    // A well-shaped but unknown id passes validation and gets the
    // structured unknown-id error (never a panic).
    let valid_unknown = "0123456789abcdef0123456789abcdef";
    let collected = ShellCollectTool::new()
        .invoke(json!({ "task_id": valid_unknown, "wait_ms": 0 }))
        .await
        .unwrap();
    assert!(
        collected["error"]
            .as_str()
            .unwrap()
            .contains("unknown task_id"),
        "{collected}"
    );
    let killed = ShellKillTool::new()
        .invoke(json!({ "task_id": valid_unknown }))
        .await
        .unwrap();
    assert!(
        killed["error"]
            .as_str()
            .unwrap()
            .contains("unknown task_id"),
        "{killed}"
    );
}

// ── Item 11 (registry half): boot sweep marks running metas orphaned ─────

#[test]
fn sweep_orphans_marks_running_meta_orphaned_without_signalling() {
    let home = set_zen_home();
    let task_dir = home.join("logs").join("tasks").join("orphans-pin-task");
    std::fs::create_dir_all(&task_dir).unwrap();
    let meta = json!({
        "task_id": "orphans-pin-task",
        "session_id": "local",
        "pid": u32::MAX,
        "command_digest": "orphan pin",
        "output_dir": task_dir.to_string_lossy(),
        "status": "running",
        "started_at_ms": chrono::Utc::now().timestamp_millis() - 60_000,
        "ended_at_ms": null,
        "bytes_stdout": 0,
        "bytes_stderr": 0,
        "truncated": false,
        "stdout_path": task_dir.join("stdout").to_string_lossy(),
        "stderr_path": task_dir.join("stderr").to_string_lossy()
    });
    std::fs::write(
        task_dir.join("meta.json"),
        serde_json::to_vec_pretty(&meta).unwrap(),
    )
    .unwrap();

    let marked = sweep_orphans_on_boot();
    assert!(marked >= 1, "the running meta must be marked: {marked}");
    let rewritten = task_registry::read_meta("orphans-pin-task").unwrap();
    assert_eq!(rewritten.status, TaskStatus::Orphaned);
    assert!(rewritten.ended_at_ms.is_some());
}

#[test]
fn sweep_orphans_skips_metas_older_than_retention_window() {
    let home = set_zen_home();
    let task_dir = home.join("logs").join("tasks").join("stale-relic-task");
    std::fs::create_dir_all(&task_dir).unwrap();
    let meta = json!({
        "task_id": "stale-relic-task",
        "session_id": "local",
        "pid": u32::MAX,
        "command_digest": "stale relic",
        "output_dir": task_dir.to_string_lossy(),
        "status": "running",
        "started_at_ms": 1,
        "ended_at_ms": null,
        "bytes_stdout": 0,
        "bytes_stderr": 0,
        "truncated": false,
        "stdout_path": task_dir.join("stdout").to_string_lossy(),
        "stderr_path": task_dir.join("stderr").to_string_lossy()
    });
    std::fs::write(
        task_dir.join("meta.json"),
        serde_json::to_vec_pretty(&meta).unwrap(),
    )
    .unwrap();

    // The 7d-stale skip: retention deletes these dirs anyway; the sweep
    // must not spend probe/signal work on relics.
    let _ = sweep_orphans_on_boot();
    let untouched = task_registry::read_meta("stale-relic-task").unwrap();
    assert_eq!(untouched.status, TaskStatus::Running);
    assert!(untouched.ended_at_ms.is_none());
}

// ── Oracle ITEM 2 (2026-10-31 arbitration): live-group two-tier sweep ────
// A crashed daemon can still have REAL orphan processes running. The sweep
// probes kill(-pgid, 0): dead group ⇒ mark-only; live group whose leader's
// kernel-recorded start time matches the spawn record ⇒ SIGKILL (verified
// ours); live but unverifiable (start-time mismatch) ⇒ mark-only.

#[cfg(unix)]
fn spawn_real_orphan(sleep_secs: &str) -> std::process::Child {
    std::process::Command::new("/bin/sleep")
        .arg(sleep_secs)
        .process_group(0)
        .spawn()
        .expect("sleep spawns in its own process group")
}

#[cfg(unix)]
fn write_running_meta(home: &std::path::Path, task_id: &str, pid: u32, started_at_ms: i64) {
    let task_dir = home.join("logs").join("tasks").join(task_id);
    std::fs::create_dir_all(&task_dir).unwrap();
    let meta = json!({
        "task_id": task_id,
        "session_id": "local",
        "pid": pid,
        "command_digest": "real orphan",
        "output_dir": task_dir.to_string_lossy(),
        "status": "running",
        "started_at_ms": started_at_ms,
        "ended_at_ms": null,
        "bytes_stdout": 0,
        "bytes_stderr": 0,
        "truncated": false,
        "stdout_path": task_dir.join("stdout").to_string_lossy(),
        "stderr_path": task_dir.join("stderr").to_string_lossy()
    });
    std::fs::write(
        task_dir.join("meta.json"),
        serde_json::to_vec_pretty(&meta).unwrap(),
    )
    .unwrap();
}

#[cfg(unix)]
fn wait_reaped(child: &mut std::process::Child) -> std::process::ExitStatus {
    for _ in 0..500 {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return status;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("orphan child was not reaped within the poll window");
}

#[tokio::test]
#[cfg(unix)]
async fn sweep_sends_kill_when_orphan_group_is_verifiably_ours() {
    let home = set_zen_home();
    let mut child = spawn_real_orphan("30");
    let pid = child.id();
    // Spawn bookkeeping happened ~now; kernel start time matches within
    // the ±10s tolerance ⇒ identity verified.
    write_running_meta(
        &home,
        "verified-orphan-task",
        pid,
        chrono::Utc::now().timestamp_millis(),
    );

    let marked = sweep_orphans_on_boot();
    assert!(marked >= 1, "verified orphan marked: {marked}");
    let rewritten = task_registry::read_meta("verified-orphan-task").unwrap();
    assert_eq!(rewritten.status, TaskStatus::Orphaned);

    // Bounded reap (LEAK-detector lesson): the SIGKILLed group must be
    // reaped by the test itself.
    let status = wait_reaped(&mut child);
    assert!(!status.success(), "killed group ⇒ non-zero exit");
}

#[tokio::test]
#[cfg(unix)]
async fn sweep_never_signals_a_live_group_with_mismatched_identity() {
    let home = set_zen_home();
    let mut child = spawn_real_orphan("30");
    let pid = child.id();
    // Start time recorded >10s away from the kernel's — identity cannot be
    // verified ⇒ mark-only, no signal (pid-reuse fail-safe).
    write_running_meta(
        &home,
        "mismatch-orphan-task",
        pid,
        chrono::Utc::now().timestamp_millis() - 60_000,
    );

    let _ = sweep_orphans_on_boot();
    let rewritten = task_registry::read_meta("mismatch-orphan-task").unwrap();
    assert_eq!(rewritten.status, TaskStatus::Orphaned);

    // The real child must STILL be alive (unverifiable ⇒ no signal).
    let mut alive = true;
    for _ in 0..100 {
        if child.try_wait().expect("try_wait").is_some() {
            alive = false;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        alive,
        "unverifiable group must not be signalled by the sweep"
    );

    // Test teardown: kill the real spawn ourselves and reap (bounded).
    // SAFETY: kill(-pid, SIGKILL) targets exactly the group the test
    // spawned with process_group(0) — no other process is in it.
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
    }
    let _ = wait_reaped(&mut child);
}

#[test]
fn sweep_orphans_skips_corrupt_meta_and_terminal_metas() {
    let home = set_zen_home();
    let corrupt_dir = home.join("logs").join("tasks").join("corrupt-meta-task");
    std::fs::create_dir_all(&corrupt_dir).unwrap();
    std::fs::write(corrupt_dir.join("meta.json"), b"{not json").unwrap();

    let terminal_dir = home.join("logs").join("tasks").join("terminal-meta-task");
    std::fs::create_dir_all(&terminal_dir).unwrap();
    let meta = json!({
        "task_id": "terminal-meta-task",
        "session_id": "local",
        "pid": 1,
        "command_digest": "done",
        "output_dir": terminal_dir.to_string_lossy(),
        "status": { "completed": 0 },
        "started_at_ms": 1,
        "ended_at_ms": 2,
        "bytes_stdout": 0,
        "bytes_stderr": 0,
        "truncated": false,
        "stdout_path": terminal_dir.join("stdout").to_string_lossy(),
        "stderr_path": terminal_dir.join("stderr").to_string_lossy()
    });
    std::fs::write(
        terminal_dir.join("meta.json"),
        serde_json::to_vec_pretty(&meta).unwrap(),
    )
    .unwrap();

    // Must not panic on the corrupt file and must not touch the terminal one.
    let _ = sweep_orphans_on_boot();
    let terminal = task_registry::read_meta("terminal-meta-task").unwrap();
    assert_eq!(terminal.status, TaskStatus::Completed(0));
}

// ── Item 5 companion: expire_due enforces the wall-clock deadline ────────

#[tokio::test]
#[cfg(unix)]
async fn expire_due_timeouts_overdue_running_tasks() {
    set_zen_home();
    let mut cmd = tokio::process::Command::new("/bin/sleep");
    cmd.args(["30"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null());
    #[cfg(unix)]
    cmd.process_group(0);
    cmd.kill_on_drop(false);
    let snapshot = registry()
        .spawn_background(cmd, "local", "sleep 30", &limits(4))
        .unwrap();

    // started_at_ms + max_wall_secs*1000 <= now ⇒ overdue.
    let now = task_registry::now_ms();
    let expired = registry().expire_due(now + Duration::from_secs(60).as_millis() as i64);
    assert!(
        expired.iter().any(|id| id == &snapshot.task_id),
        "overdue task must expire: {expired:?}"
    );
    let after = registry().snapshot(&snapshot.task_id).unwrap();
    assert_eq!(after.status, TaskStatus::Timeout);
    reap_wait(&snapshot.task_id, Duration::from_secs(3)).await;
}

// ── Review fix (a): crafted meta with an escaping capture path is refused ─

#[tokio::test]
#[cfg(unix)]
async fn crafted_meta_with_escaping_capture_path_is_refused() {
    set_zen_home();
    let home = std::env::var("ZEN_HOME").unwrap();
    let task_id = "0123456789abcdef0123456789abcdef";
    let task_dir = PathBuf::from(&home)
        .join("logs")
        .join("tasks")
        .join(task_id);
    std::fs::create_dir_all(&task_dir).unwrap();
    let meta = json!({
        "task_id": task_id,
        "session_id": "local",
        "pid": 1,
        "command_digest": "crafted",
        "output_dir": task_dir.to_string_lossy(),
        "status": { "completed": 0 },
        "started_at_ms": 1,
        "ended_at_ms": 2,
        "bytes_stdout": 0,
        "bytes_stderr": 0,
        "truncated": false,
        "stdout_path": "/etc/hosts",
        "stderr_path": task_dir.join("stderr").to_string_lossy()
    });
    std::fs::write(
        task_dir.join("meta.json"),
        serde_json::to_vec_pretty(&meta).unwrap(),
    )
    .unwrap();

    let collected = ShellCollectTool::new()
        .invoke(json!({ "task_id": task_id, "wait_ms": 0 }))
        .await
        .unwrap();
    let err = collected["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("corrupt meta"),
        "escaping capture path must be refused as corrupt meta: {collected}"
    );
}

// ── Review fix (b): kill/expire on a terminal entry are no-ops ───────────

#[tokio::test]
#[cfg(unix)]
async fn kill_after_natural_completion_returns_completed_snapshot() {
    set_zen_home();
    let res = exec_tool()
        .invoke(json!({
            "binary": "/bin/echo",
            "args": ["done"],
            "background": true
        }))
        .await
        .unwrap();
    let task_id = res["task_id"].as_str().unwrap().to_string();
    reap_wait(&task_id, Duration::from_secs(5)).await;

    let killed = ShellKillTool::new()
        .invoke(json!({ "task_id": task_id }))
        .await
        .unwrap();
    assert_eq!(killed["status"], "completed", "{killed}");
    assert!(
        killed["error"].as_str().unwrap_or_default().is_empty(),
        "no error for a terminal entry: {killed}"
    );
    let after = registry().snapshot(&task_id).unwrap();
    assert_eq!(after.status, TaskStatus::Completed(0));
}

#[tokio::test]
#[cfg(unix)]
async fn expire_due_ignores_terminal_entries() {
    set_zen_home();
    let res = exec_tool()
        .invoke(json!({
            "binary": "/bin/echo",
            "args": ["done"],
            "background": true
        }))
        .await
        .unwrap();
    let task_id = res["task_id"].as_str().unwrap().to_string();
    reap_wait(&task_id, Duration::from_secs(5)).await;

    let expired = registry().expire_due(i64::MAX);
    assert!(
        !expired.iter().any(|id| id == &task_id),
        "terminal entries must never expire: {expired:?}"
    );
    let after = registry().snapshot(&task_id).unwrap();
    assert_eq!(after.status, TaskStatus::Completed(0));
}

// ── Review fix (c): SIGTERM-immune group gets the SIGKILL escalation ─────

#[tokio::test]
#[cfg(unix)]
async fn kill_escalation_sigkills_sigterm_immune_group() {
    set_zen_home();
    let res = exec_tool()
        .invoke(json!({
            "binary": "/bin/sh",
            "args": ["-c", "trap \"\" TERM; sleep 30"],
            "background": true
        }))
        .await
        .unwrap();
    let task_id = res["task_id"].as_str().unwrap().to_string();
    let pid = registry().snapshot(&task_id).unwrap().pid;

    let killed = ShellKillTool::new()
        .invoke(json!({ "task_id": task_id }))
        .await
        .unwrap();
    assert_eq!(killed["status"], "killed", "{killed}");

    // The group ignores SIGTERM; only the 3s-probe SIGKILL can end it.
    assert!(
        wait_pid_gone(pid, Duration::from_secs(8)).await,
        "SIGTERM-immune leader must die via SIGKILL escalation"
    );
    reap_wait(&task_id, Duration::from_secs(3)).await;
}

// ── Review fix (d): kill_all_blocking escalates within its budget ────────

#[tokio::test]
#[cfg(unix)]
async fn kill_all_blocking_kills_sigterm_immune_group() {
    set_zen_home();
    let mut cmd = tokio::process::Command::new("/bin/sh");
    cmd.args(["-c", "trap \"\" TERM; sleep 30"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null());
    cmd.process_group(0);
    cmd.kill_on_drop(false);
    let snapshot = registry()
        .spawn_background(cmd, "local", "trap-term sleep", &limits(4))
        .unwrap();

    let killed = registry().kill_all_blocking("pin");
    assert!(
        killed.iter().any(|id| id == &snapshot.task_id),
        "drain must report the killed id: {killed:?}"
    );
    assert!(
        wait_pid_gone(snapshot.pid, Duration::from_secs(8)).await,
        "kill_all_blocking must SIGKILL the TERM-immune group within budget"
    );
    reap_wait(&snapshot.task_id, Duration::from_secs(3)).await;
}

// ── Review fix (f): terminal meta rewrite settles final bytes ────────────

#[tokio::test]
#[cfg(unix)]
async fn terminal_meta_rewrite_settles_final_bytes_after_drains() {
    set_zen_home();
    let res = exec_tool()
        .invoke(json!({
            "binary": "/bin/sh",
            "args": ["-c", "echo hi; sleep 0.2; echo bye"],
            "background": true
        }))
        .await
        .unwrap();
    let task_id = res["task_id"].as_str().unwrap().to_string();

    // Poll until the finalizer has rewritten the meta: terminal status AND
    // the full byte count ("hi\nbye\n" = 7 bytes) — the spawn-time meta
    // would show 0 bytes.
    let deadline = Instant::now() + Duration::from_secs(5);
    let meta = loop {
        let m = task_registry::read_meta(&task_id).unwrap();
        if m.status.is_terminal() && m.bytes_stdout == 7 {
            break m;
        }
        assert!(
            Instant::now() < deadline,
            "finalizer must settle final bytes: {m:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(matches!(meta.status, TaskStatus::Completed(0)), "{meta:?}");
    let stdout = std::fs::read_to_string(&meta.stdout_path).unwrap();
    assert_eq!(stdout, "hi\nbye\n");
    reap_wait(&task_id, Duration::from_secs(3)).await;
}

// ── Review fix (g): concurrent spawns reserve slots atomically ───────────

#[tokio::test]
#[cfg(unix)]
async fn concurrent_spawns_reserve_slots_atomically() {
    set_zen_home();
    let handles: Vec<_> = (0..6)
        .map(|_| {
            tokio::task::spawn_blocking(|| {
                let mut cmd = tokio::process::Command::new("/bin/sleep");
                cmd.args(["5"])
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .stdin(std::process::Stdio::null());
                cmd.process_group(0);
                cmd.kill_on_drop(false);
                registry().spawn_background(cmd, "local", "sleep 5", &limits(2))
            })
        })
        .collect();
    let mut results = Vec::new();
    for h in handles {
        results.push(h.await.unwrap());
    }
    let accepted: Vec<_> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
    let rejected: Vec<_> = results.iter().filter_map(|r| r.as_ref().err()).collect();
    assert_eq!(accepted.len(), 2, "exactly the ceiling passes: {results:?}");
    assert_eq!(rejected.len(), 4, "the rest are rejected: {results:?}");
    assert!(
        rejected
            .iter()
            .all(|e| e.to_string().contains("max_concurrent")),
        "rejections must name the limit: {rejected:?}"
    );
    assert_eq!(registry().live_count(), 2);
    for s in accepted {
        registry().kill(&s.task_id).unwrap();
        reap_wait(&s.task_id, Duration::from_secs(3)).await;
    }
}

// ── Review fix (h): boot sweep never marks a live registry task ──────────

#[tokio::test]
#[cfg(unix)]
async fn boot_sweep_skips_live_registry_tasks() {
    set_zen_home();
    let res = exec_tool()
        .invoke(json!({
            "binary": "/bin/sleep",
            "args": ["30"],
            "background": true
        }))
        .await
        .unwrap();
    let task_id = res["task_id"].as_str().unwrap().to_string();

    let marked = sweep_orphans_on_boot();
    assert_eq!(
        marked, 0,
        "a live registry task must never be marked orphaned: {marked}"
    );
    let meta = task_registry::read_meta(&task_id).unwrap();
    assert_eq!(meta.status, TaskStatus::Running, "meta untouched");

    registry().kill(&task_id).unwrap();
    reap_wait(&task_id, Duration::from_secs(3)).await;
}

// ── Review fix (i): draining flag rejects spawns until cleared ───────────

#[tokio::test]
#[cfg(unix)]
async fn draining_flag_rejects_spawn_until_cleared() {
    set_zen_home();
    set_draining();
    let res = exec_tool()
        .invoke(json!({
            "binary": "/bin/sleep",
            "args": ["5"],
            "background": true
        }))
        .await
        .unwrap();
    let err = res["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("drain in progress"),
        "spawn during drain must be a structured rejection: {res}"
    );
    assert_eq!(registry().live_count(), 0, "nothing spawned");

    clear_draining();
    let res = exec_tool()
        .invoke(json!({
            "binary": "/bin/sleep",
            "args": ["5"],
            "background": true
        }))
        .await
        .unwrap();
    assert!(res["error"].as_str().is_none(), "{res}");
    let task_id = res["task_id"].as_str().unwrap().to_string();
    registry().kill(&task_id).unwrap();
    reap_wait(&task_id, Duration::from_secs(3)).await;
}

// ── Review fix (k1): non-boolean `background` is a structured arg error ──

#[tokio::test]
#[cfg(unix)]
async fn background_wrong_type_is_structured_arg_error() {
    set_zen_home();
    for bad in [json!("true"), json!(1)] {
        let res = exec_tool()
            .invoke(json!({
                "binary": "/bin/echo",
                "args": ["x"],
                "background": bad
            }))
            .await;
        assert!(
            res.is_err(),
            "non-boolean background must be rejected: {bad}"
        );
        assert!(
            res.unwrap_err().to_string().contains("background"),
            "rejection must name the argument"
        );
    }
}
