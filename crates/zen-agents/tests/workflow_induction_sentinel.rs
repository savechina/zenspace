//! W13.9 report-only grep sentinel (design §10 AC#5) plus the W13.8
//! last-run sidecar pins.
//!
//! The sentinel scans every production source file (`crates/*/src/**/*.rs`)
//! and asserts the workflow-induction path literals live ONLY in the core
//! induction module (plus the daily worker, which design §10 AC#5 explicitly
//! allows). No other code path may reference the drafts/rejected ledgers, the
//! flock file, the sidecar, or the promoted-template directory — that is the
//! report-only invariant (nothing outside the induction surface reads a
//! template to alter execution).

use std::path::{Path, PathBuf};

use zen_agents::workflow_induction::{
    DRAFTS_FILE, InsufficientReason, LAST_RUN_FILE, WorkflowInduction, read_last_run,
};
use zen_repo::{SqliteClient, TaskCheckpoint, WorkflowRepo};

const NOW: i64 = 1_800_000_000;

const PATH_LITERALS: &[&str] = &[
    "workflow-induction-drafts.json",
    "workflow-induction-rejected.json",
    "workflow-induction.lock",
    "workflow-induction-last-run.json",
    "wiki/wisdom/workflow-templates",
];

const ALLOWED_RELATIVE: &[&str] = &[
    "zen-agents/src/workflow_induction.rs",
    "zen-agents/src/scheduler/workers/workflow_induction.rs",
];

fn crates_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ parent")
        .to_path_buf()
}

fn collect_rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rust_sources(&path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn production_sources() -> Vec<PathBuf> {
    let mut sources = Vec::new();
    for entry in std::fs::read_dir(crates_root()).unwrap().flatten() {
        if entry.path().is_dir() {
            collect_rust_sources(&entry.path().join("src"), &mut sources);
        }
    }
    sources
}

#[test]
fn workflow_induction_path_literals_confined_to_core_module() {
    let allowed: Vec<PathBuf> = ALLOWED_RELATIVE
        .iter()
        .map(|relative| {
            std::fs::canonicalize(crates_root().join(relative))
                .unwrap_or_else(|_| panic!("allowlisted induction source `{relative}` exists"))
        })
        .collect();

    let mut violations = Vec::new();
    let mut scanned = 0usize;
    for source in production_sources() {
        scanned += 1;
        let Ok(raw) = std::fs::read_to_string(&source) else {
            continue;
        };
        let canonical = std::fs::canonicalize(&source).unwrap_or_else(|_| source.clone());
        let is_allowed = allowed.contains(&canonical);
        for (index, line) in raw.lines().enumerate() {
            for needle in PATH_LITERALS {
                if line.contains(needle) && !is_allowed {
                    violations.push(format!("{}:{}: `{needle}`", source.display(), index + 1,));
                }
            }
        }
    }

    assert!(
        scanned > 0,
        "sentinel scanned no sources — the walk root is wrong"
    );
    assert!(
        violations.is_empty(),
        "workflow-induction path literals escaped the induction module \
         (AC#5 report-only invariant):\n{}",
        violations.join("\n")
    );
}

#[test]
fn workflow_induction_sidecar_name_is_single_sourced() {
    let occurrences: usize = production_sources()
        .iter()
        .filter_map(|source| std::fs::read_to_string(source).ok())
        .map(|raw| raw.matches(LAST_RUN_FILE).count())
        .sum();
    assert_eq!(
        occurrences, 1,
        "the `{LAST_RUN_FILE}` literal must be defined exactly once (core module)"
    );
}

fn dag_spec() -> String {
    let tasks: Vec<serde_json::Value> = [
        ("t1", "researcher", Vec::<&str>::new()),
        ("t2", "analyst", vec!["t1"]),
        ("t3", "synthesizer", vec!["t2"]),
    ]
    .iter()
    .map(|(id, agent, deps)| {
        serde_json::json!({
            "id": id,
            "agent": agent,
            "prompt": "deterministic prompt",
            "depends_on": deps,
        })
    })
    .collect();
    serde_json::json!({
        "tasks": tasks,
        "sensitivity": "Private",
        "session_id": "sess",
    })
    .to_string()
}

async fn seed_completed_plan(client: &SqliteClient, plan_id: &str, created_at: i64) {
    let repo = WorkflowRepo::new(client);
    repo.create_plan(plan_id, Some("shape"), &dag_spec(), created_at)
        .await
        .unwrap();
    for (task_id, agent) in [
        ("t1", "researcher"),
        ("t2", "analyst"),
        ("t3", "synthesizer"),
    ] {
        repo.checkpoint_task(TaskCheckpoint {
            plan_id,
            task_id,
            agent,
            status: "completed",
            response: None,
            error: None,
            now: created_at,
            model: Some("model-x"),
            duration_ms: Some(1_000),
            cost_usd: Some(0.01),
        })
        .await
        .unwrap();
    }
    // W14.3: admission requires the engine sign-off.
    repo.complete_plan(plan_id, "completed", None, None, None, created_at)
        .await
        .unwrap();
}

#[tokio::test]
async fn workflow_induction_sidecar_records_run_counters() {
    let logs = tempfile::tempdir().unwrap();
    let db = tempfile::tempdir().unwrap();
    let client = SqliteClient::open(&db.path().join("state.db"))
        .await
        .unwrap();
    seed_completed_plan(&client, "a", NOW - 10).await;
    seed_completed_plan(&client, "b", NOW - 20).await;

    let run = WorkflowInduction::new(logs.path().to_path_buf())
        .run(&client, NOW)
        .await
        .unwrap();
    assert_eq!(run.drafts_written, 1);

    let summary = read_last_run(logs.path()).expect("sidecar written on a corpus run");
    assert_eq!(summary.drafts_written, 1);
    assert_eq!(summary.skipped_corrupt, 0);
    assert_eq!(summary.skipped_titles_only, 0);
    assert!(!summary.insufficient);
    assert_eq!(summary.missing, None);
}

#[tokio::test]
async fn workflow_induction_zero_corpus_sidecar_only_no_drafts() {
    let logs = tempfile::tempdir().unwrap();
    let db = tempfile::tempdir().unwrap();
    let client = SqliteClient::open(&db.path().join("state.db"))
        .await
        .unwrap();

    WorkflowInduction::new(logs.path().join("logs"))
        .run(&client, NOW)
        .await
        .unwrap();

    let summary = read_last_run(&logs.path().join("logs"))
        .expect("zero-corpus run records the sidecar (W14.5 amended AC#2)");
    assert!(summary.insufficient);
    assert_eq!(
        summary.missing.as_deref(),
        Some(InsufficientReason::NoPlansInWindow.missing())
    );
    assert!(
        !logs.path().join("logs").join(DRAFTS_FILE).exists(),
        "zero-corpus run never writes the drafts queue"
    );
}

#[test]
fn workflow_induction_sidecar_reader_is_fail_open() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        read_last_run(dir.path()).is_none(),
        "missing sidecar ⇒ None"
    );
    std::fs::write(dir.path().join(LAST_RUN_FILE), "{ broken").unwrap();
    assert!(
        read_last_run(dir.path()).is_none(),
        "corrupt sidecar ⇒ None"
    );
}
