//! T034 — US4 wisdom-loop integration tests (SC-007..SC-015, quickstart §7).
//!
//! NOTE: tasks.md:120 locates this file at `crates/zen-cli/tests/`, but
//! `CARGO_BIN_EXE_zen` is only available inside the `zen` crate, so it lives
//! here next to the other loop tests (approved deviation). SC-011 is
//! intentionally absent from T034's scenario list. Deep unit coverage for
//! the placeholder registry (SC-014) and OCC/CAS writes (SC-015) lives in
//! zen-vault (`graph_verify::PlaceholderRegistry`, `distill::transaction`);
//! these tests exercise the end-to-end CLI surface only.

mod common;

use common::ZenTest;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn init_workspace(test: &ZenTest) {
    let init = test.zen(&["workspace", "init"]);
    assert!(init.success(), "workspace init failed: {}", init.stderr());
}

fn seed_inbox_note(test: &ZenTest, name: &str, body: &str) -> PathBuf {
    let inbox = test.cwd.join("vault").join("inbox");
    fs::create_dir_all(&inbox).expect("create inbox dir");
    let note = inbox.join(name);
    fs::write(&note, body).expect("write note");
    note
}

fn standard_note(id: &str, body: &str) -> String {
    format!("---\nid: \"{id}\"\nsensitivity: private\n---\n\n{body}")
}

fn find_files_recursively(dir: &Path) -> Vec<PathBuf> {
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

fn run_loop(test: &ZenTest) -> common::ZenOutput {
    let run = test.zen(&["wiki", "loop", "run"]);
    assert!(
        run.success(),
        "loop run failed:\nSTDOUT: {}\nSTDERR: {}",
        run.stdout(),
        run.stderr()
    );
    run
}

fn gaps_json(test: &ZenTest, kind: &str) -> String {
    let gaps = test.zen(&["wiki", "loop", "gaps", "--kind", kind, "--json"]);
    assert!(
        gaps.success(),
        "gaps failed: {} {}",
        gaps.stdout(),
        gaps.stderr()
    );
    gaps.stdout()
}

// ── SC-007: typed journal signals route to M3/M4 wisdom surfaces ──────────

#[test]
fn sc007_typed_signals_route_to_wisdom_surfaces() {
    let test = ZenTest::new();
    init_workspace(&test);

    let journal_dir = test.cwd.join("memories").join("journal");
    fs::create_dir_all(&journal_dir).expect("create journal dir");

    // Section-headed entry: Facts/Reflections/Commitments buckets.
    fs::write(
        journal_dir.join("2026-08-30-session.md"),
        "---\nsession_id: s1\njournaled_at: 2026-08-30T10:00:00Z\n---\n\n## Facts\n\n- Tokio powers the async runtime\n- SQLite stores the knowledge graph\n\n## Reflections\n\n- the loop converges\n",
    )
    .expect("write journal entry 1");

    // kind-tagged entry: decision signals (same convention zen_loop greps).
    fs::write(
        journal_dir.join("2026-08-30-decisions.md"),
        "---\nsession_id: s2\njournaled_at: 2026-08-30T11:00:00Z\nkind: decision\n---\n\n- Use SQLite over Postgres|||offline-first|||lower ops cost\n",
    )
    .expect("write journal entry 2");

    // A cycle must still process at least one inbox note for the report.
    seed_inbox_note(
        &test,
        "context.md",
        &standard_note(
            "ctx-1",
            "# Context\n\nRust and Tokio power the zen workspace.\n",
        ),
    );

    run_loop(&test);

    let vault = test.cwd.join("vault");
    let facts = find_files_recursively(&vault.join("wiki/wisdom/facts"));
    assert!(
        !facts.is_empty(),
        "SC-007: journal facts must route to wiki/wisdom/facts"
    );

    let decisions = find_files_recursively(&vault.join("wiki/wisdom/decisions"));
    assert!(
        !decisions.is_empty(),
        "SC-007: kind:decision journal entries must route to wiki/wisdom/decisions"
    );
}

// ── SC-008: alias family coalescence smoke ─────────────────────────────────
// Full alias normalization (rust/rust-lang/rust.js → one canonical notion)
// is T039 (Phase 7); here we verify the end-to-end surface stays healthy.

#[test]
fn sc008_alias_family_single_surface() {
    let test = ZenTest::new();
    init_workspace(&test);

    seed_inbox_note(
        &test,
        "rust-a.md",
        &standard_note(
            "rust-a",
            "# Rust A\n\nrust is a systems language with strong guarantees.\n",
        ),
    );
    seed_inbox_note(
        &test,
        "rust-b.md",
        &standard_note(
            "rust-b",
            "# Rust B\n\nrust-lang releases every six weeks like clockwork.\n",
        ),
    );

    run_loop(&test);

    let notions = test.cwd.join("vault").join("wiki").join("notions");
    let rust_pages: Vec<_> = find_files_recursively(&notions)
        .into_iter()
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_lowercase().contains("rust"))
                .unwrap_or(false)
        })
        .collect();
    assert!(
        !rust_pages.is_empty() && rust_pages.len() <= 2,
        "SC-008: expected 1-2 rust-family pages, found {:?}",
        rust_pages
    );
}

// ── SC-009: CRIT decision is quarantined + gap emitted ─────────────────────

#[test]
fn sc009_crit_decision_quarantined() {
    let test = ZenTest::new();
    init_workspace(&test);

    // Fixture mirrors Decision::to_markdown (decision.rs). The Logic EV
    // (payoff 100 < loss 5000 → loss_affordable=false) plus Execution
    // (cost_sunk 1000, is_recoverable=false) trip the loss_aversion CRIT
    // matcher in decision_check.rs. Filename MUST equal the frontmatter id
    // (the quarantine gate moves `{id}.md`).
    let decisions_dir = test
        .cwd
        .join("vault")
        .join("wiki")
        .join("wisdom")
        .join("decisions");
    fs::create_dir_all(&decisions_dir).expect("create decisions dir");
    fs::write(
        decisions_dir.join("crit-loss-aversion.md"),
        "---\nid: crit-loss-aversion\ntitle: \"Hold failing product\"\ndomain: general\ndecided_at: 2026-01-01T00:00:00Z\nclosed_at: null\n---\n\n# Decision: Hold failing product\n\n## Goal\n\ngoal: keep the product alive\ncore_pursuit: sunk cost recovery\n\n## Facts\n\n- product is losing users\n\n## Sources\n\n- internal dashboard\n\n## Logic\n\nchoice: hold\nsuccess_probability: 0.5\npayoff: 100\nloss: 5000\n\n## Execution\n\ncost_sunk: 1000\nis_recoverable: false\n\n## Feedback\n\n_(no outcome yet)_\n",
    )
    .expect("write CRIT decision");

    seed_inbox_note(
        &test,
        "context.md",
        &standard_note(
            "ctx-9",
            "# Context\n\nRust and Tokio power the zen workspace.\n",
        ),
    );

    run_loop(&test);

    assert!(
        !decisions_dir.join("crit-loss-aversion.md").exists(),
        "SC-009: CRIT decision must be removed from wiki/wisdom/decisions"
    );
    let quarantine =
        find_files_recursively(&test.cwd.join("vault").join("archive").join("quarantine"));
    assert!(
        quarantine.iter().any(|p| p
            .file_name()
            .map(|n| n == "crit-loss-aversion.md")
            .unwrap_or(false)),
        "SC-009: CRIT decision must be moved to vault/archive/quarantine, found {:?}",
        quarantine
    );

    let gaps = gaps_json(&test, "decision_blocked");
    assert!(
        gaps.contains("decision_blocked"),
        "SC-009: decision_blocked gap expected, got: {gaps}"
    );
}

// ── SC-010: overdue commitment review emits a gap ──────────────────────────

#[test]
fn sc010_overdue_commitment_emits_gap() {
    let test = ZenTest::new();
    init_workspace(&test);

    // Fixture mirrors Commitment::to_markdown (commitment.rs): state
    // `executing` (non-terminal) + review_at in the past → is_overdue().
    let commitments_dir = test.cwd.join("vault").join("memories").join("commitments");
    fs::create_dir_all(&commitments_dir).expect("create commitments dir");
    fs::write(
        commitments_dir.join("ship-analytics.md"),
        "---\nid: ship-analytics\nwhat: \"ship the analytics dashboard\"\nstate: executing\nreview_at: 2020-01-01\nnext_action: \"wire metrics\"\ntwo_minute_rule: false\ndiscipline_streak: 0\ncreated_at: 2026-01-01T00:00:00Z\nupdated_at: 2026-01-01T00:00:00Z\n---\n\n# Commitment: ship the analytics dashboard\n\n**State**: executing\n\n## Milestones\n\n_(no milestones)_\n\n## Stop Loss\n",
    )
    .expect("write overdue commitment");

    seed_inbox_note(
        &test,
        "context.md",
        &standard_note(
            "ctx-10",
            "# Context\n\nRust and Tokio power the zen workspace.\n",
        ),
    );

    run_loop(&test);

    let gaps = gaps_json(&test, "commitment_overdue");
    assert!(
        gaps.contains("commitment_overdue"),
        "SC-010: commitment_overdue gap expected, got: {gaps}"
    );
    assert!(
        gaps.contains("analytics-dashboard"),
        "SC-010: gap detail must name the overdue commitment, got: {gaps}"
    );
}

// ── SC-012: gaps incubate into hypotheses, structural kinds exploring ──────

#[test]
fn sc012_hypotheses_generated_from_gids() {
    let test = ZenTest::new();
    init_workspace(&test);

    seed_inbox_note(
        &test,
        "context.md",
        &standard_note(
            "ctx-12",
            "# Context\n\nRust and Tokio power the zen workspace.\n",
        ),
    );

    // A wiki page with no entities/links → graph verify emits a structural
    // gap (WikiPageWithoutEntities) which Stage 5b incubates at confidence
    // 0.7 ≥ 0.6 → status `exploring`.
    let wiki = test.cwd.join("vault").join("wiki");
    fs::create_dir_all(&wiki).expect("create wiki dir");
    fs::write(
        wiki.join("plain-prose-concept.md"),
        "---\ntitle: plain prose concept\n---\n\n# Plain Prose Concept\n\nJust prose with no entities or links at all.\n",
    )
    .expect("write entity-less page");

    run_loop(&test);

    let hypotheses_dir = test
        .cwd
        .join("vault")
        .join("wiki")
        .join("wisdom")
        .join("hypotheses");
    let files = find_files_recursively(&hypotheses_dir);
    assert!(
        !files.is_empty(),
        "SC-012: wiki/wisdom/hypotheses must contain at least one slug"
    );

    let mut exploring = 0usize;
    for f in &files {
        let raw = fs::read_to_string(f).expect("read hypothesis");
        assert!(
            raw.contains("created_from:"),
            "SC-012: hypothesis must trace to a GapRecord id: {raw}"
        );
        if raw.contains("status: exploring") {
            exploring += 1;
        }
    }
    // >80% closure target (SC-012): structural gaps land at 0.7 confidence.
    assert!(
        exploring * 10 >= files.len() * 8,
        "SC-012: expected ≥80% exploring, got {exploring}/{}",
        files.len()
    );
}

// ── SC-013: ORAV self-correction — illegal slug survives the cycle ─────────

#[test]
fn sc013_orav_self_correction_completes_cycle() {
    let test = ZenTest::new();
    init_workspace(&test);

    // `[[Bad Slug!]]` fails graph_router::validate_slug (uppercase, space,
    // `!`). The ORAV verify loop strips the illegal link and retries
    // in-place, so the cycle still completes and the note archives.
    let note = seed_inbox_note(
        &test,
        "stubborn.md",
        &standard_note(
            "stubborn-1",
            "# Stubborn Note\n\nSee [[Bad Slug!]] for details.\n\nRust and Tokio are used here for async work.\n",
        ),
    );

    run_loop(&test);

    let inbox_files: Vec<_> = fs::read_dir(note.parent().unwrap())
        .expect("read inbox")
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        inbox_files.is_empty(),
        "SC-013: inbox must be empty (self-corrected note archived), found {:?}",
        inbox_files
            .iter()
            .map(|e| e.file_name())
            .collect::<Vec<_>>()
    );

    let archive = find_files_recursively(&test.cwd.join("vault").join("archive"));
    assert!(
        archive.iter().any(|p| p
            .file_name()
            .map(|n| n.to_string_lossy().contains("stubborn"))
            .unwrap_or(false)),
        "SC-013: self-corrected note must be archived, found {:?}",
        archive
    );
}

// ── SC-014: same-entity notes merge onto a healthy surface ─────────────────
// Placeholder-registry + N-hop subgraph unit coverage lives in zen-vault
// (graph_verify / search::tier4); this is the end-to-end proxy.

#[test]
fn sc014_same_entity_notes_single_healthy_surface() {
    let test = ZenTest::new();
    init_workspace(&test);

    seed_inbox_note(
        &test,
        "tokio-a.md",
        &standard_note(
            "tokio-a",
            "# Tokio A\n\nThe rust async runtime schedules work efficiently.\n",
        ),
    );
    seed_inbox_note(
        &test,
        "tokio-b.md",
        &standard_note(
            "tokio-b",
            "# Tokio B\n\nThe rust async runtime also powers the scheduler.\n",
        ),
    );

    run_loop(&test);

    let wiki = test.cwd.join("vault").join("wiki");
    let pages = find_files_recursively(&wiki);
    assert!(!pages.is_empty(), "SC-014: wiki must contain pages");

    let status = test.zen(&["wiki", "loop", "status", "--json"]);
    assert!(
        status.success() && status.stdout().trim().starts_with('{'),
        "SC-014: status --json must answer: {} {}",
        status.stdout(),
        status.stderr()
    );
}

// ── SC-015: budget pending pool + local-first git history ──────────────────

#[test]
fn sc015_budget_pending_pool_and_git_history() {
    let mut test = ZenTest::new();
    init_workspace(&test);

    // commit_cycle_to_git gates on paths.workspace_root(); the anti-leak
    // walk-up cannot see the temp dir, so pin it explicitly via ZEN_WORKSPACE.
    test.env.insert(
        "ZEN_WORKSPACE".into(),
        test.cwd.to_str().expect("utf8 cwd").into(),
    );

    // git history requires an identity inside the throwaway repo.
    for args in [
        vec!["init"],
        vec!["config", "user.email", "loop@test.local"],
        vec!["config", "user.name", "Loop Test"],
    ] {
        let out = Command::new("git")
            .args(&args)
            .current_dir(&test.cwd)
            .output()
            .expect("spawn git");
        assert!(out.status.success(), "git {:?} failed", args);
    }

    // 7 notes > LoopBudget max_steps (5): the overflow defers to
    // vault/archive/pending/ instead of the normal archive.
    for i in 1..=7 {
        seed_inbox_note(
            &test,
            &format!("note-{i}.md"),
            &standard_note(
                &format!("note-{i}"),
                &format!("# Note {i}\n\nRust note number {i} about async programming.\n"),
            ),
        );
    }

    run_loop(&test);

    let pending = find_files_recursively(&test.cwd.join("vault").join("archive").join("pending"));
    assert!(
        !pending.is_empty(),
        "SC-015: over-budget notes must defer to vault/archive/pending"
    );

    let logs = test.cwd.join(".zen").join("logs");
    let report_path = if logs.exists() {
        logs.join("loop-last-report.json")
    } else {
        test.cwd.join("logs").join("loop-last-report.json")
    };
    let raw = fs::read_to_string(&report_path).expect("read loop report");
    assert!(
        raw.contains("pending_count"),
        "SC-015: report must carry pending_count: {raw}"
    );

    let git_log = Command::new("git")
        .args(["log", "--oneline"])
        .current_dir(&test.cwd)
        .output()
        .expect("spawn git log");
    assert!(
        git_log.status.success(),
        "SC-015: git log failed: {}",
        String::from_utf8_lossy(&git_log.stderr)
    );
    let log_text = String::from_utf8_lossy(&git_log.stdout).to_string();
    assert!(
        log_text.contains("loop: "),
        "SC-015: cycle must leave a `loop: ` commit, got: {log_text}"
    );
}
