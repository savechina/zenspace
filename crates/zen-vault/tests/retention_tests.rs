//! Retention policy-engine tests (review D2): rotation cascade, age-delete
//! boundaries, keep-newest, report-only quarantine, dry-run, disabled.

use chrono::{DateTime, Duration, TimeZone, Utc};
use std::fs;
use std::path::Path;

use zen_core::config::RetentionConfig;
use zen_core::paths::ZenPaths;
use zen_vault::distill::retention::apply_policies;

fn setup() -> (tempfile::TempDir, ZenPaths) {
    let dir = tempfile::tempdir().unwrap();
    let paths = ZenPaths::for_testing(dir.path().to_path_buf());
    (dir, paths)
}

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 21, 3, 30, 0).unwrap()
}

fn cfg() -> RetentionConfig {
    RetentionConfig::default()
}

fn dry_cfg() -> RetentionConfig {
    RetentionConfig {
        dry_run: Some(true),
        ..Default::default()
    }
}

fn off_cfg() -> RetentionConfig {
    RetentionConfig {
        enabled: Some(false),
        ..Default::default()
    }
}

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn write_aged(path: &Path, content: &str, age: Duration) {
    write_file(path, content);
    set_mtime(path, now() - age);
}

fn set_mtime(path: &Path, t: DateTime<Utc>) {
    let sys = std::time::UNIX_EPOCH + std::time::Duration::from_secs(t.timestamp().max(0) as u64);
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(sys)
        .unwrap();
}

const MIB: usize = 1024 * 1024;

#[test]
fn rotation_cascade_keeps_exactly_n_siblings_and_continues_appending() {
    let (_dir, paths) = setup();
    let audit = paths.logs().join("audit.jsonl");
    write_file(&audit, &"a".repeat(10 * MIB));
    write_file(&paths.logs().join("audit.jsonl.1"), "one");
    write_file(&paths.logs().join("audit.jsonl.2"), "two");
    write_file(&paths.logs().join("audit.jsonl.3"), "three");

    let report = apply_policies(&paths, &cfg(), now());
    let home = &report.homes["logs/audit.jsonl"];
    assert_eq!(home.rotated, 1);
    assert_eq!(
        home.bytes_freed, 5,
        "dropped oldest sibling frees its bytes"
    );

    assert_eq!(
        fs::metadata(&audit).unwrap().len(),
        0,
        "fresh empty live file"
    );
    assert_eq!(
        fs::metadata(paths.logs().join("audit.jsonl.1"))
            .unwrap()
            .len(),
        10 * MIB as u64
    );
    assert_eq!(
        fs::read_to_string(paths.logs().join("audit.jsonl.2")).unwrap(),
        "one"
    );
    assert_eq!(
        fs::read_to_string(paths.logs().join("audit.jsonl.3")).unwrap(),
        "two"
    );
    assert!(!paths.logs().join("audit.jsonl.4").exists());

    // An appender continues on the fresh live file after rotation.
    zen_core::jsonl::append_jsonl_line(&audit, &serde_json::json!({"kind": "post-rotation"}))
        .unwrap();
    let entries = zen_core::jsonl::read_jsonl_lines(&audit).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["kind"], "post-rotation");
}

#[test]
fn rotation_two_sibling_homes_drop_beyond_keep() {
    let (_dir, paths) = setup();
    let gaps = paths.logs().join("loop-gaps.jsonl");
    write_file(&gaps, &"g".repeat(5 * MIB));
    write_file(&paths.logs().join("loop-gaps.jsonl.1"), "one");
    write_file(&paths.logs().join("loop-gaps.jsonl.2"), "two");

    let report = apply_policies(&paths, &cfg(), now());
    assert_eq!(report.homes["logs/loop-gaps.jsonl"].rotated, 1);
    assert!(!paths.logs().join("loop-gaps.jsonl.3").exists());
    assert_eq!(
        fs::read_to_string(paths.logs().join("loop-gaps.jsonl.2")).unwrap(),
        "one"
    );

    let tree = paths.logs().join("discovery-tree.jsonl");
    write_file(&tree, &"t".repeat(5 * MIB - 1));
    let report = apply_policies(&paths, &cfg(), now());
    assert_eq!(
        report.homes["logs/discovery-tree.jsonl"].rotated, 0,
        "below threshold: no rotation"
    );
    assert!(!paths.logs().join("discovery-tree.jsonl.1").exists());
}

#[test]
fn age_deletes_respect_boundary() {
    let (_dir, paths) = setup();
    let outbox = paths.logs().join("outbox");
    write_aged(&outbox.join("old.json"), "12345", Duration::days(31));
    write_aged(&outbox.join("exact.json"), "xy", Duration::days(30));
    write_aged(&outbox.join("fresh.json"), "{}", Duration::days(1));
    write_aged(&outbox.join("notes.txt"), "keep me", Duration::days(400));

    let report = apply_policies(&paths, &cfg(), now());
    let home = &report.homes["logs/outbox"];
    assert_eq!(home.removed_files, 1);
    assert_eq!(home.bytes_freed, 5);

    assert!(!outbox.join("old.json").exists());
    assert!(
        outbox.join("exact.json").exists(),
        "exactly-30-days is kept"
    );
    assert!(outbox.join("fresh.json").exists());
    assert!(
        outbox.join("notes.txt").exists(),
        "pattern only matches *.json"
    );
}

#[test]
fn journal_delete_is_recursive_on_mtime() {
    let (_dir, paths) = setup();
    let journal = paths.journal_entries();
    write_aged(&journal.join("2026/05/old.md"), "old", Duration::days(91));
    write_aged(&journal.join("2026/09/new.md"), "new", Duration::days(10));
    write_aged(&journal.join("2026/06/exact.md"), "e", Duration::days(90));

    let report = apply_policies(&paths, &cfg(), now());
    assert_eq!(report.homes["memories/journal"].removed_files, 1);
    assert!(!journal.join("2026/05/old.md").exists());
    assert!(journal.join("2026/09/new.md").exists());
    assert!(journal.join("2026/06/exact.md").exists());
}

#[test]
fn keep_newest_200_demoted_beliefs() {
    let (_dir, paths) = setup();
    let dir = paths.memory().join("demoted-beliefs");
    for i in 0..205 {
        write_aged(
            &dir.join(format!("b-{i:03}.md")),
            "belief",
            Duration::days(i as i64),
        );
    }

    let report = apply_policies(&paths, &cfg(), now());
    let home = &report.homes["memories/demoted-beliefs"];
    assert_eq!(home.removed_files, 5);
    assert_eq!(home.bytes_freed, 5 * 6);

    let remaining = fs::read_dir(&dir).unwrap().count();
    assert_eq!(remaining, 200);
    assert!(!dir.join("b-204.md").exists(), "oldest deleted");
    assert!(dir.join("b-199.md").exists());
    assert!(dir.join("b-000.md").exists(), "newest kept");
}

#[test]
fn quarantine_is_report_only_and_never_deletes() {
    let (_dir, paths) = setup();
    let q = paths.archive().join("quarantine");
    write_aged(
        &q.join("user-note.md"),
        "precious data",
        Duration::days(1000),
    );

    let report = apply_policies(&paths, &cfg(), now());
    let home = &report.homes["vault/archive/quarantine"];
    assert_eq!(home.observed_files, 1);
    assert_eq!(home.observed_bytes, 13);
    assert_eq!(home.removed_files, 0);
    assert_eq!(report.removed(), 0);
    assert!(q.join("user-note.md").exists());

    // Even under dry_run the quarantine entry is observation-only.
    let report = apply_policies(&paths, &dry_cfg(), now());
    assert_eq!(report.homes["vault/archive/quarantine"].removed_files, 0);
    assert!(q.join("user-note.md").exists());
}

#[test]
fn dry_run_computes_report_without_mutating() {
    let (_dir, paths) = setup();
    let audit = paths.logs().join("audit.jsonl");
    write_file(&audit, &"a".repeat(10 * MIB));
    write_file(&paths.logs().join("audit.jsonl.3"), "three");
    let outbox = paths.logs().join("outbox");
    write_aged(&outbox.join("old.json"), "12345", Duration::days(31));

    let report = apply_policies(&paths, &dry_cfg(), now());
    assert_eq!(report.rotated(), 1);
    assert_eq!(report.removed(), 1);
    assert_eq!(
        report.bytes_freed(),
        5 + 5,
        "dropped sibling + deleted file"
    );

    assert_eq!(fs::metadata(&audit).unwrap().len() as usize, 10 * MIB);
    assert!(!paths.logs().join("audit.jsonl.1").exists());
    assert!(outbox.join("old.json").exists());
}

#[test]
fn disabled_config_runs_nothing() {
    let (_dir, paths) = setup();
    write_file(&paths.logs().join("audit.jsonl"), &"a".repeat(10 * MIB));
    write_aged(
        &paths.logs().join("outbox/old.json"),
        "{}",
        Duration::days(31),
    );

    let report = apply_policies(&paths, &off_cfg(), now());
    assert!(report.homes.is_empty());
    assert_eq!(report.removed(), 0);
    assert_eq!(report.rotated(), 0);
    assert!(paths.logs().join("outbox/old.json").exists());
    assert!(!paths.logs().join("audit.jsonl.1").exists());
}

#[test]
fn wake_up_pattern_only_matches_prefixed_md_files() {
    let (_dir, paths) = setup();
    let logs = paths.logs();
    write_aged(
        &logs.join("wake-up-2026-08-01.md"),
        "brief",
        Duration::days(20),
    );
    write_aged(
        &logs.join("wake-up-2026-09-20.md"),
        "brief",
        Duration::days(1),
    );
    write_aged(&logs.join("other.md"), "keep", Duration::days(400));
    write_aged(&logs.join("wake-up-notes.txt"), "keep", Duration::days(400));

    let report = apply_policies(&paths, &cfg(), now());
    assert_eq!(report.homes["logs/wake-up"].removed_files, 1);
    assert!(!logs.join("wake-up-2026-08-01.md").exists());
    assert!(logs.join("wake-up-2026-09-20.md").exists());
    assert!(logs.join("other.md").exists());
    assert!(logs.join("wake-up-notes.txt").exists());
}

#[test]
fn long_window_homes_follow_their_own_ages() {
    let (_dir, paths) = setup();

    let suggestions = paths.memory().join("research-suggestions");
    write_aged(&suggestions.join("old.md"), "s", Duration::days(31));
    write_aged(&suggestions.join("new.md"), "s", Duration::days(29));

    let virtues = paths.vault().join("memories/virtue_logs/diligence");
    write_aged(&virtues.join("ancient.md"), "v", Duration::days(366));
    write_aged(&virtues.join("year.md"), "v", Duration::days(364));

    let output = paths.vault().join("output");
    write_aged(
        &output.join("weekly-review-2025.md"),
        "o",
        Duration::days(366),
    );

    let archive = paths.logs().join("archive");
    write_aged(&archive.join("log-2025.md"), "l", Duration::days(366));

    let wisdom = paths.wiki().join("wisdom/suggestions");
    write_aged(&wisdom.join("stale.md"), "w", Duration::days(181));
    write_aged(&wisdom.join("recent.md"), "w", Duration::days(179));

    let report = apply_policies(&paths, &cfg(), now());
    assert_eq!(
        report.homes["memories/research-suggestions"].removed_files,
        1
    );
    assert_eq!(report.homes["memories/virtue_logs"].removed_files, 1);
    assert_eq!(report.homes["vault/output"].removed_files, 1);
    assert_eq!(report.homes["logs/archive"].removed_files, 1);
    assert_eq!(
        report.homes["vault/wiki/wisdom/suggestions"].removed_files,
        1
    );

    assert!(!suggestions.join("old.md").exists());
    assert!(suggestions.join("new.md").exists());
    assert!(!virtues.join("ancient.md").exists());
    assert!(virtues.join("year.md").exists());
    assert!(!output.join("weekly-review-2025.md").exists());
    assert!(!archive.join("log-2025.md").exists());
    assert!(!wisdom.join("stale.md").exists());
    assert!(wisdom.join("recent.md").exists());
}

#[test]
fn missing_homes_produce_zero_entries_not_errors() {
    let (_dir, paths) = setup();
    let report = apply_policies(&paths, &cfg(), now());
    assert_eq!(report.removed(), 0);
    assert_eq!(report.rotated(), 0);
    assert_eq!(report.bytes_freed(), 0);
    assert!(report.homes.contains_key("logs/audit.jsonl"));
    assert!(report.homes.contains_key("vault/archive/quarantine"));
}

#[test]
fn report_serializes_per_home_shape() {
    let (_dir, paths) = setup();
    write_aged(
        &paths.logs().join("outbox/old.json"),
        "12345",
        Duration::days(31),
    );
    let report = apply_policies(&paths, &cfg(), now());
    let json = serde_json::to_value(&report).unwrap();
    let home = &json["homes"]["logs/outbox"];
    assert_eq!(home["removed_files"], 1);
    assert_eq!(home["rotated"], 0);
    assert_eq!(home["bytes_freed"], 5);
}
