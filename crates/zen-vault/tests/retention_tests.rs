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

#[test]
fn stale_txn_tracking_files_age_out_but_fresh_ones_stay() {
    let (_dir, paths) = setup();
    let stale = paths.logs().join(".txn-distill-old.jsonl");
    let fresh = paths.logs().join(".txn-distill-live.jsonl");
    write_aged(&stale, "{\"dead\"}", Duration::days(8));
    write_aged(&fresh, "{\"live\"}", Duration::days(1));

    let report = apply_policies(&paths, &cfg(), now());
    assert_eq!(report.homes["logs/.txn-*"].removed_files, 1);
    assert!(!stale.exists(), "8-day-old tracking file must age out");
    assert!(
        fresh.exists(),
        "a tracking file within the age window must stay (live-cycle guard)"
    );
}

#[test]
fn stale_session_archives_age_out_and_fresh_sessions_survive() {
    let (_dir, paths) = setup();
    let sessions = paths.sessions();
    // ConversationStore::open date layout + dream-compression .jsonl.zst twin.
    write_aged(
        &sessions.join("2026/06/old-session.jsonl"),
        "{\"turn\":1}",
        Duration::days(91),
    );
    write_aged(
        &sessions.join("2026/06/old-session.jsonl.zst"),
        "zst",
        Duration::days(91),
    );
    write_aged(
        &sessions.join("2026/09/fresh-session.jsonl"),
        "{\"turn\":2}",
        Duration::days(1),
    );
    // Dream's prune_expired_sessions(30d) owns terminal sessions'
    // tool_calls.jsonl; the 90d retention row is only the backstop for
    // content nothing else bounds (a >90d tool log is outside the 30d
    // aggregation window, so removal here is inert).
    write_aged(
        &sessions.join("stale-session/tool_calls.jsonl"),
        "{}",
        Duration::days(91),
    );

    let report = apply_policies(&paths, &cfg(), now());
    let home = &report.homes["sessions"];
    assert_eq!(home.removed_files, 3);
    assert!(!sessions.join("2026/06/old-session.jsonl").exists());
    assert!(!sessions.join("2026/06/old-session.jsonl.zst").exists());
    assert!(!sessions.join("stale-session/tool_calls.jsonl").exists());
    assert!(sessions.join("2026/09/fresh-session.jsonl").exists());
}

#[test]
fn wiki_log_md_rotates_at_cap_keeping_two_siblings() {
    let (_dir, paths) = setup();
    let log = paths.wiki().join("log.md");
    write_file(&log, &"l".repeat(MIB));
    write_file(&paths.wiki().join("log.md.1"), "one");
    write_file(&paths.wiki().join("log.md.2"), "two");

    let report = apply_policies(&paths, &cfg(), now());
    let home = &report.homes["vault/wiki/log.md"];
    assert_eq!(home.rotated, 1);
    assert_eq!(
        fs::metadata(&log).unwrap().len(),
        0,
        "fresh empty live file"
    );
    assert_eq!(
        fs::read_to_string(paths.wiki().join("log.md.2")).unwrap(),
        "one"
    );
    assert!(!paths.wiki().join("log.md.3").exists());
}

#[test]
fn wiki_contradictions_md_rotates_at_cap() {
    let (_dir, paths) = setup();
    let contra = paths.wiki().join("contradictions.md");
    write_file(&contra, &"c".repeat(MIB));

    let report = apply_policies(&paths, &cfg(), now());
    assert_eq!(report.homes["vault/wiki/contradictions.md"].rotated, 1);
    assert!(paths.wiki().join("contradictions.md.1").exists());
    assert_eq!(fs::metadata(&contra).unwrap().len(), 0);
}

#[test]
fn decision_labels_window_boundary_and_dataset_immunity() {
    let (_dir, paths) = setup();
    let dir = paths.logs().join("decision-audit");
    write_aged(&dir.join("labels.jsonl"), "old", Duration::days(366));
    write_aged(&dir.join("dataset.jsonl"), "d", Duration::days(400));

    let report = apply_policies(&paths, &cfg(), now());
    assert_eq!(
        report.homes["logs/decision-audit/labels.jsonl"].removed_files,
        1
    );
    assert!(!dir.join("labels.jsonl").exists());
    assert!(
        dir.join("dataset.jsonl").exists(),
        "dataset.jsonl is not this row's target"
    );

    write_aged(&dir.join("labels.jsonl"), "fresh", Duration::days(364));
    let report = apply_policies(&paths, &cfg(), now());
    assert_eq!(
        report.homes["logs/decision-audit/labels.jsonl"].removed_files, 0,
        "labels within the 365d window survive"
    );
    assert!(dir.join("labels.jsonl").exists());
}

#[test]
fn cso_audit_rotate_homes_fire_at_their_caps() {
    let (_dir, paths) = setup();
    let logs = paths.logs();
    let caps: &[(&str, u64)] = &[
        ("memory-nudges.jsonl", 512 * 1024),
        ("promotion-confirmed.jsonl", 512 * 1024),
        ("safety-audit.jsonl", 1024 * 1024),
    ];
    for (name, cap) in caps {
        write_file(&logs.join(name), &"x".repeat(*cap as usize));
    }

    let report = apply_policies(&paths, &cfg(), now());
    for (file, home) in [
        ("memory-nudges.jsonl", "logs/memory-nudges.jsonl"),
        (
            "promotion-confirmed.jsonl",
            "logs/promotion-confirmed.jsonl",
        ),
        ("safety-audit.jsonl", "logs/safety-audit.jsonl"),
    ] {
        assert_eq!(report.homes[home].rotated, 1, "{home} must rotate");
        assert!(logs.join(format!("{file}.1")).exists());
    }
}

#[test]
fn skills_history_and_wisdom_event_dirs_age_out() {
    let (_dir, paths) = setup();
    let skills = paths.skills();
    write_aged(
        &skills.join("zen-wiki-history.jsonl"),
        "h",
        Duration::days(366),
    );
    write_aged(
        &skills.join("zen-search-history.jsonl"),
        "h",
        Duration::days(10),
    );
    write_aged(&skills.join("SKILL.md"), "keep", Duration::days(400));

    let rewinds = paths.wiki().join("wisdom/rewinds");
    write_aged(&rewinds.join("old.md"), "r", Duration::days(91));
    write_aged(&rewinds.join("new.md"), "r", Duration::days(10));

    let reflections = paths.wiki().join("wisdom/reflections");
    write_aged(&reflections.join("ancient.md"), "f", Duration::days(91));
    write_aged(&reflections.join("recent.md"), "f", Duration::days(10));

    let report = apply_policies(&paths, &cfg(), now());
    assert_eq!(report.homes["skills"].removed_files, 1);
    assert!(!skills.join("zen-wiki-history.jsonl").exists());
    assert!(skills.join("zen-search-history.jsonl").exists());
    assert!(
        skills.join("SKILL.md").exists(),
        "non-history skill files never match the pattern"
    );

    assert_eq!(report.homes["vault/wiki/wisdom/rewinds"].removed_files, 1);
    assert!(!rewinds.join("old.md").exists());
    assert!(rewinds.join("new.md").exists());
    assert_eq!(
        report.homes["vault/wiki/wisdom/reflections"].removed_files,
        1
    );
    assert!(!reflections.join("ancient.md").exists());
    assert!(reflections.join("recent.md").exists());
}

// Background tool execution (design record
// background-tool-execution-2026-10-30.md, Section 7 item 12): task output
// dirs age out at 7d, recursively on mtime.
#[test]
fn background_task_dirs_age_out_at_seven_days() {
    let (_dir, paths) = setup();
    let tasks = paths.logs().join("tasks");
    write_aged(
        &tasks.join("task-old/stdout"),
        "stale output",
        Duration::days(8),
    );
    write_aged(&tasks.join("task-old/meta.json"), "{}", Duration::days(8));
    write_aged(
        &tasks.join("task-fresh/stdout"),
        "live output",
        Duration::days(6),
    );

    let report = apply_policies(&paths, &cfg(), now());
    assert_eq!(report.homes["logs/tasks"].removed_files, 2);
    // The engine deletes FILES (empty parent dirs are left behind — same
    // shape as the journal recursive delete).
    assert!(!tasks.join("task-old/stdout").exists());
    assert!(!tasks.join("task-old/meta.json").exists());
    assert!(tasks.join("task-fresh/stdout").exists());
}
