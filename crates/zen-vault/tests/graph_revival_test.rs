//! Graph-revival pins (2026-11-21): `store_entity_embedding` and
//! `extract_relationships` were dead APIs — production ran 62 entities /
//! 0 edges / 0 entity vectors. The wiring must keep both derived indexes
//! alive: upserts embed, co-occurrence edges persist with deterministic
//! pair-level ids, and the reindex revival pass rebuilds both from the
//! wiki content.
//!
//! Runs under nextest (process-per-test); embeddings take the hash
//! fallback (no LLM provider in the test environment).

use std::path::{Path, PathBuf};

use zen_repo::SqliteClient;
use zen_vault::notion::{Notion, NotionKind, NotionService};
use zen_vault::tindy::Reindexer;

async fn open(db: &Path) -> SqliteClient {
    SqliteClient::open(db).await.unwrap()
}

async fn relationship_count(db: &Path) -> i64 {
    let client = open(db).await;
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM relationships")
        .fetch_one(client.pool())
        .await
        .unwrap()
}

async fn entity_vector_count(db: &Path) -> i64 {
    let client = open(db).await;
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM notion_embeddings_rowids")
        .fetch_one(client.pool())
        .await
        .unwrap()
}

fn notion(name: &str) -> Notion {
    Notion::new(name, NotionKind::Technology, "test-note")
}

#[tokio::test]
async fn upsert_with_embedding_writes_entity_vector() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("state.db");
    let client = open(&db).await;
    let svc = NotionService::new();

    svc.upsert_entity_with_embedding(&client, &notion("Rust"))
        .await
        .unwrap();
    svc.upsert_entity_with_embedding(&client, &notion("SQLite"))
        .await
        .unwrap();
    // Re-upsert the same entity (alias path) — still one vector, refreshed.
    svc.upsert_entity_with_embedding(&client, &notion("Rust"))
        .await
        .unwrap();
    drop(client);

    assert_eq!(
        entity_vector_count(&db).await,
        2,
        "one vector per canonical entity"
    );
}

#[tokio::test]
async fn cooccurrence_edges_are_idempotent_canonical_and_capped() {
    let tmp = tempfile::tempdir().unwrap();
    let db: PathBuf = tmp.path().join("state.db");
    let client = open(&db).await;
    let svc = NotionService::new();

    // The raw builder keeps its pairwise cap contract.
    let notions: Vec<Notion> = (1..=12).map(|i| notion(&format!("Tech{i:02}"))).collect();
    let rels = svc.extract_relationships(&notions).unwrap();
    assert_eq!(
        rels.len(),
        45,
        "capped at MAX_COOCCURRENCE_NOTIONS=10 → C(10,2) pairs"
    );

    // Persistence is canonical-aware: spelling variants collapse onto one
    // row (the FK incident shape — "Sqlite" from known-techs, "SQLite"
    // from capitalized-word extraction), and the pair never self-edges.
    svc.upsert_entity_with_embedding(&client, &notion("Sqlite"))
        .await
        .unwrap();
    svc.upsert_entity_with_embedding(&client, &notion("Rust"))
        .await
        .unwrap();
    svc.upsert_entity_with_embedding(&client, &notion("SQLite"))
        .await
        .unwrap();
    let batch = vec![notion("Sqlite"), notion("Rust"), notion("SQLite")];
    svc.persist_cooccurrence_edges(&client, &batch)
        .await
        .unwrap();
    svc.persist_cooccurrence_edges(&client, &batch)
        .await
        .unwrap();
    drop(client);

    assert_eq!(
        relationship_count(&db).await,
        1,
        "variants collapse → exactly the Sqlite↔Rust pair, re-persist replaces"
    );

    let client = open(&db).await;
    let id: String = sqlx::query_scalar("SELECT id FROM relationships")
        .fetch_one(client.pool())
        .await
        .unwrap();
    assert!(id.starts_with("rel-"), "deterministic pair-level id: {id}");
}

#[tokio::test]
async fn reindex_revival_pass_embeds_and_edges_from_wiki() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("state.db");
    let vault = tmp.path().join("vault");
    let wiki = vault.join("wiki");
    std::fs::create_dir_all(&wiki).unwrap();
    std::fs::write(
        wiki.join("tech.md"),
        "---\nid: tech\n---\n\nWe use Rust with SQLite and Docker for the service.\n",
    )
    .unwrap();

    let client = open(&db).await;
    let reindexer = Reindexer::with_client(client);
    let report = reindexer.reindex(&vault).await.unwrap();
    let (entities, edges) = (report.entities_embedded, report.edges_written);
    drop(reindexer);

    assert!(
        entities >= 3,
        "known techs extracted and embedded: {entities}"
    );
    assert!(edges >= 3, "pairwise edges written: {edges}");
    assert!(relationship_count(&db).await >= 3);
    assert!(entity_vector_count(&db).await >= 3);
}
