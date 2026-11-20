//! W12 review-fix pins (review 2026-11-17, findings W12.5/W12.6/W12.7):
//!
//! 1. W12.5 — the `[agentic.background] max_concurrent` union cap counts
//!    LIVE handles, not merely `Running` statuses: an expiry-zombie
//!    (terminal status, handle still alive — W5.1 expiry never aborts)
//!    consumes a slot until its handle terminates; process-path and
//!    async-path tasks share the SAME union cap.
//! 2. W12.6 — spawn-during-drain is refused (integration contract pin;
//!    the tight-window interleaving proof lives in `task_registry.rs`
//!    `mod tests` because it needs private entries-lock access).
//! 3. W12.7 — `collect_delegate`'s registry-miss fallback normalizes a
//!    stale (>7d) `running` meta to `Expired` for the response AND on
//!    disk (never running-forever); fresh running metas are untouched.
//!
//! Runs under nextest (process-per-test): the process-global
//! [`zen_plugin::task_registry::registry`] and the ZEN_HOME env mutation
//! are safe per test (T118 discipline).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::json;
use zen_plugin::task_registry::{
    self, BackgroundLimits, TaskMeta, TaskStatus, clear_draining, meta_path, now_ms, read_meta,
    registry, set_draining,
};

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

fn limits(max_concurrent: usize) -> BackgroundLimits {
    BackgroundLimits {
        max_wall_secs: 60,
        max_concurrent,
        output_cap_bytes: task_registry::OUTPUT_CAP_BYTES,
    }
}

fn valid_task_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Bounded wait until a spawned JoinHandle terminates (the completion
/// wrapper runs synchronously after the future resolves; the handle flag
/// flips within a tick or two).
async fn wait_handle_finished(handle: &tokio::task::JoinHandle<serde_json::Value>) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !handle.is_finished() {
        assert!(
            Instant::now() < deadline,
            "zombie handle never terminated after release"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Bounded reap-wait (house pattern): poll until the registry snapshot is
/// terminal, then a fixed settle for escalation threads — prevents nextest
/// LEAK flags from unreaped zombies at process drop.
async fn reap_wait(task_id: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if registry()
            .snapshot(task_id)
            .is_some_and(|s| s.status.is_terminal())
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "task never reached a terminal state"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
}

/// W12.5 pin: an expiry-zombie (Expired status, live handle) still consumes
/// a concurrency slot; once the handle terminates the slot frees.
#[tokio::test]
async fn expired_zombie_and_new_admissions_never_exceed_max_concurrent() {
    let _home = set_zen_home();
    clear_draining();

    let zombie_id = valid_task_id();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let zombie = registry()
        .spawn_async(
            &zombie_id,
            "local",
            "prometheus",
            None,
            &limits(1),
            async move {
                let _ = rx.await;
                Ok(json!({"late": true}))
            },
        )
        .expect("first admission under an empty registry must succeed");

    // GC expiry marks Expired WITHOUT aborting the handle (W5.1) — the
    // future is still parked on the oneshot, i.e. a live zombie.
    let expired = registry().expire_due(now_ms() + 61_000);
    assert!(
        expired.iter().any(|id| id == &zombie_id),
        "GC must expire the parked future's task"
    );
    assert_eq!(
        registry().snapshot(&zombie_id).unwrap().status,
        TaskStatus::Expired
    );
    assert!(!zombie.is_finished(), "expiry must not abort the handle");

    // THE PIN: the zombie holds the slot even though its status is
    // terminal — admission at max_concurrent=1 must be refused.
    let refused = registry().spawn_async(
        &valid_task_id(),
        "local",
        "prometheus",
        None,
        &limits(1),
        async { Ok(json!({})) },
    );
    let err = refused.expect_err("zombie must consume a slot until its handle terminates");
    assert!(
        err.to_string().contains("max_concurrent"),
        "structured concurrency rejection, got: {err}"
    );

    // Release the zombie; once the handle terminates the slot frees.
    tx.send(()).expect("zombie still parked");
    wait_handle_finished(&zombie).await;
    registry()
        .spawn_async(
            &valid_task_id(),
            "local",
            "prometheus",
            None,
            &limits(1),
            async { Ok(json!({})) },
        )
        .expect("admission must succeed once the zombie's handle terminated");
    clear_draining();
}

/// W12.5 pin (union): one process-path task + one async-path task fill the
/// cap together — neither path can admit past the shared union cap.
#[tokio::test]
async fn union_cap_spans_process_and_async_paths() {
    let _home = set_zen_home();
    clear_draining();

    // Slot 1: process path (sleep 30, own group — house spawn shape).
    let mut cmd = tokio::process::Command::new("/bin/sleep");
    cmd.arg("30");
    cmd.process_group(0);
    let process_task = registry()
        .spawn_background(cmd, "local", "sleep 30", &limits(2))
        .expect("process-path admission under an empty registry must succeed");

    // Slot 2: async path (parked future).
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let async_task = registry()
        .spawn_async(
            &valid_task_id(),
            "local",
            "prometheus",
            None,
            &limits(2),
            async move {
                let _ = rx.await;
                Ok(json!({}))
            },
        )
        .expect("async-path admission at 1/2 live must succeed");

    // THE PIN: with 1 process + 1 async live, BOTH paths refuse at cap 2.
    let async_refused = registry().spawn_async(
        &valid_task_id(),
        "local",
        "prometheus",
        None,
        &limits(2),
        async { Ok(json!({})) },
    );
    assert!(
        async_refused.is_err(),
        "async admission must refuse at the union cap (1 process + 1 async live)"
    );

    let mut process_cmd = tokio::process::Command::new("/bin/sleep");
    process_cmd.arg("30");
    process_cmd.process_group(0);
    let process_refused = registry().spawn_background(process_cmd, "local", "sleep 30", &limits(2));
    assert!(
        process_refused.is_err(),
        "process admission must refuse at the same union cap"
    );

    // Cleanup: kill + reap the process task, release the async future.
    registry()
        .kill(&process_task.task_id)
        .expect("kill of a live process task must succeed");
    reap_wait(&process_task.task_id, Duration::from_secs(10)).await;
    tx.send(()).expect("async future still parked");
    wait_handle_finished(&async_task).await;
    clear_draining();
}

/// W12.6 pin (integration contract): with the drain flag set, BOTH spawn
/// paths refuse with the structured drain rejection. The tight-window
/// interleaving (outer check passes, flag set while the spawn parks on the
/// entries lock) is pinned in `task_registry.rs` `mod tests`.
#[tokio::test]
async fn spawn_during_drain_refused_inside_the_critical_section() {
    let _home = set_zen_home();
    clear_draining();
    set_draining();

    let mut cmd = tokio::process::Command::new("/bin/sleep");
    cmd.arg("5");
    cmd.process_group(0);
    let process_refused = registry().spawn_background(cmd, "local", "sleep 5", &limits(4));
    assert!(process_refused.is_err(), "drain must refuse process spawns");

    let async_refused = registry().spawn_async(
        &valid_task_id(),
        "local",
        "prometheus",
        None,
        &limits(4),
        async { Ok(json!({})) },
    );
    assert!(async_refused.is_err(), "drain must refuse async spawns");

    // The registry stays empty — a drain-window refusal must not insert.
    assert_eq!(
        registry().live_count(),
        0,
        "drain-window refusals must not leave entries behind"
    );

    clear_draining();
    let after_clear = registry().spawn_async(
        &valid_task_id(),
        "local",
        "prometheus",
        None,
        &limits(4),
        async { Ok(json!({})) },
    );
    assert!(
        after_clear.is_ok(),
        "clearing the drain flag must re-enable admissions"
    );
    clear_draining();
}

/// W12.7 pin: a registry-miss collect over a stale (>7d) `running` meta
/// yields `Expired` for the response AND persists the normalized rewrite.
#[tokio::test]
async fn collect_of_stale_running_meta_yields_expired_and_persists_rewrite() {
    let _home = set_zen_home();

    let task_id = valid_task_id();
    let meta_file = meta_path(&task_id);
    std::fs::create_dir_all(meta_file.parent().unwrap()).unwrap();
    let stale = TaskMeta {
        task_id: task_id.clone(),
        session_id: "sess".into(),
        pid: 0,
        command_digest: "delegate:prometheus".into(),
        output_dir: meta_file.parent().unwrap().to_string_lossy().into_owned(),
        status: TaskStatus::Running,
        started_at_ms: now_ms() - 8 * 86_400_000,
        ended_at_ms: None,
        bytes_stdout: 0,
        bytes_stderr: 0,
        truncated: false,
        stdout_path: "/o/stdout".into(),
        stderr_path: "/o/stderr".into(),
        agent: Some("prometheus".into()),
        task_index: Some(0),
        result: None,
    };
    std::fs::write(&meta_file, serde_json::to_vec_pretty(&stale).unwrap()).unwrap();

    let out = registry()
        .collect_delegate(&task_id, 0)
        .await
        .expect("collect over an existing meta must not error");
    assert_eq!(
        out.status, "expired",
        "stale running meta must read expired"
    );
    assert_eq!(
        out.error.as_deref(),
        Some("task ended as expired"),
        "expired outcome carries the explanatory error"
    );
    assert!(out.from_meta_fallback, "registry-miss collect reads meta");
    assert_eq!(out.agent.as_deref(), Some("prometheus"));
    assert_eq!(out.task_index, Some(0));

    // THE PIN (persist half): the normalized rewrite landed on disk.
    let back = read_meta(&task_id).expect("meta must remain readable after rewrite");
    assert_eq!(back.status, TaskStatus::Expired);
    assert!(back.ended_at_ms.is_some(), "expiry stamps ended_at_ms");

    // Idempotent: a second collect observes the same terminal status.
    let again = registry().collect_delegate(&task_id, 0).await.unwrap();
    assert_eq!(again.status, "expired");
}

/// W12.7 control: a FRESH (<7d) running meta stays `running` and the disk
/// meta is untouched — normalization must never eat live-restartable state.
#[tokio::test]
async fn collect_of_fresh_running_meta_stays_running_and_untouched() {
    let _home = set_zen_home();

    let task_id = valid_task_id();
    let meta_file = meta_path(&task_id);
    std::fs::create_dir_all(meta_file.parent().unwrap()).unwrap();
    let fresh = TaskMeta {
        task_id: task_id.clone(),
        session_id: "sess".into(),
        pid: 0,
        command_digest: "delegate:prometheus".into(),
        output_dir: meta_file.parent().unwrap().to_string_lossy().into_owned(),
        status: TaskStatus::Running,
        started_at_ms: now_ms() - 3_600_000,
        ended_at_ms: None,
        bytes_stdout: 0,
        bytes_stderr: 0,
        truncated: false,
        stdout_path: "/o/stdout".into(),
        stderr_path: "/o/stderr".into(),
        agent: Some("prometheus".into()),
        task_index: None,
        result: None,
    };
    let raw = serde_json::to_vec_pretty(&fresh).unwrap();
    std::fs::write(&meta_file, &raw).unwrap();

    let out = registry().collect_delegate(&task_id, 0).await.unwrap();
    assert_eq!(
        out.status, "running",
        "fresh running meta must stay running"
    );
    assert_eq!(
        out.error.as_deref(),
        Some("task ended as running"),
        "non-terminal fallback reports the current label"
    );
    assert!(out.from_meta_fallback);

    // Disk untouched: byte-identical to the fixture (no rewrite).
    assert_eq!(
        std::fs::read(&meta_file).unwrap(),
        raw,
        "fresh running meta must not be rewritten"
    );
    let back = read_meta(&task_id).unwrap();
    assert_eq!(back.status, TaskStatus::Running);
    assert!(back.ended_at_ms.is_none());
}
