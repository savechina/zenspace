use std::time::Instant;

use rig_memvid::MemvidStore;
use tempfile::TempDir;

fn bench_retrieval(n: usize, label: &str) {
    let dir = TempDir::new().expect("temp dir");
    let db_path = dir.path().join("bench.mv2");
    let store = MemvidStore::builder()
        .path(&db_path)
        .enable_lex()
        .open_or_create()
        .expect("store creation");

    for i in 0..n {
        store
            .put_text(
                &format!("Turn {i}: user asks about topic {i}. Assistant responds with info {i}."),
                rig_memvid::memvid_core::PutOptions::default(),
            )
            .expect("put_text");
    }

    let start = Instant::now();
    let cards = store.entity_memories(&format!("bench-session-{label}")).expect("entity_memories");
    let elapsed = start.elapsed();

    println!(
        "[{label}] {n} frames: {} cards, retrieval in {}ms",
        cards.len(),
        elapsed.as_millis(),
    );
}

#[test]
fn bench_retrieval_100_frames() {
    let dir = TempDir::new().expect("temp dir");
    let db_path = dir.path().join("bench.mv2");
    let store = MemvidStore::builder()
        .path(&db_path)
        .enable_lex()
        .open_or_create()
        .expect("store creation");

    for i in 0..100 {
        store
            .put_text(
                &format!("Turn {i}: user asks about topic {i}. Assistant responds with info {i}."),
                rig_memvid::memvid_core::PutOptions::default(),
            )
            .expect("put_text");
    }

    let start = Instant::now();
    let cards = store.entity_memories("bench-session-100").expect("entity_memories");
    let elapsed = start.elapsed();

    println!("[100] 100 frames: {} cards, retrieval in {}ms", cards.len(), elapsed.as_millis());

    assert!(
        elapsed.as_millis() < 100,
        "Retrieval took {}ms, expected <100ms",
        elapsed.as_millis(),
    );
}

#[test]
#[ignore]
fn bench_retrieval_1000_frames() {
    bench_retrieval(1000, "1k");
}