//! Principle XIII #6: migration invariants for 004_qq_bindings.sql.
//!
//! PURPOSE: Verifies the qq_bindings table applies cleanly and the
//! QqBindingRepo round-trips chat→session mappings with
//! upsert-preserved created_at semantics.

use tempfile::tempdir;
use zen_repo::{QqBindingRepo, SqliteClient};

#[tokio::test]
async fn upsert_then_get_round_trip() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("m004.db");
    let client = SqliteClient::open(&db).await.unwrap();
    let repo = QqBindingRepo::new(&client);

    let missing = repo.get("group_A").await.unwrap();
    assert!(missing.is_none(), "unbound chat must return None");

    repo.upsert("group_A", "sess-1").await.unwrap();
    let row = repo.get("group_A").await.unwrap().expect("binding exists");
    assert_eq!(row.chat_id, "group_A");
    assert_eq!(row.session_id, "sess-1");
    assert!(row.created_at > 0);
    assert!(row.updated_at >= row.created_at);
}

#[tokio::test]
async fn reupsert_updates_session_but_preserves_created_at() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("m004b.db");
    let client = SqliteClient::open(&db).await.unwrap();
    let repo = QqBindingRepo::new(&client);

    repo.upsert("user_B", "sess-old").await.unwrap();
    let first = repo.get("user_B").await.unwrap().unwrap();

    repo.upsert("user_B", "sess-new").await.unwrap();
    let second = repo.get("user_B").await.unwrap().unwrap();

    assert_eq!(second.session_id, "sess-new");
    assert_eq!(
        second.created_at, first.created_at,
        "conflicting upsert must not reset created_at"
    );
    assert!(second.updated_at >= first.updated_at);
}

#[tokio::test]
async fn delete_removes_binding() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("m004c.db");
    let client = SqliteClient::open(&db).await.unwrap();
    let repo = QqBindingRepo::new(&client);

    repo.upsert("group_C", "sess-3").await.unwrap();
    repo.delete("group_C").await.unwrap();
    assert!(repo.get("group_C").await.unwrap().is_none());

    repo.delete("group_C").await.unwrap();
}
