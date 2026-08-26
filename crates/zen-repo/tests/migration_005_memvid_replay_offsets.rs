//! Principle XIII #6: migration invariants for 005_memvid_replay_offsets.sql.
//!
//! PURPOSE: Verifies the migration applies additively over a
//! prior-schema (001..004) database — preserving seeded rows,
//! creating the `memvid_replay_offsets` table with the locked shape
//! — and that the `MemvidReplayOffsetRepo` application queries work
//! post-migration, covering resume-after-partial (offset updated
//! then reloaded) and missing-row full replay (no row → None).

use std::path::Path;

use sqlx::migrate::Migrator;
use sqlx::sqlite::SqlitePoolOptions;
use tempfile::tempdir;
use zen_repo::{MemvidReplayOffsetRepo, SqliteClient};

/// Copies migrations 001..004 from the crate's `migrations/` into
/// `prior_dir`, excluding 005 so the seeded database starts at the
/// prior schema state.
fn stage_prior_migrations(crate_root: &Path, prior_dir: &Path) {
    let manifest = crate_root.join("migrations");
    std::fs::create_dir(prior_dir).unwrap();
    for name in [
        "001_initial.sql",
        "002_vec.sql",
        "003_entity_graph_enhancements.sql",
        "004_qq_bindings.sql",
    ] {
        std::fs::copy(manifest.join(name), prior_dir.join(name)).unwrap();
    }
}

/// Constitution XIII #6 (a) row preservation, (b) new schema
/// elements, (c) post-migration application queries — in one
/// prior-state → migrate → assert flow over the same database.
#[tokio::test]
async fn migration_005_applies_additively_over_prior_schema() {
    let dir = tempdir().unwrap();

    // Register the sqlite-vec extension process-wide (a throwaway
    // client run also proves migrations 001..005 apply cleanly on a
    // fresh database).
    let warm = dir.path().join("warm.db");
    let _warm_client = SqliteClient::open(&warm).await.unwrap();

    // Prior schema state: apply only 001..004 via sqlx's own
    // migrator so `_sqlx_migrations` bookkeeping matches what
    // `SqliteClient::open` later validates.
    let prior_dir = dir.path().join("prior_migrations");
    stage_prior_migrations(Path::new(env!("CARGO_MANIFEST_DIR")), &prior_dir);
    let db = dir.path().join("m005.db");
    let url = format!("sqlite://{}?mode=rwc", db.display());
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let prior = Migrator::new(prior_dir.clone()).await.unwrap();
    prior.run(&pool).await.unwrap();

    // Representative prior-schema rows.
    sqlx::query(
        "INSERT INTO qq_bindings (chat_id, session_id, created_at, updated_at) \
         VALUES ('group_X', 'sess-prior', 111, 111), \
                ('user_Y', 'sess-prior2', 222, 222)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO notions (id, name, kind, aliases, created_at, last_updated, domain) \
         VALUES ('e9', 'PriorEntity', 'concept', NULL, \
                 '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z', 'dev')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let pre_table: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='memvid_replay_offsets'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        pre_table, 0,
        "checkpoint table must not exist pre-migration"
    );

    // Apply the migration through the production path: open() runs
    // pending migrations — only 005 should remain.
    pool.close().await;
    let client = SqliteClient::open(&db).await.unwrap();

    // (a) Prior rows preserved — no data loss.
    let bindings: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM qq_bindings")
        .fetch_one(client.pool())
        .await
        .unwrap();
    assert_eq!(bindings, 2, "seeded qq_bindings rows must survive");
    let prior_session: String =
        sqlx::query_scalar("SELECT session_id FROM qq_bindings WHERE chat_id = 'group_X'")
            .fetch_one(client.pool())
            .await
            .unwrap();
    assert_eq!(prior_session, "sess-prior");
    let notions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notions")
        .fetch_one(client.pool())
        .await
        .unwrap();
    assert_eq!(notions, 1, "seeded notions row must survive");

    // (b) New schema elements exist.
    let table: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='memvid_replay_offsets'",
    )
    .fetch_one(client.pool())
    .await
    .unwrap();
    assert_eq!(table, 1, "memvid_replay_offsets must exist post-migration");
    let applied_005: (i64,) =
        sqlx::query_as("SELECT version FROM _sqlx_migrations WHERE version = 5")
            .fetch_one(client.pool())
            .await
            .unwrap();
    assert_eq!(
        applied_005.0, 5,
        "migration 005 must be recorded as applied"
    );

    let cols: Vec<(String, String, i64, i64)> = sqlx::query_as(
        "SELECT name, type, \"notnull\", pk FROM pragma_table_info('memvid_replay_offsets') \
         ORDER BY cid",
    )
    .fetch_all(client.pool())
    .await
    .unwrap();
    assert_eq!(
        cols,
        vec![
            ("session_path".into(), "TEXT".into(), 0, 1),
            ("applied_offset".into(), "INTEGER".into(), 1, 0),
        ],
        "column names/types must match the locked table shape (session_path TEXT PRIMARY KEY, applied_offset INTEGER NOT NULL)"
    );

    // (c) Post-migration application queries work: missing row →
    // full replay; checkpoint write → resume-after-partial.
    let repo = MemvidReplayOffsetRepo::new(&client);
    let missing = repo.load("/sessions/sess-prior.jsonl").await.unwrap();
    assert!(missing.is_none(), "no checkpoint row must mean full replay");

    repo.update("/sessions/sess-prior.jsonl", 4096)
        .await
        .unwrap();
    let resumed = repo.load("/sessions/sess-prior.jsonl").await.unwrap();
    assert_eq!(resumed, Some(4096), "updated offset must reload exactly");

    repo.update("/sessions/sess-prior.jsonl", 8192)
        .await
        .unwrap();
    let advanced = repo.load("/sessions/sess-prior.jsonl").await.unwrap();
    assert_eq!(
        advanced,
        Some(8192),
        "re-update must advance, not duplicate"
    );
}

/// Missing-row full replay on a fresh database (no checkpoint →
/// `None` → caller replays from byte 0).
#[tokio::test]
async fn missing_row_means_full_replay() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("m005_missing.db");
    let client = SqliteClient::open(&db).await.unwrap();
    let repo = MemvidReplayOffsetRepo::new(&client);

    let offset = repo.load("/sessions/never-replayed.jsonl").await.unwrap();
    assert!(offset.is_none(), "absent checkpoint must load as None");
}

/// Resume-after-partial on a fresh database: the offset row is
/// updated (inserted, then advanced) and each write reloads the
/// persisted value.
#[tokio::test]
async fn resume_after_partial_updates_then_reloads_offset() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("m005_resume.db");
    let client = SqliteClient::open(&db).await.unwrap();
    let repo = MemvidReplayOffsetRepo::new(&client);

    repo.update("/sessions/sess-A.jsonl", 1024).await.unwrap();
    assert_eq!(
        repo.load("/sessions/sess-A.jsonl").await.unwrap(),
        Some(1024),
        "first checkpoint insert must be readable back"
    );

    repo.update("/sessions/sess-A.jsonl", 2048).await.unwrap();
    assert_eq!(
        repo.load("/sessions/sess-A.jsonl").await.unwrap(),
        Some(2048),
        "second checkpoint write must overwrite in place"
    );

    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM memvid_replay_offsets")
        .fetch_one(client.pool())
        .await
        .unwrap();
    assert_eq!(rows, 1, "upsert must not duplicate checkpoint rows");

    let other = repo.load("/sessions/sess-B.jsonl").await.unwrap();
    assert!(other.is_none(), "unrelated session must be unaffected");
}
