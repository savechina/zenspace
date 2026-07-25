//! Principle XIII #6: Every migration MUST ship with a test proving invariants.
//!
//! Verifies migration 001_initial.sql: 10 tables, 12 indexes, 1 trigger,
//! 1 FTS5 virtual table, and the sqlx_migrations audit entry.

use sqlx::Row;
use tempfile::tempdir;
use zen_repo::SqliteClient;

#[tokio::test]
async fn migration_001_creates_all_tables_indexes_and_trigger() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("m001.db");
    let client = SqliteClient::open(&db).await.unwrap();

    let tables: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .fetch_all(client.pool())
            .await
            .unwrap();

    for expected in [
        "sessions",
        "notes_meta",
        "notions",
        "relationships",
        "notion_aliases",
        "dispatch_tasks",
        "self_nodes",
        "goal_nodes",
        "path_nodes",
        "belief_nodes",
    ] {
        assert!(
            tables.iter().any(|t| t == expected),
            "migration 001 must create table '{expected}'; got {tables:?}"
        );
    }

    let indexes: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type='index' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .fetch_all(client.pool())
            .await
            .unwrap();
    for expected in [
        "idx_sessions_updated",
        "idx_notions_name",
        "idx_notions_type",
        "idx_rel_source",
        "idx_rel_target",
        "idx_rel_type",
        "idx_aliases_lookup",
        "idx_dispatch_status",
        "idx_self_nodes_layer",
        "idx_goal_nodes_name",
        "idx_path_nodes_name",
        "idx_belief_nodes_name",
    ] {
        assert!(
            indexes.iter().any(|t| t == expected),
            "migration 001 must create index '{expected}'; got {indexes:?}"
        );
    }

    let triggers: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type='trigger' ORDER BY name")
            .fetch_all(client.pool())
            .await
            .unwrap();
    assert!(
        triggers.iter().any(|t| t == "notes_fts_delete"),
        "migration 001 must create notes_fts_delete trigger; got {triggers:?}"
    );

    let fts_exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='notes_fts'",
    )
    .fetch_one(client.pool())
    .await
    .unwrap();
    assert_eq!(fts_exists, 1, "notes_fts virtual table must exist");

    let applied: i64 = sqlx::query("SELECT version FROM _sqlx_migrations WHERE version = 1")
        .fetch_optional(client.pool())
        .await
        .unwrap()
        .map(|r| r.get::<i64, _>("version"))
        .unwrap_or(0);
    assert_eq!(applied, 1, "_sqlx_migrations must record migration 001");
}
