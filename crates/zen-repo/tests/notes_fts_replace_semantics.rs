//! Regression guard for the FTS5 rowid-churn bug: a re-indexed note must
//! REPLACE its previous index entry, never append.
//!
//! PURPOSE: Pins the false-positive hazard directly — after re-indexing the
//! same id with different content, the PREVIOUS version's terms must not be
//! searchable, only one FTS row may exist, and the notes_meta/notes_fts rowids
//! must coincide so the search JOIN resolves. Stable rowid without
//! `contentless_delete=1` leaves stale terms behind; bare append (the
//! production bug) leaves ~one orphan row per re-index.

use sha2::{Digest, Sha256};
use tempfile::tempdir;
use zen_repo::{IndexNoteRequest, NotesRepo, SqliteClient};

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect()
}

async fn index(notes: &NotesRepo<'_>, file_path: &str, content: &str) {
    notes
        .index_note(IndexNoteRequest {
            id: "x",
            title: "t",
            content,
            tags: "",
            file_path,
            source: "test",
        })
        .await
        .unwrap();
}

async fn match_count(client: &SqliteClient, term: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM notes_fts WHERE notes_fts MATCH ?1")
        .bind(term)
        .fetch_one(client.pool())
        .await
        .unwrap()
}

async fn meta_rowid(client: &SqliteClient) -> i64 {
    sqlx::query_scalar("SELECT rowid FROM notes_meta WHERE id = 'x'")
        .fetch_one(client.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn reindex_replaces_and_never_leaves_stale_terms() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("replace.db");
    let client = SqliteClient::open(&db).await.unwrap();
    let notes = NotesRepo::new(&client);

    let alpha = dir.path().join("alpha.md");
    let gamma = dir.path().join("gamma.md");
    std::fs::write(&alpha, "body with alphaunique in it").unwrap();
    std::fs::write(&gamma, "body with gammaunique in it").unwrap();
    let alpha_path = alpha.display().to_string();
    let gamma_path = gamma.display().to_string();

    index(&notes, &alpha_path, "alphaunique").await;
    let rowid_first = meta_rowid(&client).await;

    index(&notes, &gamma_path, "gammaunique").await;
    let rowid_second = meta_rowid(&client).await;

    let meta_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notes_meta WHERE id = 'x'")
        .fetch_one(client.pool())
        .await
        .unwrap();
    assert_eq!(
        meta_count, 1,
        "re-indexing must not duplicate notes_meta ids"
    );

    assert_eq!(
        rowid_first, rowid_second,
        "the rowid must be derived deterministically from the id, not re-allocated"
    );

    let fts_rowid: i64 = sqlx::query_scalar("SELECT rowid FROM notes_fts")
        .fetch_one(client.pool())
        .await
        .unwrap();
    assert_eq!(
        fts_rowid, rowid_second,
        "notes_meta and notes_fts must share the rowid the search JOIN depends on"
    );

    assert_eq!(
        match_count(&client, "alphaunique").await,
        0,
        "the previous version's terms MUST NOT remain searchable (false-positive hazard)"
    );
    assert_eq!(match_count(&client, "gammaunique").await, 1);

    let fts_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notes_fts")
        .fetch_one(client.pool())
        .await
        .unwrap();
    assert_eq!(fts_count, 1, "re-indexing must replace, not append");

    index(&notes, &gamma_path, "gammaunique").await;

    assert_eq!(
        meta_rowid(&client).await,
        rowid_second,
        "an unchanged re-index must keep the same rowid"
    );
    assert_eq!(match_count(&client, "alphaunique").await, 0);
    assert_eq!(match_count(&client, "gammaunique").await, 1);
    let fts_count_third: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notes_fts")
        .fetch_one(client.pool())
        .await
        .unwrap();
    assert_eq!(
        fts_count_third, 1,
        "idempotent re-index must stay at one row"
    );

    let total_meta: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notes_meta")
        .fetch_one(client.pool())
        .await
        .unwrap();
    assert_eq!(total_meta, 1, "no duplicate ids anywhere in notes_meta");

    let checksums = notes.all_file_checksums().await.unwrap();
    assert_eq!(
        checksums,
        vec![(
            gamma_path.clone(),
            sha256_hex(&std::fs::read(&gamma).unwrap())
        )],
        "content_hash must be the whole-file SHA-256 the reindex gate compares against"
    );

    let hits = notes.search("gammaunique", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, gamma_path);
}
