//! Principle XIII #6: migration invariants for 006_workflow_plans.sql.
//!
//! PURPOSE: Verifies the workflow tables apply cleanly and the
//! WorkflowRepo round-trips plan lifecycle + task checkpoints with
//! (plan_id, task_id) idempotency for resume.

use tempfile::tempdir;
use zen_repo::{SqliteClient, TaskCheckpoint, WorkflowRepo};

#[tokio::test]
async fn plan_lifecycle_and_task_checkpoints_round_trip() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("m006.db");
    let client = SqliteClient::open(&db).await.unwrap();
    let repo = WorkflowRepo::new(&client);

    repo.create_plan("p1", Some("diamond"), r#"{"tasks":[]}"#, 1000)
        .await
        .unwrap();
    repo.checkpoint_task(TaskCheckpoint {
        plan_id: "p1",
        task_id: "a",
        agent: "Explore",
        status: "ok",
        response: Some("done"),
        error: None,
        now: 1001,
    })
    .await
    .unwrap();
    repo.checkpoint_task(TaskCheckpoint {
        plan_id: "p1",
        task_id: "b",
        agent: "Explore",
        status: "pending",
        response: None,
        error: None,
        now: 1002,
    })
    .await
    .unwrap();

    let plan = repo.load_plan("p1").await.unwrap().expect("plan row");
    assert_eq!(plan.status, "running");
    assert_eq!(plan.name.as_deref(), Some("diamond"));

    let tasks = repo.load_task_rows("p1").await.unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].task_id, "a");
    assert_eq!(tasks[0].status, "ok");
    assert_eq!(tasks[0].response.as_deref(), Some("done"));

    repo.complete_plan(
        "p1",
        "failed",
        Some("upstream failed"),
        Some(true),
        Some(false),
        2000,
    )
    .await
    .unwrap();
    let done = repo.load_plan("p1").await.unwrap().unwrap();
    assert_eq!(done.status, "failed");
    assert_eq!(done.summary.as_deref(), Some("upstream failed"));
    assert_eq!(done.plan_approved, Some(1));
    assert_eq!(done.delivery_ready, Some(0));
    assert!(done.updated_at >= done.created_at);
}

#[tokio::test]
async fn checkpoint_upsert_is_idempotent_per_task() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("m006b.db");
    let client = SqliteClient::open(&db).await.unwrap();
    let repo = WorkflowRepo::new(&client);

    repo.create_plan("p2", None, "{}", 1000).await.unwrap();
    repo.checkpoint_task(TaskCheckpoint {
        plan_id: "p2",
        task_id: "a",
        agent: "Explore",
        status: "pending",
        response: None,
        error: None,
        now: 1001,
    })
    .await
    .unwrap();
    repo.checkpoint_task(TaskCheckpoint {
        plan_id: "p2",
        task_id: "a",
        agent: "Explore",
        status: "ok",
        response: Some("redone"),
        error: None,
        now: 1002,
    })
    .await
    .unwrap();

    let tasks = repo.load_task_rows("p2").await.unwrap();
    assert_eq!(
        tasks.len(),
        1,
        "same (plan, task) replaces, never duplicates"
    );
    assert_eq!(tasks[0].status, "ok");
    assert_eq!(tasks[0].response.as_deref(), Some("redone"));
}
