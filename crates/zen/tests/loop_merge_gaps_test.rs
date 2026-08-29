//! T019 — US2 integration: merge provenance, gap kinds via `zen wiki loop
//! gaps --json`, quarantine flow (quickstart §2/§3/§6).

mod common;

use common::ZenTest;
use std::fs;

fn seed_note(test: &ZenTest, name: &str, body: &str) {
    let inbox = test.cwd.join("vault").join("inbox");
    fs::create_dir_all(&inbox).expect("create inbox");
    fs::write(inbox.join(name), body).expect("write note");
}

const BASE: &str = "# Rust Notes\n\nRust and Tokio power the zen workspace for async programming across many crates.";
// Distinct H1 → distinct concept pages; identical body → near-duplicate cluster.
const TWIN: &str = "# Rust Handbook\n\nRust and Tokio power the zen workspace for async programming across many crates.";

#[test]
fn merge_folds_pure_duplicates_into_one_page() {
    let test = ZenTest::new();
    assert!(test.zen(&["workspace", "init"]).success());
    seed_note(&test, "rust-a.md", &format!("---\nid: \"rust-a\"\n---\n\n{BASE}\n"));
    seed_note(&test, "rust-b.md", &format!("---\nid: \"rust-b\"\n---\n\n{TWIN}\n"));

    let run = test.zen(&["wiki", "loop", "run"]);
    assert!(run.success(), "run failed: {} {}", run.stdout(), run.stderr());

    let archive = test.cwd.join("vault").join("archive");
    let archived: Vec<String> = find_md(&archive)
        .iter()
        .filter_map(|p| fs::read_to_string(p).ok())
        .collect();
    // At least one twin carries merged_into provenance (merge never deletes).
    let merged_away = archived
        .iter()
        .filter(|c| c.contains("merged_into:") && !c.contains("merged_into: \"\""))
        .count();
    assert!(
        merged_away >= 1 || archived.iter().any(|c| c.contains("merged_from:")),
        "merge provenance missing; archived contents: {archived:?}"
    );
}

#[test]
fn gaps_json_lists_detected_kinds() {
    let test = ZenTest::new();
    assert!(test.zen(&["workspace", "init"]).success());
    // No notes → no gaps expected; JSON array shape must hold.
    let gaps = test.zen(&["wiki", "loop", "gaps", "--json"]);
    assert!(gaps.success(), "gaps failed: {}", gaps.stderr());
    assert!(gaps.stdout().trim().starts_with('['));

    seed_note(
        &test,
        "orphan.md",
        "---\nid: \"orphan-1\"\n---\n\n# Orphan Concept\n\nA page about a concept nobody links to.\n",
    );
    let run = test.zen(&["wiki", "loop", "run"]);
    assert!(run.success(), "run failed: {}", run.stderr());

    let gaps = test.zen(&["wiki", "loop", "gaps", "--json"]);
    assert!(gaps.success(), "gaps failed after run: {}", gaps.stderr());
    // Gap kinds are snake_case per data-model §3.
    let out = gaps.stdout();
    assert!(out.contains("kind"), "gap records expose kind: {out}");
}

#[test]
fn malformed_note_quarantined_after_max_attempts() {
    let test = ZenTest::new();
    assert!(test.zen(&["workspace", "init"]).success());
    seed_note(
        &test,
        "broken.md",
        "---\nid: \"broken-1\"\nsensitivity: nonsense-value\n---\n\nThis frontmatter never parses.\n",
    );

    let mut run = test.zen(&["wiki", "loop", "run"]);
    assert!(run.success(), "first run failed: {}", run.stderr());
    // max_attempts default 3 → run 2 more cycles.
    for _ in 0..2 {
        run = test.zen(&["wiki", "loop", "run"]);
        assert!(run.success(), "cycle failed: {}", run.stderr());
    }

    let quarantine = test.cwd.join("vault").join("archive").join("quarantine");
    let quarantined = find_md(&quarantine);
    assert!(
        !quarantined.is_empty(),
        "note must be quarantined after max_attempts cycles"
    );
    assert!(
        fs::read_dir(test.cwd.join("vault").join("inbox"))
            .map(|mut d| d.next().is_none())
            .unwrap_or(true),
        "inbox must be empty (guarantee FR-006)"
    );
}

fn find_md(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            out.extend(find_md(&path));
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            out.push(path);
        }
    }
    out
}
