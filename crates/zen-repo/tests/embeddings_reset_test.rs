//! vec0 embedding reset: drop + recreate leaves a functional, empty index.

use tempfile::tempdir;
use zen_repo::{
    EmbeddingsRepo, IndexNoteRequest, InsertNoteEmbeddingRequest, NotesRepo, SqliteClient,
};

async fn rowid_count(client: &SqliteClient) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM note_embeddings_rowids")
        .fetch_one(client.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn reset_embedding_tables_empties_and_recreates_the_index() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("reset.db");
    let client = SqliteClient::open(&db).await.unwrap();
    let repo = EmbeddingsRepo::new(&client);

    let embedding = vec![0.1_f32; 4096];
    repo.insert_note_embedding(InsertNoteEmbeddingRequest {
        note_id: "n1",
        embedding: &embedding,
    })
    .await
    .unwrap();
    assert_eq!(rowid_count(&client).await, 1, "seed row must be indexed");

    repo.reset_embedding_tables().await.unwrap();
    assert_eq!(rowid_count(&client).await, 0, "reset must empty the index");

    repo.insert_note_embedding(InsertNoteEmbeddingRequest {
        note_id: "n2",
        embedding: &embedding,
    })
    .await
    .unwrap();
    assert_eq!(
        rowid_count(&client).await,
        1,
        "recreated table must accept new embeddings"
    );
}

#[tokio::test]
async fn reset_clears_embedded_hash_so_freshness_cannot_outlive_the_emptied_index() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("reset-hash.db");
    let client = SqliteClient::open(&db).await.unwrap();
    let notes = NotesRepo::new(&client);

    notes
        .index_note(IndexNoteRequest {
            id: "n1",
            title: "t",
            content: "body",
            tags: "",
            file_path: "/nodisk/n1.md",
            source: "test",
        })
        .await
        .unwrap();
    notes.mark_embedded("n1", "content-hash").await.unwrap();

    let before: String = sqlx::query_scalar("SELECT embedded_hash FROM notes_meta WHERE id = 'n1'")
        .fetch_one(client.pool())
        .await
        .unwrap();
    assert_eq!(before, "content-hash");

    EmbeddingsRepo::new(&client)
        .reset_embedding_tables()
        .await
        .unwrap();

    let after: String = sqlx::query_scalar("SELECT embedded_hash FROM notes_meta WHERE id = 'n1'")
        .fetch_one(client.pool())
        .await
        .unwrap();
    assert_eq!(
        after, "",
        "an empty vec0 table must never coexist with a non-empty embedded_hash"
    );
    assert_eq!(rowid_count(&client).await, 0);
}
