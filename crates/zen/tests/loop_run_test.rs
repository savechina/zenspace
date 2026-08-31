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

/// D1 regression (2026-08-31 review): manual `zen wiki distill` must persist
/// to the canonical `<data>/state.db` (the same file `ZenLoopWorker` opens),
/// never to a decoy `<logs>/state.db`. The branch briefly split the two,
/// giving the loop and manual runs disjoint DistillState rows.
#[test]
fn manual_distill_persists_to_canonical_data_db() {
    let test = ZenTest::new();
    let init = test.zen(&["workspace", "init"]);
    assert!(init.success(), "workspace init failed: {}", init.stderr());

    seed_note(&test);
    let distill = test.zen(&["wiki", "distill"]);
    assert!(
        distill.success(),
        "distill failed:\nSTDOUT: {}\nSTDERR: {}",
        distill.stdout(),
        distill.stderr()
    );

    let data_variants = [
        test.cwd.join("data").join("state.db"),
        test.cwd.join(".zen").join("data").join("state.db"),
    ];
    assert!(
        data_variants.iter().any(|p| p.exists()),
        "manual distill must persist to <data>/state.db (found neither {} nor {})",
        data_variants[0].display(),
        data_variants[1].display()
    );

    let decoys = [
        test.cwd.join("logs").join("state.db"),
        test.cwd.join(".zen").join("logs").join("state.db"),
    ];
    for decoy in &decoys {
        assert!(
            !decoy.exists(),
            "decoy DB must not exist under logs/: {}",
            decoy.display()
        );
    }
}

/// D2 regression (2026-08-31 review): when `workspace_root()` resolves to a
/// git repo that is UNRELATED to the zen vault (ZEN_WORKSPACE override), the
/// loop cycle must complete without committing anything to that repo. The
/// 2026-08 incident: `git add -A` inside the source repo swept unrelated
/// dirty state into `loop:` commits.
#[test]
fn loop_cycle_skips_commit_for_unrelated_workspace_repo() {
    let mut test = ZenTest::new();
    let init = test.zen(&["workspace", "init"]);
    assert!(init.success(), "workspace init failed: {}", init.stderr());

    let repo_dir = test.cwd.join("unrelated-repo");
    fs::create_dir_all(&repo_dir).expect("create repo dir");
    let git_env = test.env.clone();
    let git_in = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo_dir)
            .env_clear()
            .envs(&git_env)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    };
    let rev_count = || {
        let out = git_in(&["rev-list", "--count", "HEAD"]);
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    git_in(&["init"]);
    git_in(&["config", "user.email", "test@zen.local"]);
    git_in(&["config", "user.name", "Zen Test"]);
    fs::write(
        repo_dir.join("unrelated.txt"),
        "dirty file that must NOT be committed\n",
    )
    .expect("seed dirty file");
    git_in(&["add", "."]);
    git_in(&["commit", "-m", "base"]);
    let base_revs = rev_count();

    // Vault/logs stay under ZEN_HOME (= cwd), outside the unrelated repo:
    // the containment check must see zero managed paths and skip the commit.
    test.env
        .insert("ZEN_WORKSPACE".into(), repo_dir.to_str().unwrap().into());
    seed_note(&test);

    let run = test.zen(&["wiki", "loop", "run"]);
    assert!(
        run.success(),
        "loop run failed:\nSTDOUT: {}\nSTDERR: {}",
        run.stdout(),
        run.stderr()
    );

    assert_eq!(
        rev_count(),
        base_revs,
        "loop must not commit an unrelated workspace repo"
    );
    assert!(
        repo_dir.join("unrelated.txt").exists(),
        "unrelated file must survive untouched"
    );
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
