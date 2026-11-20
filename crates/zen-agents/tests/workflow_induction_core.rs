use std::path::Path;

use tempfile::TempDir;
use zen_agents::workflow_induction::{
    DRAFTS_FILE, INDUCTION_WINDOW_DAYS, InsufficientReason, MIN_OBSERVATIONS,
    OBSERVATION_SIMILARITY, REJECTED_FILE, SpecShape, WorkflowDraft, WorkflowInduction,
    acquire_induction_lock, read_drafts, read_last_run, shape_signature_from_spec_json,
};
use zen_repo::{SqliteClient, TaskCheckpoint, WorkflowRepo};
use zen_vault::distill::trigram_jaccard;

const NOW: i64 = 1_800_000_000;

fn spec(tasks: &[(&str, &str, &[&str], &str)]) -> String {
    let tasks: Vec<serde_json::Value> = tasks
        .iter()
        .map(|(id, agent, deps, prompt)| {
            serde_json::json!({
                "id": id,
                "agent": agent,
                "prompt": prompt,
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

fn dag_spec(analyst: &str, writer: &str, prompt: &str) -> String {
    spec(&[
        ("t1", "researcher", &[], prompt),
        ("t2", analyst, &["t1"], prompt),
        ("t3", writer, &["t1"], prompt),
        ("t4", "synthesizer", &["t2", "t3"], prompt),
    ])
}

fn dag_rows<'a>(analyst: &'a str, writer: &'a str) -> [(&'a str, &'a str, &'a str); 4] {
    [
        ("t1", "researcher", "completed"),
        ("t2", analyst, "completed"),
        ("t3", writer, "completed"),
        ("t4", "synthesizer", "completed"),
    ]
}

async fn seed_plan(
    client: &SqliteClient,
    plan_id: &str,
    name: &str,
    spec_json: &str,
    created_at: i64,
    status: &str,
    rows: &[(&str, &str, &str)],
) {
    let repo = WorkflowRepo::new(client);
    repo.create_plan(plan_id, Some(name), spec_json, created_at)
        .await
        .unwrap();
    for &(task_id, agent, row_status) in rows {
        repo.checkpoint_task(TaskCheckpoint {
            plan_id,
            task_id,
            agent,
            status: row_status,
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
    // W14.3: admission requires the engine sign-off — every fixture
    // completes the plan row (statuses vary to exercise the gate).
    repo.complete_plan(plan_id, status, None, None, None, created_at)
        .await
        .unwrap();
}

async fn open_db(dir: &Path) -> SqliteClient {
    SqliteClient::open(&dir.join("state.db")).await.unwrap()
}

fn signature_of(spec_json: &str) -> String {
    match shape_signature_from_spec_json(spec_json).unwrap() {
        SpecShape::Dag { signature, .. } => signature,
        SpecShape::TitlesOnly => panic!("expected a DAG shape"),
    }
}

#[test]
fn workflow_induction_shuffled_declaration_order_produces_identical_signature() {
    let ordered = dag_spec("analyst", "writer", "p");
    let shuffled = spec(&[
        ("t4", "synthesizer", &["t2", "t3"], "p"),
        ("t2", "analyst", &["t1"], "p"),
        ("t1", "researcher", &[], "p"),
        ("t3", "writer", &["t1"], "p"),
    ]);

    let a = signature_of(&ordered);
    let b = signature_of(&shuffled);

    assert_eq!(a, b, "declaration order must not leak into the signature");
    assert_eq!(
        a, "analyst:1,1;researcher:0,2;synthesizer:2,0;writer:1,1",
        "lexicographic (agent, in, out) serialization with deduplicated degrees"
    );
}

#[tokio::test]
async fn workflow_induction_titles_only_shape_is_not_corrupt() {
    let items = serde_json::json!({
        "items": [
            {"id": "a", "title": "A", "status": "pending"},
            {"id": "b", "title": "B", "status": "pending"},
        ],
        "sensitivity": "Private",
        "session_id": "sess",
    })
    .to_string();
    assert_eq!(
        shape_signature_from_spec_json(&items).unwrap(),
        SpecShape::TitlesOnly
    );

    let logs = TempDir::new().unwrap();
    let db = TempDir::new().unwrap();
    let client = open_db(db.path()).await;
    seed_plan(
        &client,
        "todos-a",
        "todos",
        &items,
        NOW - 10,
        "completed",
        &[("a", "", "completed"), ("b", "", "completed")],
    )
    .await;
    seed_plan(
        &client,
        "todos-b",
        "todos",
        &items,
        NOW - 20,
        "completed",
        &[("a", "", "completed"), ("b", "", "completed")],
    )
    .await;

    let run = WorkflowInduction::new(logs.path().to_path_buf())
        .run(&client, NOW)
        .await
        .unwrap();

    assert_eq!(run.skipped_titles_only, 2);
    assert_eq!(run.skipped_corrupt, 0, "titles-only is a valid class");
    assert_eq!(run.drafts_written, 0);
    assert!(
        logs.path().join(DRAFTS_FILE).is_file(),
        "non-empty corpus overwrites the queue"
    );
}

#[tokio::test]
async fn workflow_induction_corrupt_spec_json_counted_and_skipped() {
    let logs = TempDir::new().unwrap();
    let db = TempDir::new().unwrap();
    let client = open_db(db.path()).await;
    let rows = dag_rows("analyst", "writer");
    let good = dag_spec("analyst", "writer", "p");

    let corrupt: [(&str, String); 5] = [
        ("bad-json", "{not json".to_string()),
        (
            "missing-tasks",
            serde_json::json!({"session_id": "s"}).to_string(),
        ),
        (
            "unknown-dep",
            spec(&[("t1", "researcher", &["ghost"], "p")]),
        ),
        (
            "duplicate-id",
            spec(&[("t1", "researcher", &[], "p"), ("t1", "analyst", &[], "p")]),
        ),
        (
            "cycle",
            spec(&[
                ("t1", "researcher", &["t2"], "p"),
                ("t2", "analyst", &["t1"], "p"),
            ]),
        ),
    ];
    for (i, (plan_id, spec_json)) in corrupt.iter().enumerate() {
        seed_plan(
            &client,
            plan_id,
            "corrupt",
            spec_json,
            NOW - 100 - i as i64,
            "completed",
            &[("t1", "researcher", "completed")],
        )
        .await;
    }
    seed_plan(
        &client,
        "good-a",
        "good",
        &good,
        NOW - 10,
        "completed",
        &rows,
    )
    .await;
    seed_plan(
        &client,
        "good-b",
        "good",
        &good,
        NOW - 20,
        "completed",
        &rows,
    )
    .await;

    let run = WorkflowInduction::new(logs.path().to_path_buf())
        .run(&client, NOW)
        .await
        .unwrap();

    assert_eq!(run.skipped_corrupt, corrupt.len(), "AC#8 fail-open count");
    assert_eq!(run.drafts_written, 1, "the run continues past corrupt rows");
    let drafts = read_drafts(logs.path());
    assert_eq!(drafts.len(), 1);
    assert_eq!(
        drafts[0].source_plan_ids,
        vec!["good-a".to_string(), "good-b".to_string()],
        "corrupt plans never enter a cluster"
    );
}

#[tokio::test]
async fn workflow_induction_failed_task_plan_excluded_from_corpus() {
    let logs = TempDir::new().unwrap();
    let db = TempDir::new().unwrap();
    let client = open_db(db.path()).await;
    let spec_json = dag_spec("analyst", "writer", "p");
    let rows = dag_rows("analyst", "writer");

    seed_plan(
        &client,
        "ok-a",
        "shape",
        &spec_json,
        NOW - 10,
        "completed",
        &rows,
    )
    .await;
    seed_plan(
        &client,
        "ok-b",
        "shape",
        &spec_json,
        NOW - 20,
        "completed",
        &rows,
    )
    .await;
    seed_plan(
        &client,
        "broken",
        "shape",
        &spec_json,
        NOW - 5,
        "completed",
        &[
            ("t1", "researcher", "completed"),
            ("t2", "analyst", "failed"),
            ("t3", "writer", "completed"),
            ("t4", "synthesizer", "completed"),
        ],
    )
    .await;

    let run = WorkflowInduction::new(logs.path().to_path_buf())
        .run(&client, NOW)
        .await
        .unwrap();

    assert_eq!(run.drafts_written, 1);
    let drafts = read_drafts(logs.path());
    assert_eq!(
        drafts[0].source_plan_ids,
        vec!["ok-a".to_string(), "ok-b".to_string()],
        "a plan with any failed task signals a broken shape, never a pattern"
    );
}

#[tokio::test]
async fn workflow_induction_identical_topology_pair_emits_one_draft() {
    let logs = TempDir::new().unwrap();
    let db = TempDir::new().unwrap();
    let client = open_db(db.path()).await;
    let spec_json = dag_spec("analyst", "writer", "Research then synthesize.");
    let rows = dag_rows("analyst", "writer");

    seed_plan(
        &client,
        "plan-a",
        "research-synthesis",
        &spec_json,
        NOW - 10,
        "completed",
        &rows,
    )
    .await;
    seed_plan(
        &client,
        "plan-b",
        "research-synthesis",
        &spec_json,
        NOW - 20,
        "completed",
        &rows,
    )
    .await;

    let run = WorkflowInduction::new(logs.path().to_path_buf())
        .run(&client, NOW)
        .await
        .unwrap();

    assert_eq!(run.drafts_written, 1);
    assert_eq!(run.insufficient, None);
    let drafts = read_drafts(logs.path());
    assert_eq!(drafts.len(), 1);
    let draft = &drafts[0];
    assert_eq!(
        draft.source_plan_ids,
        vec!["plan-a".to_string(), "plan-b".to_string()]
    );
    assert_eq!(draft.name, "research-synthesis");
    assert_eq!(draft.shape_signature, signature_of(&spec_json));
    assert_eq!(draft.tasks.len(), 4);
    assert_eq!(draft.tasks[0].role, "source");
    assert_eq!(draft.tasks[1].role, "intermediate");
    assert_eq!(draft.tasks[3].role, "sink");
    assert_eq!(
        draft.tasks[3].depends_on,
        vec!["t2".to_string(), "t3".to_string()]
    );
    assert_eq!(draft.success_stats.cluster_size, 2);
    assert_eq!(draft.success_stats.success_rate, 1.0);
    assert_eq!(draft.source_plan_dates.len(), 2);
    assert!(draft.induced_at.ends_with('Z'));
}

#[tokio::test]
async fn workflow_induction_threshold_boundary_072_clusters_071_does_not() {
    let logs = TempDir::new().unwrap();
    let db = TempDir::new().unwrap();
    let client = open_db(db.path()).await;

    // Same topology, one agent differs per variant. The seed/near pair sits
    // just ABOVE the threshold (author vs writer) and the seed/far pair just
    // BELOW it (planner vs analyst) — 0.7241 vs 0.7193 computed by
    // trigram_jaccard, so a constant drift in either direction fails here.
    let seed_spec = dag_spec("analyst", "writer", "p");
    let near_spec = dag_spec("analyst", "author", "p");
    let far_spec = dag_spec("planner", "writer", "p");

    seed_plan(
        &client,
        "far",
        "shape",
        &far_spec,
        NOW - 300,
        "completed",
        &dag_rows("planner", "writer"),
    )
    .await;
    seed_plan(
        &client,
        "near",
        "shape",
        &near_spec,
        NOW - 200,
        "completed",
        &dag_rows("analyst", "author"),
    )
    .await;
    seed_plan(
        &client,
        "seed",
        "shape",
        &seed_spec,
        NOW - 100,
        "completed",
        &dag_rows("analyst", "writer"),
    )
    .await;

    let seed_sig = signature_of(&seed_spec);
    let near_sig = signature_of(&near_spec);
    let far_sig = signature_of(&far_spec);
    let near_similarity = trigram_jaccard(&seed_sig, &near_sig);
    let far_similarity = trigram_jaccard(&seed_sig, &far_sig);
    assert_eq!(OBSERVATION_SIMILARITY, 0.72);
    assert_eq!(MIN_OBSERVATIONS, 2);
    assert!(
        near_similarity >= OBSERVATION_SIMILARITY,
        "near must straddle from above, got {near_similarity}"
    );
    assert!(
        far_similarity < OBSERVATION_SIMILARITY,
        "far must straddle from below, got {far_similarity}"
    );

    let run = WorkflowInduction::new(logs.path().to_path_buf())
        .run(&client, NOW)
        .await
        .unwrap();

    assert_eq!(run.drafts_written, 1, "only the ≥ threshold pair clusters");
    let drafts = read_drafts(logs.path());
    assert_eq!(drafts.len(), 1);
    assert_eq!(
        drafts[0].source_plan_ids,
        vec!["seed".to_string(), "near".to_string()],
        "greedy seed cluster = plans at/above the threshold"
    );
    assert!(!drafts[0].source_plan_ids.contains(&"far".to_string()));
}

#[tokio::test]
async fn workflow_induction_rejected_signature_never_reemerges() {
    let logs = TempDir::new().unwrap();
    let db = TempDir::new().unwrap();
    let client = open_db(db.path()).await;
    let spec_json = dag_spec("analyst", "writer", "p");
    let rows = dag_rows("analyst", "writer");
    seed_plan(
        &client,
        "a",
        "shape",
        &spec_json,
        NOW - 10,
        "completed",
        &rows,
    )
    .await;
    seed_plan(
        &client,
        "b",
        "shape",
        &spec_json,
        NOW - 20,
        "completed",
        &rows,
    )
    .await;

    let signature = signature_of(&spec_json);
    let rejected = serde_json::json!([{
        "shape_signature": signature,
        "rejected_at": "2026-11-18T00:00:00Z",
        "source_plan_ids": ["a", "b"],
    }]);
    std::fs::write(
        logs.path().join(REJECTED_FILE),
        serde_json::to_string_pretty(&rejected).unwrap(),
    )
    .unwrap();

    let run = WorkflowInduction::new(logs.path().to_path_buf())
        .run(&client, NOW)
        .await
        .unwrap();

    assert_eq!(run.drafts_written, 0);
    assert!(read_drafts(logs.path()).is_empty());
    assert_eq!(
        std::fs::read_to_string(logs.path().join(DRAFTS_FILE)).unwrap(),
        "[]",
        "the queue is overwritten with an empty array, never a stale draft"
    );

    let second = WorkflowInduction::new(logs.path().to_path_buf())
        .run(&client, NOW)
        .await
        .unwrap();
    assert_eq!(second.drafts_written, 0, "quarantine is permanent (AC#4)");
}

#[tokio::test]
async fn workflow_induction_deterministic_identical_corpus_identical_drafts() {
    let logs = TempDir::new().unwrap();
    let db = TempDir::new().unwrap();
    let client = open_db(db.path()).await;
    let spec_json = dag_spec("analyst", "writer", "Research then synthesize.");
    let rows = dag_rows("analyst", "writer");
    seed_plan(
        &client,
        "a",
        "shape",
        &spec_json,
        NOW - 10,
        "completed",
        &rows,
    )
    .await;
    seed_plan(
        &client,
        "b",
        "shape",
        &spec_json,
        NOW - 20,
        "completed",
        &rows,
    )
    .await;

    let induction = WorkflowInduction::new(logs.path().to_path_buf());
    induction.run(&client, NOW).await.unwrap();
    let first = std::fs::read_to_string(logs.path().join(DRAFTS_FILE)).unwrap();
    induction.run(&client, NOW).await.unwrap();
    let second = std::fs::read_to_string(logs.path().join(DRAFTS_FILE)).unwrap();

    assert_eq!(
        first, second,
        "identical corpus ⇒ byte-identical drafts (AC#6)"
    );
}

#[tokio::test]
async fn workflow_induction_zero_corpus_records_sidecar_only() {
    let root = TempDir::new().unwrap();
    let logs_dir = root.path().join("logs");
    let db = TempDir::new().unwrap();
    let client = open_db(db.path()).await;

    let run = WorkflowInduction::new(logs_dir.clone())
        .run(&client, NOW)
        .await
        .unwrap();

    assert_eq!(run.drafts_written, 0);
    assert_eq!(run.insufficient, Some(InsufficientReason::NoPlansInWindow));
    // W14.5 amended AC#2: the sidecar records "ran, nothing to do"; the
    // drafts queue stays untouched.
    assert!(
        !logs_dir.join(DRAFTS_FILE).exists(),
        "zero-corpus run never writes the drafts queue"
    );
    let summary = read_last_run(&logs_dir).expect("zero-corpus run records the sidecar");
    assert!(summary.insufficient);
    assert_eq!(
        summary.missing.as_deref(),
        Some(InsufficientReason::NoPlansInWindow.missing())
    );
}

#[tokio::test]
async fn workflow_induction_median_prompt_skeleton_truncates_at_char_boundary() {
    let logs = TempDir::new().unwrap();
    let db = TempDir::new().unwrap();
    let client = open_db(db.path()).await;
    let prompt: String = "研".repeat(250);
    let spec_json = spec(&[("t1", "writer", &[], &prompt)]);
    seed_plan(
        &client,
        "a",
        "cjk",
        &spec_json,
        NOW - 10,
        "completed",
        &[("t1", "writer", "completed")],
    )
    .await;
    seed_plan(
        &client,
        "b",
        "cjk",
        &spec_json,
        NOW - 20,
        "completed",
        &[("t1", "writer", "completed")],
    )
    .await;

    WorkflowInduction::new(logs.path().to_path_buf())
        .run(&client, NOW)
        .await
        .unwrap();

    let drafts = read_drafts(logs.path());
    assert_eq!(drafts.len(), 1);
    let skeleton = &drafts[0].tasks[0].typical_prompt_skeleton;
    let prefix: String = prompt.chars().take(200).collect();
    assert_eq!(skeleton.chars().count(), 203);
    assert!(skeleton.ends_with("..."));
    assert_eq!(skeleton, &format!("{prefix}..."));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workflow_induction_flock_race_induction_and_confirm_consistent() {
    let logs = TempDir::new().unwrap();
    let db = TempDir::new().unwrap();
    let client = open_db(db.path()).await;
    let spec_json = dag_spec("analyst", "writer", "p");
    let rows = dag_rows("analyst", "writer");
    seed_plan(
        &client,
        "a",
        "race-shape",
        &spec_json,
        NOW - 10,
        "completed",
        &rows,
    )
    .await;
    seed_plan(
        &client,
        "b",
        "race-shape",
        &spec_json,
        NOW - 20,
        "completed",
        &rows,
    )
    .await;

    // A draft already queued by a previous run; the concurrent "confirm"
    // removes it while induction rewrites the queue.
    let queued = serde_json::json!([{
        "name": "existing",
        "shape_signature": "existing:0,0",
        "source_plan_ids": ["old"],
        "source_plan_dates": ["2026-01-01"],
        "tasks": [],
        "success_stats": {"cluster_size": 1, "avg_duration_ms": 0, "avg_cost_usd": 0.0, "success_rate": 1.0},
        "induced_at": "2026-01-01T00:00:00Z",
    }]);
    std::fs::write(
        logs.path().join(DRAFTS_FILE),
        serde_json::to_string_pretty(&queued).unwrap(),
    )
    .unwrap();

    let induction_logs = logs.path().to_path_buf();
    let induction = tokio::spawn(async move {
        WorkflowInduction::new(induction_logs)
            .run(&client, NOW)
            .await
            .unwrap()
    });
    let confirm_logs = logs.path().to_path_buf();
    let confirm = tokio::spawn(async move {
        let _lock = acquire_induction_lock(&confirm_logs).unwrap();
        let kept: Vec<WorkflowDraft> = read_drafts(&confirm_logs)
            .into_iter()
            .filter(|draft| draft.name != "existing")
            .collect();
        let json = serde_json::to_string_pretty(&kept).unwrap();
        zen_core::atomic_file::write_atomic(&confirm_logs.join(DRAFTS_FILE), json.as_bytes())
            .unwrap();
    });

    let (run, confirmed) = tokio::join!(induction, confirm);
    confirmed.unwrap();
    let run = run.unwrap();

    assert_eq!(run.drafts_written, 1);
    let drafts = read_drafts(logs.path());
    assert_eq!(
        drafts.len(),
        1,
        "the flock serializes read-merge-write — no update is lost (AC#9)"
    );
    assert_eq!(drafts[0].name, "race-shape");
    assert_eq!(
        drafts[0].source_plan_ids,
        vec!["a".to_string(), "b".to_string()]
    );
}

#[test]
fn workflow_induction_window_constant_matches_daemon_gc() {
    assert_eq!(INDUCTION_WINDOW_DAYS, 90);
}

// W14.2 (eng-review #2): the tool contract makes `depends_on` optional
// (`plan_task.rs:457`) — a spec whose tasks omit the key entirely is the
// flat/parallel DAG shape, never `skipped_corrupt`.
#[tokio::test]
async fn workflow_induction_missing_depends_on_is_a_flat_dag() {
    let flat = r#"{"tasks":[
        {"id": "t1", "agent": "researcher", "prompt": "gather"},
        {"id": "t2", "agent": "writer", "prompt": "write"},
        {"id": "t3", "agent": "synthesizer", "prompt": "merge"}
    ]}"#;
    let shape = shape_signature_from_spec_json(flat).unwrap();
    let SpecShape::Dag { signature, tasks } = shape else {
        panic!("a depends_on-less spec is a DAG, not corrupt: {shape:?}");
    };
    assert!(tasks.iter().all(|t| t.in_degree == 0 && t.out_degree == 0));
    assert_eq!(signature, "researcher:0,0;synthesizer:0,0;writer:0,0");

    // A present-but-non-array key stays corrupt.
    let bad = r#"{"tasks":[{"id": "t1", "agent": "a", "prompt": "p", "depends_on": "t2"}]}"#;
    assert!(shape_signature_from_spec_json(bad).is_err());
}

// W14.3 (eng-review #4): a cancelled plan whose surviving rows are all
// settled must never contribute an observation — the status gate governs
// even when the task rows alone would pass `fully_completed`.
#[tokio::test]
async fn workflow_induction_cancelled_plan_excluded_even_with_settled_rows() {
    let logs = tempfile::tempdir().unwrap();
    let db = tempfile::tempdir().unwrap();
    let client = open_db(db.path()).await;
    let spec = r#"{"tasks":[
        {"id": "t1", "agent": "researcher", "prompt": "p", "depends_on": []},
        {"id": "t2", "agent": "writer", "prompt": "q", "depends_on": ["t1"]}
    ]}"#;
    let rows: Vec<(&str, &str, &str)> = vec![
        ("t1", "researcher", "completed"),
        ("t2", "writer", "completed"),
    ];
    seed_plan(
        &client,
        "cancelled-a",
        "shape",
        spec,
        NOW - 10,
        "cancelled",
        &rows,
    )
    .await;
    seed_plan(
        &client,
        "cancelled-b",
        "shape",
        spec,
        NOW - 20,
        "cancelled",
        &rows,
    )
    .await;

    let run = WorkflowInduction::new(logs.path().to_path_buf())
        .run(&client, NOW)
        .await
        .unwrap();
    assert_eq!(
        run.drafts_written, 0,
        "cancelled plans never induce a shape"
    );
    assert_eq!(
        run.insufficient,
        Some(InsufficientReason::NoFullyCompletedPlans)
    );
}

// W14.3 (eng-review #3): a fresh DAG mint whose checkpoint rows never
// covered the spec (crash-partial) is excluded even with a completed row
// set — the row-count-vs-spec gate closes the partial-observation hole.
#[tokio::test]
async fn workflow_induction_partial_fresh_plan_excluded() {
    let logs = tempfile::tempdir().unwrap();
    let db = tempfile::tempdir().unwrap();
    let client = open_db(db.path()).await;
    let spec = r#"{"tasks":[
        {"id": "t1", "agent": "researcher", "prompt": "p", "depends_on": []},
        {"id": "t2", "agent": "writer", "prompt": "q", "depends_on": ["t1"]},
        {"id": "t3", "agent": "synthesizer", "prompt": "m", "depends_on": ["t2"]}
    ]}"#;
    // Only 2 of the 3 spec tasks have settled rows — a crashed run.
    let rows: Vec<(&str, &str, &str)> = vec![
        ("t1", "researcher", "completed"),
        ("t2", "writer", "completed"),
    ];
    seed_plan(
        &client,
        "partial-a",
        "shape",
        spec,
        NOW - 10,
        "completed",
        &rows,
    )
    .await;
    seed_plan(
        &client,
        "partial-b",
        "shape",
        spec,
        NOW - 20,
        "completed",
        &rows,
    )
    .await;

    let run = WorkflowInduction::new(logs.path().to_path_buf())
        .run(&client, NOW)
        .await
        .unwrap();
    assert_eq!(
        run.drafts_written, 0,
        "partial observations never induce a shape"
    );
    assert_eq!(
        run.insufficient,
        Some(InsufficientReason::NoFullyCompletedPlans)
    );
}

// W14.5 (eng-review #11): a titles-only-dominant corpus reports its own
// honest reason — an operator must not wait for data that cannot arrive.
#[tokio::test]
async fn workflow_induction_titles_only_corpus_reports_its_own_reason() {
    let logs = tempfile::tempdir().unwrap();
    let db = tempfile::tempdir().unwrap();
    let client = open_db(db.path()).await;
    let items = r#"{"items":[{"id":"a","title":"A","status":"completed"}],"sensitivity":"Public"}"#;
    let rows: Vec<(&str, &str, &str)> = vec![("a", "", "completed")];
    seed_plan(
        &client,
        "todos-a",
        "todos",
        items,
        NOW - 10,
        "completed",
        &rows,
    )
    .await;
    seed_plan(
        &client,
        "todos-b",
        "todos",
        items,
        NOW - 20,
        "completed",
        &rows,
    )
    .await;

    let run = WorkflowInduction::new(logs.path().to_path_buf())
        .run(&client, NOW)
        .await
        .unwrap();
    assert_eq!(run.drafts_written, 0);
    assert_eq!(run.skipped_titles_only, 2);
    assert_eq!(run.insufficient, Some(InsufficientReason::TitlesOnlyCorpus));
}

// F12 (W15): `has_items_key` is a parsed-key check, so a DAG whose task id
// (or any other string value) is literally `items` is NOT exempted from the
// partial-row admission gate — the substring probe falsely admitted it.
#[tokio::test]
async fn workflow_induction_task_id_named_items_without_items_key_is_not_exempt() {
    let logs = tempfile::tempdir().unwrap();
    let db = tempfile::tempdir().unwrap();
    let client = open_db(db.path()).await;
    let spec_json = serde_json::json!({
        "tasks": [
            {"id": "items", "agent": "researcher", "prompt": "p", "depends_on": []},
            {"id": "t2", "agent": "writer", "prompt": "q", "depends_on": ["items"]}
        ]
    })
    .to_string();
    // Only 1 of the 2 spec tasks settled — a partial observation. With the
    // substring probe the `"items"` task id faked the mint exemption and the
    // plan was admitted; the parsed check excludes it.
    let rows: Vec<(&str, &str, &str)> = vec![("items", "researcher", "completed")];
    seed_plan(
        &client,
        "items-a",
        "shape",
        &spec_json,
        NOW - 10,
        "completed",
        &rows,
    )
    .await;
    seed_plan(
        &client,
        "items-b",
        "shape",
        &spec_json,
        NOW - 20,
        "completed",
        &rows,
    )
    .await;

    let run = WorkflowInduction::new(logs.path().to_path_buf())
        .run(&client, NOW)
        .await
        .unwrap();
    assert_eq!(
        run.drafts_written, 0,
        "a task id `items` must not exempt the partial-row gate"
    );
    assert_eq!(
        run.insufficient,
        Some(InsufficientReason::NoFullyCompletedPlans)
    );
}
