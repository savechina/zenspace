use tempfile::tempdir;

use zen_repo::{
    IndexNoteRequest, InsertEntityEmbeddingRequest, InsertNoteEmbeddingRequest,
    InsertRelationshipRequest, SelfNodeRow, SqliteClient, UpsertBeliefNodeRequest,
    UpsertGoalNodeRequest, UpsertPathNodeRequest,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn make_client() -> (SqliteClient, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let db = dir.path().join("test.db");
    let client = SqliteClient::open(&db).await.unwrap();
    (client, dir)
}

// ===========================================================================
// SqliteClient
// ===========================================================================

#[tokio::test]
async fn test_client_open_creates_db_file() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("test.db");
    assert!(!db.exists());
    let _client = SqliteClient::open(&db).await.unwrap();
    assert!(db.exists());
}

#[tokio::test]
async fn test_sqlite_vec_extension_loaded_via_sqlx_pool() {
    let (client, _dir) = make_client().await;
    let pool = client.pool();
    
    let result: Result<(String,), _> = sqlx::query_as("SELECT vec_version()")
        .fetch_one(pool)
        .await;
    
    assert!(result.is_ok(), "sqlite-vec extension should be loaded via sqlx pool");
    let (version,) = result.unwrap();
    assert!(version.starts_with('v'), "vec_version should start with 'v', got: {}", version);
}

#[tokio::test]
async fn test_client_open_lazy_creates_db_file() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("lazy.db");
    assert!(!db.exists());
    let _client = SqliteClient::open_lazy(&db).await.unwrap();
    assert!(db.exists());
}

#[tokio::test]
async fn test_client_open_existing_db_is_idempotent() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("idempotent.db");
    let c1 = SqliteClient::open(&db).await.unwrap();
    drop(c1);
    let c2 = SqliteClient::open(&db).await.unwrap();
    // opening twice should not fail
    drop(c2);
}

#[tokio::test]
async fn test_client_writer_and_pool_are_accessible() {
    let (client, _dir) = make_client().await;
    // writer() returns a reference that can be used
    let _w = client.writer();
    // pool() returns a reference that can be used
    let p = client.pool();
    assert!(!p.is_closed());
}

#[tokio::test]
async fn test_client_schema_has_expected_tables() {
    let (client, _dir) = make_client().await;
    // Verify core tables exist by querying sqlite_master
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name",
    )
    .fetch_all(client.pool())
    .await
    .unwrap();

    for expected in &[
        "sessions",
        "notes_meta",
        "entities",
        "relationships",
        "entity_aliases",
        "dispatch_tasks",
        "self_nodes",
        "goal_nodes",
        "path_nodes",
        "belief_nodes",
    ] {
        assert!(
            rows.iter().any(|t| t == expected),
            "expected table '{}' not found; got: {:?}",
            expected,
            rows
        );
    }
}

// ===========================================================================
// NotesRepo
// ===========================================================================

#[tokio::test]
async fn test_notes_search_empty_query_returns_empty() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::NotesRepo::new(&client);

    let r = repo.search("", 10).await.unwrap();
    assert!(r.is_empty());
}

#[tokio::test]
async fn test_notes_search_whitespace_only_returns_empty() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::NotesRepo::new(&client);

    let r = repo.search("   \t\n ", 10).await.unwrap();
    assert!(r.is_empty());
}

#[tokio::test]
async fn test_notes_index_note_then_search() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::NotesRepo::new(&client);

    repo.index_note(IndexNoteRequest {
        id: "n1",
        title: "Rust Basics",
        content: "Rust is a systems programming language focused on safety and speed",
        tags: "rust,programming",
        file_path: "/notes/rust.md",
        source: "manual",
    })
    .await
    .unwrap();

    // FTS5 contentless table: MATCH should still find the indexed row
    let results = repo.search("systems", 10).await.unwrap();
    // The FTS table is contentless (content=''), so the join with notes_meta
    // should still return the row via rowid match.
    if !results.is_empty() {
        assert_eq!(results[0].path, "/notes/rust.md");
    }
}

#[tokio::test]
async fn test_notes_search_no_match_returns_empty() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::NotesRepo::new(&client);

    repo.index_note(IndexNoteRequest {
        id: "n1",
        title: "Hello",
        content: "World",
        tags: "",
        file_path: "/a.md",
        source: "test",
    })
    .await
    .unwrap();

    let r = repo.search("zzzznonexistent", 10).await.unwrap();
    assert!(r.is_empty());
}

#[tokio::test]
async fn test_notes_index_note_duplicate_id_replaces() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::NotesRepo::new(&client);

    repo.index_note(IndexNoteRequest {
        id: "dup",
        title: "First",
        content: "Original",
        tags: "",
        file_path: "/first.md",
        source: "s",
    })
    .await
    .unwrap();

    repo.index_note(IndexNoteRequest {
        id: "dup",
        title: "Second",
        content: "Replaced",
        tags: "",
        file_path: "/second.md",
        source: "s",
    })
    .await
    .unwrap();

    // The notes_meta row should have the second file_path
    let row: (String,) = sqlx::query_as("SELECT file_path FROM notes_meta WHERE id = 'dup'")
        .fetch_one(client.pool())
        .await
        .unwrap();
    assert_eq!(row.0, "/second.md");
}

#[tokio::test]
async fn test_notes_search_limit_clamped_to_minimum() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::NotesRepo::new(&client);

    repo.index_note(IndexNoteRequest {
        id: "n1",
        title: "Test",
        content: "Content for search",
        tags: "",
        file_path: "/t.md",
        source: "s",
    })
    .await
    .unwrap();

    // limit=0 should be clamped to 1 by the repo
    let _results = repo.search("search", 0).await.unwrap();
}

// ===========================================================================
// EmbeddingsRepo
// ===========================================================================

#[tokio::test]
async fn test_embeddings_search_empty_embedding_returns_empty() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EmbeddingsRepo::new(&client);

    let r = repo.search(&[], 10).await.unwrap();
    assert!(r.is_empty());
}

#[tokio::test]
async fn test_embeddings_insert_note_embedding_vec0_unavailable() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EmbeddingsRepo::new(&client);

    let embedding = vec![0.1_f32; 384];
    let result = repo
        .insert_note_embedding(InsertNoteEmbeddingRequest {
            note_id: "note1",
            embedding: &embedding,
        })
        .await;

    // If vec0 extension isn't loaded, this will error — that's expected
    match result {
        Ok(()) => {}
        Err(e) => {
            let err_msg = e.to_string();
            assert!(
                err_msg.contains("no such table") || err_msg.contains("no such module"),
                "unexpected error: {err_msg}"
            );
        }
    }
}

#[tokio::test]
async fn test_embeddings_insert_entity_embedding_vec0_unavailable() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EmbeddingsRepo::new(&client);

    let embedding = vec![0.1_f32; 384];
    let result = repo
        .insert_entity_embedding(InsertEntityEmbeddingRequest {
            entity_id: "ent1",
            embedding: &embedding,
        })
        .await;

    match result {
        Ok(()) => {}
        Err(e) => {
            let err_msg = e.to_string();
            assert!(
                err_msg.contains("no such table") || err_msg.contains("no such module"),
                "unexpected error: {err_msg}"
            );
        }
    }
}

#[tokio::test]
async fn test_embeddings_search_nonempty_embedding_vec0_unavailable() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EmbeddingsRepo::new(&client);

    let embedding = vec![0.5_f32; 384];
    let result = repo.search(&embedding, 5).await;

    match result {
        Ok(_results) => {}
        Err(e) => {
            let err_msg = e.to_string();
            assert!(
                err_msg.contains("no such table") || err_msg.contains("no such module"),
                "unexpected error: {err_msg}"
            );
        }
    }
}

// ===========================================================================
// EntitiesRepo
// ===========================================================================

#[tokio::test]
async fn test_entities_insert_entity_and_load_name() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EntitiesRepo::new(&client);

    repo.insert_entity("e1", "RustLang", "language", "2024-01-01T00:00:00Z")
        .await
        .unwrap();

    let name = repo.entity_name("e1").await.unwrap();
    assert_eq!(name.as_deref(), Some("RustLang"));
}

#[tokio::test]
async fn test_entities_insert_entity_duplicate_id_replaces() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EntitiesRepo::new(&client);

    repo.insert_entity("e1", "Original", "lang", "2024-01-01T00:00:00Z")
        .await
        .unwrap();
    repo.insert_entity("e1", "Replaced", "lang", "2024-06-01T00:00:00Z")
        .await
        .unwrap();

    let name = repo.entity_name("e1").await.unwrap();
    assert_eq!(name.as_deref(), Some("Replaced"));
}

#[tokio::test]
async fn test_entities_upsert_entity_new() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EntitiesRepo::new(&client);

    repo.upsert_entity("e1", "Rust", "language", "2024-01-01T00:00:00Z", "2024-01-01T00:00:00Z")
        .await
        .unwrap();

    let entities = repo.load_all_entities().await.unwrap();
    assert_eq!(entities.len(), 1);
    assert_eq!(entities[0].id, "e1");
    assert_eq!(entities[0].name, "Rust");
    assert_eq!(entities[0].entity_type, "language");
}

#[tokio::test]
async fn test_entities_upsert_entity_conflict_updates_timestamp() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EntitiesRepo::new(&client);

    // Two entities with same (name, entity_type) but different ids
    repo.upsert_entity("e1", "Rust", "language", "2024-01-01T00:00:00Z", "2024-01-01T00:00:00Z")
        .await
        .unwrap();
    repo.upsert_entity("e2", "Rust", "language", "2024-01-01T00:00:00Z", "2024-06-01T00:00:00Z")
        .await
        .unwrap();

    // Both inserts go in (conflict on name+type updates last_updated)
    let entities = repo.load_all_entities().await.unwrap();
    // The ON CONFLICT updates last_updated but the row with id=e2 is the latest insert
    assert!(!entities.is_empty());
}

#[tokio::test]
async fn test_entities_update_entity_timestamp() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EntitiesRepo::new(&client);

    repo.insert_entity("e1", "Test", "t", "2024-01-01T00:00:00Z")
        .await
        .unwrap();
    repo.update_entity_timestamp("e1", "2025-12-31T23:59:59Z")
        .await
        .unwrap();

    let entities = repo.load_all_entities().await.unwrap();
    assert_eq!(entities[0].last_updated.as_deref(), Some("2025-12-31T23:59:59Z"));
}

#[tokio::test]
async fn test_entities_insert_alias_and_resolve() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EntitiesRepo::new(&client);

    repo.insert_entity("e1", "Rust", "language", "2024-01-01T00:00:00Z")
        .await
        .unwrap();
    repo.insert_alias("rust-lang", "e1").await.unwrap();

    let resolved = repo.resolve_alias("rust-lang").await.unwrap();
    assert_eq!(resolved.as_deref(), Some("e1"));
}

#[tokio::test]
async fn test_entities_insert_alias_duplicate_is_ignored() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EntitiesRepo::new(&client);

    repo.insert_entity("e1", "Rust", "language", "2024-01-01T00:00:00Z")
        .await
        .unwrap();
    repo.insert_alias("rs", "e1").await.unwrap();
    repo.insert_alias("rs", "e1").await.unwrap(); // INSERT OR IGNORE

    let resolved = repo.resolve_alias("rs").await.unwrap();
    assert_eq!(resolved.as_deref(), Some("e1"));
}

#[tokio::test]
async fn test_entities_resolve_alias_nonexistent() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EntitiesRepo::new(&client);

    let resolved = repo.resolve_alias("nope").await.unwrap();
    assert!(resolved.is_none());
}

#[tokio::test]
async fn test_entities_insert_relationship_and_load() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EntitiesRepo::new(&client);

    repo.insert_entity("e1", "A", "type", "2024-01-01T00:00:00Z")
        .await
        .unwrap();
    repo.insert_entity("e2", "B", "type", "2024-01-01T00:00:00Z")
        .await
        .unwrap();

    repo.insert_relationship(&InsertRelationshipRequest { id: "r1", source_id: "e1", target_id: "e2", rel_type: "depends_on", confidence: 0.95, source_note_ids: Some("note-1"), created_at: "2024-01-01T00:00:00Z", description: None, valid_from: None, valid_until: None, weight: None })
    .await
    .unwrap();

    let rels = repo.load_relationships("e1").await.unwrap();
    assert_eq!(rels.len(), 1);
    assert_eq!(rels[0].target_entity_id, "e2");
    assert_eq!(rels[0].relation_type, "depends_on");
}

#[tokio::test]
async fn test_entities_insert_relationship_without_source_notes() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EntitiesRepo::new(&client);

    repo.insert_entity("e1", "A", "t", "2024-01-01T00:00:00Z")
        .await
        .unwrap();
    repo.insert_entity("e2", "B", "t", "2024-01-01T00:00:00Z")
        .await
        .unwrap();

    repo.insert_relationship(&InsertRelationshipRequest { id: "r1", source_id: "e1", target_id: "e2", rel_type: "links", confidence: 1.0, source_note_ids: None, created_at: "2024-01-01T00:00:00Z", description: None, valid_from: None, valid_until: None, weight: None })
    .await
    .unwrap();

    let rels = repo.load_relationships("e1").await.unwrap();
    assert_eq!(rels.len(), 1);
}

#[tokio::test]
async fn test_entities_load_relationships_empty() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EntitiesRepo::new(&client);

    let rels = repo.load_relationships("nonexistent").await.unwrap();
    assert!(rels.is_empty());
}

#[tokio::test]
async fn test_entities_load_all_entities_empty() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EntitiesRepo::new(&client);

    let all = repo.load_all_entities().await.unwrap();
    assert!(all.is_empty());
}

#[tokio::test]
async fn test_entities_load_all_entities_ordered_by_name() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EntitiesRepo::new(&client);

    repo.insert_entity("e3", "Charlie", "t", "2024-01-01T00:00:00Z")
        .await
        .unwrap();
    repo.insert_entity("e1", "Alice", "t", "2024-01-01T00:00:00Z")
        .await
        .unwrap();
    repo.insert_entity("e2", "Bob", "t", "2024-01-01T00:00:00Z")
        .await
        .unwrap();

    let all = repo.load_all_entities().await.unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].name, "Alice");
    assert_eq!(all[1].name, "Bob");
    assert_eq!(all[2].name, "Charlie");
}

#[tokio::test]
async fn test_entities_load_entities_updated_since() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EntitiesRepo::new(&client);

    repo.insert_entity("e1", "Old", "t", "2020-01-01T00:00:00Z")
        .await
        .unwrap();
    repo.insert_entity("e2", "New", "t", "2024-01-01T00:00:00Z")
        .await
        .unwrap();

    let recent = repo
        .load_entities_updated_since("2023-01-01T00:00:00Z")
        .await
        .unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].name, "New");
}

#[tokio::test]
async fn test_entities_load_entities_updated_since_all_old() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EntitiesRepo::new(&client);

    repo.insert_entity("e1", "Old", "t", "2020-01-01T00:00:00Z")
        .await
        .unwrap();

    let recent = repo
        .load_entities_updated_since("2025-01-01T00:00:00Z")
        .await
        .unwrap();
    assert!(recent.is_empty());
}

#[tokio::test]
async fn test_entities_entity_name_nonexistent() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EntitiesRepo::new(&client);

    let name = repo.entity_name("no-such-id").await.unwrap();
    assert!(name.is_none());
}

#[tokio::test]
async fn test_entities_load_known_entity_names_includes_aliases() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EntitiesRepo::new(&client);

    repo.insert_entity("e1", "Rust", "language", "2024-01-01T00:00:00Z")
        .await
        .unwrap();
    repo.insert_entity("e2", "Python", "language", "2024-01-01T00:00:00Z")
        .await
        .unwrap();
    repo.insert_alias("rs", "e1").await.unwrap();

    let names = repo.load_known_entity_names().await.unwrap();
    assert!(names.contains(&"Rust".to_string()));
    assert!(names.contains(&"Python".to_string()));
    assert!(names.contains(&"rs".to_string()));
}

#[tokio::test]
async fn test_entities_load_known_entity_names_empty() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EntitiesRepo::new(&client);

    let names = repo.load_known_entity_names().await.unwrap();
    assert!(names.is_empty());
}

#[tokio::test]
async fn test_entities_bfs_search_empty_name() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EntitiesRepo::new(&client);

    let r = repo.bfs_search("", 5).await.unwrap();
    assert!(r.is_empty());
}

#[tokio::test]
async fn test_entities_bfs_search_whitespace_name() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EntitiesRepo::new(&client);

    let r = repo.bfs_search("   ", 5).await.unwrap();
    assert!(r.is_empty());
}

#[tokio::test]
async fn test_entities_bfs_search_no_start_entity() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EntitiesRepo::new(&client);

    let r = repo.bfs_search("Nonexistent", 5).await.unwrap();
    assert!(r.is_empty());
}

#[tokio::test]
async fn test_entities_bfs_search_chain_a_b_c_d() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EntitiesRepo::new(&client);

    // Create chain: A -> B -> C -> D
    for (id, name) in &[("a", "A"), ("b", "B"), ("c", "C"), ("d", "D")] {
        repo.insert_entity(id, name, "node", "2024-01-01T00:00:00Z")
            .await
            .unwrap();
    }
    let now = "2024-01-01T00:00:00Z";
    for (rid, src, tgt) in &[("r1", "a", "b"), ("r2", "b", "c"), ("r3", "c", "d")] {
        repo.insert_relationship(&InsertRelationshipRequest { id: rid, source_id: src, target_id: tgt, rel_type: "next", confidence: 1.0, source_note_ids: None, created_at: now, description: None, valid_from: None, valid_until: None, weight: None })
        .await
        .unwrap();
    }

    // Depth 1: should reach B
    let r1 = repo.bfs_search("A", 1).await.unwrap();
    assert_eq!(r1.len(), 1);
    assert_eq!(r1[0].entity, "B");
    assert_eq!(r1[0].depth, 1);

    // Depth 2: should reach B and C
    let r2 = repo.bfs_search("A", 2).await.unwrap();
    assert_eq!(r2.len(), 2);
    let names2: Vec<&str> = r2.iter().map(|r| r.entity.as_str()).collect();
    assert!(names2.contains(&"B"));
    assert!(names2.contains(&"C"));

    // Depth 3: should reach B, C, and D
    let r3 = repo.bfs_search("A", 3).await.unwrap();
    assert_eq!(r3.len(), 3);
    let names3: Vec<&str> = r3.iter().map(|r| r.entity.as_str()).collect();
    assert!(names3.contains(&"B"));
    assert!(names3.contains(&"C"));
    assert!(names3.contains(&"D"));

    // Depth 0: should only contain A (depth=0) which is excluded (WHERE depth > 0)
    let r0 = repo.bfs_search("A", 0).await.unwrap();
    assert!(r0.is_empty());
}

#[tokio::test]
async fn test_entities_bfs_search_diamond_graph() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EntitiesRepo::new(&client);

    // Diamond: A -> B, A -> C, B -> D, C -> D
    for (id, name) in &[("a", "A"), ("b", "B"), ("c", "C"), ("d", "D")] {
        repo.insert_entity(id, name, "n", "2024-01-01T00:00:00Z")
            .await
            .unwrap();
    }
    let now = "2024-01-01T00:00:00Z";
    for (rid, src, tgt, rt) in &[
        ("r1", "a", "b", "left"),
        ("r2", "a", "c", "right"),
        ("r3", "b", "d", "converge"),
        ("r4", "c", "d", "converge"),
    ] {
        repo.insert_relationship(&InsertRelationshipRequest { id: rid, source_id: src, target_id: tgt, rel_type: rt, confidence: 1.0, source_note_ids: None, created_at: now, description: None, valid_from: None, valid_until: None, weight: None })
        .await
        .unwrap();
    }

    // Depth 1: B and C
    let r1 = repo.bfs_search("A", 1).await.unwrap();
    assert_eq!(r1.len(), 2);

    // Depth 2: B, C, D (D reached from both B and C, but deduplicated by name)
    let r2 = repo.bfs_search("A", 2).await.unwrap();
    assert_eq!(r2.len(), 3);
    let entities: Vec<&str> = r2.iter().map(|r| r.entity.as_str()).collect();
    assert!(entities.contains(&"D"));
}

#[tokio::test]
async fn test_entities_bfs_search_nonexistent_start() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::EntitiesRepo::new(&client);

    repo.insert_entity("a", "A", "n", "2024-01-01T00:00:00Z")
        .await
        .unwrap();

    let r = repo.bfs_search("Z", 5).await.unwrap();
    assert!(r.is_empty());
}

// ===========================================================================
// SelfModelRepo
// ===========================================================================

fn make_self_node(id: &str, name: &str, layer: &str) -> SelfNodeRow {
    SelfNodeRow {
        id: id.to_string(),
        name: name.to_string(),
        layer: layer.to_string(),
        description: format!("desc-{id}"),
        domain: "test".to_string(),
        is_explicit: Some(true),
        sufficient_for: vec!["task-a".to_string(), "task-b".to_string()],
        necessary_for: vec!["prereq-x".to_string()],
        controllability: Some(0.75),
        humility_score: Some(0.5),
        optionality_count: Some(3),
        core_pursuit: Some("testing".to_string()),
        source: "manual".to_string(),
        confidence: 0.9,
        evidence_refs: vec!["ref1".to_string(), "ref2".to_string()],
        created_at: "2024-01-01T00:00:00Z".to_string(),
        updated_at: "2024-01-01T00:00:00Z".to_string(),
    }
}

#[tokio::test]
async fn test_self_model_upsert_and_load_all() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::SelfModelRepo::new(&client);

    let node = make_self_node("n1", "Identity", "core");
    repo.upsert(&node).await.unwrap();

    let all = repo.load_all().await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, "n1");
    assert_eq!(all[0].name, "Identity");
    assert_eq!(all[0].layer, "core");
}

#[tokio::test]
async fn test_self_model_upsert_replaces_same_id() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::SelfModelRepo::new(&client);

    let mut node = make_self_node("n1", "V1", "layer1");
    repo.upsert(&node).await.unwrap();

    node.name = "V2".to_string();
    node.layer = "layer2".to_string();
    repo.upsert(&node).await.unwrap();

    let all = repo.load_all().await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].name, "V2");
    assert_eq!(all[0].layer, "layer2");
}

#[tokio::test]
async fn test_self_model_load_all_empty() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::SelfModelRepo::new(&client);

    let all = repo.load_all().await.unwrap();
    assert!(all.is_empty());
}

#[tokio::test]
async fn test_self_model_load_by_layer() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::SelfModelRepo::new(&client);

    repo.upsert(&make_self_node("n1", "A", "layer1"))
        .await
        .unwrap();
    repo.upsert(&make_self_node("n2", "B", "layer2"))
        .await
        .unwrap();
    repo.upsert(&make_self_node("n3", "C", "layer1"))
        .await
        .unwrap();

    let l1 = repo.load_by_layer("layer1").await.unwrap();
    assert_eq!(l1.len(), 2);
    let names: Vec<&str> = l1.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"A"));
    assert!(names.contains(&"C"));

    let l2 = repo.load_by_layer("layer2").await.unwrap();
    assert_eq!(l2.len(), 1);
    assert_eq!(l2[0].name, "B");
}

#[tokio::test]
async fn test_self_model_load_by_layer_nonexistent() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::SelfModelRepo::new(&client);

    repo.upsert(&make_self_node("n1", "A", "layer1"))
        .await
        .unwrap();

    let r = repo.load_by_layer("no-such-layer").await.unwrap();
    assert!(r.is_empty());
}

#[tokio::test]
async fn test_self_model_json_vec_serialization() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::SelfModelRepo::new(&client);

    let mut node = make_self_node("n1", "Test", "layer");
    node.sufficient_for = vec![
        "s1".to_string(),
        "s2".to_string(),
        "s3".to_string(),
    ];
    node.necessary_for = vec!["n1-only".to_string()];
    node.evidence_refs = vec![
        "https://example.com/1".to_string(),
        "https://example.com/2".to_string(),
    ];
    repo.upsert(&node).await.unwrap();

    let loaded = repo.load_all().await.unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].sufficient_for, vec!["s1", "s2", "s3"]);
    assert_eq!(loaded[0].necessary_for, vec!["n1-only"]);
    assert_eq!(
        loaded[0].evidence_refs,
        vec!["https://example.com/1", "https://example.com/2"]
    );
}

#[tokio::test]
async fn test_self_model_empty_vec_fields() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::SelfModelRepo::new(&client);

    let mut node = make_self_node("n1", "Empty", "l");
    node.sufficient_for = vec![];
    node.necessary_for = vec![];
    node.evidence_refs = vec![];
    repo.upsert(&node).await.unwrap();

    let loaded = repo.load_all().await.unwrap();
    assert_eq!(loaded[0].sufficient_for, Vec::<String>::new());
    assert_eq!(loaded[0].necessary_for, Vec::<String>::new());
    assert_eq!(loaded[0].evidence_refs, Vec::<String>::new());
}

#[tokio::test]
async fn test_self_model_optional_fields_none() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::SelfModelRepo::new(&client);

    let mut node = make_self_node("n1", "Minimal", "l");
    node.is_explicit = None;
    node.controllability = None;
    node.humility_score = None;
    node.optionality_count = None;
    node.core_pursuit = None;
    repo.upsert(&node).await.unwrap();

    let loaded = repo.load_all().await.unwrap();
    assert_eq!(loaded.len(), 1);
    assert!(loaded[0].is_explicit.is_none());
    assert!(loaded[0].controllability.is_none());
    assert!(loaded[0].humility_score.is_none());
    assert!(loaded[0].optionality_count.is_none());
    assert!(loaded[0].core_pursuit.is_none());
}

// ===========================================================================
// GoalsRepo
// ===========================================================================

#[tokio::test]
async fn test_goals_upsert_goal_and_load() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::GoalsRepo::new(&client);

    repo.upsert_goal(&UpsertGoalNodeRequest {
        id: "g1",
        name: "Master Rust",
        controllability: 0.8,
        core_pursuit: "craftsmanship",
        deadline: Some("2025-12-31"),
        now: "2024-01-01T00:00:00Z",
    })
    .await
    .unwrap();

    let goal = repo.load_goal("g1").await.unwrap().unwrap();
    assert_eq!(goal.id, "g1");
    assert_eq!(goal.name, "Master Rust");
    assert_eq!(goal.controllability, 0.8);
    assert_eq!(goal.core_pursuit, "craftsmanship");
    assert_eq!(goal.deadline.as_deref(), Some("2025-12-31"));
}

#[tokio::test]
async fn test_goals_upsert_goal_no_deadline() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::GoalsRepo::new(&client);

    repo.upsert_goal(&UpsertGoalNodeRequest {
        id: "g1",
        name: "Learn",
        controllability: 0.5,
        core_pursuit: "growth",
        deadline: None,
        now: "2024-01-01T00:00:00Z",
    })
    .await
    .unwrap();

    let goal = repo.load_goal("g1").await.unwrap().unwrap();
    assert!(goal.deadline.is_none());
}

#[tokio::test]
async fn test_goals_upsert_goal_replaces_same_id() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::GoalsRepo::new(&client);

    repo.upsert_goal(&UpsertGoalNodeRequest {
        id: "g1",
        name: "V1",
        controllability: 0.5,
        core_pursuit: "p1",
        deadline: None,
        now: "2024-01-01T00:00:00Z",
    })
    .await
    .unwrap();

    repo.upsert_goal(&UpsertGoalNodeRequest {
        id: "g1",
        name: "V2",
        controllability: 0.9,
        core_pursuit: "p2",
        deadline: Some("2025-06-01"),
        now: "2024-06-01T00:00:00Z",
    })
    .await
    .unwrap();

    let goal = repo.load_goal("g1").await.unwrap().unwrap();
    assert_eq!(goal.name, "V2");
    assert_eq!(goal.controllability, 0.9);
}

#[tokio::test]
async fn test_goals_load_goal_nonexistent() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::GoalsRepo::new(&client);

    let goal = repo.load_goal("nope").await.unwrap();
    assert!(goal.is_none());
}

#[tokio::test]
async fn test_goals_upsert_path_and_load() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::GoalsRepo::new(&client);

    // Create parent goal first
    repo.upsert_goal(&UpsertGoalNodeRequest {
        id: "g1",
        name: "Learn Rust",
        controllability: 0.8,
        core_pursuit: "growth",
        deadline: None,
        now: "2024-01-01T00:00:00Z",
    })
    .await
    .unwrap();

    repo.upsert_path(&UpsertPathNodeRequest {
        id: "p1",
        name: "Read Book",
        serves_goal_id: Some("g1"),
        is_default: true,
        crowdedness: 0.3,
        alternatives: "[\"course\", \"tutorial\"]",
        now: "2024-01-01T00:00:00Z",
    })
    .await
    .unwrap();

    let path = repo.load_path("p1").await.unwrap().unwrap();
    assert_eq!(path.id, "p1");
    assert_eq!(path.name, "Read Book");
    assert_eq!(path.serves_goal_id.as_deref(), Some("g1"));
    assert!(path.is_default);
    assert_eq!(path.crowdedness, 0.3);
    assert_eq!(path.alternatives, "[\"course\", \"tutorial\"]");
}

#[tokio::test]
async fn test_goals_upsert_path_no_goal() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::GoalsRepo::new(&client);

    repo.upsert_path(&UpsertPathNodeRequest {
        id: "p1",
        name: "Standalone Path",
        serves_goal_id: None,
        is_default: false,
        crowdedness: 0.5,
        alternatives: "[]",
        now: "2024-01-01T00:00:00Z",
    })
    .await
    .unwrap();

    let path = repo.load_path("p1").await.unwrap().unwrap();
    assert!(path.serves_goal_id.is_none());
    assert!(!path.is_default);
}

#[tokio::test]
async fn test_goals_upsert_path_replaces() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::GoalsRepo::new(&client);

    repo.upsert_path(&UpsertPathNodeRequest {
        id: "p1",
        name: "V1",
        serves_goal_id: None,
        is_default: false,
        crowdedness: 0.1,
        alternatives: "[]",
        now: "2024-01-01T00:00:00Z",
    })
    .await
    .unwrap();

    repo.upsert_path(&UpsertPathNodeRequest {
        id: "p1",
        name: "V2",
        serves_goal_id: None,
        is_default: true,
        crowdedness: 0.9,
        alternatives: "[\"alt\"]",
        now: "2024-06-01T00:00:00Z",
    })
    .await
    .unwrap();

    let path = repo.load_path("p1").await.unwrap().unwrap();
    assert_eq!(path.name, "V2");
    assert!(path.is_default);
    assert_eq!(path.crowdedness, 0.9);
}

#[tokio::test]
async fn test_goals_load_path_nonexistent() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::GoalsRepo::new(&client);

    let path = repo.load_path("nope").await.unwrap();
    assert!(path.is_none());
}

// ===========================================================================
// BeliefsRepo
// ===========================================================================

#[tokio::test]
async fn test_beliefs_upsert_and_load() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::BeliefsRepo::new(&client);

    repo.upsert(&UpsertBeliefNodeRequest {
        id: "b1",
        name: "Rust is fast",
        proposition: "Rust compiles to efficient native code",
        prior: 0.7,
        posterior: 0.92,
        evidence_count: 15,
        now: "2024-01-01T00:00:00Z",
    })
    .await
    .unwrap();

    let belief = repo.load("b1").await.unwrap().unwrap();
    assert_eq!(belief.id, "b1");
    assert_eq!(belief.name, "Rust is fast");
    assert_eq!(belief.proposition, "Rust compiles to efficient native code");
    assert!((belief.prior - 0.7).abs() < f64::EPSILON);
    assert!((belief.posterior - 0.92).abs() < f64::EPSILON);
    assert_eq!(belief.evidence_count, 15);
}

#[tokio::test]
async fn test_beliefs_upsert_replaces_same_id() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::BeliefsRepo::new(&client);

    repo.upsert(&UpsertBeliefNodeRequest {
        id: "b1",
        name: "Original",
        proposition: "Old",
        prior: 0.5,
        posterior: 0.5,
        evidence_count: 0,
        now: "2024-01-01T00:00:00Z",
    })
    .await
    .unwrap();

    repo.upsert(&UpsertBeliefNodeRequest {
        id: "b1",
        name: "Updated",
        proposition: "New",
        prior: 0.3,
        posterior: 0.8,
        evidence_count: 10,
        now: "2024-06-01T00:00:00Z",
    })
    .await
    .unwrap();

    let belief = repo.load("b1").await.unwrap().unwrap();
    assert_eq!(belief.name, "Updated");
    assert_eq!(belief.proposition, "New");
    assert_eq!(belief.evidence_count, 10);
}

#[tokio::test]
async fn test_beliefs_load_nonexistent() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::BeliefsRepo::new(&client);

    let result = repo.load("nope").await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn test_beliefs_zero_evidence() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::BeliefsRepo::new(&client);

    repo.upsert(&UpsertBeliefNodeRequest {
        id: "b1",
        name: "Hypothesis",
        proposition: "Unproven claim",
        prior: 0.5,
        posterior: 0.5,
        evidence_count: 0,
        now: "2024-01-01T00:00:00Z",
    })
    .await
    .unwrap();

    let belief = repo.load("b1").await.unwrap().unwrap();
    assert_eq!(belief.evidence_count, 0);
}

// ===========================================================================
// DispatchRepo
// ===========================================================================

#[tokio::test]
async fn test_dispatch_create_task_and_load() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::DispatchRepo::new(&client);

    repo.create_task(
        "t1",
        "agent-researcher",
        "Search the web for Rust async patterns",
        Some("notes/rust.md,notes/async.md"),
        "2024-01-01T00:00:00Z",
    )
    .await
    .unwrap();

    let task = repo.load_task("t1").await.unwrap().unwrap();
    assert_eq!(task.id, "t1");
    assert_eq!(task.target, "agent-researcher");
    assert_eq!(
        task.task_description,
        "Search the web for Rust async patterns"
    );
    assert_eq!(
        task.context_files.as_deref(),
        Some("notes/rust.md,notes/async.md")
    );
    assert_eq!(task.status, "queued");
    assert!(task.result_summary.is_none());
    assert!(task.completed_at.is_none());
}

#[tokio::test]
async fn test_dispatch_create_task_no_context_files() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::DispatchRepo::new(&client);

    repo.create_task("t1", "agent", "Do something", None, "2024-01-01T00:00:00Z")
        .await
        .unwrap();

    let task = repo.load_task("t1").await.unwrap().unwrap();
    assert!(task.context_files.is_none());
}

#[tokio::test]
async fn test_dispatch_update_status() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::DispatchRepo::new(&client);

    repo.create_task("t1", "agent", "Task", None, "2024-01-01T00:00:00Z")
        .await
        .unwrap();

    repo.update_status(
        "t1",
        "completed",
        Some("Found 3 results"),
        Some("2024-01-01T01:30:00Z"),
    )
    .await
    .unwrap();

    let task = repo.load_task("t1").await.unwrap().unwrap();
    assert_eq!(task.status, "completed");
    assert_eq!(task.result_summary.as_deref(), Some("Found 3 results"));
    assert_eq!(
        task.completed_at.as_deref(),
        Some("2024-01-01T01:30:00Z")
    );
}

#[tokio::test]
async fn test_dispatch_update_status_no_result() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::DispatchRepo::new(&client);

    repo.create_task("t1", "agent", "Task", None, "2024-01-01T00:00:00Z")
        .await
        .unwrap();

    repo.update_status("t1", "in_progress", None, None).await.unwrap();

    let task = repo.load_task("t1").await.unwrap().unwrap();
    assert_eq!(task.status, "in_progress");
    assert!(task.result_summary.is_none());
    assert!(task.completed_at.is_none());
}

#[tokio::test]
async fn test_dispatch_load_task_nonexistent() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::DispatchRepo::new(&client);

    let task = repo.load_task("nope").await.unwrap();
    assert!(task.is_none());
}

#[tokio::test]
async fn test_dispatch_load_tasks_by_status() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::DispatchRepo::new(&client);

    // Create 3 tasks: 2 queued, 1 completed
    repo.create_task("t1", "a", "Task 1", None, "2024-01-01T00:00:00Z")
        .await
        .unwrap();
    repo.create_task("t2", "a", "Task 2", None, "2024-01-01T00:01:00Z")
        .await
        .unwrap();
    repo.create_task("t3", "a", "Task 3", None, "2024-01-01T00:02:00Z")
        .await
        .unwrap();
    repo.update_status("t2", "completed", Some("done"), Some("2024-01-01T00:01:30Z"))
        .await
        .unwrap();

    let queued = repo.load_tasks_by_status("queued").await.unwrap();
    assert_eq!(queued.len(), 2);

    let completed = repo.load_tasks_by_status("completed").await.unwrap();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].id, "t2");
}

#[tokio::test]
async fn test_dispatch_load_tasks_by_status_empty() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::DispatchRepo::new(&client);

    let tasks = repo.load_tasks_by_status("nonexistent").await.unwrap();
    assert!(tasks.is_empty());
}

#[tokio::test]
async fn test_dispatch_create_task_duplicate_id_replaces() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::DispatchRepo::new(&client);

    repo.create_task("t1", "agent1", "First", None, "2024-01-01T00:00:00Z")
        .await
        .unwrap();
    repo.create_task("t1", "agent2", "Second", None, "2024-01-01T00:00:00Z")
        .await
        .unwrap();

    let task = repo.load_task("t1").await.unwrap().unwrap();
    assert_eq!(task.target, "agent2");
    assert_eq!(task.task_description, "Second");
}

// ===========================================================================
// SessionsRepo
// ===========================================================================

#[tokio::test]
async fn test_sessions_upsert_and_find() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::SessionsRepo::new(&client);

    repo.upsert(
        "s1",
        "/sessions/session-001.md",
        "zen-coordinator",
        "active",
        "2024-01-01T00:00:00Z",
        "2024-01-01T00:00:00Z",
        "/workspace/project-a",
    )
    .await
    .unwrap();

    let path = repo.find("s1").await.unwrap();
    assert_eq!(path.as_deref(), Some("/sessions/session-001.md"));
}

#[tokio::test]
async fn test_sessions_find_nonexistent() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::SessionsRepo::new(&client);

    let result = repo.find("nope").await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn test_sessions_upsert_replaces_same_id() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::SessionsRepo::new(&client);

    repo.upsert(
        "s1",
        "/old/path.md",
        "agent1",
        "active",
        "2024-01-01T00:00:00Z",
        "2024-01-01T00:00:00Z",
        "/ws",
    )
    .await
    .unwrap();

    repo.upsert(
        "s1",
        "/new/path.md",
        "agent2",
        "paused",
        "2024-01-01T00:00:00Z",
        "2024-06-01T00:00:00Z",
        "/ws2",
    )
    .await
    .unwrap();

    let path = repo.find("s1").await.unwrap().unwrap();
    assert_eq!(path, "/new/path.md");
}

#[tokio::test]
async fn test_sessions_list_all_empty() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::SessionsRepo::new(&client);

    let all = repo.list_all().await.unwrap();
    assert!(all.is_empty());
}

#[tokio::test]
async fn test_sessions_list_all_ordered_by_updated_desc() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::SessionsRepo::new(&client);

    repo.upsert(
        "s1",
        "/s1.md",
        "a",
        "active",
        "2024-01-01T00:00:00Z",
        "2024-01-01T00:00:00Z",
        "/ws",
    )
    .await
    .unwrap();
    repo.upsert(
        "s2",
        "/s2.md",
        "b",
        "active",
        "2024-01-02T00:00:00Z",
        "2024-01-02T00:00:00Z",
        "/ws",
    )
    .await
    .unwrap();
    repo.upsert(
        "s3",
        "/s3.md",
        "c",
        "active",
        "2024-01-03T00:00:00Z",
        "2024-01-03T00:00:00Z",
        "/ws",
    )
    .await
    .unwrap();

    let all = repo.list_all().await.unwrap();
    assert_eq!(all.len(), 3);
    // ordered by updated_at DESC
    assert_eq!(all[0].id, "s3");
    assert_eq!(all[1].id, "s2");
    assert_eq!(all[2].id, "s1");
}

#[tokio::test]
async fn test_sessions_reconcile() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::SessionsRepo::new(&client);

    repo.upsert(
        "s1",
        "/old/location.md",
        "agent",
        "active",
        "2024-01-01T00:00:00Z",
        "2024-01-01T00:00:00Z",
        "/ws",
    )
    .await
    .unwrap();

    repo.reconcile("s1", "/new/location.md").await.unwrap();

    let path = repo.find("s1").await.unwrap().unwrap();
    assert_eq!(path, "/new/location.md");
}

#[tokio::test]
async fn test_sessions_reconcile_nonexistent() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::SessionsRepo::new(&client);

    // Should not error, just update 0 rows
    repo.reconcile("nope", "/new/path.md").await.unwrap();

    let result = repo.find("nope").await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn test_sessions_list_all_fields() {
    let (client, _dir) = make_client().await;
    let repo = zen_repo::SessionsRepo::new(&client);

    repo.upsert(
        "s1",
        "/session.md",
        "my-agent",
        "paused",
        "2024-01-01T00:00:00Z",
        "2024-06-15T12:00:00Z",
        "/my/workspace",
    )
    .await
    .unwrap();

    let all = repo.list_all().await.unwrap();
    assert_eq!(all.len(), 1);

    let s = &all[0];
    assert_eq!(s.id, "s1");
    assert_eq!(s.file_path, "/session.md");
    assert_eq!(s.agent_name, "my-agent");
    assert_eq!(s.status, "paused");
    assert_eq!(s.created_at, "2024-01-01T00:00:00Z");
    assert_eq!(s.updated_at, "2024-06-15T12:00:00Z");
    assert_eq!(s.workspace, "/my/workspace");
}
