//! Spec D16 (Runtime Compatibility Guard — Design): `run_migrations`
//! tolerates an applied-ahead schema (old binary vs newer DB) while
//! checksum divergence and `Dirty` states still hard-fail.
//!
//! PURPOSE: Pin the three D16 behaviors at the `SqliteClient::open` choke
//! point — (1) the old-binary-vs-newer-DB direction now succeeds and
//! `detect_schema_skew` reports the skew; (2) a mutated applied migration
//! (VersionMismatch) is NOT weakened by tolerance; (3) a fresh DB produces
//! no skew signal and `embedded_migration_max()` equals the real latest
//! migration version (derived, not hardcoded).
//!
//! Harness: the prior-dir pattern from `migration_005_memvid_replay_offsets.rs`
//! (`sqlx::migrate::Migrator::new(dir)` against a real DB, then the production
//! `SqliteClient::open` path).

use std::path::Path;

use sqlx::migrate::Migrator;
use sqlx::sqlite::SqlitePoolOptions;
use tempfile::tempdir;
use zen_repo::{SqliteClient, detect_schema_skew, embedded_migration_max};

/// Copy every real `*.sql` migration from the crate into `dest`.
fn copy_real_migrations(crate_root: &Path, dest: &Path) {
    let manifest = crate_root.join("migrations");
    std::fs::create_dir(dest).unwrap();
    for entry in std::fs::read_dir(&manifest).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if name.to_string_lossy().ends_with(".sql") {
            std::fs::copy(entry.path(), dest.join(&name)).unwrap();
        }
    }
}

/// Highest `NNN_` version prefix among the real migration files.
fn max_version_from_dir(dir: &Path) -> i64 {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let stem = name.strip_suffix(".sql")?;
            let digits: String = stem.chars().take_while(char::is_ascii_digit).collect();
            digits.parse::<i64>().ok()
        })
        .max()
        .expect("migrations dir must contain at least one NNN_*.sql file")
}

/// D16 tolerance: embedded set (001..011) vs DB applied ahead at 012.
///
/// Old code (`sqlx::migrate!()` without `ignore_missing`) fails here with
/// `VersionMissing(12)`: "migration 12 was previously applied but is missing
/// in the resolved migrations". The tolerant runtime migrator succeeds, and
/// the read-only probe surfaces `(db_max, embedded_max)`.
#[tokio::test]
async fn tolerates_newer_schema_and_reports_skew() {
    let dir = tempdir().unwrap();

    // Register sqlite-vec process-wide before the fixture migrator applies
    // 002_vec (which creates the vec0 virtual table).
    let warm = dir.path().join("warm.db");
    let _warm_client = SqliteClient::open(&warm).await.unwrap();

    // Tempdir A: all real migrations plus a fabricated future migration.
    let probe_version = embedded_migration_max() + 1;
    let newer_dir = dir.path().join("newer_migrations");
    copy_real_migrations(Path::new(env!("CARGO_MANIFEST_DIR")), &newer_dir);
    std::fs::write(
        newer_dir.join(format!("{probe_version:03}_future_probe.sql")),
        "CREATE TABLE future_probe(x);\n",
    )
    .unwrap();

    let db = dir.path().join("newer.db");
    let url = format!("sqlite://{}?mode=rwc", db.display());
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let newer = Migrator::new(newer_dir).await.unwrap();
    newer.run(&pool).await.unwrap();

    let applied_max: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        applied_max, probe_version,
        "fixture DB must be applied ahead at {probe_version}"
    );
    pool.close().await;

    // The old-binary direction: open through the production path.
    let client = SqliteClient::open(&db)
        .await
        .expect("tolerant open must succeed on an applied-ahead schema");

    assert_eq!(
        detect_schema_skew(client.pool()).await,
        Some((probe_version, embedded_migration_max())),
        "probe must report db_max > embedded_max"
    );
}

/// D16 still-armed: a mutated applied migration (same version, different
/// checksum) must hard-fail — tolerance must not weaken checksum validation.
#[tokio::test]
async fn checksum_divergence_still_hard_fails() {
    let dir = tempdir().unwrap();

    // Tempdir B: only 001_initial.sql, content mutated after apply.
    let mutated_dir = dir.path().join("mutated_migrations");
    std::fs::create_dir(&mutated_dir).unwrap();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations/001_initial.sql");
    let mut content = std::fs::read_to_string(&src).unwrap();
    content.push_str("\n-- mutated after apply\n");
    std::fs::write(mutated_dir.join("001_initial.sql"), content).unwrap();

    let db = dir.path().join("mismatch.db");
    let url = format!("sqlite://{}?mode=rwc", db.display());
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let prior = Migrator::new(mutated_dir).await.unwrap();
    prior.run(&pool).await.unwrap();
    pool.close().await;

    let result = SqliteClient::open(&db).await;
    assert!(
        result.is_err(),
        "checksum divergence (VersionMismatch) must still hard-fail under tolerance"
    );
}

/// D16 fresh-DB: no applied-ahead signal, and the embedded max equals the
/// real latest migration version.
#[tokio::test]
async fn fresh_db_has_no_skew_signal() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("fresh.db");
    let client = SqliteClient::open(&db).await.unwrap();

    assert_eq!(
        detect_schema_skew(client.pool()).await,
        None,
        "fresh DB (db_max == embedded_max) must report no skew"
    );

    let expected = max_version_from_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"));
    assert_eq!(
        embedded_migration_max(),
        expected,
        "embedded_migration_max must equal the real highest migration version"
    );
}
