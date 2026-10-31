//! Background-task pins (design record
//! `docs/designs/background-tool-execution-2026-10-30.md`, Section 7 items
//! 2,3,4,5,6,7,9,10 + registry half of 11).
//!
//! Runs under nextest (process-per-test): the process-global
//! [`zen_plugin::task_registry::registry`] and the ZEN_HOME env mutation are
//! safe per test (T118 discipline).

use std::path::PathBuf;
use std::time::Duration;

use rig_compose::tool::Tool;
use serde_json::json;
use zen_plugin::task_registry::{
    self, BackgroundLimits, TaskStatus, registry, sweep_orphans_on_boot,
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
}

// ── Item 10: unknown task_id is a structured error, never a panic ────────

#[tokio::test]
#[cfg(unix)]
async fn unknown_task_id_is_structured_error_for_collect_and_kill() {
    set_zen_home();
    let collected = ShellCollectTool::new()
        .invoke(json!({ "task_id": "does-not-exist", "wait_ms": 0 }))
        .await
        .unwrap();
    assert!(
        collected["error"]
            .as_str()
            .unwrap()
            .contains("does-not-exist"),
        "{collected}"
    );

    let killed = ShellKillTool::new()
        .invoke(json!({ "task_id": "does-not-exist" }))
        .await
        .unwrap();
    assert!(killed["error"].as_str().unwrap().contains("does-not-exist"));
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

    let marked = sweep_orphans_on_boot();
    assert!(marked >= 1, "the running meta must be marked: {marked}");
    let rewritten = task_registry::read_meta("orphans-pin-task").unwrap();
    assert_eq!(rewritten.status, TaskStatus::Orphaned);
    assert!(rewritten.ended_at_ms.is_some());
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
}
