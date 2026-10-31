//! Lane G pins (background-tool-execution design, D1/D7): gateway daemon
//! lifecycle wiring for the background TaskRegistry.
//!
//! PURPOSE: pin the daemon-side seams — (a) SIGTERM drain kills live
//! background tasks and emits exactly one `gateway.background.killed`
//! audit line per id (zero live tasks ⇒ empty return, zero audit lines);
//! (b) the boot-orphan-sweep wrapper is fail-soft (missing/corrupt
//! `logs/tasks/` ⇒ 0, never a panic); (c) deadline expiry emits one
//! `gateway.background.expired` audit line per expired id and none when
//! empty; (d) audit-line shape: `taskId` first-8 + `taskIdFull` + `reason`.
//!
//! USAGE: cargo nextest run -p zen-gateway --test background_lifecycle
//!
//! EXPECTED: all pins pass; the one real-process pin spawns `/bin/sleep`
//! into the process-global registry, drains it, and finds exactly one
//! kill audit line plus a `killed` meta.json.
//!
//! ERRORS: an empty audit file after a drain that returns ids means the
//! audit-emission seam diverged from the drain path; a non-empty audit
//! file with an empty registry means the zero-event = zero-lines contract
//! regressed.
//!
//! Single-test binary: ZEN_HOME and the process-global TaskRegistry are
//! shared per process, so registry/env-touching scenarios serialize on a
//! mutex (session_persistence env-mutex precedent; nextest is
//! process-per-test, the mutex only guards plain `cargo test` runs).

use std::sync::{Mutex, OnceLock};

use serde_json::Value;
use zen_gateway::{
    drain_background_tasks, expire_background_due, report_expired_background_tasks,
    report_killed_background_tasks, sweep_background_orphans_blocking,
};

fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
}

/// Unique temp ZEN_HOME per call (fresh user_root resolution on every
/// path read under test-support).
fn isolate_home() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    unsafe { std::env::set_var("ZEN_HOME", tmp.path()) };
    tmp
}

fn read_audit_lines(path: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("audit line parses as JSON"))
        .collect()
}

// ── (a) drain: zero live tasks ⇒ empty + zero audit lines ──

#[tokio::test]
async fn drain_with_zero_live_tasks_returns_empty_and_writes_no_audit_lines() {
    // Guard scoped to the env-mutating setup: the drain await must not run
    // with a std MutexGuard alive (clippy::await_holding_lock). The drain
    // itself is env-independent (in-memory registry + explicit audit path).
    let tmp = {
        let _guard = test_lock();
        isolate_home()
    };
    let audit = tmp.path().join("audit.jsonl");

    let killed = drain_background_tasks(Some(&audit)).await;

    assert!(killed.is_empty(), "empty registry ⇒ no killed ids");
    assert!(
        !audit.exists(),
        "zero-event = zero-lines: no audit file may be created"
    );
    assert_eq!(read_audit_lines(&audit).len(), 0);
}

// ── (a, real-process) drain kills a live task and audits the kill ──
// The single honest real-process pin (registry entries exist only via
// spawn_background): /bin/sleep 30 spawned into the process-global
// registry, then drained.

#[tokio::test]
async fn drain_kills_live_background_task_and_audits_the_kill() {
    // tmp outlives the locked block: the reap poll below must run with NO
    // lock held (clippy::await_holding_lock) while the temp ZEN_HOME is
    // still on disk for the meta read.
    let tmp = tempfile::tempdir().expect("tempdir");
    let pid = {
        let _guard = test_lock();
        unsafe { std::env::set_var("ZEN_HOME", tmp.path()) };
        let audit = tmp.path().join("audit.jsonl");

        let mut cmd = zen_plugin::task_registry::BackgroundCommand::new("/bin/sleep");
        // Spawn contract (registry docstring): the CALLER sets
        // process_group(0) so the drain's kill(-pid) reaches the whole
        // group — mirrors shell_exec.rs's background path.
        cmd.arg("30")
            .stdout(std::process::Stdio::piped())
            .process_group(0);
        let snapshot = zen_plugin::task_registry::registry()
            .spawn_background(
                cmd,
                "pin-session",
                "/bin/sleep 30",
                &zen_plugin::task_registry::BackgroundLimits {
                    max_wall_secs: 1800,
                    max_concurrent: 4,
                    output_cap_bytes: zen_plugin::task_registry::OUTPUT_CAP_BYTES,
                },
            )
            .expect("sleep spawns");
        assert_eq!(
            zen_plugin::task_registry::registry().live_count(),
            1,
            "one live task after spawn"
        );

        let task_id = snapshot.task_id.clone();
        // Drop the guard before the drain await (clippy::await_holding_lock):
        // the drain is env-independent (in-memory registry + absolute
        // output_dir + explicit audit path). Re-acquired below for the
        // env-sensitive meta read.
        drop(_guard);
        let killed = drain_background_tasks(Some(&audit)).await;

        assert_eq!(killed, vec![task_id.clone()], "drain returns the killed id");
        assert_eq!(
            zen_plugin::task_registry::registry().live_count(),
            0,
            "no live tasks after drain"
        );

        let lines = read_audit_lines(&audit);
        assert_eq!(lines.len(), 1, "exactly one kill audit line");
        let line = &lines[0];
        assert_eq!(line["kind"], "gateway.background.killed");
        assert_eq!(line["taskId"], &task_id[..8], "taskId is the first 8 chars");
        assert_eq!(line["taskIdFull"], task_id.as_str());
        assert_eq!(line["reason"], "daemon-shutdown");
        assert!(line["ts"].is_string(), "ts present (hosting.rs shape)");

        // Disk truth: the drain rewrote meta.json to Killed. Re-acquire the
        // lock: read_meta resolves user_root() from ZEN_HOME.
        let _guard = test_lock();
        let meta = zen_plugin::task_registry::read_meta(&task_id).expect("meta exists");
        assert_eq!(meta.status, zen_plugin::task_registry::TaskStatus::Killed);
        snapshot.pid
    };

    // Wait for the registry's wait task to REAP the SIGTERMed child: an
    // unreaped zombie at runtime drop makes nextest flag the test LEAK.
    // kill_all only signals — the reap is the wait task's job, so keep
    // the runtime alive (bounded poll, lock-free) until the pid leaves
    // the table.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while zen_gateway::is_pid_alive(pid) && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        !zen_gateway::is_pid_alive(pid),
        "SIGTERMed child must be reaped before the runtime drops"
    );
}

// ── (b) boot sweep wrapper is fail-soft ──

#[test]
fn boot_sweep_wrapper_is_fail_soft_on_missing_and_corrupt_tasks_dir() {
    let _guard = test_lock();
    let tmp = isolate_home();

    // Missing logs/tasks/ entirely.
    assert_eq!(sweep_background_orphans_blocking(), 0);

    // Corrupt meta.json (never a panic on external data).
    let tasks = tmp.path().join("logs").join("tasks").join("bad-task");
    std::fs::create_dir_all(&tasks).unwrap();
    std::fs::write(tasks.join("meta.json"), "{not json").unwrap();
    assert_eq!(
        sweep_background_orphans_blocking(),
        0,
        "corrupt meta ⇒ skipped, 0 marked, no panic"
    );
}

// ── (c) expire: zero expired ⇒ empty + zero audit lines ──

#[tokio::test]
async fn expire_with_zero_live_tasks_returns_empty_and_writes_no_audit_lines() {
    // Guard scoped to the env-mutating setup (clippy::await_holding_lock);
    // the expire itself is env-independent.
    let tmp = {
        let _guard = test_lock();
        isolate_home()
    };
    let audit = tmp.path().join("audit.jsonl");

    let expired = expire_background_due(Some(&audit), i64::MAX).await;

    assert!(expired.is_empty(), "empty registry ⇒ nothing expires");
    assert!(!audit.exists(), "zero-event = zero-lines");
}

// ── (c) expired/killed audit shape: one line per id, none when empty ──

#[test]
fn expired_report_emits_one_line_per_id_and_none_when_empty() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let audit = tmp.path().join("audit.jsonl");

    report_expired_background_tasks(Some(&audit), &[]);
    assert!(!audit.exists(), "empty list writes nothing");

    let ids = vec!["abcdef1234567890".to_string(), "full-β-id-2".to_string()];
    report_expired_background_tasks(Some(&audit), &ids);

    let lines = read_audit_lines(&audit);
    assert_eq!(lines.len(), 2, "one line per expired id");
    assert_eq!(lines[0]["kind"], "gateway.background.expired");
    assert_eq!(lines[0]["taskId"], "abcdef12");
    assert_eq!(lines[0]["taskIdFull"], "abcdef1234567890");
    assert_eq!(lines[0]["reason"], "max_wall_secs deadline");
    // CJK id: taskId stays char-boundary-safe (first 8 CHARS, not bytes).
    assert_eq!(lines[1]["taskIdFull"], "full-β-id-2");
    assert_eq!(lines[1]["taskId"], "full-β-i");
}

#[test]
fn killed_report_emits_one_line_per_id_and_none_when_empty() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let audit = tmp.path().join("audit.jsonl");

    report_killed_background_tasks(Some(&audit), &[]);
    assert!(!audit.exists(), "empty list writes nothing");

    report_killed_background_tasks(Some(&audit), &["task-uuid-full".to_string()]);

    let lines = read_audit_lines(&audit);
    assert_eq!(lines.len(), 1, "one line per killed id");
    assert_eq!(lines[0]["kind"], "gateway.background.killed");
    assert_eq!(lines[0]["taskId"], "task-uui");
    assert_eq!(lines[0]["taskIdFull"], "task-uuid-full");
    assert_eq!(lines[0]["reason"], "daemon-shutdown");
}
