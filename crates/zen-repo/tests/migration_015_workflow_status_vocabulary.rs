//! Principle XIII #6: migration invariants for 015_workflow_status_vocabulary.sql.
//!
//! PURPOSE: prove the W11.14/T15 backfill converts legacy `status='ok'`
//! workflow_tasks rows to `'completed'` (the vocabulary the resume path
//! replays), leaves every other status untouched, and is idempotent — so a
//! pre-upgrade plan resumes WITHOUT re-running its completed tasks
//! (SDD review OV-2: code-vs-existing-data, not just code-vs-spec).

use tempfile::tempdir;
use zen_repo::{SqliteClient, WorkflowRepo};

async fn raw_exec(client: &SqliteClient, sql: &str) {
    let sql = sql.to_string();
    client
        .writer()
        .call(move |conn| {
            conn.execute_batch(&sql)?;
            Ok(())
        })
        .await
        .unwrap();
}

async fn make_client() -> (SqliteClient, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let db = dir.path().join("m015.db");
    let client = SqliteClient::open(&db).await.unwrap();
    (client, dir)
}

/// Insert a legacy-shaped plan: two 'ok' tasks (pre-upgrade vocabulary),
/// one 'failed' and one 'pending' control.
async fn legacy_fixture(client: &SqliteClient) {
    raw_exec(
        client,
        "INSERT INTO workflow_plans (plan_id, name, status, spec_json, created_at, updated_at) \
         VALUES ('legacy', NULL, 'running', '{}', 1000, 1000); \
         INSERT INTO workflow_tasks (plan_id, task_id, agent, status, updated_at) VALUES \
         ('legacy', 't1', 'sisyphus', 'ok', 1000), \
         ('legacy', 't2', 'sisyphus', 'ok', 1000), \
         ('legacy', 't3', 'sisyphus', 'failed', 1000), \
         ('legacy', 't4', 'sisyphus', 'pending', 1000);",
    )
    .await;
}

#[tokio::test]
async fn migration_015_is_recorded_and_applied_by_the_migrator() {
    let (client, _dir) = make_client().await;
    let applied: Option<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE version = 15")
            .fetch_optional(client.pool())
            .await
            .unwrap();
    assert_eq!(applied, Some(15), "_sqlx_migrations must record 015");
}

#[tokio::test]
async fn migration_015_backfills_legacy_ok_rows_and_spares_other_statuses() {
    let (client, _dir) = make_client().await;
    legacy_fixture(&client).await;

    raw_exec(
        &client,
        include_str!("../migrations/015_workflow_status_vocabulary.sql"),
    )
    .await;

    let repo = WorkflowRepo::new(&client);
    let rows = repo.load_task_rows("legacy").await.unwrap();
    let status = |id: &str| {
        rows.iter()
            .find(|r| r.task_id == id)
            .map(|r| r.status.clone())
            .unwrap_or_default()
    };
    assert_eq!(status("t1"), "completed", "legacy 'ok' must backfill");
    assert_eq!(status("t2"), "completed", "legacy 'ok' must backfill");
    assert_eq!(status("t3"), "failed", "other statuses must be untouched");
    assert_eq!(status("t4"), "pending", "other statuses must be untouched");

    // Idempotence: a second application is a no-op (no 'ok' rows remain).
    raw_exec(
        &client,
        include_str!("../migrations/015_workflow_status_vocabulary.sql"),
    )
    .await;
    let ok_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM workflow_tasks WHERE status = 'ok'")
            .fetch_one(client.pool())
            .await
            .unwrap();
    assert_eq!(ok_count, 0);
}

#[tokio::test]
async fn legacy_ok_plan_resumes_without_rerunning_completed_tasks() {
    let (client, _dir) = make_client().await;
    legacy_fixture(&client).await;
    raw_exec(
        &client,
        include_str!("../migrations/015_workflow_status_vocabulary.sql"),
    )
    .await;

    // The resume replay predicate (plan_task.rs): a task replays only when
    // status == "completed". After the backfill BOTH legacy tasks satisfy
    // it, so the resume re-runs neither (the OV-2 double-spend regression).
    let repo = WorkflowRepo::new(&client);
    let rows = repo.load_task_rows("legacy").await.unwrap();
    let replayable = rows.iter().filter(|r| r.status == "completed").count();
    let rerun = rows.iter().filter(|r| r.status != "completed").count();
    assert_eq!(replayable, 2, "both legacy tasks must replay as completed");
    assert_eq!(rerun, 2, "only the failed + pending controls may re-run");
}
