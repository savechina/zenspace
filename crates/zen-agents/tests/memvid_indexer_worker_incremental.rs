//! T219 regression (review R1): the nightly memvid-indexer worker must run
//! INCREMENTAL indexing — two consecutive worker runs over an unchanged
//! corpus add ZERO frames.
//!
//! PURPOSE: pin the worker's `index_all → index_incremental` switch at the
//! production call site. The store is append-only with memvid-core dedup
//! deliberately OFF (dedup(true) would block the T210/T211 provenance
//! backfill), so the pre-fix worker re-appended every M2/M3/M4 frame plus a
//! fresh MemoryCard per chunk on every nightly run. The zen-memory unit
//! tests pin the mechanism; this test pins the wiring.
//!
//! USAGE: cargo nextest run -p zen-agents --test memvid_indexer_worker_incremental
//!
//! EXPECTED: run 1 indexes the seeded Path-Spec-v2 corpus (fact_count > 0);
//! run 2 over the unchanged corpus reports fact_count 0 and the store
//! frame_count is stable.
//!
//! ERRORS: a frame-count jump between the two runs means the worker
//! regressed to full-corpus re-append.

use chrono::Utc;
use zen_agents::scheduler::{MemvidIndexerWorker, WorkerContext, ZenWorker};
use zen_core::constants::MEMVID_STORE_FILE;
use zen_memory::ZenMemvidStore;

/// Single-test binary: the ZEN_HOME env mutation below is process-global, so
/// this file must stay one test (cargo test isolates binaries; nextest also
/// gives every test its own process).
#[tokio::test]
async fn two_worker_runs_over_unchanged_corpus_add_zero_frames() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().to_path_buf();
    // SAFETY: single-test binary; set before any ZenPaths/config read.
    // zen-core test-support (dev-dep feature) makes user_root() read
    // ZEN_HOME fresh per call.
    unsafe { std::env::set_var("ZEN_HOME", &home) };
    zen_core::config::invalidate_config_cache();

    // Path Spec v2 layout (T195): exactly one file per indexed tier.
    let journal = home.join("memories").join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    std::fs::write(
        journal.join("2026-06-01.md"),
        "## Facts\n- t219worker journal fact\n",
    )
    .unwrap();
    let notions = home
        .join("vault")
        .join("wiki")
        .join("notions")
        .join("technology");
    std::fs::create_dir_all(&notions).unwrap();
    std::fs::write(
        notions.join("topic.md"),
        "# Topic\n\nt219worker notion body.\n",
    )
    .unwrap();
    let wisdom = home
        .join("vault")
        .join("wiki")
        .join("wisdom")
        .join("reflections");
    std::fs::create_dir_all(&wisdom).unwrap();
    std::fs::write(
        wisdom.join("lesson.md"),
        "# Lesson\n\nt219worker wisdom body.\n",
    )
    .unwrap();

    let worker = MemvidIndexerWorker::new();
    let ctx = WorkerContext::new(Utc::now());

    let run1 = worker.execute(&ctx).await.expect("run 1 succeeds");
    assert!(run1.success);
    assert!(
        run1.fact_count > 0,
        "first run must index the seeded corpus, got {}",
        run1.fact_count
    );

    // Same singleton key the worker used ⇒ the live in-process store
    // (memvid.rs get_or_init cache), no second file lock.
    let store_path = home.join("memories").join(MEMVID_STORE_FILE);
    let frames_after_run1 = ZenMemvidStore::new(store_path.clone())
        .unwrap()
        .store()
        .frame_count()
        .unwrap();
    assert!(frames_after_run1 > 0);

    let run2 = worker.execute(&ctx).await.expect("run 2 succeeds");
    assert!(run2.success);
    assert_eq!(
        run2.fact_count, 0,
        "an unchanged corpus must add nothing on the second run"
    );
    let frames_after_run2 = ZenMemvidStore::new(store_path)
        .unwrap()
        .store()
        .frame_count()
        .unwrap();
    assert_eq!(
        frames_after_run2, frames_after_run1,
        "R1 regression: frame count must be stable across worker runs"
    );
    assert!(
        home.join("memories").join(".index-checksums.json").exists(),
        "checksum sidecar written for the next incremental run"
    );

    // Release the process-lifetime memvid singletons so watcher threads
    // exit and the tempdir can be removed (sole_owner.rs precedent).
    zen_memory::memvid::clear_global_memvid_cache();
}
