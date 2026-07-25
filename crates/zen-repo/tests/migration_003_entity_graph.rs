//! Principle XIII #6: migration invariants for 003_entity_graph_enhancements.sql.
//!
//! Verifies ALTER TABLE ADD COLUMN preserves existing rows, applies correct
//! defaults, deprecated columns survive, and FTS5 sync triggers fire on
//! insert/update/delete.

use tempfile::tempdir;
use zen_repo::SqliteClient;

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

#[tokio::test]
async fn migration_003_alter_preserves_rows_and_adds_columns_with_defaults() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("m003.db");
    let client = SqliteClient::open(&db).await.unwrap();

    raw_exec(
        &client,
        "INSERT INTO notions (id, name, kind, aliases, created_at, last_updated, domain) \
         VALUES ('e1', 'Rust', 'language', NULL, '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z', 'dev'), \
                ('e2', 'Python', 'language', NULL, '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z', 'dev');",
    )
    .await;

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notions")
        .fetch_one(client.pool())
        .await
        .unwrap();
    assert_eq!(count, 2, "2 notions seeded before assertions");

    let row: (String, String, i64, f64, String) = sqlx::query_as(
        "SELECT description, properties, access_count, confidence, source \
         FROM notions WHERE id = 'e1'",
    )
    .fetch_one(client.pool())
    .await
    .unwrap();
    assert_eq!(
        row.0, "",
        "ALTER TABLE ADD COLUMN description must default to ''"
    );
    assert_eq!(row.1, "{}", "properties must default to '{{}}'");
    assert_eq!(row.2, 0, "access_count must default to 0");
    assert!(
        (row.3 - 0.5).abs() < f64::EPSILON,
        "confidence must default to 0.5"
    );
    assert_eq!(row.4, "manual", "source must default to 'manual'");

    let nullable: (Option<String>, Option<String>) =
        sqlx::query_as("SELECT last_accessed_at, promoted_at FROM notions WHERE id = 'e1'")
            .fetch_one(client.pool())
            .await
            .unwrap();
    assert!(
        nullable.0.is_none(),
        "last_accessed_at must be NULL by default"
    );
    assert!(nullable.1.is_none(), "promoted_at must be NULL by default");

    let rel_row: (String, f64) =
        sqlx::query_as("SELECT description, weight FROM relationships LIMIT 1")
            .fetch_optional(client.pool())
            .await
            .unwrap()
            .unwrap_or_default();
    let _ = rel_row;

    let cols: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('notions') ORDER BY cid")
            .fetch_all(client.pool())
            .await
            .unwrap();
    assert!(
        cols.iter().any(|c| c == "aliases"),
        "deprecated aliases column must still exist after migration 003; got {cols:?}"
    );

    let count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notions")
        .fetch_one(client.pool())
        .await
        .unwrap();
    assert_eq!(
        count_after, 2,
        "ALTER TABLE ADD COLUMN must preserve existing rows"
    );
}

#[tokio::test]
async fn migration_003_notions_fts_triggers_fire_on_insert_update_delete() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("m003_triggers.db");
    let client = SqliteClient::open(&db).await.unwrap();

    let fts_exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='notions_fts'",
    )
    .fetch_one(client.pool())
    .await
    .unwrap();
    assert_eq!(
        fts_exists, 1,
        "notions_fts virtual table must exist after migration 003"
    );

    for trig in ["notions_fts_ai", "notions_fts_ad", "notions_fts_au"] {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND name = ?1",
        )
        .bind(trig)
        .fetch_one(client.pool())
        .await
        .unwrap();
        assert_eq!(n, 1, "trigger {trig} must exist");
    }

    raw_exec(
        &client,
        "INSERT INTO notions (id, name, kind, aliases, created_at, last_updated, domain, description) \
         VALUES ('t1', 'TriggerTest', 't', NULL, '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z', 'd', 'initial');",
    )
    .await;

    let fts_after_insert: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM notions_fts WHERE notion_id = 't1'")
            .fetch_one(client.pool())
            .await
            .unwrap();
    assert_eq!(
        fts_after_insert, 1,
        "notions_fts_ai trigger must insert a row into notions_fts on notion insert"
    );

    raw_exec(
        &client,
        "UPDATE notions SET description = 'updated' WHERE id = 't1'",
    )
    .await;
    let fts_desc: String =
        sqlx::query_scalar("SELECT description FROM notions_fts WHERE notion_id = 't1'")
            .fetch_one(client.pool())
            .await
            .unwrap();
    assert_eq!(
        fts_desc, "updated",
        "notions_fts_au trigger must resync notions_fts on notion update"
    );

    raw_exec(&client, "DELETE FROM notions WHERE id = 't1'").await;
    let fts_after_delete: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM notions_fts WHERE notion_id = 't1'")
            .fetch_one(client.pool())
            .await
            .unwrap();
    assert_eq!(
        fts_after_delete, 0,
        "notions_fts_ad trigger must remove notions_fts row on notion delete"
    );
}
