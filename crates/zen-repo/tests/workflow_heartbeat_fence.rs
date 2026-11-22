//! W12.2 (eng-review N1): plan claim-fence liveness heartbeat pins.
//!
//! PURPOSE: `checkpoint_task` must bump `workflow_plans.updated_at` in the
//! same transaction, so a live plan (checkpointing per task) is never
//! stale-swept nor claim-stolen inside its sanctioned window — while a plan
//! with NO checkpoint progress is still swept and stealable (crashed-run
//! recovery regression).
//!
//! SQL-level deterministic tests: explicit timestamps, explicit staleness
//! window (4×900s — the unified default, aligned with the
//! `PLAN_WATCHDOG_CEILING` renewal ceiling). No config/env dependency.

use tempfile::tempdir;
use zen_repo::{SqliteClient, TaskCheckpoint, WorkflowRepo};

/// The unified staleness window at defaults: 4× the 900s turn watchdog.
const STALE_SECS: i64 = 4 * 900;

fn cp<'a>(plan_id: &'a str, task_id: &'a str, now: i64) -> TaskCheckpoint<'a> {
    TaskCheckpoint {
        plan_id,
        task_id,
        agent: "Explore",
        status: "completed",
        response: Some("ok"),
        error: None,
        now,
        model: None,
        duration_ms: None,
        cost_usd: None,
    }
}

/// W12.2/N1 pin: a plan whose claim is stale-by-mint but heartbeat-fresh
/// (a task checkpoint just bumped `updated_at`) is NOT stealable by
/// `claim_plan` and NOT cancelled by the sweep within the window — the
/// pre-fix behavior (plan row frozen at mint time) failed exactly here.
#[tokio::test]
async fn fresh_checkpoint_plan_is_not_swept_nor_stolen_within_the_window() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("wf_heartbeat_fresh.db");
    let client = SqliteClient::open(&db).await.unwrap();
    let repo = WorkflowRepo::new(&client);

    let t0 = 1_000i64;
    repo.create_plan("live", None, "{}", t0).await.unwrap();
    assert!(
        repo.claim_plan("live", "runner-a", t0, STALE_SECS)
            .await
            .unwrap(),
        "first claim succeeds"
    );

    // A long sanctioned task finishes at t0+3000 (well inside the 4×watchdog
    // renewal ceiling) and checkpoints — the heartbeat must move the PLAN
    // row's updated_at to t0+3000.
    let t_heartbeat = t0 + 3000;
    repo.checkpoint_task(cp("live", "long-task", t_heartbeat))
        .await
        .unwrap();
    let plan = repo.load_plan("live").await.unwrap().unwrap();
    assert_eq!(
        plan.updated_at, t_heartbeat,
        "checkpoint_task must bump workflow_plans.updated_at (heartbeat)"
    );
    assert_eq!(plan.status, "running");
    assert_eq!(plan.owner.as_deref(), Some("runner-a"));

    // 1s inside the window measured FROM THE HEARTBEAT: no steal, no sweep.
    let t_probe = t_heartbeat + STALE_SECS - 1;
    assert!(
        !repo
            .claim_plan("live", "thief", t_probe, STALE_SECS)
            .await
            .unwrap(),
        "heartbeat-fresh claim must not be stealable within the window"
    );
    let (swept, _) = repo.mark_stale_plans(t_probe, STALE_SECS).await.unwrap();
    assert!(
        swept.is_empty(),
        "heartbeat-fresh running plan must not be stale-swept"
    );
    let plan = repo.load_plan("live").await.unwrap().unwrap();
    assert_eq!(plan.status, "running", "plan survives the sweep");
    assert_eq!(plan.owner.as_deref(), Some("runner-a"), "claim intact");

    // The heartbeat defers, it does not immortalize: past the window from
    // the LAST checkpoint the claim is stealable again.
    let t_after = t_heartbeat + STALE_SECS + 1;
    assert!(
        repo.claim_plan("live", "fresh-run", t_after, STALE_SECS)
            .await
            .unwrap(),
        "no progress past the window → stealable (crashed-run recovery)"
    );
}

/// W12.2/N1 regression pin: a plan with NO checkpoint progress is still
/// swept (terminal `cancelled`, reason `stale`, owner released) and its
/// claim is stealable past the window — the crashed-run recovery contract
/// must survive the heartbeat.
#[tokio::test]
async fn stale_plan_without_progress_is_swept_and_stealable() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("wf_heartbeat_stale.db");
    let client = SqliteClient::open(&db).await.unwrap();
    let repo = WorkflowRepo::new(&client);

    let t0 = 1_000i64;

    // Plan A: claimed, then silence — the sweep cancels it.
    repo.create_plan("swept", None, "{}", t0).await.unwrap();
    assert!(
        repo.claim_plan("swept", "dead-run", t0, STALE_SECS)
            .await
            .unwrap()
    );
    // Plan B: claimed, then silence — a thief steals it directly.
    repo.create_plan("stolen", None, "{}", t0).await.unwrap();
    assert!(
        repo.claim_plan("stolen", "dead-run", t0, STALE_SECS)
            .await
            .unwrap()
    );

    let t_after = t0 + STALE_SECS + 1;

    // The thief steals plan B directly (running row, silent past the
    // window) — the claim write refreshes its updated_at to t_after.
    assert!(
        repo.claim_plan("stolen", "thief", t_after, STALE_SECS)
            .await
            .unwrap(),
        "silent claim past the window is stealable"
    );

    // The sweep then cancels only plan A (plan B is heartbeat-fresh again
    // via the claim write) — the two recovery paths are independent.
    let (cancelled, _finalized) = repo.mark_stale_plans(t_after, STALE_SECS).await.unwrap();
    assert_eq!(
        cancelled,
        vec!["swept".to_string()],
        "the silent plan is swept"
    );
    let plan = repo.load_plan("swept").await.unwrap().unwrap();
    assert_eq!(plan.status, "cancelled");
    assert_eq!(plan.summary.as_deref(), Some("stale"), "reason record");
    assert!(plan.owner.is_none(), "sweep releases the claim");
}
