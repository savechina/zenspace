//! Principle XIII #6: migration invariants for 011_contentless_delete.sql.
//!
//! PURPOSE: The empirical proof that the BUNDLED SQLite in this workspace
//! accepts `contentless_delete=1`, that the recreated index is deletable by
//! rowid (the capability the option buys), and that the migration's clearing
//! UPDATE forces a full reindex rather than leaving a permanently empty index.

use tempfile::tempdir;
use zen_repo::{IndexNoteRequest, NotesRepo, SqliteClient};

async fn raw_exec(client: &SqliteClient, sql: &str) {
    let sql = sql.to_string();
    client
        .writer()
        .call(move |conn| {
            conn.execute_batch(&sql)?;
            Ok(())
        })
        .await
        .unwrap();
}

async fn make_client() -> (SqliteClient, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let db = dir.path().join("m011.db");
    let client = SqliteClient::open(&db).await.unwrap();
    (client, dir)
}

#[tokio::test]
async fn migration_011_enables_contentless_delete_and_records_the_version() {
    let (client, _dir) = make_client().await;

    let sql: String =
        sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE type='table' AND name='notes_fts'")
            .fetch_one(client.pool())
            .await
            .unwrap();
    assert!(
        sql.contains("contentless_delete=1"),
        "notes_fts must be recreated with contentless_delete=1; got {sql}"
    );
    assert!(
        sql.contains("content=''"),
        "notes_fts must stay contentless (external-content migration was rejected); got {sql}"
    );

    let applied: Option<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE version = 11")
            .fetch_optional(client.pool())
            .await
            .unwrap();
    assert_eq!(
        applied,
        Some(11),
        "_sqlx_migrations must record migration 011"
    );

    let notes = NotesRepo::new(&client);
    notes
        .index_note(IndexNoteRequest {
            id: "del",
            title: "t",
            content: "deletablebody",
            tags: "",
            file_path: "/nodisk/del.md",
            source: "test",
        })
        .await
        .unwrap();

    let rowid: i64 = sqlx::query_scalar("SELECT rowid FROM notes_meta WHERE id = 'del'")
        .fetch_one(client.pool())
        .await
        .unwrap();
    raw_exec(
        &client,
        &format!("DELETE FROM notes_fts WHERE rowid = {rowid};"),
    )
    .await;

    let hits = notes.search("deletablebody", 10).await.unwrap();
    assert!(
        hits.is_empty(),
        "DELETE by rowid must be supported on the recreated index"
    );
}

#[tokio::test]
async fn migration_011_clears_legacy_hashes_and_empties_the_old_index() {
    let (client, _dir) = make_client().await;

    let legacy_fixture = "DROP TABLE IF EXISTS notes_fts; \
         DROP TRIGGER IF EXISTS notes_fts_delete; \
         CREATE VIRTUAL TABLE notes_fts USING fts5(id, title, content, tags, content='', tokenize='porter'); \
         CREATE TRIGGER notes_fts_delete AFTER DELETE ON notes_meta BEGIN DELETE FROM notes_fts WHERE id = old.id; END; \
         INSERT INTO notes_meta (id, file_path, source, content_hash) VALUES ('legacy', '/vault/legacy.md', 'test', 'deadbeef'); \
         INSERT INTO notes_fts (rowid, id, title, content, tags) VALUES (1, 'legacy', 't', 'legacyalphaunique', '');";
    raw_exec(&client, legacy_fixture).await;

    let before: String =
        sqlx::query_scalar("SELECT content_hash FROM notes_meta WHERE id = 'legacy'")
            .fetch_one(client.pool())
            .await
            .unwrap();
    assert_eq!(before, "deadbeef", "legacy fixture must start with a hash");

    raw_exec(
        &client,
        include_str!("../migrations/011_contentless_delete.sql"),
    )
    .await;

    let after: String =
        sqlx::query_scalar("SELECT content_hash FROM notes_meta WHERE id = 'legacy'")
            .fetch_one(client.pool())
            .await
            .unwrap();
    assert_eq!(
        after, "",
        "the migration must clear hashes so the reindex gate cannot skip every file"
    );

    let fts_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notes_fts")
        .fetch_one(client.pool())
        .await
        .unwrap();
    assert_eq!(fts_rows, 0, "the recreated index starts empty");

    let notes = NotesRepo::new(&client);
    notes
        .index_note(IndexNoteRequest {
            id: "legacy",
            title: "t",
            content: "refilledbody",
            tags: "",
            file_path: "/nodisk/legacy.md",
            source: "test",
        })
        .await
        .unwrap();

    let refilled = notes.search("refilledbody", 10).await.unwrap();
    assert_eq!(refilled.len(), 1, "reindex must repopulate the new index");
}
