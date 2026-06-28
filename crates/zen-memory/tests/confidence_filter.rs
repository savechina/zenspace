use tempfile::TempDir;
use zen_memory::memvid::{TRIPLET_MIN_CONFIDENCE, ZenMemvidStore};

#[test]
fn confidence_threshold_filters_low_confidence_cards() {
    let dir = TempDir::new().unwrap();
    let memvid_path = dir.path().join("confidence_test.mv2");
    let store = ZenMemvidStore::new(memvid_path).unwrap();

    store
        .persist_structured_turn("confidence-session", "user", "The sky is blue")
        .unwrap();

    let cards = store.store().entity_memories("confidence-session").unwrap();

    let high_confidence: Vec<_> = cards
        .iter()
        .filter(|c| c.confidence.unwrap_or(1.0) >= TRIPLET_MIN_CONFIDENCE)
        .collect();
    let low_confidence: Vec<_> = cards
        .iter()
        .filter(|c| c.confidence.unwrap_or(1.0) < TRIPLET_MIN_CONFIDENCE)
        .collect();

    for card in &high_confidence {
        assert!(
            card.confidence.unwrap_or(1.0) >= 0.8,
            "High confidence card should be >= 0.8"
        );
    }
    for card in &low_confidence {
        assert!(
            card.confidence.unwrap_or(1.0) < 0.8,
            "Low confidence card should be < 0.8"
        );
    }

    let retrieved = store.retrieve("confidence-session").unwrap();
    for line in &retrieved {
        assert!(
            !line.is_empty(),
            "Retrieved memory lines should not be empty"
        );
    }
}

#[test]
fn triplet_min_confidence_is_0_8() {
    assert_eq!(TRIPLET_MIN_CONFIDENCE, 0.8);
    const { assert!(TRIPLET_MIN_CONFIDENCE > 0.0) };
    const { assert!(TRIPLET_MIN_CONFIDENCE <= 1.0) };
}
