//! Principle XIII #6: migration invariants for 014_task_dispatch_history.sql.
//!
//! PURPOSE: prove the W9.1 dispatch-history columns exist and are nullable on
//! legacy rows, that `checkpoint_task` persists model/duration/cost, and that
//! `duration_history` returns only completed tasks with a recorded duration
//! (the forecast's omission contract) while `recent_plans` windows by
//! `updated_at`.

use tempfile::tempdir;
use zen_repo::{DurationHistoryRow, SqliteClient, TaskCheckpoint, WorkflowRepo};

async fn open_client(dir: &tempfile::TempDir) -> SqliteClient {
    let db = dir.path().join("migrated.db");
    SqliteClient::open(&db).await.unwrap()
}

#[tokio::test]
async fn migration_014_is_recorded_and_applied_by_the_migrator() {
    let dir = tempdir().unwrap();
    let client = open_client(&dir).await;

    let applied: Option<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE version = 14")
            .fetch_optional(client.pool())
            .await
            .unwrap();
    assert_eq!(applied, Some(14), "_sqlx_migrations must record 014");

    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('workflow_tasks')")
            .fetch_all(client.pool())
            .await
            .unwrap();
    for expected in ["model", "duration_ms", "cost_usd"] {
        assert!(
            columns.iter().any(|c| c == expected),
            "workflow_tasks must gain column `{expected}` (got {columns:?})"
        );
    }
}

#[tokio::test]
async fn checkpoint_roundtrips_dispatch_history_fields() {
    let dir = tempdir().unwrap();
    let client = open_client(&dir).await;
    let repo = WorkflowRepo::new(&client);

    repo.create_plan("p1", None, "{}", 1_000).await.unwrap();
    repo.checkpoint_task(TaskCheckpoint {
        plan_id: "p1",
        task_id: "t1",
        agent: "Explore",
        status: "completed",
        response: Some("done"),
        error: None,
        now: 1_100,
        model: Some("gpt-test"),
        duration_ms: Some(42_500),
        cost_usd: Some(0.0125),
    })
    .await
    .unwrap();

    let history = repo.duration_history().await.unwrap();
    assert_eq!(
        history,
        vec![DurationHistoryRow {
            agent: "Explore".into(),
            model: Some("gpt-test".into()),
            duration_ms: 42_500,
            cost_usd: Some(0.0125),
        }]
    );
}

#[tokio::test]
async fn duration_history_excludes_unrecorded_and_non_completed_rows() {
    let dir = tempdir().unwrap();
    let client = open_client(&dir).await;
    let repo = WorkflowRepo::new(&client);

    repo.create_plan("p1", None, "{}", 1_000).await.unwrap();
    // completed but no duration recorded (legacy/pre-014 shape)
    repo.checkpoint_task(TaskCheckpoint {
        plan_id: "p1",
        task_id: "legacy",
        agent: "Explore",
        status: "completed",
        response: Some("ok"),
        error: None,
        now: 1_050,
        model: None,
        duration_ms: None,
        cost_usd: None,
    })
    .await
    .unwrap();
    // duration recorded but failed — never forecast history
    repo.checkpoint_task(TaskCheckpoint {
        plan_id: "p1",
        task_id: "failed",
        agent: "Explore",
        status: "failed",
        response: None,
        error: Some("boom"),
        now: 1_060,
        model: None,
        duration_ms: Some(9_999),
        cost_usd: None,
    })
    .await
    .unwrap();

    let history = repo.duration_history().await.unwrap();
    assert!(
        history.is_empty(),
        "only completed+recorded rows are history (got {history:?})"
    );
}

#[tokio::test]
async fn recent_plans_windows_by_updated_at() {
    let dir = tempdir().unwrap();
    let client = open_client(&dir).await;
    let repo = WorkflowRepo::new(&client);

    repo.create_plan("old", None, "{}", 1_000).await.unwrap();
    repo.create_plan("new", None, "{}", 5_000).await.unwrap();

    let recent = repo.recent_plans(2_000).await.unwrap();
    let ids: Vec<&str> = recent.iter().map(|p| p.plan_id.as_str()).collect();
    assert_eq!(ids, vec!["new"], "cutoff excludes older plans");
}
