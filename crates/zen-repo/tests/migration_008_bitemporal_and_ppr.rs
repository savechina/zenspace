//! Principle XIII #6: migration invariants for 008_bitemporal_and_ppr.sql +
//! behavior pins for bi-temporal relationship validity (Graphiti pattern) and
//! Personalized PageRank (HippoRAG pattern) in NotionsRepo.
//!
//! PURPOSE: Verifies t_valid/t_invalid columns exist with correct defaults,
//! soft invalidation never deletes rows, contradiction-aware insert supersedes
//! open edges, point-in-time queries honor the half-open [t_valid, t_invalid)
//! window, and PPR concentrates score mass in the seed neighborhood.

use tempfile::tempdir;
use zen_repo::{InsertRelationshipRequest, NotionsRepo, SqliteClient};

const T1: &str = "2024-01-01T00:00:00Z";
const T2: &str = "2024-06-01T00:00:00Z";
const T3: &str = "2024-09-01T00:00:00Z";

async fn make_client() -> (SqliteClient, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let db = dir.path().join("m008.db");
    let client = SqliteClient::open(&db).await.unwrap();
    (client, dir)
}

fn rel<'a>(
    id: &'a str,
    source_id: &'a str,
    target_id: &'a str,
    rel_type: &'a str,
    created_at: &'a str,
) -> InsertRelationshipRequest<'a> {
    InsertRelationshipRequest {
        id,
        source_id,
        target_id,
        rel_type,
        confidence: 1.0,
        source_note_ids: None,
        created_at,
        description: None,
        valid_from: None,
        valid_until: None,
        weight: None,
    }
}

async fn seed_entities(repo: &NotionsRepo<'_>, entities: &[(&str, &str)]) {
    for (id, name) in entities {
        repo.insert_entity(id, name, "node", T1).await.unwrap();
    }
}

#[tokio::test]
async fn migration_008_adds_bitemporal_columns_with_expected_defaults() {
    let (client, _dir) = make_client().await;
    let repo = NotionsRepo::new(&client);

    let cols: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('relationships') ORDER BY cid")
            .fetch_all(client.pool())
            .await
            .unwrap();
    assert!(
        cols.contains(&"t_valid".to_string()) && cols.contains(&"t_invalid".to_string()),
        "relationships must have t_valid/t_invalid after migration 008; got: {cols:?}"
    );

    seed_entities(&repo, &[("a", "A"), ("b", "B")]).await;

    repo.insert_relationship(&rel("r1", "a", "b", "next", T1))
        .await
        .unwrap();
    let (t_valid, t_invalid): (String, Option<String>) =
        sqlx::query_as("SELECT t_valid, t_invalid FROM relationships WHERE id = 'r1'")
            .fetch_one(client.pool())
            .await
            .unwrap();
    assert_eq!(
        t_valid, T1,
        "repo insert path must record t_valid = created_at (insertion time)"
    );
    assert_eq!(
        t_invalid, None,
        "freshly inserted edge must be open-ended (t_invalid NULL)"
    );

    let sql = "INSERT INTO relationships (id, source_notion_id, target_notion_id, \
               relation_type, confidence, created_at) \
               VALUES ('r_raw', 'a', 'b', 'next', 1.0, '2024-01-01T00:00:00Z')"
        .to_string();
    client
        .writer()
        .call(move |conn| {
            conn.execute(&sql, [])?;
            Ok(())
        })
        .await
        .unwrap();
    let raw_t_valid: String =
        sqlx::query_scalar("SELECT t_valid FROM relationships WHERE id = 'r_raw'")
            .fetch_one(client.pool())
            .await
            .unwrap();
    assert_eq!(
        raw_t_valid, "",
        "raw insert without t_valid must get the '' DDL default (valid since epoch)"
    );
}

#[tokio::test]
async fn invalidate_relationship_is_soft_and_first_wins() {
    let (client, _dir) = make_client().await;
    let repo = NotionsRepo::new(&client);
    seed_entities(&repo, &[("a", "A"), ("b", "B")]).await;

    repo.insert_relationship(&rel("r1", "a", "b", "next", T1))
        .await
        .unwrap();

    assert!(
        repo.invalidate_relationship("r1", T2).await.unwrap(),
        "invalidating an open edge must report success"
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM relationships")
        .fetch_one(client.pool())
        .await
        .unwrap();
    assert_eq!(count, 1, "invalidation must preserve the row (soft only)");

    let t_invalid: Option<String> =
        sqlx::query_scalar("SELECT t_invalid FROM relationships WHERE id = 'r1'")
            .fetch_one(client.pool())
            .await
            .unwrap();
    assert_eq!(t_invalid.as_deref(), Some(T2));

    assert!(
        !repo.invalidate_relationship("r1", T3).await.unwrap(),
        "re-invalidating a closed edge must report false (first stamp wins)"
    );
    let t_invalid_after: Option<String> =
        sqlx::query_scalar("SELECT t_invalid FROM relationships WHERE id = 'r1'")
            .fetch_one(client.pool())
            .await
            .unwrap();
    assert_eq!(
        t_invalid_after.as_deref(),
        Some(T2),
        "original t_invalid must be preserved"
    );

    assert!(
        !repo
            .invalidate_relationship("no-such-id", T3)
            .await
            .unwrap(),
        "unknown id must report false, not error"
    );
}

#[tokio::test]
async fn temporal_insert_invalidates_contradicting_open_edge() {
    let (client, _dir) = make_client().await;
    let repo = NotionsRepo::new(&client);
    seed_entities(
        &repo,
        &[
            ("bob", "Bob"),
            ("nyc", "NYC"),
            ("sf", "SF"),
            ("ali", "Alice"),
        ],
    )
    .await;

    let n0 = repo
        .insert_relationship_temporal(&rel("r1", "bob", "nyc", "lives_in", T1))
        .await
        .unwrap();
    assert_eq!(n0, 0, "first fact contradicts nothing");

    let n1 = repo
        .insert_relationship_temporal(&rel("r2", "bob", "sf", "lives_in", T2))
        .await
        .unwrap();
    assert_eq!(n1, 1, "contradicting object must invalidate the open edge");

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM relationships")
        .fetch_one(client.pool())
        .await
        .unwrap();
    assert_eq!(
        count, 2,
        "contradiction invalidates, never deletes — both rows must survive"
    );

    let (old_invalid, new_invalid): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT (SELECT t_invalid FROM relationships WHERE id = 'r1'), \
                (SELECT t_invalid FROM relationships WHERE id = 'r2')",
    )
    .fetch_one(client.pool())
    .await
    .unwrap();
    assert_eq!(
        old_invalid.as_deref(),
        Some(T2),
        "superseded edge must close at the new edge's t_valid"
    );
    assert_eq!(new_invalid, None, "new edge must stay open-ended");

    let n2 = repo
        .insert_relationship_temporal(&rel("r3", "ali", "nyc", "lives_in", T3))
        .await
        .unwrap();
    assert_eq!(n2, 0, "different subject is not a contradiction");

    let n3 = repo
        .insert_relationship_temporal(&rel("r4", "bob", "sf", "lives_in", T3))
        .await
        .unwrap();
    assert_eq!(
        n3, 0,
        "re-asserting the same (subject, predicate, object) is not a contradiction"
    );
}

#[tokio::test]
async fn relationships_as_of_returns_edges_valid_at_timestamp() {
    let (client, _dir) = make_client().await;
    let repo = NotionsRepo::new(&client);
    seed_entities(&repo, &[("bob", "Bob"), ("nyc", "NYC"), ("sf", "SF")]).await;

    repo.insert_relationship_temporal(&rel("r1", "bob", "nyc", "lives_in", T1))
        .await
        .unwrap();
    repo.insert_relationship_temporal(&rel("r2", "bob", "sf", "lives_in", T2))
        .await
        .unwrap();

    let mid: Vec<String> = repo
        .relationships_as_of("2024-03-01T00:00:00Z")
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(
        mid,
        vec!["r1".to_string()],
        "only r1 valid between T1 and T2"
    );

    let boundary: Vec<String> = repo
        .relationships_as_of(T2)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(
        boundary,
        vec!["r2".to_string()],
        "half-open [t_valid, t_invalid): at exactly T2 the old edge is out, the new one is in"
    );

    let later: Vec<String> = repo
        .relationships_as_of(T3)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(later, vec!["r2".to_string()], "r2 stays valid after T2");

    let before: Vec<String> = repo
        .relationships_as_of("2023-01-01T00:00:00Z")
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert!(before.is_empty(), "nothing valid before the first t_valid");

    let row = &repo.relationships_as_of(T3).await.unwrap()[0];
    assert_eq!(row.t_valid, T2, "as-of rows must carry t_valid");
    assert_eq!(row.t_invalid, None, "as-of rows must carry t_invalid");
}

#[tokio::test]
async fn personalized_pagerank_ranks_seed_neighborhood_above_unrelated_component() {
    let (client, _dir) = make_client().await;
    let repo = NotionsRepo::new(&client);
    seed_entities(
        &repo,
        &[
            ("alpha", "Alpha"),
            ("beta", "Beta"),
            ("gamma", "Gamma"),
            ("xray", "Xray"),
            ("yankee", "Yankee"),
            ("zulu", "Zulu"),
        ],
    )
    .await;
    for (rid, src, tgt) in &[
        ("e1", "alpha", "beta"),
        ("e2", "alpha", "gamma"),
        ("e3", "xray", "yankee"),
        ("e4", "yankee", "zulu"),
    ] {
        repo.insert_relationship(&rel(rid, src, tgt, "next", T1))
            .await
            .unwrap();
    }

    let scores = repo
        .personalized_pagerank(&["Alpha".to_string()], 30, 0.85, 0.15)
        .await
        .unwrap();
    assert_eq!(scores.len(), 6, "every entity is ranked");

    let score_of = |name: &str| {
        scores
            .iter()
            .find(|r| r.notion == name)
            .map(|r| r.score)
            .unwrap()
    };
    let neighborhood_min = ["Alpha", "Beta", "Gamma"]
        .iter()
        .map(|n| score_of(n))
        .fold(f64::MAX, f64::min);
    let unrelated_max = ["Xray", "Yankee", "Zulu"]
        .iter()
        .map(|n| score_of(n))
        .fold(f64::MIN, f64::max);
    assert!(
        neighborhood_min > unrelated_max,
        "seed neighborhood ({neighborhood_min}) must outrank the unrelated component ({unrelated_max})"
    );
    assert!(
        score_of("Alpha") > score_of("Beta"),
        "the seed itself must hold the top personalized score"
    );

    let unknown = repo
        .personalized_pagerank(&["NoSuchEntity".to_string()], 30, 0.85, 0.15)
        .await
        .unwrap();
    assert!(
        unknown.is_empty(),
        "no resolvable seed must yield an empty ranking"
    );

    let global = repo.pagerank(40, 0.85).await.unwrap();
    let total: f64 = global.iter().map(|r| r.score).sum();
    assert!(
        (total - 1.0).abs() < 0.01,
        "global pagerank must still sum to ~1.0 after the shared-core refactor, got {total}"
    );
}
