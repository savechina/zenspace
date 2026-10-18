//! Principle XIII #6: migration invariants for 012_embedded_hash.sql.
//!
//! PURPOSE: prove the column is added as `TEXT NOT NULL DEFAULT ''`, that a
//! row inserted before the ALTER reads the empty default (i.e. "re-embed me"),
//! and that the sqlx migrator records and applies 012.

use rusqlite::Connection;
use tempfile::tempdir;
use zen_repo::SqliteClient;

const NOTES_META_001: &str = "
    CREATE TABLE notes_meta (
        id TEXT PRIMARY KEY,
        file_path TEXT NOT NULL,
        source TEXT NOT NULL,
        domain TEXT NOT NULL DEFAULT '',
        project TEXT NOT NULL DEFAULT '',
        created_at TEXT NOT NULL DEFAULT (datetime('now')),
        updated_at TEXT NOT NULL DEFAULT (datetime('now')),
        content_hash TEXT NOT NULL DEFAULT ''
    );";

#[test]
fn migration_012_adds_a_not_null_string_column_defaulting_to_empty() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("m012.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(NOTES_META_001).unwrap();
    conn.execute(
        "INSERT INTO notes_meta (id, file_path, source, content_hash) \
         VALUES ('legacy', '/vault/legacy.md', 'test', 'deadbeef')",
        [],
    )
    .unwrap();

    conn.execute_batch(include_str!("../migrations/012_embedded_hash.sql"))
        .unwrap();

    let mut stmt = conn.prepare("PRAGMA table_info(notes_meta)").unwrap();
    let columns: Vec<(String, i64, Option<String>)> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let embedded = columns
        .iter()
        .find(|(name, _, _)| name == "embedded_hash")
        .expect("migration 012 must add the embedded_hash column");
    assert_eq!(embedded.1, 1, "embedded_hash must be NOT NULL");
    assert_eq!(
        embedded.2.as_deref(),
        Some("''"),
        "embedded_hash must default to the empty string"
    );

    let value: String = conn
        .query_row(
            "SELECT embedded_hash FROM notes_meta WHERE id = 'legacy'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        value, "",
        "a row predating the ALTER must read '' — unknown vector provenance is dirty"
    );
}

#[tokio::test]
async fn migration_012_is_recorded_and_applied_by_the_migrator() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("migrated.db");
    let client = SqliteClient::open(&db).await.unwrap();

    let applied: Option<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE version = 12")
            .fetch_optional(client.pool())
            .await
            .unwrap();
    assert_eq!(applied, Some(12), "_sqlx_migrations must record 012");

    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('notes_meta')")
            .fetch_all(client.pool())
            .await
            .unwrap();
    assert!(
        columns.iter().any(|c| c == "embedded_hash"),
        "the migrated schema must expose embedded_hash, got {columns:?}"
    );
}
