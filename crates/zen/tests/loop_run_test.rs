//! T012 — US1 happy path: one loop cycle processes inbox → wiki → archive
//! (quickstart §1, isolated ZEN_HOME temp dir).

mod common;

use common::ZenTest;
use std::fs;

fn seed_note(test: &ZenTest) -> std::path::PathBuf {
    // ZenTest sets ZEN_HOME = cwd, so ZenPaths resolves the vault to
    // `<cwd>/vault` regardless of workspace detection (which is disabled
    // when ZEN_HOME is set, per find_workspace_root's anti-leak rule).
    let inbox = test.cwd.join("vault").join("inbox");
    fs::create_dir_all(&inbox).expect("create inbox dir");
    let note = inbox.join("rust-notes.md");
    fs::write(
        &note,
        "---\nid: \"rust-notes-1\"\nsensitivity: private\n---\n\n# Rust Notes\n\nRust and Tokio power the zen workspace for async programming.\n",
    )
    .expect("write note");
    note
}

#[test]
fn loop_run_processes_inbox_to_wiki_and_archive() {
    let test = ZenTest::new();

    let init = test.zen(&["workspace", "init"]);
    assert!(init.success(), "workspace init failed: {}", init.stderr());

    let note = seed_note(&test);
    assert!(note.exists(), "note must be seeded before the cycle");

    let run = test.zen(&["wiki", "loop", "run"]);
    assert!(
        run.success(),
        "loop run failed:\nSTDOUT: {}\nSTDERR: {}",
        run.stdout(),
        run.stderr()
    );

    // Inbox-empty guarantee (FR-006).
    let inbox = note.parent().unwrap();
    let remaining: Vec<_> = fs::read_dir(inbox)
        .expect("read inbox")
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        remaining.is_empty(),
        "inbox must be empty after a Completed cycle, found: {:?}",
        remaining.iter().map(|e| e.file_name()).collect::<Vec<_>>()
    );

    // Archive provenance (FR-006/007).
    let archive = inbox.parent().unwrap().join("archive");
    let archived = find_files_recursively(&archive);
    assert!(
        !archived.is_empty(),
        "archive must contain the processed note"
    );
    let archived_content = fs::read_to_string(&archived[0]).expect("read archived note");
    assert!(
        archived_content.contains("source_path:"),
        "archived note must carry provenance: {archived_content}"
    );
    assert!(archived_content.contains("cycle_id:"));

    // Wiki page created.
    let wiki = inbox.parent().unwrap().join("wiki");
    let pages = find_files_recursively(&wiki);
    assert!(!pages.is_empty(), "wiki must contain at least one page");

    // Report persisted for status.
    let logs = test.cwd.join(".zen").join("logs");
    let report_path = if logs.exists() {
        logs.join("loop-last-report.json")
    } else {
        test.cwd.join("logs").join("loop-last-report.json")
    };
    assert!(
        report_path.exists(),
        "loop-last-report.json must be persisted at {:?}",
        report_path
    );
}

#[test]
fn loop_status_reports_before_first_cycle() {
    let test = ZenTest::new();
    let init = test.zen(&["workspace", "init"]);
    assert!(init.success(), "workspace init failed: {}", init.stderr());

    let status = test.zen(&["wiki", "loop", "status"]);
    assert!(
        status.success(),
        "status failed: {} {}",
        status.stdout(),
        status.stderr()
    );
    assert!(status.stdout().contains("Enabled"));
}

#[test]
fn loop_gaps_empty_state_and_json_flag() {
    let test = ZenTest::new();
    let init = test.zen(&["workspace", "init"]);
    assert!(init.success(), "workspace init failed: {}", init.stderr());

    let gaps = test.zen(&["wiki", "loop", "gaps", "--json"]);
    assert!(
        gaps.success(),
        "gaps failed: {} {}",
        gaps.stdout(),
        gaps.stderr()
    );
    assert!(gaps.stdout().trim().starts_with('['), "JSON array expected");
}

#[test]
fn loop_enable_disable_persists_config() {
    let test = ZenTest::new();
    let init = test.zen(&["workspace", "init"]);
    assert!(init.success(), "workspace init failed: {}", init.stderr());

    let disable = test.zen(&["wiki", "loop", "disable"]);
    assert!(
        disable.success(),
        "disable failed: {} {}",
        disable.stdout(),
        disable.stderr()
    );
    let config_path = test.cwd.join("config.toml");
    let raw = fs::read_to_string(&config_path).expect("workspace config.toml exists");
    assert!(raw.contains("enabled = false"), "disabled persisted: {raw}");

    let enable = test.zen(&["wiki", "loop", "enable"]);
    assert!(
        enable.success(),
        "enable failed: {} {}",
        enable.stdout(),
        enable.stderr()
    );
    let raw = fs::read_to_string(&config_path).expect("config re-read");
    assert!(raw.contains("enabled = true"), "enabled persisted: {raw}");
}

fn find_files_recursively(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                out.extend(find_files_recursively(&path));
            } else {
                out.push(path);
            }
        }
    }
    out
}
