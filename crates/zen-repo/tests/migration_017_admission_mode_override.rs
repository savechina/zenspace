//! Principle XIII #6: migration invariants for 017_admission_mode_override.sql.
//!
//! PURPOSE: prove the single-row runtime override table is created by the
//! migrator and that the AdmissionRepo accessors round-trip, upsert-overwrite
//! within the single row, and clear back to absent.
//! USAGE: `cargo test -p zen-repo --test migration_017_admission_mode_override`.
//! EXPECTED: version 17 recorded in `_sqlx_migrations`; exactly one row max.

use tempfile::tempdir;
use zen_repo::{AdmissionRepo, SqliteClient};

#[tokio::test]
async fn migration_017_is_recorded_and_applied_by_the_migrator() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("migrated.db");
    let client = SqliteClient::open(&db).await.unwrap();

    let applied: Option<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE version = 17")
            .fetch_optional(client.pool())
            .await
            .unwrap();
    assert_eq!(applied, Some(17), "_sqlx_migrations must record 017");

    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('admission_mode_override')")
            .fetch_all(client.pool())
            .await
            .unwrap();
    for expected in ["id", "accept_mode", "updated_at"] {
        assert!(
            columns.iter().any(|c| c == expected),
            "the migrated schema must expose {expected}, got {columns:?}"
        );
    }
}

#[tokio::test]
async fn override_starts_none_roundtrips_upserts_and_clears_within_the_single_row() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("override.db");
    let client = SqliteClient::open(&db).await.unwrap();
    let repo = AdmissionRepo::new(&client);

    assert_eq!(
        repo.mode_override().await.unwrap(),
        None,
        "a fresh database must have no override"
    );

    repo.set_mode_override("auto").await.unwrap();
    assert_eq!(
        repo.mode_override().await.unwrap(),
        Some("auto".to_string()),
        "set must round-trip"
    );

    repo.set_mode_override("off").await.unwrap();
    assert_eq!(
        repo.mode_override().await.unwrap(),
        Some("off".to_string()),
        "a second set must overwrite — never a second row"
    );

    let row_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM admission_mode_override")
        .fetch_one(client.pool())
        .await
        .unwrap();
    assert_eq!(
        row_count, 1,
        "CHECK (id = 1) plus UPSERT keeps exactly one row"
    );

    assert!(
        repo.clear_mode_override().await.unwrap(),
        "clear must report a removed row"
    );
    assert_eq!(
        repo.mode_override().await.unwrap(),
        None,
        "clear falls back to absent — config takes charge again"
    );
    assert!(
        !repo.clear_mode_override().await.unwrap(),
        "a second clear reports nothing removed"
    );
}
