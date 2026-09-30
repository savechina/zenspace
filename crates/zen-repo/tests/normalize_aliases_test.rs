// PURPOSE: Pin the NFC alias backfill contract (T143 closure, plan-eng-review
//          2A, 2026-09-30): `zen wiki reindex` heals legacy decomposed
//          (NFD) `notion_aliases` rows that equality lookups can never hit.
// USAGE: cargo nextest run -p zen-repo --test normalize_aliases_test
// EXPECTED: All three tests pass — no-op on clean table, NFD→NFC rewrite,
//           collision dedup without panic.
// ERRORS: A panic on the collision case means the PK-conflict path regressed.

use zen_repo::{NotionsRepo, SqliteClient};

/// `(client, repo, _dir)` — the client handle stays out here because raw
/// seeding/count helpers need the sqlx pool and the repo only borrows it.
async fn make_repo() -> (
    &'static SqliteClient,
    NotionsRepo<'static>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("test.db");
    let client = SqliteClient::open(&db).await.unwrap();
    let leaked: &'static SqliteClient = Box::leak(Box::new(client));
    (leaked, NotionsRepo::new(leaked), dir)
}

/// Precomposed `café` (NFC) vs `cafe` + U+0301 combining accent (NFD):
/// same name, different byte sequences — the exact FR-022 hazard.
const NFC_ALIAS: &str = "café";
const NFD_ALIAS: &str = "cafe\u{0301}";

/// Seed a notion + an alias row BYPASSING `insert_alias` normalization —
/// the only way a legacy NFD row can exist in the wild.
async fn seed_raw_alias(client: &SqliteClient, alias: &str, notion_id: &str) {
    let repo = NotionsRepo::new(client);
    repo.upsert_entity(
        notion_id,
        "test-entity",
        "concept",
        "2024-01-01T00:00:00Z",
        "2024-01-01T00:00:00Z",
    )
    .await
    .unwrap();
    sqlx::query("INSERT INTO notion_aliases (alias, canonical_notion_id) VALUES (?1, ?2)")
        .bind(alias)
        .bind(notion_id)
        .execute(client.pool())
        .await
        .unwrap();
}

async fn count_alias(client: &SqliteClient, alias: &str) -> i64 {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM notion_aliases WHERE alias = ?1")
        .bind(alias)
        .fetch_one(client.pool())
        .await
        .unwrap();
    row.0
}

#[tokio::test]
async fn clean_table_is_a_no_op() {
    let (client, repo, _dir) = make_repo().await;
    repo.upsert_entity(
        "n1",
        "rust",
        "concept",
        "2024-01-01T00:00:00Z",
        "2024-01-01T00:00:00Z",
    )
    .await
    .unwrap();
    repo.insert_alias("Rust Language", "n1").await.unwrap();

    let before = count_alias(client, "rust").await;
    assert!(before >= 1);
    assert_eq!(repo.normalize_aliases_pass().await.unwrap(), 0);
    assert_eq!(count_alias(client, "rust").await, before);
    // Idempotent: second pass also repairs nothing.
    assert_eq!(repo.normalize_aliases_pass().await.unwrap(), 0);
}

#[tokio::test]
async fn nfd_row_is_rewritten_to_nfc() {
    let (client, repo, _dir) = make_repo().await;
    seed_raw_alias(client, NFD_ALIAS, "n1").await;

    assert_eq!(count_alias(client, NFD_ALIAS).await, 1);
    assert_eq!(repo.normalize_aliases_pass().await.unwrap(), 1);
    assert_eq!(
        count_alias(client, NFD_ALIAS).await,
        0,
        "legacy row must be gone"
    );
    assert_eq!(
        count_alias(client, NFC_ALIAS).await,
        1,
        "canonical row must exist"
    );
    // The healed row now answers an equality lookup (the whole point).
    assert_eq!(
        repo.resolve_alias(NFC_ALIAS).await.unwrap().as_deref(),
        Some("n1")
    );
}

#[tokio::test]
async fn collision_dedups_without_panic() {
    let (client, repo, _dir) = make_repo().await;
    seed_raw_alias(client, NFC_ALIAS, "n1").await; // canonical row already present
    seed_raw_alias(client, NFD_ALIAS, "n1").await; // legacy row normalizes onto it

    // Must not panic on the (alias, notion_id) PK collision.
    assert_eq!(repo.normalize_aliases_pass().await.unwrap(), 1);
    assert_eq!(count_alias(client, NFC_ALIAS).await, 1);
    assert_eq!(count_alias(client, NFD_ALIAS).await, 0);
}
