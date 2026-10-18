//! `notes_meta.embedded_hash` lifecycle: `index_note` marks it dirty, only
//! `mark_embedded` makes it fresh, and `all_embedded_hashes` reports every row
//! so the reindex gate can see both fresh and dirty vectors.

use tempfile::tempdir;
use zen_repo::{IndexNoteRequest, NotesRepo, SqliteClient};

async fn make_client() -> (SqliteClient, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let db = dir.path().join("embedded.db");
    let client = SqliteClient::open(&db).await.unwrap();
    (client, dir)
}

async fn index(client: &SqliteClient, id: &str, content: &str) {
    NotesRepo::new(client)
        .index_note(IndexNoteRequest {
            id,
            title: "t",
            content,
            tags: "",
            file_path: &format!("/nodisk/{id}.md"),
            source: "test",
        })
        .await
        .unwrap();
}

async fn stored_embedded(client: &SqliteClient, id: &str) -> String {
    sqlx::query_scalar("SELECT embedded_hash FROM notes_meta WHERE id = ?1")
        .bind(id)
        .fetch_one(client.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn mark_embedded_and_all_embedded_hashes_roundtrip() {
    let (client, _dir) = make_client().await;
    let notes = NotesRepo::new(&client);
    index(&client, "a", "alpha").await;
    index(&client, "b", "beta").await;

    notes.mark_embedded("a", "hash-a").await.unwrap();

    let map = notes.all_embedded_hashes().await.unwrap();
    assert_eq!(map.get("a").map(String::as_str), Some("hash-a"));
    assert_eq!(
        map.get("b").map(String::as_str),
        Some(""),
        "unmarked rows must be reported as dirty, not omitted"
    );
    assert_eq!(map.len(), 2);
    assert_eq!(stored_embedded(&client, "a").await, "hash-a");
}

#[tokio::test]
async fn mark_embedded_on_an_unknown_id_is_not_an_error() {
    let (client, _dir) = make_client().await;
    NotesRepo::new(&client)
        .mark_embedded("does-not-exist", "hash")
        .await
        .unwrap();
    assert!(
        NotesRepo::new(&client)
            .all_embedded_hashes()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn index_note_resets_a_previously_marked_embedding_to_dirty() {
    let (client, _dir) = make_client().await;
    let notes = NotesRepo::new(&client);

    index(&client, "a", "alpha v1").await;
    notes.mark_embedded("a", "hash-v1").await.unwrap();
    assert_eq!(stored_embedded(&client, "a").await, "hash-v1");

    index(&client, "a", "alpha v2").await;
    assert_eq!(
        stored_embedded(&client, "a").await,
        "",
        "re-indexing must reset embedded_hash to '' — the old vector is stale"
    );
}
