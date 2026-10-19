//! Principle XIII #6: migration invariants for 013_embedding_engine_fingerprint.sql.
//!
//! PURPOSE: prove the single-row fingerprint table is created by the migrator,
//! that `set`/`engine_fingerprint` round-trip and upsert-overwrite, and that
//! `reset_embedding_tables` deletes the fingerprint in the same transaction
//! as the vec0 reset.

use tempfile::tempdir;
use zen_repo::{EmbeddingsRepo, SqliteClient};

#[tokio::test]
async fn migration_013_is_recorded_and_applied_by_the_migrator() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("migrated.db");
    let client = SqliteClient::open(&db).await.unwrap();

    let applied: Option<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE version = 13")
            .fetch_optional(client.pool())
            .await
            .unwrap();
    assert_eq!(applied, Some(13), "_sqlx_migrations must record 013");

    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('embedding_engine')")
            .fetch_all(client.pool())
            .await
            .unwrap();
    for expected in ["id", "backend", "model", "dim", "updated_at"] {
        assert!(
            columns.iter().any(|c| c == expected),
            "the migrated schema must expose {expected}, got {columns:?}"
        );
    }
}

#[tokio::test]
async fn fingerprint_starts_none_roundtrips_and_upserts_over_the_single_row() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("fingerprint.db");
    let client = SqliteClient::open(&db).await.unwrap();
    let repo = EmbeddingsRepo::new(&client);

    assert_eq!(
        repo.engine_fingerprint().await.unwrap(),
        None,
        "a fresh database must have no fingerprint"
    );

    repo.set_engine_fingerprint("fastembed", "bge-small", 384)
        .await
        .unwrap();
    assert_eq!(
        repo.engine_fingerprint().await.unwrap(),
        Some(("fastembed".to_string(), "bge-small".to_string(), 384)),
        "set must round-trip (backend, model, dim)"
    );

    repo.set_engine_fingerprint("ollama", "qwen3-embedding", 4096)
        .await
        .unwrap();
    assert_eq!(
        repo.engine_fingerprint().await.unwrap(),
        Some(("ollama".to_string(), "qwen3-embedding".to_string(), 4096)),
        "a second set must overwrite — never a second row"
    );

    let row_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM embedding_engine")
        .fetch_one(client.pool())
        .await
        .unwrap();
    assert_eq!(
        row_count, 1,
        "the CHECK (id = 1) table must stay single-row"
    );
}

#[tokio::test]
async fn reset_embedding_tables_clears_the_fingerprint() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("reset-fingerprint.db");
    let client = SqliteClient::open(&db).await.unwrap();
    let repo = EmbeddingsRepo::new(&client);

    repo.set_engine_fingerprint("fastembed", "bge-small", 384)
        .await
        .unwrap();
    assert!(repo.engine_fingerprint().await.unwrap().is_some());

    repo.reset_embedding_tables().await.unwrap();
    assert_eq!(
        repo.engine_fingerprint().await.unwrap(),
        None,
        "the fingerprint describes vectors the reset destroyed — it must die with them"
    );
}
