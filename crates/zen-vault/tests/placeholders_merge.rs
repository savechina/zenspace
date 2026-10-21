//! T180: `merge_save_placeholders` must never drop a concurrent writer's
//! declarations — pre-seeded foreign slots survive, collisions resolve by
//! status progression (merged > claimed > placeholder, ties to incoming),
//! corrupt files fail open to empty, and the flock is released after save.

use std::path::Path;

use zen_vault::distill::types::PlaceholderStatus;
use zen_vault::distill::{merge_save_placeholders, registry_path};
use zen_vault::graph_verify::PlaceholderRegistry;

fn claimed_registry(slug: &str, owner: &str) -> PlaceholderRegistry {
    let mut reg = PlaceholderRegistry::new();
    reg.declare(slug, owner);
    reg.claim(slug, owner);
    reg
}

fn merged_registry(slug: &str) -> PlaceholderRegistry {
    let mut reg = PlaceholderRegistry::new();
    reg.merge(slug);
    reg
}

#[test]
fn foreign_declarations_survive_merge_save() {
    let tmp = tempfile::tempdir().unwrap();
    let logs = tmp.path();

    let mut foreign = PlaceholderRegistry::new();
    foreign.declare("foreign-slot", "other-writer");
    foreign.save(&registry_path(logs)).unwrap();

    let mut ours = PlaceholderRegistry::new();
    ours.declare("our-slot", "zen-loop");
    let merged = merge_save_placeholders(logs, &ours).unwrap();

    assert!(merged.lookup("foreign-slot").is_some());
    assert!(merged.lookup("our-slot").is_some());

    let reloaded = PlaceholderRegistry::load(&registry_path(logs)).unwrap();
    assert_eq!(
        reloaded
            .lookup("foreign-slot")
            .unwrap()
            .claimed_by
            .as_deref(),
        Some("other-writer")
    );
    assert_eq!(
        reloaded.lookup("our-slot").unwrap().claimed_by.as_deref(),
        Some("zen-loop")
    );
}

#[test]
fn collision_prefers_more_progressed_status() {
    let tmp = tempfile::tempdir().unwrap();
    let logs = tmp.path();
    let path = registry_path(logs);

    let mut foreign = PlaceholderRegistry::new();
    foreign.declare("slot", "other-writer");
    foreign.save(&path).unwrap();
    let ours = claimed_registry("slot", "zen-loop");
    let merged = merge_save_placeholders(logs, &ours).unwrap();
    assert_eq!(
        merged.lookup("slot").unwrap().status,
        PlaceholderStatus::Claimed,
        "foreign=placeholder, ours=claimed ⇒ claimed wins"
    );

    merged_registry("slot").save(&path).unwrap();
    let mut ours = PlaceholderRegistry::new();
    ours.declare("slot", "zen-loop");
    let merged = merge_save_placeholders(logs, &ours).unwrap();
    assert_eq!(
        merged.lookup("slot").unwrap().status,
        PlaceholderStatus::Merged,
        "foreign=merged, ours=placeholder ⇒ merged survives"
    );
    let reloaded = PlaceholderRegistry::load(&path).unwrap();
    assert_eq!(
        reloaded.lookup("slot").unwrap().status,
        PlaceholderStatus::Merged
    );
}

#[test]
fn equal_status_collision_prefers_incoming_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let logs = tmp.path();

    claimed_registry("slot", "other-writer")
        .save(&registry_path(logs))
        .unwrap();
    let merged = merge_save_placeholders(logs, &claimed_registry("slot", "zen-loop")).unwrap();
    assert_eq!(
        merged.lookup("slot").unwrap().claimed_by.as_deref(),
        Some("zen-loop"),
        "equal status ⇒ incoming entry wins"
    );
}

#[test]
fn corrupt_file_is_treated_as_empty_and_repaired() {
    let tmp = tempfile::tempdir().unwrap();
    let logs = tmp.path();
    std::fs::write(registry_path(logs), "{not valid json").unwrap();

    let mut ours = PlaceholderRegistry::new();
    ours.declare("our-slot", "zen-loop");
    let merged = merge_save_placeholders(logs, &ours).unwrap();
    assert!(merged.lookup("our-slot").is_some());

    let reloaded = PlaceholderRegistry::load(&registry_path(logs)).unwrap();
    assert!(reloaded.lookup("our-slot").is_some());
}

#[test]
fn lock_is_released_after_save() {
    let tmp = tempfile::tempdir().unwrap();
    let logs = tmp.path();

    let mut first = PlaceholderRegistry::new();
    first.declare("slot-a", "zen-loop");
    merge_save_placeholders(logs, &first).unwrap();

    let mut second = PlaceholderRegistry::new();
    second.declare("slot-b", "zen-loop");
    let merged = merge_save_placeholders(logs, &second).unwrap();
    assert!(
        merged.lookup("slot-a").is_some() && merged.lookup("slot-b").is_some(),
        "second save immediately after the first must not deadlock and must keep both slots"
    );
}

#[test]
fn lock_file_lives_beside_registry() {
    let tmp = tempfile::tempdir().unwrap();
    let logs = tmp.path();
    merge_save_placeholders(logs, &PlaceholderRegistry::new()).unwrap();
    assert!(Path::new(&logs.join("placeholders.lock")).exists());
}

/// Chained `missing_entity-missing_entity-…` generations (the 2026-10-15
/// hypothesis-explosion artifacts) must not survive a merge-save —
/// single-generation hypothesis slugs and legit slots do.
#[test]
fn merge_save_prunes_chained_ghost_generations_only() {
    let tmp = tempfile::tempdir().unwrap();
    let logs = tmp.path();
    let path = registry_path(logs);

    let mut disk = PlaceholderRegistry::new();
    // Pre-explosion pollution on disk: nested generations of every depth.
    disk.declare("missing_entity-rust-concurrency", "zen-loop");
    disk.declare("missing_entity-missing-entity-rust-concurrency", "zen-loop");
    disk.declare(
        "missing_entity-missing-entity-missing-entity-ent-3c01f35e",
        "zen-loop",
    );
    // Legit slots in the same registry that must survive.
    disk.declare("rust-ownership", "zen-loop");
    disk.save(&path).unwrap();

    let mut ours = PlaceholderRegistry::new();
    ours.declare("missing_entity-fresh-gap", "zen-loop");
    ours.declare("missing_entity-missing-entity-staged-today", "zen-loop");
    let merged = merge_save_placeholders(logs, &ours).unwrap();

    assert!(merged.lookup("rust-ownership").is_some());
    assert!(merged.lookup("missing_entity-rust-concurrency").is_some());
    assert!(merged.lookup("missing_entity-fresh-gap").is_some());
    assert!(
        merged
            .lookup("missing_entity-missing-entity-staged-today")
            .is_none(),
        "incoming chained ghosts are dropped at merge time"
    );

    // The persisted file is swept too, not just the returned registry.
    let reloaded = PlaceholderRegistry::load(&path).unwrap();
    assert!(reloaded.lookup("rust-ownership").is_some());
    assert!(
        reloaded
            .lookup("missing_entity-missing-entity-missing-entity-ent-3c01f35e")
            .is_none(),
        "disk-side chained generations are swept on merge-save"
    );
    assert!(
        reloaded
            .lookup("missing_entity-missing-entity-rust-concurrency")
            .is_none()
    );
}
