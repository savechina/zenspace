//! TODO-004: Verify INSERT OR REPLACE preserves FTS5 rowid alignment.
//!
//! NotesRepo::index_note() does INSERT OR REPLACE on notes_meta (TEXT PK),
//! then INSERT OR REPLACE INTO notes_fts using last_insert_rowid() to bind
//! the FTS row to the new notes_meta rowid. This test documents what
//! actually happens to the alignment under INSERT OR REPLACE.

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

    assert_ne!(
        meta_row_after_first.0, meta_row_after_second.0,
        "INSERT OR REPLACE must allocate a fresh rowid (DELETE+INSERT semantics)"
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
