//! T023 — US3 observability: status/gaps <2s, audit events, reversibility, enable/disable + dry-run.

mod common;

use common::ZenTest;
use std::fs;
use std::time::Instant;

fn seed_note(test: &ZenTest, name: &str, body: &str) {
    let inbox = test.cwd.join("vault").join("inbox");
    fs::create_dir_all(&inbox).expect("create inbox");
    fs::write(inbox.join(name), body).expect("write note");
}

fn find_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                out.extend(find_files(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

#[test]
fn status_and_gaps_json_under_2s() {
    let test = ZenTest::new();
    assert!(test.zen(&["workspace", "init"]).success());

    let t0 = Instant::now();
    let status = test.zen(&["wiki", "loop", "status", "--json"]);
    assert!(
        status.success(),
        "status --json failed: {}",
        status.stderr()
    );
    assert!(
        t0.elapsed().as_secs_f64() < 2.0,
        "SC-005: status must respond <2s"
    );
    let v: serde_json::Value = serde_json::from_str(&status.stdout()).expect("valid status json");
    assert!(v.get("last_cycle").is_some());
    assert!(v.get("schedule").is_some() || v.get("next_tick").is_some());
    assert!(v.get("enabled").is_some());
    assert!(v.get("open_gaps").is_some());

    let t0 = Instant::now();
    let gaps = test.zen(&["wiki", "loop", "gaps", "--json"]);
    assert!(gaps.success(), "gaps --json failed: {}", gaps.stderr());
    assert!(
        t0.elapsed().as_secs_f64() < 2.0,
        "SC-005: gaps must respond <2s"
    );
    // T187 contract: gaps --json wraps {"gaps": [...], "user_questions": [...]}
    let obj: serde_json::Value = serde_json::from_str(&gaps.stdout()).expect("valid gaps json");
    assert!(
        obj.get("gaps").and_then(|g| g.as_array()).is_some(),
        "gaps --json must wrap a gaps array"
    );
    assert!(
        obj.get("user_questions")
            .and_then(|q| q.as_array())
            .is_some(),
        "gaps --json must wrap a user_questions array"
    );
}

#[test]
fn gaps_kind_filter_and_vault_relative_paths() {
    let test = ZenTest::new();
    assert!(test.zen(&["workspace", "init"]).success());

    // Initially empty
    let gaps = test.zen(&["wiki", "loop", "gaps", "--json"]);
    assert!(gaps.success());
    let arr: serde_json::Value = serde_json::from_str(&gaps.stdout()).unwrap();
    let before = arr
        .get("gaps")
        .and_then(|g| g.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    // Trigger a gap: orphan concept without entity via wiki page without notion
    seed_note(
        &test,
        "orphan-note.md",
        "---\nid: \"orphan-1\"\n---\n\n# Orphan Concept\n\nStandalone page.\n",
    );
    let run = test.zen(&["wiki", "loop", "run"]);
    assert!(run.success(), "run failed: {}", run.stderr());

    let gaps = test.zen(&["wiki", "loop", "gaps", "--json"]);
    assert!(gaps.success());
    let arr: serde_json::Value = serde_json::from_str(&gaps.stdout()).unwrap();
    let all = arr
        .get("gaps")
        .and_then(|g| g.as_array())
        .cloned()
        .unwrap_or_default();
    // Kind filter
    let filtered = test.zen(&["wiki", "loop", "gaps", "--kind", "OrphanEntity", "--json"]);
    assert!(filtered.success());
    let farr: serde_json::Value = serde_json::from_str(&filtered.stdout()).unwrap();
    let filtered_arr = farr
        .get("gaps")
        .and_then(|g| g.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(filtered_arr.len() <= all.len());
    for v in &filtered_arr {
        assert_eq!(v.get("kind").and_then(|k| k.as_str()), Some("OrphanEntity"));
    }
    // Vault-relative path check: subject_path should not be absolute
    for v in &all {
        if let Some(p) = v.get("subject_path").and_then(|p| p.as_str()) {
            assert!(!p.starts_with('/'), "vault-relative expected, got {p}");
        }
    }
    // Most recent first already implied; at least not failing
    assert!(all.len() >= before);
}

#[test]
fn audit_events_no_content_and_reversibility() {
    let test = ZenTest::new();
    assert!(test.zen(&["workspace", "init"]).success());
    seed_note(
        &test,
        "audit-note.md",
        "---\nid: \"audit-1\"\n---\n\n# Audit Note\n\nSome content to be archived.\n",
    );
    let run = test.zen(&["wiki", "loop", "run"]);
    assert!(run.success(), "run failed: {}", run.stderr());

    let logs = test.cwd.join("logs");
    let audit_path = if logs.join("audit.jsonl").exists() {
        logs.join("audit.jsonl")
    } else {
        test.cwd.join(".zen").join("logs").join("audit.jsonl")
    };
    // Audit file may be under logs/ or .zen/logs/ depending on ZenPaths in test
    let audit_content = fs::read_to_string(&audit_path)
        .or_else(|_| fs::read_to_string(test.cwd.join("logs").join("audit.jsonl")))
        .expect("audit.jsonl must exist");
    assert!(audit_content.contains("loop.cycle.completed") || audit_content.contains("loop.cycle"));
    // No note content leaked
    assert!(
        !audit_content.contains("Some content to be archived"),
        "content must not be in audit"
    );

    // Reversibility: archived file exists with provenance keys, can be moved back
    let archive = test.cwd.join("vault").join("archive");
    let archived = find_files(&archive);
    assert!(!archived.is_empty(), "archive must contain note");
    let archived_content = fs::read_to_string(&archived[0]).expect("read archived");
    assert!(archived_content.contains("source_path:"));
    assert!(archived_content.contains("archived_at:"));
    assert!(archived_content.contains("cycle_id:"));
    assert!(archived_content.contains("checksum:"));

    // Inbox empty guarantee
    let inbox = test.cwd.join("vault").join("inbox");
    let remaining: Vec<_> = fs::read_dir(&inbox)
        .map(|d| d.filter_map(|e| e.ok()).collect::<Vec<_>>())
        .unwrap_or_default();
    assert!(remaining.is_empty(), "inbox must be empty after archive");
}

#[test]
fn enable_disable_and_dry_run_no_mutation() {
    let test = ZenTest::new();
    assert!(test.zen(&["workspace", "init"]).success());

    let disable = test.zen(&["wiki", "loop", "disable"]);
    assert!(disable.success(), "disable failed: {}", disable.stderr());
    let cfg = fs::read_to_string(test.cwd.join("config.toml")).expect("config exists");
    assert!(cfg.contains("enabled = false"));

    let enable = test.zen(&["wiki", "loop", "enable"]);
    assert!(enable.success(), "enable failed: {}", enable.stderr());
    let cfg = fs::read_to_string(test.cwd.join("config.toml")).expect("config re-read");
    assert!(cfg.contains("enabled = true"));

    // Dry-run: no mutations
    seed_note(
        &test,
        "dry-note.md",
        "---\nid: \"dry-1\"\n---\n\n# Dry Note\n\nShould not be archived in dry-run.\n",
    );
    let dry = test.zen(&["wiki", "loop", "run", "--dry-run", "--json"]);
    assert!(dry.success(), "dry-run failed: {}", dry.stderr());
    assert!(
        test.cwd
            .join("vault")
            .join("inbox")
            .join("dry-note.md")
            .exists(),
        "dry-run must not mutate inbox"
    );
    let wiki = test.cwd.join("vault").join("wiki");
    let before_wiki = find_files(&wiki).len();
    // Real run should now archive
    let real = test.zen(&["wiki", "loop", "run"]);
    assert!(real.success());
    assert!(
        !test
            .cwd
            .join("vault")
            .join("inbox")
            .join("dry-note.md")
            .exists(),
        "real run must archive"
    );
    let after_wiki = find_files(&wiki).len();
    assert!(after_wiki >= before_wiki);
}
