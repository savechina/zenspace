use rig_memvid::MemvidStore;
use tempfile::TempDir;

#[test]
fn crash_recovery_committed_frames_intact() {
    let dir = TempDir::new().expect("temp dir");
    let db_path = dir.path().join("crash.mv2");

    let store = MemvidStore::builder()
        .path(&db_path)
        .enable_lex()
        .open_or_create()
        .expect("store creation");

    for i in 0..5 {
        store
            .put_text(
                &format!("Turn {i}: user asks about topic {i}. Assistant responds with info {i}."),
                rig_memvid::memvid_core::PutOptions::default(),
            )
            .expect("put_text");
    }

    drop(store);

    let reopened = MemvidStore::builder()
        .path(&db_path)
        .enable_lex()
        .open_or_create()
        .expect("reopen after crash");

    let count = reopened.frame_count().expect("frame count");
    assert!(count >= 5, "expected >=5 frames after recovery, got {count}");
}

#[test]
fn crash_recovery_writes_after_reopen() {
    let dir = TempDir::new().expect("temp dir");
    let db_path = dir.path().join("crash2.mv2");

    let store = MemvidStore::builder()
        .path(&db_path)
        .enable_lex()
        .open_or_create()
        .expect("store creation");

    store
        .put_text("First write before crash", rig_memvid::memvid_core::PutOptions::default())
        .expect("put_text before crash");

    drop(store);

    let reopened = MemvidStore::builder()
        .path(&db_path)
        .enable_lex()
        .open_or_create()
        .expect("reopen");

    reopened
        .put_text("Second write after recovery", rig_memvid::memvid_core::PutOptions::default())
        .expect("put_text after recovery");

    let count = reopened.frame_count().expect("frame count");
    assert!(count >= 2, "expected >=2 frames after recovery + new write, got {count}");
}