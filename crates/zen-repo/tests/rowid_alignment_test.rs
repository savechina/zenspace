//! Verify INSERT OR REPLACE preserves FTS5 rowid alignment.
//!
//! NotesRepo::index_note() derives ONE deterministic rowid from the note id and
//! writes it explicitly to BOTH notes_meta and notes_fts, so the search JOIN
//! (`notes_fts.rowid = notes_meta.rowid`) keeps resolving after a re-index and
//! the rowid no longer churns on every write (2026-10-17 data-bloat fix).

use tempfile::tempdir;
use zen_repo::{IndexNoteRequest, NotesRepo, SqliteClient};

#[tokio::test]
async fn insert_or_replace_preserves_fts_rowid_alignment() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("rowid.db");
    let client = SqliteClient::open(&db).await.unwrap();
    let notes = NotesRepo::new(&client);

    notes
        .index_note(IndexNoteRequest {
            id: "dup",
            title: "First",
            content: "alpha content",
            tags: "",
            file_path: "/first.md",
            source: "test",
        })
        .await
        .unwrap();

    let hits_first = notes.search("alpha", 10).await.unwrap();
    assert_eq!(hits_first.len(), 1, "first insert must be searchable");
    assert_eq!(hits_first[0].path, "/first.md");

    let meta_row_after_first: (i64,) =
        sqlx::query_as("SELECT rowid FROM notes_meta WHERE id = 'dup'")
            .fetch_one(client.pool())
            .await
            .unwrap();
    let fts_count_after_first: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM notes_fts WHERE rowid = ?")
            .bind(meta_row_after_first.0)
            .fetch_one(client.pool())
            .await
            .unwrap();
    assert_eq!(
        fts_count_after_first, 1,
        "exactly one notes_fts row must exist at the notes_meta rowid after first index_note"
    );

    notes
        .index_note(IndexNoteRequest {
            id: "dup",
            title: "Second",
            content: "alpha content",
            tags: "",
            file_path: "/second.md",
            source: "test",
        })
        .await
        .unwrap();

    let meta_row_after_second: (i64,) =
        sqlx::query_as("SELECT rowid FROM notes_meta WHERE id = 'dup'")
            .fetch_one(client.pool())
            .await
            .unwrap();

    let fts_count_after_second: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM notes_fts WHERE rowid = ?")
            .bind(meta_row_after_second.0)
            .fetch_one(client.pool())
            .await
            .unwrap();
    assert_eq!(
        fts_count_after_second, 1,
        "INSERT OR REPLACE on notes_meta + app-side FTS re-insert must leave one notes_fts row at the new meta rowid"
    );

    assert_eq!(
        meta_row_after_first.0, meta_row_after_second.0,
        "the rowid is derived deterministically from the note id and must be stable across re-index calls"
    );

    let hits_second = notes.search("alpha", 10).await.unwrap();
    assert_eq!(
        hits_second.len(),
        1,
        "search must still resolve after REPLACE"
    );
    assert_eq!(
        hits_second[0].path, "/second.md",
        "search must return the replaced file_path"
    );
}
