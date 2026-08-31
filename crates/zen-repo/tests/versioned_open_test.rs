use tempfile::tempdir;
use zen_repo::{SqliteClient, probe_latest_version};

/// Fresh directory → creates `{domain}_1.sqlite` with all migrations applied.
#[tokio::test]
async fn fresh_dir_creates_state_1() {
    let dir = tempdir().unwrap();
    let logs = dir.path();

    let client = SqliteClient::open_versioned(logs, "state").await.unwrap();

    let path = logs.join("state_1.sqlite");
    assert!(path.exists(), "state_1.sqlite must be created");

    let tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='notions'",
    )
    .fetch_one(client.pool())
    .await
    .unwrap();
    assert_eq!(tables, 1, "migrations must have created the notions table");
}

/// When state_1 and state_5 both exist → picks state_5.
#[tokio::test]
async fn picks_latest_versioned_file() {
    let dir = tempdir().unwrap();
    let logs = dir.path();

    // Create both files — state_1 first so it exists, then state_5.
    let _c1 = SqliteClient::open(&logs.join("state_1.sqlite"))
        .await
        .unwrap();
    let _c5 = SqliteClient::open(&logs.join("state_5.sqlite"))
        .await
        .unwrap();

    let client = SqliteClient::open_versioned(logs, "state").await.unwrap();

    let tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='notions'",
    )
    .fetch_one(client.pool())
    .await
    .unwrap();
    assert_eq!(
        tables, 1,
        "state_5 must be the selected file with migrations applied"
    );
}

/// Only state_3 exists → picks state_3 and opens+migrates cleanly.
#[tokio::test]
async fn opens_existing_versioned_file() {
    let dir = tempdir().unwrap();
    let logs = dir.path();

    // Create only state_3.
    let _c3 = SqliteClient::open(&logs.join("state_3.sqlite"))
        .await
        .unwrap();

    let client = SqliteClient::open_versioned(logs, "state").await.unwrap();

    let tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='notions'",
    )
    .fetch_one(client.pool())
    .await
    .unwrap();
    assert_eq!(tables, 1, "state_3 must open and migrate cleanly");
}

/// probe_latest_version returns correct N ordering and None for empty dirs.
#[test]
fn probe_returns_correct_ordering() {
    let dir = tempdir().unwrap();
    let logs = dir.path();

    // Empty dir → None.
    assert_eq!(probe_latest_version(logs, "state"), None);

    // Create files with various versions.
    std::fs::write(logs.join("state_2.sqlite"), b"").unwrap();
    assert_eq!(probe_latest_version(logs, "state"), Some(2));

    std::fs::write(logs.join("state_7.sqlite"), b"").unwrap();
    assert_eq!(probe_latest_version(logs, "state"), Some(7));

    std::fs::write(logs.join("state_1.sqlite"), b"").unwrap();
    assert_eq!(probe_latest_version(logs, "state"), Some(7));

    // Different domain is unaffected.
    assert_eq!(probe_latest_version(logs, "logs"), None);

    // Non-numeric suffixes are ignored.
    std::fs::write(logs.join("state_abc.sqlite"), b"").unwrap();
    assert_eq!(probe_latest_version(logs, "state"), Some(7));

    // Wrong extension is ignored.
    std::fs::write(logs.join("state_99.db"), b"").unwrap();
    assert_eq!(probe_latest_version(logs, "state"), Some(7));
}
