//! Principle XIII #6: migration invariants for 010_communities.sql, plus
//! behavior pins for the T141 community persistence surface.
//!
//! PURPOSE: Verifies the two community tables exist with the expected columns,
//! `replace_communities` round-trips communities + members, recomputation
//! replaces rather than appends (row counts do not grow), different
//! resolutions coexist, and the repo wrapper builds the projection from open
//! edges only (soft-invalidated edges are excluded).

use tempfile::tempdir;
use zen_repo::{Community, CommunityMember, InsertRelationshipRequest, NotionsRepo, SqliteClient};

const T1: &str = "2024-01-01T00:00:00Z";
const T2: &str = "2024-06-01T00:00:00Z";

async fn make_client() -> (SqliteClient, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let db = dir.path().join("m010.db");
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
async fn migration_010_adds_community_tables_with_expected_columns() {
    let (client, _dir) = make_client().await;

    let cols: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('notion_communities') ORDER BY cid")
            .fetch_all(client.pool())
            .await
            .unwrap();
    assert_eq!(
        cols,
        vec![
            "id".to_string(),
            "algorithm".to_string(),
            "resolution".to_string(),
            "computed_at".to_string(),
            "label".to_string(),
            "size".to_string(),
        ],
        "notion_communities must carry the T141 columns"
    );

    let mcols: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM pragma_table_info('notion_community_members') ORDER BY cid",
    )
    .fetch_all(client.pool())
    .await
    .unwrap();
    assert_eq!(
        mcols,
        vec![
            "community_id".to_string(),
            "entity_name".to_string(),
            "weight".to_string(),
        ],
        "notion_community_members must carry the T141 columns"
    );
}

#[tokio::test]
async fn replace_communities_roundtrips_communities_and_members() {
    let (client, _dir) = make_client().await;
    let repo = NotionsRepo::new(&client);

    let comms = vec![
        Community {
            id: "c0".into(),
            label: "Alpha".into(),
            members: vec![
                CommunityMember {
                    entity_name: "A".into(),
                    weight: 2.0,
                },
                CommunityMember {
                    entity_name: "B".into(),
                    weight: 2.0,
                },
            ],
        },
        Community {
            id: "c1".into(),
            label: "Beta".into(),
            members: vec![CommunityMember {
                entity_name: "C".into(),
                weight: 1.0,
            }],
        },
    ];

    repo.replace_communities("louvain", 1.0, &comms, T1)
        .await
        .unwrap();

    let loaded = repo.load_communities().await.unwrap();
    assert_eq!(loaded.len(), 2);
    // Ids are scoped by algorithm+resolution so coexisting runs never collide
    // in the members table (keyed by community_id alone).
    assert_eq!(loaded[0].id, "louvain:1:c0");
    assert_eq!(loaded[1].id, "louvain:1:c1");
    assert_eq!(loaded[0].members, comms[0].members);
    assert_eq!(loaded[1].members, comms[1].members);
    assert_eq!(loaded[0].label, "Alpha");
    assert_eq!(loaded[1].label, "Beta");
}

#[tokio::test]
async fn recompute_replaces_instead_of_appending() {
    let (client, _dir) = make_client().await;
    let repo = NotionsRepo::new(&client);

    let first = vec![Community {
        id: "c0".into(),
        label: "Alpha".into(),
        members: vec![CommunityMember {
            entity_name: "A".into(),
            weight: 1.0,
        }],
    }];
    let second = vec![Community {
        id: "c0".into(),
        label: "Alpha".into(),
        members: vec![
            CommunityMember {
                entity_name: "A".into(),
                weight: 1.0,
            },
            CommunityMember {
                entity_name: "B".into(),
                weight: 1.0,
            },
        ],
    }];

    repo.replace_communities("louvain", 1.0, &first, T1)
        .await
        .unwrap();
    repo.replace_communities("louvain", 1.0, &second, T2)
        .await
        .unwrap();

    let loaded = repo.load_communities().await.unwrap();
    assert_eq!(
        loaded.len(),
        1,
        "recompute must not append a second community"
    );
    assert_eq!(loaded[0].members.len(), 2, "the newer run must win");

    let (comm_count, member_count): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM notion_communities), \
                (SELECT COUNT(*) FROM notion_community_members)",
    )
    .fetch_one(client.pool())
    .await
    .unwrap();
    assert_eq!(comm_count, 1, "notion_communities must not grow");
    assert_eq!(member_count, 2, "notion_community_members must not grow");
}

#[tokio::test]
async fn different_resolutions_coexist() {
    let (client, _dir) = make_client().await;
    let repo = NotionsRepo::new(&client);

    let comms = vec![Community {
        id: "c0".into(),
        label: "Alpha".into(),
        members: vec![CommunityMember {
            entity_name: "A".into(),
            weight: 1.0,
        }],
    }];

    repo.replace_communities("louvain", 1.0, &comms, T1)
        .await
        .unwrap();
    repo.replace_communities("louvain", 2.0, &comms, T2)
        .await
        .unwrap();

    let loaded = repo.load_communities().await.unwrap();
    assert_eq!(
        loaded.len(),
        2,
        "resolution is data: 1.0 and 2.0 runs must coexist"
    );
}

#[tokio::test]
async fn compute_communities_uses_open_edges_only() {
    let (client, _dir) = make_client().await;
    let repo = NotionsRepo::new(&client);

    // Two triangles joined by a bridge edge → two communities.
    seed_entities(
        &repo,
        &[
            ("a", "A"),
            ("b", "B"),
            ("c", "C"),
            ("d", "D"),
            ("e", "E"),
            ("f", "F"),
        ],
    )
    .await;
    for (id, s, t) in [
        ("r1", "a", "b"),
        ("r2", "b", "c"),
        ("r3", "a", "c"),
        ("r4", "d", "e"),
        ("r5", "e", "f"),
        ("r6", "d", "f"),
        ("r7", "c", "d"),
    ] {
        repo.insert_relationship(&rel(id, s, t, "next", T1))
            .await
            .unwrap();
    }

    let comms = repo.compute_communities(1.0).await.unwrap();
    assert_eq!(comms.len(), 2, "two cliques joined by one bridge edge");

    // Soft-invalidate the bridge edge: the two cliques become disconnected,
    // so the projection must now yield two communities with no bridge — the
    // invalidated edge must not appear in the projection.
    repo.invalidate_relationship("r7", T2).await.unwrap();
    let comms_after = repo.compute_communities(1.0).await.unwrap();
    assert_eq!(
        comms_after.len(),
        2,
        "invalidated bridge edge must be excluded from the projection"
    );
}

#[tokio::test]
async fn compute_then_replace_then_load_full_pipeline() {
    let (client, _dir) = make_client().await;
    let repo = NotionsRepo::new(&client);

    seed_entities(&repo, &[("a", "A"), ("b", "B"), ("c", "C"), ("d", "D")]).await;
    for (id, s, t) in [("r1", "a", "b"), ("r2", "b", "c"), ("r3", "c", "d")] {
        repo.insert_relationship(&rel(id, s, t, "next", T1))
            .await
            .unwrap();
    }

    let comms = repo.compute_communities(1.0).await.unwrap();
    assert!(!comms.is_empty());
    repo.replace_communities("louvain", 1.0, &comms, T1)
        .await
        .unwrap();

    let loaded = repo.load_communities().await.unwrap();
    assert_eq!(loaded.len(), comms.len());
    for (persisted, computed) in loaded.iter().zip(&comms) {
        assert_eq!(persisted.members, computed.members);
        assert_eq!(persisted.label, computed.label);
        assert_eq!(
            persisted.id,
            format!("louvain:1:{}", computed.id),
            "compute → replace → load must round-trip members with scoped ids"
        );
    }
}
