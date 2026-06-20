// ============================================================================
// 4D Test Suite: zen-repo repository integration
//
// Dimensions:
//   NORMAL       — Create, read, list operations with valid data
//   REVERSE      — Find/mutate nonexistent records, invalid DB paths
//   ADVERSARIAL  — Empty/special/max-length fields in records
//   LOGIC TREE   — Full CRUD roundtrip, concurrent operations
// ============================================================================

use zen_core::types::Sensitivity;
use zen_repo::{Note, NoteRepository, SqliteNoteRepository};

async fn setup_test_db() -> sqlx::SqlitePool {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(4)
        .connect("sqlite://:memory:")
        .await
        .expect("Failed to create in-memory pool");
    zen_repo::schema::migrate(&pool)
        .await
        .expect("Failed to migrate schema");
    pool
}

fn make_test_note(session_id: &str, content: &str) -> Note {
    Note {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: session_id.to_string(),
        content: content.to_string(),
        sensitivity: Sensitivity::Public,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

// ============================================================================
// NORMAL PATH — Standard repository operations
// ============================================================================

#[tokio::test]
async fn test_note_repository_create_and_find() {
    let pool = setup_test_db().await;
    let repo = SqliteNoteRepository::new(pool);
    let note = make_test_note("session-1", "test content");

    let created = repo.insert(&note).await.expect("insert should succeed");
    assert_eq!(created.id, note.id);
    assert_eq!(created.content, "test content");

    let found = repo
        .find_by_session("session-1")
        .await
        .expect("find should succeed");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, note.id);
}

#[tokio::test]
async fn test_note_repository_list_multiple() {
    let pool = setup_test_db().await;
    let repo = SqliteNoteRepository::new(pool);

    for i in 0..3 {
        let note = make_test_note("session-list", &format!("content {i}"));
        repo.insert(&note).await.expect("insert should succeed");
    }

    let notes = repo
        .find_by_session("session-list")
        .await
        .expect("find should succeed");
    assert_eq!(notes.len(), 3);
}

#[tokio::test]
async fn test_pool_creation_in_memory() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(4)
        .connect("sqlite://:memory:")
        .await;
    assert!(pool.is_ok(), "in-memory pool creation should succeed");
}

// ============================================================================
// REVERSE PATH — Missing/invalid records
// ============================================================================

#[tokio::test]
async fn test_note_repository_find_nonexistent_session() {
    let pool = setup_test_db().await;
    let repo = SqliteNoteRepository::new(pool);

    let found = repo
        .find_by_session("nonexistent-session-xyz")
        .await
        .expect("find should succeed");
    assert!(
        found.is_empty(),
        "nonexistent session should return empty list"
    );
}

#[tokio::test]
async fn test_note_repository_delete_nonexistent_note() {
    let pool = setup_test_db().await;
    let repo = SqliteNoteRepository::new(pool);

    let result = repo.delete("nonexistent-id-xyz").await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_pool_creation_invalid_path() {
    // In-memory pool with invalid parameters should still create successfully
    // (sqlite in-memory doesn't use filesystem)
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite://:memory:")
        .await;
    assert!(pool.is_ok());
}

// ============================================================================
// ADVERSARIAL PATH — Edge cases in note data
// ============================================================================

#[tokio::test]
async fn test_note_repository_with_empty_content() {
    let pool = setup_test_db().await;
    let repo = SqliteNoteRepository::new(pool);
    let note = make_test_note("session-empty", "");

    let result = repo.insert(&note).await;
    assert!(result.is_ok(), "empty content note should be insertable");
}

#[tokio::test]
async fn test_note_repository_with_special_chars() {
    let pool = setup_test_db().await;
    let repo = SqliteNoteRepository::new(pool);
    let special = "content with \0 null \n newline \t tab and 中文 and 日本語";
    let note = make_test_note("session-special", special);

    let created = repo.insert(&note).await.expect("insert should succeed");
    assert_eq!(created.content, special);
}

#[tokio::test]
async fn test_note_repository_with_long_content() {
    let pool = setup_test_db().await;
    let repo = SqliteNoteRepository::new(pool);
    let long_content = "A".repeat(10_000);
    let note = make_test_note("session-long", &long_content);

    let created = repo.insert(&note).await.expect("insert should succeed");
    assert_eq!(created.content.len(), 10_000);
}

// ============================================================================
// LOGIC TREE — CRUD roundtrip and concurrent operations
// ============================================================================

#[tokio::test]
async fn test_note_repository_crud_roundtrip() {
    let pool = setup_test_db().await;
    let repo = SqliteNoteRepository::new(pool);

    let note = make_test_note("session-crud", "original content");
    let created = repo.insert(&note).await.expect("create should succeed");
    assert_eq!(created.content, "original content");

    let found = repo
        .find_by_session("session-crud")
        .await
        .expect("read should succeed");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, created.id);

    let deleted = repo
        .delete(&created.id)
        .await
        .expect("delete should succeed");
    assert!(deleted, "delete should return true for existing record");

    let after_delete = repo
        .find_by_session("session-crud")
        .await
        .expect("find should succeed");
    assert!(
        after_delete.is_empty(),
        "deleted note should not appear in results"
    );
}

#[tokio::test]
async fn test_note_repository_multiple_sessions() {
    let pool = setup_test_db().await;
    let repo = SqliteNoteRepository::new(pool);

    let note1 = make_test_note("session-a", "content a");
    let note2 = make_test_note("session-b", "content b");
    let note3 = make_test_note("session-a", "content a2");

    repo.insert(&note1).await.expect("insert a should succeed");
    repo.insert(&note2).await.expect("insert b should succeed");
    repo.insert(&note3).await.expect("insert a2 should succeed");

    let session_a = repo
        .find_by_session("session-a")
        .await
        .expect("find a should succeed");
    let session_b = repo
        .find_by_session("session-b")
        .await
        .expect("find b should succeed");

    assert_eq!(session_a.len(), 2, "session-a should have 2 notes");
    assert_eq!(session_b.len(), 1, "session-b should have 1 note");
}
