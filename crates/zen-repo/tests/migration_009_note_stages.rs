//! Principle XIII #6: migration invariants for 009_note_stages.sql, plus
//! behavior pins for the FR-011 durable per-note processing projection.
//!
//! PURPOSE: Verifies the projection table exists, is keyed by
//! (file_path, content_hash), upserts rather than duplicating, and treats the
//! same note with different content as a distinct identity — the property that
//! makes crash re-derivation safe rather than a source of skipped notes.

use tempfile::tempdir;
use zen_repo::{NotionsRepo, SqliteClient};

const HASH_A: &str = "aaaa";
const HASH_B: &str = "bbbb";
const NOW: &str = "2026-09-19T00:00:00Z";

async fn make_client() -> (SqliteClient, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let db = dir.path().join("m009.db");
    let client = SqliteClient::open(&db).await.unwrap();
    (client, dir)
}

#[tokio::test]
async fn test_note_stages_table_and_columns_exist() {
    let (client, _dir) = make_client().await;
    let columns: Vec<(String,)> =
        sqlx::query_as("SELECT name FROM pragma_table_info('note_stages') ORDER BY cid")
            .fetch_all(client.pool())
            .await
            .unwrap();
    let names: Vec<String> = columns.into_iter().map(|(name,)| name).collect();
    assert_eq!(
        names,
        vec![
            "file_path".to_string(),
            "content_hash".to_string(),
            "last_completed_stage".to_string(),
            "stage_updated_at".to_string(),
        ],
        "note_stages must carry the FR-011 projection columns"
    );
}

#[tokio::test]
async fn test_record_and_load_note_stage_roundtrip() {
    let (client, _dir) = make_client().await;
    let repo = NotionsRepo::new(&client);

    repo.record_note_stage("/vault/inbox/a.md", HASH_A, "archived", NOW)
        .await
        .unwrap();

    let rows = repo.load_note_stages().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].file_path, "/vault/inbox/a.md");
    assert_eq!(rows[0].content_hash, HASH_A);
    assert_eq!(rows[0].last_completed_stage, "archived");
}

#[tokio::test]
async fn test_record_note_stage_upserts_on_the_same_identity() {
    let (client, _dir) = make_client().await;
    let repo = NotionsRepo::new(&client);

    repo.record_note_stage("/vault/inbox/a.md", HASH_A, "normalized", NOW)
        .await
        .unwrap();
    repo.record_note_stage("/vault/inbox/a.md", HASH_A, "archived", NOW)
        .await
        .unwrap();

    let rows = repo.load_note_stages().await.unwrap();
    assert_eq!(rows.len(), 1, "same identity must update, not duplicate");
    assert_eq!(
        rows[0].last_completed_stage, "archived",
        "the later stage must win"
    );
}

#[tokio::test]
async fn test_changed_content_is_a_distinct_identity() {
    let (client, _dir) = make_client().await;
    let repo = NotionsRepo::new(&client);

    repo.record_note_stage("/vault/inbox/a.md", HASH_A, "archived", NOW)
        .await
        .unwrap();
    repo.record_note_stage("/vault/inbox/a.md", HASH_B, "archived", NOW)
        .await
        .unwrap();

    let rows = repo.load_note_stages().await.unwrap();
    assert_eq!(
        rows.len(),
        2,
        "the same path with new content is a different identity, so it is \
         processed again rather than skipped"
    );
}
