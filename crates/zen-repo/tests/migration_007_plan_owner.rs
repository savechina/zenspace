//! Principle XIII #6: migration invariants for 007_plan_owner.sql.
//!
//! PURPOSE: Verifies the `owner` column was added to `workflow_plans` by
//! migration 007, and that `claim_plan` round-trips through the new column.

use tempfile::tempdir;
use zen_repo::{SqliteClient, WorkflowRepo};

#[tokio::test]
async fn owner_column_exists_and_claim_round_trips() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("m007.db");
    let client = SqliteClient::open(&db).await.unwrap();
    let repo = WorkflowRepo::new(&client);

    // Create a plan and claim it — proves the `owner` column is live.
    repo.create_plan("p1", Some("test"), "{}", 1000)
        .await
        .unwrap();
    assert!(
        repo.claim_plan("p1", "runner-a", 1001).await.unwrap(),
        "claim should succeed on fresh running plan"
    );

    // Introspect schema: PRAGMA table_info to verify `owner` column exists.
    let cols: Vec<String> = sqlx::query_scalar::<_, String>(
        "SELECT name FROM pragma_table_info('workflow_plans') ORDER BY cid",
    )
    .fetch_all(client.pool())
    .await
    .unwrap();
    assert!(
        cols.contains(&"owner".to_string()),
        "workflow_plans must have an `owner` column after migration 007; got: {cols:?}"
    );
}
