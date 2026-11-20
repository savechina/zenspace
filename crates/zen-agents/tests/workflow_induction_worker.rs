//! W13.7 pins: the daily workflow-induction scheduler worker.
//!
//! Covers the worker identity/cron, registration in the configured scheduler,
//! an end-to-end seeded run (one draft) plus the zero-corpus AC#2 path, and the
//! fail-soft posture on an unreadable `state.db`.

use chrono::{TimeZone, Utc};

use zen_agents::scheduler::{
    WORKFLOW_INDUCTION_SCHEDULE, WorkerContext, WorkflowInductionWorker, ZenWorker,
    create_configured_scheduler,
};
use zen_agents::workflow_induction::{
    DRAFTS_FILE, InsufficientReason, WorkflowDraft, WorkflowInduction,
};
use zen_core::config::CronConfig;
use zen_core::paths::ZenPaths;
use zen_repo::{SqliteClient, TaskCheckpoint, WorkflowRepo};

const NOW: i64 = 1_800_000_000;

fn setup() -> (tempfile::TempDir, ZenPaths) {
    let dir = tempfile::tempdir().unwrap();
    let paths = ZenPaths::for_testing(dir.path().to_path_buf());
    (dir, paths)
}

fn ctx() -> WorkerContext {
    WorkerContext::new(Utc.with_ymd_and_hms(2026, 11, 19, 3, 0, 0).unwrap())
}

fn dag_spec() -> String {
    let tasks: Vec<serde_json::Value> = [
        ("t1", "researcher", Vec::<&str>::new()),
        ("t2", "analyst", vec!["t1"]),
        ("t3", "writer", vec!["t1"]),
        ("t4", "synthesizer", vec!["t2", "t3"]),
    ]
    .iter()
    .map(|(id, agent, deps)| {
        serde_json::json!({
            "id": id,
            "agent": agent,
            "prompt": "deterministic prompt",
            "depends_on": deps,
        })
    })
    .collect();
    serde_json::json!({
        "tasks": tasks,
        "sensitivity": "Private",
        "session_id": "sess",
    })
    .to_string()
}

async fn seed_completed_plan(client: &SqliteClient, plan_id: &str, created_at: i64) {
    let repo = WorkflowRepo::new(client);
    repo.create_plan(
        plan_id,
        Some("research-then-synthesize"),
        &dag_spec(),
        created_at,
    )
    .await
    .unwrap();
    for (task_id, agent) in [
        ("t1", "researcher"),
        ("t2", "analyst"),
        ("t3", "writer"),
        ("t4", "synthesizer"),
    ] {
        repo.checkpoint_task(TaskCheckpoint {
            plan_id,
            task_id,
            agent,
            status: "completed",
            response: Some("ok"),
            error: None,
            now: created_at,
            model: Some("model-x"),
            duration_ms: Some(1_000),
            cost_usd: Some(0.01),
        })
        .await
        .unwrap();
    }
    // W14.3: admission requires the engine sign-off.
    repo.complete_plan(plan_id, "completed", None, None, None, created_at)
        .await
        .unwrap();
}

#[test]
fn workflow_induction_worker_id_and_cron() {
    let worker = WorkflowInductionWorker::new();
    assert_eq!(worker.id(), "workflow-induction");
    assert_eq!(worker.schedule(), WORKFLOW_INDUCTION_SCHEDULE);
    assert_eq!(WORKFLOW_INDUCTION_SCHEDULE, "0 0 3 * * *");
    assert!(!worker.description().is_empty());
    assert_eq!(
        worker.with_schedule("0 5 4 * * *").schedule(),
        "0 5 4 * * *"
    );
}

#[test]
fn workflow_induction_worker_registration_via_configured_scheduler() {
    let scheduler = create_configured_scheduler(&CronConfig::default());
    let ids = scheduler.worker_ids();
    assert!(
        ids.contains(&"workflow-induction".to_string()),
        "configured scheduler missing workflow-induction: {ids:?}"
    );
}

#[tokio::test]
async fn workflow_induction_worker_end_to_end_daily_run() {
    let (_dir, paths) = setup();
    let client = SqliteClient::open(&paths.data().join("state.db"))
        .await
        .unwrap();
    seed_completed_plan(&client, "plan-a", NOW - 200).await;
    seed_completed_plan(&client, "plan-b", NOW - 100).await;
    drop(client);

    let worker = WorkflowInductionWorker::new();
    let report = worker.execute_with_paths(&paths, &ctx()).await.unwrap();
    assert!(report.success);
    assert_eq!(report.worker_id, "workflow-induction");
    assert_eq!(report.fact_count, 1);

    let drafts_path = paths.logs().join(DRAFTS_FILE);
    let drafts: Vec<WorkflowDraft> =
        serde_json::from_str(&std::fs::read_to_string(&drafts_path).unwrap()).unwrap();
    assert_eq!(
        drafts.len(),
        1,
        "two identical fully-completed shapes ⇒ one draft"
    );
    assert_eq!(drafts[0].source_plan_ids.len(), 2);

    // AC#2: zero corpus ⇒ NoPlansInWindow and NO drafts file written.
    let (empty_dir, empty_paths) = setup();
    let empty_client = SqliteClient::open(&empty_paths.data().join("state.db"))
        .await
        .unwrap();
    let run = WorkflowInduction::new(empty_paths.logs())
        .run(&empty_client, NOW)
        .await
        .unwrap();
    assert_eq!(run.insufficient, Some(InsufficientReason::NoPlansInWindow));
    assert_eq!(run.drafts_written, 0);
    drop(empty_client);

    let empty_report = worker
        .execute_with_paths(&empty_paths, &ctx())
        .await
        .unwrap();
    assert!(empty_report.success);
    assert_eq!(empty_report.fact_count, 0);
    assert!(
        !empty_paths.logs().join(DRAFTS_FILE).exists(),
        "zero-corpus run must not create the drafts file"
    );
    drop(empty_dir);
}

#[tokio::test]
async fn workflow_induction_worker_fail_soft_on_bad_db() {
    let (_dir, paths) = setup();
    std::fs::create_dir_all(paths.data()).unwrap();
    std::fs::create_dir_all(paths.data().join("state.db")).unwrap();

    let worker = WorkflowInductionWorker::new();
    let report = worker
        .execute_with_paths(&paths, &ctx())
        .await
        .expect("fail-soft: an unreadable state.db must not return Err");
    assert!(!report.success, "failed induction reports success: false");
    assert_eq!(report.fact_count, 0);
    assert!(!paths.logs().join(DRAFTS_FILE).exists());
}

#[tokio::test]
async fn workflow_induction_worker_kill_switch_skips_pass() {
    let (dir, paths) = setup();
    let saved_home = std::env::var("ZEN_HOME").ok();
    // SAFETY: nextest runs one test per process; both vars are restored below.
    unsafe {
        std::env::set_var("ZEN_HOME", dir.path());
        std::env::set_var("ZEN_WORKFLOW_INDUCTION_ENABLED", "false");
    }
    zen_core::config::invalidate_config_cache();

    let client = SqliteClient::open(&paths.data().join("state.db"))
        .await
        .unwrap();
    seed_completed_plan(&client, "plan-a", NOW - 200).await;
    seed_completed_plan(&client, "plan-b", NOW - 100).await;
    drop(client);

    let worker = WorkflowInductionWorker::new();
    let gated = worker.execute_with_paths(&paths, &ctx()).await.unwrap();
    assert!(gated.success);
    assert_eq!(gated.fact_count, 0);
    assert!(
        !paths.logs().join(DRAFTS_FILE).exists(),
        "kill switch must skip the pass (no drafts written)"
    );

    unsafe { std::env::remove_var("ZEN_WORKFLOW_INDUCTION_ENABLED") };
    zen_core::config::invalidate_config_cache();

    let ungated = worker.execute_with_paths(&paths, &ctx()).await.unwrap();
    assert!(ungated.success);
    assert_eq!(ungated.fact_count, 1);
    assert!(
        paths.logs().join(DRAFTS_FILE).exists(),
        "cleared kill switch ⇒ the pass runs and writes the draft"
    );

    match saved_home {
        Some(v) => unsafe { std::env::set_var("ZEN_HOME", v) },
        None => unsafe { std::env::remove_var("ZEN_HOME") },
    }
    zen_core::config::invalidate_config_cache();
}
