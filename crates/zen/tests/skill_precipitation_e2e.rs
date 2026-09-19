//! T078 — e2e: skill precipitation through the CLI (FR-037 Hybrid C, D02).
//!
//! Flow: seed ≥2 similar tool successes into the skill history →
//! `zen skill precipitate` stages a draft → `zen skill confirm` (the
//! first-occurrence gate) → `~/.zen/skills/<name>/SKILL.md` exists with the
//! D02 frontmatter. The follow-on "3rd matching query auto-injects" leg
//! requires a live orchestrator turn (LLM) and is covered in-process by
//! `zen-agents/tests/skill_hit_contract.rs`
//! (`orchestrator_injects_skill_prompt_before_route_and_honors_gates`).

mod common;

use common::ZenTest;
use std::fs;

fn seed_success_history(test: &ZenTest, skill: &str, successes: usize) {
    let skills_dir = test.cwd.join("skills");
    fs::create_dir_all(&skills_dir).expect("create skills dir");
    let mut jsonl = String::new();
    for i in 0..successes {
        let record = serde_json::json!({
            "skill_name": skill,
            "timestamp": format!("2026-09-0{}T10:00:00Z", i + 1),
            "duration_ms": 120,
            "quality_rating": 9,
            "context_summary": "fix cargo build error in workspace",
            "result_summary": "build succeeded",
        });
        jsonl.push_str(&record.to_string());
        jsonl.push('\n');
    }
    fs::write(skills_dir.join(format!("{skill}-history.jsonl")), jsonl)
        .expect("seed skill history");
}

#[test]
fn skill_precipitation_two_successes_confirm_then_skill_md_exists() {
    let test = ZenTest::new();

    let init = test.zen(&["workspace", "init"]);
    assert!(init.success(), "workspace init failed: {}", init.stderr());

    // Hybrid C detection signal: ≥2 similar high-quality successes.
    seed_success_history(&test, "rust-build-fix", 2);

    // Before detection there is no skill and no pending draft.
    let skills_dir = test.cwd.join("skills");
    assert!(!skills_dir.join("rust-build-fix").exists());

    // Dream's precipitation step (same code path via the CLI surface).
    let precipitate = test.zen(&["skill", "precipitate"]);
    assert!(
        precipitate.success(),
        "precipitate failed:\nSTDOUT: {}\nSTDERR: {}",
        precipitate.stdout(),
        precipitate.stderr()
    );
    let stdout = precipitate.stdout();
    assert!(
        stdout.contains("rust-build-fix"),
        "staged draft must be listed: {stdout}"
    );

    // Hybrid C first-occurrence gate: the draft is pending, NOT promoted.
    assert!(
        !skills_dir.join("rust-build-fix").join("SKILL.md").exists(),
        "unconfirmed draft must not auto-promote"
    );
    let queue = test.cwd.join("logs").join("skill-confirmations.json");
    assert!(queue.is_file(), "pending queue must exist at {queue:?}");

    // User confirms → SKILL.md with D02 frontmatter.
    let confirm = test.zen(&["skill", "confirm", "rust-build-fix"]);
    assert!(
        confirm.success(),
        "confirm failed:\nSTDOUT: {}\nSTDERR: {}",
        confirm.stdout(),
        confirm.stderr()
    );

    let skill_md = skills_dir.join("rust-build-fix").join("SKILL.md");
    assert!(
        skill_md.is_file(),
        "confirmed skill must exist at {skill_md:?}"
    );
    let content = fs::read_to_string(&skill_md).expect("read SKILL.md");
    assert!(
        content.contains("name: rust-build-fix"),
        "frontmatter: {content}"
    );
    assert!(content.contains("description:"), "frontmatter: {content}");
    assert!(content.contains("triggers: ["), "frontmatter: {content}");

    // FR-040: the rendered SKILL.md carries a Gotchas section, and a pitfall
    // recorded in the evidence (the seeded context mentions an error) appears
    // there rather than only under Evidence.
    assert!(
        content.contains("## Gotchas"),
        "FR-040: SKILL.md must carry a Gotchas section: {content}"
    );
    let gotchas = content
        .split("## Gotchas")
        .nth(1)
        .expect("Gotchas section body");
    assert!(
        gotchas.contains("error"),
        "a recorded pitfall must be listed under Gotchas: {gotchas}"
    );

    // A second confirm without a pending draft fails cleanly.
    let confirm_again = test.zen(&["skill", "confirm", "rust-build-fix"]);
    assert!(
        !confirm_again.success(),
        "confirm must fail when nothing is pending"
    );

    // The router side: once SKILL.md exists with triggers, the skill is
    // auto-discoverable — `zen skill list` must show it.
    let list = test.zen(&["skill", "list"]);
    assert!(list.success(), "skill list failed: {}", list.stderr());
    assert!(
        list.stdout().contains("rust-build-fix"),
        "confirmed skill must be discoverable: {}",
        list.stdout()
    );

    // FR-039 neutral plane: `--json` must emit valid JSON (an array of
    // skill definitions) containing the precipitated skill.
    let list_json = test.zen(&["skill", "list", "--json"]);
    assert!(
        list_json.success(),
        "skill list --json failed: {}",
        list_json.stderr()
    );
    let defs: serde_json::Value =
        serde_json::from_str(&list_json.stdout()).expect("--json must emit valid JSON");
    let defs = defs.as_array().expect("--json must emit an array");
    assert!(
        defs.iter()
            .any(|d| d.get("name").and_then(|n| n.as_str()) == Some("rust-build-fix")),
        "confirmed skill must appear in --json output: {defs:?}"
    );
}
