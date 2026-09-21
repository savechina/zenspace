//! RetentionWorker tests (review D2): audit-line emission, dry-run, disabled.

use chrono::{Duration, TimeZone, Utc};
use std::fs;
use std::path::Path;

use zen_agents::scheduler::{RETENTION_SCHEDULE, RetentionWorker, WorkerContext, ZenWorker};
use zen_core::config::RetentionConfig;
use zen_core::paths::ZenPaths;

fn setup() -> (tempfile::TempDir, ZenPaths) {
    let dir = tempfile::tempdir().unwrap();
    let paths = ZenPaths::for_testing(dir.path().to_path_buf());
    (dir, paths)
}

fn ctx() -> WorkerContext {
    WorkerContext::new(Utc.with_ymd_and_hms(2026, 9, 21, 3, 30, 0).unwrap())
}

fn aged_outbox_file(paths: &ZenPaths, content: &str) -> std::path::PathBuf {
    let file = paths.logs().join("outbox/morning-brief-2026-08-01.json");
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&file, content).unwrap();
    let old = std::time::UNIX_EPOCH
        + std::time::Duration::from_secs((ctx().now - Duration::days(31)).timestamp().max(0) as u64);
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(old)
        .unwrap();
    file
}

fn read_audit_lines(paths: &ZenPaths) -> Vec<serde_json::Value> {
    let path = paths.logs().join("audit.jsonl");
    let raw = fs::read_to_string(&path).expect("audit.jsonl written");
    raw.lines()
        .map(|l| serde_json::from_str(l).expect("valid JSON line"))
        .collect()
}

#[test]
fn worker_identity_and_schedule() {
    let worker = RetentionWorker::new();
    assert_eq!(worker.id(), "retention");
    assert_eq!(worker.schedule(), RETENTION_SCHEDULE);
    assert_eq!(RETENTION_SCHEDULE, "0 30 3 * * *");
    assert!(!worker.description().is_empty());
}

#[test]
fn execute_sweeps_and_emits_one_audit_line() {
    let (_dir, paths) = setup();
    let stale = aged_outbox_file(&paths, "12345");

    let worker = RetentionWorker::new();
    let report = worker
        .execute_with_paths(&paths, &RetentionConfig::default(), &ctx())
        .unwrap();
    assert!(report.success);
    assert_eq!(report.worker_id, "retention");
    assert_eq!(report.fact_count, 1);
    assert_eq!(report.llm_cost_usd, 0.0);
    assert!(!Path::new(&stale).exists(), "stale outbox file deleted");

    let lines = read_audit_lines(&paths);
    assert_eq!(lines.len(), 1, "exactly ONE audit line per sweep");
    let line = &lines[0];
    assert_eq!(line["kind"], "loop.retention.applied");
    assert_eq!(line["removed"], 1);
    assert_eq!(line["rotated"], 0);
    assert_eq!(line["bytes_freed"], 5);
    assert_eq!(line["dry_run"], false);
    assert_eq!(line["per_home"]["logs/outbox"]["removed_files"], 1);
    assert!(line["ts"].as_str().unwrap().starts_with("2026-09-21"));
}

#[test]
fn dry_run_audits_but_mutates_nothing() {
    let (_dir, paths) = setup();
    let stale = aged_outbox_file(&paths, "12345");
    let cfg = RetentionConfig {
        dry_run: Some(true),
        ..Default::default()
    };

    let worker = RetentionWorker::new();
    let report = worker.execute_with_paths(&paths, &cfg, &ctx()).unwrap();
    assert!(report.success);
    assert!(Path::new(&stale).exists(), "dry-run deletes nothing");

    let lines = read_audit_lines(&paths);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["dry_run"], true);
    assert_eq!(lines[0]["removed"], 1, "report still computed");
}

#[test]
fn disabled_config_runs_nothing_and_writes_no_audit() {
    let (_dir, paths) = setup();
    let stale = aged_outbox_file(&paths, "12345");
    let cfg = RetentionConfig {
        enabled: Some(false),
        ..Default::default()
    };

    let worker = RetentionWorker::new();
    let report = worker.execute_with_paths(&paths, &cfg, &ctx()).unwrap();
    assert!(report.success);
    assert_eq!(report.fact_count, 0);
    assert!(Path::new(&stale).exists());
    assert!(
        !paths.logs().join("audit.jsonl").exists(),
        "disabled sweep emits no audit line"
    );
}
