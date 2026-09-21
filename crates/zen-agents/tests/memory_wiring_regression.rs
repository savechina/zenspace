use tempfile::TempDir;
use zen_memory::memvid::ZenMemvidStore;
use zen_memory::memvid_store::MemvidStore;

#[test]
fn orchestrator_with_memory_has_store() {
    let dir = TempDir::new().unwrap();
    let memvid_path = dir.path().join("test.mv2");

    let store = ZenMemvidStore::new(memvid_path).unwrap();
    assert!(store.store().entity_memories("test-session").is_ok());
}

#[test]
fn orchestrator_without_memory_is_stateless() {
    // Isolation discipline (T186 class): fixed global paths are forbidden — tempdir gives the same fresh-path premise.
    let dir = tempfile::TempDir::new().unwrap();
    let store = MemvidStore::open_or_create(&dir.path().join("stateless.mv2"));

    if let Ok(s) = store {
        let cards = s.entity_memories("no-session").unwrap_or_default();
        assert!(cards.is_empty(), "Fresh store should have no cards");
    }
}

#[test]
fn memvid_store_persists_and_retrieves_turn() {
    let dir = TempDir::new().unwrap();
    let memvid_path = dir.path().join("persist_test.mv2");
    let store = ZenMemvidStore::new(memvid_path).unwrap();

    let result = store
        .persist_structured_turn("regression-session", "user", "I prefer Rust over Python")
        .unwrap();
    assert!(result.0 > 0, "Frame ID should be positive");

    let cards = store.store().entity_memories("regression-session").unwrap();
    assert!(!cards.is_empty(), "Should retrieve cards after persist");

    let has_conversation = cards.iter().any(|c| c.slot == "conversation");
    assert!(
        has_conversation,
        "Should find conversation card with slot='conversation'"
    );
}
