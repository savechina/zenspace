use tempfile::TempDir;
use zen_memory::memvid_store::{MemvidStore, memvid_core};

#[test]
fn crash_recovery_committed_frames_intact() {
    let dir = TempDir::new().expect("temp dir");
    let db_path = dir.path().join("crash.mv2");

    let store = MemvidStore::open_or_create(&db_path).expect("store creation");

    for i in 0..5 {
        store
            .put_text(
                &format!("Turn {i}: user asks about topic {i}. Assistant responds with info {i}."),
                memvid_core::PutOptions::default(),
            )
            .expect("put_text");
    }

    drop(store);

    let reopened = MemvidStore::open_or_create(&db_path).expect("reopen after crash");

    let count = reopened.frame_count().expect("frame count");
    assert!(
        count >= 5,
        "expected >=5 frames after recovery, got {count}"
    );
}

#[test]
fn crash_recovery_writes_after_reopen() {
    let dir = TempDir::new().expect("temp dir");
    let db_path = dir.path().join("crash2.mv2");

    let store = MemvidStore::open_or_create(&db_path).expect("store creation");

    store
        .put_text(
            "First write before crash",
            memvid_core::PutOptions::default(),
        )
        .expect("put_text before crash");

    drop(store);

    let reopened = MemvidStore::open_or_create(&db_path).expect("reopen");

    reopened
        .put_text(
            "Second write after recovery",
            memvid_core::PutOptions::default(),
        )
        .expect("put_text after recovery");

    let count = reopened.frame_count().expect("frame count");
    assert!(
        count >= 2,
        "expected >=2 frames after recovery + new write, got {count}"
    );
}
