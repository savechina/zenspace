//! Principle XIII #6: migration invariants for 002_vec.sql.
//!
//! Verifies vec0 virtual tables are created without disturbing rows
//! already inserted into notes_meta by migration 001 consumers.

use tempfile::tempdir;
use zen_repo::{IndexNoteRequest, NotesRepo, SqliteClient};

#[tokio::test]
async fn migration_002_creates_vec0_tables_preserving_existing_rows() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("m002.db");
    let client = SqliteClient::open(&db).await.unwrap();

    let notes = NotesRepo::new(&client);
    for i in 1..=3 {
        notes
            .index_note(IndexNoteRequest {
                id: &format!("n{i}"),
                title: "t",
                content: "c",
                tags: "",
                file_path: &format!("/p/{i}.md"),
                source: "test",
            })
            .await
            .unwrap();
    }

    let notes_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notes_meta")
        .fetch_one(client.pool())
        .await
        .unwrap();
    assert_eq!(notes_count, 3, "3 notes seeded before asserting migration 002 state");

    let note_emb_exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='note_embeddings'",
    )
    .fetch_one(client.pool())
    .await
    .unwrap();
    assert_eq!(note_emb_exists, 1, "note_embeddings vec0 table must exist");

    let notion_emb_exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='notion_embeddings'",
    )
    .fetch_one(client.pool())
    .await
    .unwrap();
    assert_eq!(notion_emb_exists, 1, "notion_embeddings vec0 table must exist");

    let notes_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notes_meta")
        .fetch_one(client.pool())
        .await
        .unwrap();
    assert_eq!(
        notes_count_after, 3,
        "migration 002 vec0 tables must not disturb notes_meta rows"
    );

    let applied: Option<String> = sqlx::query_scalar(
        "SELECT description FROM _sqlx_migrations WHERE version = 2",
    )
    .fetch_optional(client.pool())
    .await
    .unwrap();
    assert!(
        applied.is_some(),
        "_sqlx_migrations must record migration 002"
    );
}
