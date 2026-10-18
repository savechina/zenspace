//! Reindex data-bloat regressions: the DB-backed change gate must survive a
//! fresh `Reindexer` (production constructs one per cycle), `iterations/`
//! page-version snapshots must be excluded from the walk, and
//! `rebuild_fts_indexes` must run the contentless-correct `delete-all`
//! command (then a forced full reindex must repopulate).

use std::path::Path;

use zen_repo::SqliteClient;
use zen_vault::tindy::Reindexer;

fn write_note(root: &Path, rel: &str, id: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let content = format!("---\nid: {id}\ntags: [test]\n---\n\n{body}\n");
    std::fs::write(&path, content).unwrap();
}

async fn fts_row_count(db: &Path) -> i64 {
    let client = SqliteClient::open(db).await.unwrap();
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM notes_fts")
        .fetch_one(client.pool())
        .await
        .unwrap()
}

async fn indexed_paths(db: &Path) -> Vec<String> {
    let client = SqliteClient::open(db).await.unwrap();
    sqlx::query_scalar::<_, String>("SELECT file_path FROM notes_meta")
        .fetch_all(client.pool())
        .await
        .unwrap()
}

async fn indexed_ids(db: &Path) -> Vec<String> {
    let client = SqliteClient::open(db).await.unwrap();
    sqlx::query_scalar::<_, String>("SELECT id FROM notes_meta ORDER BY id")
        .fetch_all(client.pool())
        .await
        .unwrap()
}

async fn content_hash_of(db: &Path, id: &str) -> String {
    let client = SqliteClient::open(db).await.unwrap();
    sqlx::query_scalar("SELECT content_hash FROM notes_meta WHERE id = ?1")
        .bind(id)
        .fetch_one(client.pool())
        .await
        .unwrap()
}

async fn embedded_hash_of(db: &Path, id: &str) -> String {
    let client = SqliteClient::open(db).await.unwrap();
    sqlx::query_scalar("SELECT embedded_hash FROM notes_meta WHERE id = ?1")
        .bind(id)
        .fetch_one(client.pool())
        .await
        .unwrap()
}

async fn embedding_row_count(db: &Path) -> i64 {
    let client = SqliteClient::open(db).await.unwrap();
    sqlx::query_scalar("SELECT COUNT(*) FROM note_embeddings")
        .fetch_one(client.pool())
        .await
        .unwrap()
}

async fn embedding_blob(db: &Path, note_id: &str) -> Vec<u8> {
    let client = SqliteClient::open(db).await.unwrap();
    sqlx::query_scalar("SELECT embedding FROM note_embeddings WHERE note_id = ?1")
        .bind(note_id)
        .fetch_one(client.pool())
        .await
        .unwrap()
}

async fn fresh_embedded_count(db: &Path) -> i64 {
    let client = SqliteClient::open(db).await.unwrap();
    sqlx::query_scalar(
        "SELECT count(*) FROM notes_meta \
         WHERE embedded_hash != '' AND embedded_hash = content_hash",
    )
    .fetch_one(client.pool())
    .await
    .unwrap()
}

async fn dirty_rows_with_vector(db: &Path) -> i64 {
    let client = SqliteClient::open(db).await.unwrap();
    sqlx::query_scalar(
        "SELECT count(*) FROM notes_meta \
         WHERE embedded_hash != content_hash \
           AND id IN (SELECT note_id FROM note_embeddings)",
    )
    .fetch_one(client.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn second_fresh_run_over_unchanged_dir_reports_zero_updates() {
    let tmp = tempfile::tempdir().unwrap();
    let vault = tmp.path().join("vault");
    std::fs::create_dir_all(&vault).unwrap();
    write_note(&vault, "note-a.md", "note-a", "alpha body");
    write_note(&vault, "note-b.md", "note-b", "beta body");
    let db = tmp.path().join("state.db");

    let first_client = SqliteClient::open(&db).await.unwrap();
    let first = Reindexer::with_client(first_client).without_embeddings();
    let report_one = first.reindex(&vault).await.unwrap();
    assert_eq!(report_one.files_scanned, 2);
    assert_eq!(report_one.files_updated, 2);
    assert_eq!(report_one.files_unchanged, 0);
    drop(first);

    let second_client = SqliteClient::open(&db).await.unwrap();
    let second = Reindexer::with_client(second_client).without_embeddings();
    let report_two = second.reindex(&vault).await.unwrap();
    assert_eq!(report_two.files_scanned, 2);
    assert_eq!(
        report_two.files_updated, 0,
        "a fresh runner must skip unchanged files via the DB-backed gate"
    );
    assert_eq!(report_two.files_unchanged, 2);
}

#[tokio::test]
async fn iterations_directory_is_excluded_from_the_walk() {
    let tmp = tempfile::tempdir().unwrap();
    let vault = tmp.path().join("vault");
    std::fs::create_dir_all(&vault).unwrap();
    write_note(&vault, "note-a.md", "note-a", "alpha body");
    write_note(
        &vault,
        "iterations/technology/load-note-11.md",
        "snapshot",
        "derived page-version snapshot",
    );
    let db = tmp.path().join("state.db");

    let client = SqliteClient::open(&db).await.unwrap();
    let reindexer = Reindexer::with_client(client).without_embeddings();
    let report = reindexer.reindex(&vault).await.unwrap();

    assert_eq!(
        report.files_scanned, 1,
        "only the real note is eligible; iterations snapshots are pruned"
    );
    assert_eq!(report.files_updated, 1);

    let paths = indexed_paths(&db).await;
    assert!(
        paths.iter().any(|p| p.ends_with("note-a.md")),
        "the real note must be indexed, got {paths:?}"
    );
    assert!(
        paths.iter().all(|p| !p.contains("iterations")),
        "no iterations path may reach notes_meta, got {paths:?}"
    );
}

#[tokio::test]
async fn rebuild_fts_then_forced_full_reindex_repopulates_index() {
    let tmp = tempfile::tempdir().unwrap();
    let vault = tmp.path().join("vault");
    std::fs::create_dir_all(&vault).unwrap();
    write_note(&vault, "note-a.md", "note-a", "alpha unique term");
    write_note(&vault, "note-b.md", "note-b", "beta unique term");
    let db = tmp.path().join("state.db");

    let populate_client = SqliteClient::open(&db).await.unwrap();
    let populate = Reindexer::with_client(populate_client).without_embeddings();
    assert_eq!(populate.reindex(&vault).await.unwrap().files_updated, 2);
    drop(populate);
    assert_eq!(fts_row_count(&db).await, 2);

    let reset_client = SqliteClient::open(&db).await.unwrap();
    let reset = Reindexer::with_client(reset_client).without_embeddings();
    reset.rebuild_fts_indexes().await.unwrap();
    drop(reset);
    assert_eq!(
        fts_row_count(&db).await,
        0,
        "contentless 'delete-all' must empty notes_fts"
    );

    let gated_client = SqliteClient::open(&db).await.unwrap();
    let gated = Reindexer::with_client(gated_client).without_embeddings();
    assert_eq!(
        gated.reindex(&vault).await.unwrap().files_updated,
        0,
        "stored content_hash values persist, so a gated run skips every file"
    );
    drop(gated);
    assert_eq!(
        fts_row_count(&db).await,
        0,
        "the gated run left search broken — this is why force_full exists"
    );

    let forced_client = SqliteClient::open(&db).await.unwrap();
    let forced = Reindexer::with_client(forced_client)
        .without_embeddings()
        .force_full();
    assert_eq!(forced.reindex(&vault).await.unwrap().files_updated, 2);
    drop(forced);
    assert_eq!(
        fts_row_count(&db).await,
        2,
        "the forced full reindex must repopulate the emptied index"
    );
}

#[tokio::test]
async fn embedding_enabled_reindex_refreshes_a_dirty_vector_without_rewriting_content() {
    // Host-agnostic: on a degraded (hash-fallback) provider the vector is
    // written but the row stays dirty, so the freshness/no-op expectations are
    // derived from one probe embed rather than assuming a working provider.
    let (_, probe_degraded) =
        zen_vault::tindy::embeddings::compute_embeddings_for_text_tagged("refresh probe").unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let vault = tmp.path().join("vault");
    std::fs::create_dir_all(&vault).unwrap();
    write_note(&vault, "note-a.md", "note-a", "alpha body v1");
    let db = tmp.path().join("state.db");

    // 1. Embedding-enabled initial index: vector built; the row is marked fresh
    //    only for a real (non-degraded) embedding.
    let first_client = SqliteClient::open(&db).await.unwrap();
    let first = Reindexer::with_client(first_client)
        .reindex(&vault)
        .await
        .unwrap();
    assert_eq!(
        first.files_embedded, 1,
        "the initial run must embed the file"
    );
    let v1_hash = content_hash_of(&db, "note-a").await;
    assert!(!v1_hash.is_empty());
    let v1_embedded = embedded_hash_of(&db, "note-a").await;
    let fresh_after_first = if probe_degraded { "" } else { v1_hash.as_str() };
    assert_eq!(
        v1_embedded, fresh_after_first,
        "a degraded vector is written but not marked fresh (probe degraded={probe_degraded})"
    );
    assert_eq!(embedding_row_count(&db).await, 1);
    let vector_v1 = embedding_blob(&db, "note-a").await;

    // 2. without_embeddings over unchanged content: no writes, no spurious dirty.
    let second_client = SqliteClient::open(&db).await.unwrap();
    let second = Reindexer::with_client(second_client)
        .without_embeddings()
        .reindex(&vault)
        .await
        .unwrap();
    assert_eq!(second.files_updated, 0);
    assert_eq!(second.files_unchanged, 1);
    assert_eq!(
        embedded_hash_of(&db, "note-a").await,
        v1_embedded,
        "an unchanged reindex must not touch the stored freshness marker"
    );

    // 3. Change the body, then run the 5-min (no-embedding) path: the content
    //    gate advances while the vector goes dirty and stays stale.
    write_note(&vault, "note-a.md", "note-a", "alpha body v2 rewritten");
    let third_client = SqliteClient::open(&db).await.unwrap();
    let third = Reindexer::with_client(third_client)
        .without_embeddings()
        .reindex(&vault)
        .await
        .unwrap();
    assert_eq!(third.files_updated, 1);
    assert_eq!(third.files_embedded, 0);
    let v2_hash = content_hash_of(&db, "note-a").await;
    assert_ne!(v2_hash, v1_hash);
    assert_eq!(
        embedded_hash_of(&db, "note-a").await,
        "",
        "re-indexing without embeddings must mark the vector dirty"
    );
    assert_eq!(
        embedding_row_count(&db).await,
        1,
        "the stale vector is still present, just unmarked"
    );
    assert_eq!(
        embedding_blob(&db, "note-a").await,
        vector_v1,
        "the no-embedding path must leave the stored vector bytes untouched"
    );

    // 4. Embedding-enabled reindex: content hash already matches, yet the
    //    stale vector is refreshed and counted separately.
    let fourth_client = SqliteClient::open(&db).await.unwrap();
    let fourth = Reindexer::with_client(fourth_client)
        .reindex(&vault)
        .await
        .unwrap();
    assert_eq!(fourth.files_updated, 0, "content was already indexed");
    assert_eq!(
        fourth.files_reembedded, 1,
        "the gate must detect and repair the stale vector"
    );
    assert_eq!(fourth.files_unchanged, 0);
    assert_eq!(
        embedded_hash_of(&db, "note-a").await,
        if probe_degraded { "" } else { v2_hash.as_str() },
        "only a non-degraded refresh may mark the vector fresh"
    );
    assert_ne!(
        embedding_blob(&db, "note-a").await,
        vector_v1,
        "the refresh must actually rewrite the vec0 vector"
    );

    // 5. A further embedding-enabled run: a clean no-op with a real provider,
    //    a repeated (retryable) refresh while degraded.
    let fifth_client = SqliteClient::open(&db).await.unwrap();
    let fifth = Reindexer::with_client(fifth_client)
        .reindex(&vault)
        .await
        .unwrap();
    if probe_degraded {
        assert_eq!(fifth.files_reembedded, 1);
        assert_eq!(fifth.files_unchanged, 0);
    } else {
        assert_eq!(fifth.files_reembedded, 0);
        assert_eq!(fifth.files_unchanged, 1);
    }
}

#[tokio::test]
async fn degraded_embedding_counter_is_wired_to_reality() {
    // Deliberately host-agnostic: a box with a working onnxruntime + cached
    // model embeds natively, so the expectation is derived from one probe
    // embed rather than asserting this host lacks a provider.
    let (_, probe_degraded) =
        zen_vault::tindy::embeddings::compute_embeddings_for_text_tagged("degradation probe")
            .unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let vault = tmp.path().join("vault");
    std::fs::create_dir_all(&vault).unwrap();
    write_note(&vault, "note-a.md", "note-a", "alpha body for degradation");
    write_note(&vault, "note-b.md", "note-b", "beta body for degradation");
    let db = tmp.path().join("state.db");

    // 1. Initial embedding-enabled index: the counter must reflect reality.
    let client = SqliteClient::open(&db).await.unwrap();
    let report = Reindexer::with_client(client)
        .reindex(&vault)
        .await
        .unwrap();
    assert!(
        report.files_embedded > 0,
        "the run must embed at least one file, got {report:?}"
    );
    let expected_degraded = if probe_degraded {
        report.files_embedded
    } else {
        0
    };
    assert_eq!(
        report.embeddings_degraded, expected_degraded,
        "the degraded count must match what the provider chain actually did \
         (probe says degraded={probe_degraded}), not a hardcoded zero: {report:?}"
    );

    // 2. Freshness invariant on the initial index: a degraded vector is written
    //    but must NOT be marked fresh, so only the non-degraded embedded rows
    //    read as fresh. Host-independent.
    assert_eq!(
        fresh_embedded_count(&db).await,
        (report.files_embedded - report.embeddings_degraded) as i64,
        "every embedded-but-not-degraded row must be fresh, and no degraded \
         row may be fresh: {report:?}"
    );

    // 3. A re-embedded id (vector-only refresh) obeys the same rule: after
    //    rewriting note-a's body, an embedding-enabled run refreshes the
    //    vector but only marks it fresh when the provider was real.
    write_note(&vault, "note-a.md", "note-a", "alpha body rewritten");
    let no_embed_client = SqliteClient::open(&db).await.unwrap();
    let no_embed = Reindexer::with_client(no_embed_client)
        .without_embeddings()
        .reindex(&vault)
        .await
        .unwrap();
    assert_eq!(no_embed.files_updated, 1);
    assert_eq!(no_embed.files_embedded, 0);
    assert_eq!(
        embedded_hash_of(&db, "note-a").await,
        "",
        "the no-embedding path must dirty the vector"
    );

    let refresh_client = SqliteClient::open(&db).await.unwrap();
    let refresh = Reindexer::with_client(refresh_client)
        .reindex(&vault)
        .await
        .unwrap();
    assert!(
        refresh.files_reembedded >= 1,
        "the stale note-a vector must be rewritten: {refresh:?}"
    );
    let note_a_dirty =
        embedded_hash_of(&db, "note-a").await != content_hash_of(&db, "note-a").await;
    assert_eq!(
        note_a_dirty, probe_degraded,
        "a re-embedded id must stay dirty when degraded and fresh otherwise: \
         {refresh:?}"
    );
}

#[tokio::test]
async fn degraded_embeddings_are_never_marked_fresh() {
    // Host-independent invariant. Whatever the provider chain did, the only
    // rows that hold a vec0 vector yet read as dirty are exactly the ones
    // counted in `embeddings_degraded`; every other embedded row is fresh.
    // This is what stops a hash-fallback pseudo-vector from masquerading as a
    // semantic embedding, so the pipeline retries it.
    let tmp = tempfile::tempdir().unwrap();
    let vault = tmp.path().join("vault");
    std::fs::create_dir_all(&vault).unwrap();
    write_note(&vault, "note-a.md", "note-a", "alpha body");
    write_note(&vault, "note-b.md", "note-b", "beta body");
    write_note(&vault, "note-c.md", "note-c", "gamma body");
    let db = tmp.path().join("state.db");

    let client = SqliteClient::open(&db).await.unwrap();
    let report = Reindexer::with_client(client)
        .reindex(&vault)
        .await
        .unwrap();

    let embedded = report.files_embedded as i64;
    let degraded = report.embeddings_degraded as i64;
    let fresh = fresh_embedded_count(&db).await;

    assert_eq!(
        fresh,
        embedded - degraded,
        "each non-degraded embedded row must be fresh: {report:?}"
    );
    assert_eq!(
        dirty_rows_with_vector(&db).await,
        degraded,
        "a stored vector that is left dirty must be exactly a degraded one: \
         {report:?}"
    );
    assert_eq!(
        fresh + degraded,
        embedded,
        "fresh and degraded-disjoint rows account for every embedded file: \
         {report:?}"
    );
}

#[test]
fn tagged_embedding_preserves_flag_and_matches_untagged_vector() {
    let (empty_vec, empty_degraded) =
        zen_vault::tindy::embeddings::compute_embeddings_for_text_tagged("   ").unwrap();
    assert_eq!(empty_vec, vec![0.0f32; 4096]);
    assert!(
        !empty_degraded,
        "an input with no chunks never touches a provider, so the flag must \
         not be sticky-true"
    );

    let short = "zen embedding warmup";
    let (short_vec, short_degraded) =
        zen_vault::tindy::embeddings::compute_embeddings_for_text_tagged(short).unwrap();
    assert_eq!(short_vec.len(), 4096);

    let long = "chunk word ".repeat(600);
    let (long_vec, long_degraded) =
        zen_vault::tindy::embeddings::compute_embeddings_for_text_tagged(&long).unwrap();
    assert_eq!(
        short_degraded, long_degraded,
        "both inputs traverse the same provider chain in this process, so \
         chunk count alone must not flip the flag"
    );
    assert_eq!(
        long_vec,
        zen_vault::tindy::compute_embeddings_for_text(&long).unwrap(),
        "the tagged path must return a byte-identical vector to the untagged wrapper"
    );
}

/// A block-sequence front-matter field (`evidence_refs:` as written by YAML
/// serialization) must not fail the reader: the parse error used to fall
/// through to raw indexing, so the page lost its declared id and its body was
/// indexed with the YAML block mixed in (O19, report §9.7).
#[tokio::test]
async fn block_sequence_frontmatter_keeps_the_declared_note_id() {
    let tmp = tempfile::tempdir().unwrap();
    let vault = tmp.path().join("vault");
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::write(
        vault.join("hypothesis.md"),
        "---\nid: hypothesis-one\ntags: [test]\nevidence_refs:\n  - \"notions/alpha.md\"\n  - \"notions/beta.md\"\n---\n\nclaim body\n",
    )
    .unwrap();
    std::fs::write(vault.join("plain.md"), "plain note without front matter\n").unwrap();
    let db = tmp.path().join("state.db");

    let client = SqliteClient::open(&db).await.unwrap();
    let reindexer = Reindexer::with_client(client).without_embeddings();
    assert_eq!(reindexer.reindex(&vault).await.unwrap().files_updated, 2);

    assert_eq!(
        indexed_ids(&db).await,
        vec!["auto-plain".to_string(), "hypothesis-one".to_string()],
        "the block-sequence page keeps its declared id, the frontmatter-less \
         page keeps the auto-* fallback"
    );

    let client = SqliteClient::open(&db).await.unwrap();
    let hits: i64 =
        sqlx::query_scalar("SELECT count(*) FROM notes_fts WHERE notes_fts MATCH 'claim'")
            .fetch_one(client.pool())
            .await
            .unwrap();
    assert_eq!(hits, 1, "the page body must remain searchable");
}
