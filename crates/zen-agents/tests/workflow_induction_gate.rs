use std::path::Path;

use chrono::{DateTime, SecondsFormat, Utc};
use tempfile::TempDir;
use zen_agents::workflow_induction::{
    ConfirmOutcome, DRAFTS_FILE, DraftSuccessStats, DraftTaskTemplate, InductionError,
    RejectOutcome, RejectedSignature, WorkflowDraft, WorkflowInduction, confirm_workflow,
    read_drafts, read_rejected, reject_workflow, shape_sha8,
};
use zen_repo::{SqliteClient, TaskCheckpoint, WorkflowRepo};

const NOW: i64 = 1_800_000_000;
const NAME: &str = "research-synthesis";
const SIG: &str = "analyst:1,1;researcher:0,1;writer:1,0";

fn draft(name: &str, signature: &str) -> WorkflowDraft {
    WorkflowDraft {
        name: name.to_string(),
        shape_signature: signature.to_string(),
        source_plan_ids: vec!["plan-a".to_string(), "plan-b".to_string()],
        source_plan_dates: vec!["2026-11-10".to_string(), "2026-11-15".to_string()],
        tasks: vec![
            DraftTaskTemplate {
                id: "t1".to_string(),
                agent: "researcher".to_string(),
                role: "source".to_string(),
                typical_prompt_skeleton: "Research | extract".to_string(),
                depends_on: Vec::new(),
            },
            DraftTaskTemplate {
                id: "t2".to_string(),
                agent: "writer".to_string(),
                role: "sink".to_string(),
                typical_prompt_skeleton: "Write the brief".to_string(),
                depends_on: vec!["t1".to_string()],
            },
        ],
        success_stats: DraftSuccessStats {
            cluster_size: 2,
            avg_duration_ms: 45_000,
            avg_cost_usd: 0.12,
            success_rate: 1.0,
        },
        induced_at: "2026-11-18T10:30:00Z".to_string(),
    }
}

fn write_queue(logs: &Path, drafts: &[WorkflowDraft]) {
    std::fs::create_dir_all(logs).unwrap();
    std::fs::write(
        logs.join(DRAFTS_FILE),
        serde_json::to_string_pretty(drafts).unwrap(),
    )
    .unwrap();
}

fn template_dir(vault: &Path) -> std::path::PathBuf {
    vault.join("wiki/wisdom/workflow-templates")
}

fn rfc3339(epoch: i64) -> String {
    DateTime::<Utc>::from_timestamp(epoch, 0)
        .unwrap()
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

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

async fn open_db(dir: &Path) -> SqliteClient {
    SqliteClient::open(&dir.join("state.db")).await.unwrap()
}

async fn seed_plan(
    client: &SqliteClient,
    plan_id: &str,
    name: &str,
    spec_json: &str,
    created_at: i64,
) {
    let repo = WorkflowRepo::new(client);
    repo.create_plan(plan_id, Some(name), spec_json, created_at)
        .await
        .unwrap();
    for task_id in ["t1", "t2"] {
        repo.checkpoint_task(TaskCheckpoint {
            plan_id,
            task_id,
            agent: "writer",
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
async fn workflow_induction_gate_confirm_promotes_and_is_idempotent() {
    let logs = TempDir::new().unwrap();
    let vault = TempDir::new().unwrap();
    write_queue(logs.path(), &[draft(NAME, SIG)]);

    let first = confirm_workflow(logs.path(), vault.path(), NAME, NOW)
        .await
        .unwrap();
    let ConfirmOutcome::Promoted {
        template_path,
        suffixed,
    } = first
    else {
        panic!("expected a fresh promotion, got {first:?}");
    };
    assert!(!suffixed);
    assert_eq!(
        template_path,
        template_dir(vault.path()).join(format!("{NAME}.md"))
    );

    let body = std::fs::read_to_string(&template_path).unwrap();
    assert!(body.starts_with("---\ntype: workflow-template\n"));
    assert!(body.contains(&format!("name: \"{NAME}\"")));
    assert!(body.contains(&format!("shape_signature: \"{SIG}\"")));
    assert!(body.contains("induced_at: \"2026-11-18T10:30:00Z\""));
    assert!(body.contains("advisory_only: true"));
    assert!(body.contains("| t1 | researcher | source | — | Research \\| extract |"));
    assert!(body.contains("| t2 | writer | sink | t1 | Write the brief |"));
    assert!(body.contains("- plan-a (2026-11-10)"));
    assert!(body.contains("- plan-b (2026-11-15)"));
    assert!(
        read_drafts(logs.path()).is_empty(),
        "the queue entry is consumed"
    );

    let second = confirm_workflow(logs.path(), vault.path(), NAME, NOW + 60)
        .await
        .unwrap();
    assert_eq!(
        second,
        ConfirmOutcome::AlreadyPromoted {
            template_path: template_path.clone()
        }
    );
    assert_eq!(
        std::fs::read_to_string(&template_path).unwrap(),
        body,
        "confirmation is a no-op: the template is not rewritten (AC#3)"
    );

    write_queue(logs.path(), &[draft(NAME, SIG)]);
    let third = confirm_workflow(logs.path(), vault.path(), NAME, NOW + 120)
        .await
        .unwrap();
    assert!(
        matches!(third, ConfirmOutcome::AlreadyPromoted { .. }),
        "an identical re-queued draft reconciles without a duplicate file"
    );
    assert_eq!(std::fs::read_to_string(&template_path).unwrap(), body);
    assert!(read_drafts(logs.path()).is_empty());

    let templates: Vec<_> = std::fs::read_dir(template_dir(vault.path()))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(templates.len(), 1, "no collision file was produced");
}

#[tokio::test]
async fn workflow_induction_gate_confirm_collision_suffix_keeps_both() {
    let logs = TempDir::new().unwrap();
    let vault = TempDir::new().unwrap();
    let other_sig = "analyst:1,1;researcher:0,1;synthesizer:1,0";
    write_queue(logs.path(), &[draft(NAME, SIG), draft(NAME, other_sig)]);

    let first = confirm_workflow(logs.path(), vault.path(), NAME, NOW)
        .await
        .unwrap();
    assert!(matches!(
        first,
        ConfirmOutcome::Promoted {
            suffixed: false,
            ..
        }
    ));

    let second = confirm_workflow(logs.path(), vault.path(), NAME, NOW + 60)
        .await
        .unwrap();
    let ConfirmOutcome::Promoted {
        template_path,
        suffixed,
    } = second
    else {
        panic!("expected the collision-suffixed promotion, got {second:?}");
    };
    assert!(suffixed);
    assert_eq!(
        template_path,
        template_dir(vault.path()).join(format!("{NAME}-{}.md", shape_sha8(other_sig)))
    );

    let base = template_dir(vault.path()).join(format!("{NAME}.md"));
    assert!(
        base.is_file() && template_path.is_file(),
        "both templates exist"
    );
    assert!(
        std::fs::read_to_string(&base)
            .unwrap()
            .contains(&format!("\"{SIG}\""))
    );
    assert!(
        std::fs::read_to_string(&template_path)
            .unwrap()
            .contains(&format!("\"{other_sig}\""))
    );
    assert!(read_drafts(logs.path()).is_empty());
}

#[tokio::test]
async fn workflow_induction_gate_confirm_and_reject_resolve_by_exact_name() {
    let logs = TempDir::new().unwrap();
    let vault = TempDir::new().unwrap();
    write_queue(
        logs.path(),
        &[draft(NAME, SIG), draft("other-shape", "writer:0,0")],
    );

    confirm_workflow(logs.path(), vault.path(), NAME, NOW)
        .await
        .unwrap();
    let remaining = read_drafts(logs.path());
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].name, "other-shape");

    reject_workflow(logs.path(), "other-shape", NOW)
        .await
        .unwrap();
    assert!(read_drafts(logs.path()).is_empty());
}

#[tokio::test]
async fn workflow_induction_gate_reject_quarantines_shape_end_to_end() {
    let logs = TempDir::new().unwrap();
    let db = TempDir::new().unwrap();
    let client = open_db(db.path()).await;
    let spec_json = spec(&[
        ("t1", "researcher", &[], "Research."),
        ("t2", "writer", &["t1"], "Write."),
    ]);
    seed_plan(&client, "a", NAME, &spec_json, NOW - 10).await;
    seed_plan(&client, "b", NAME, &spec_json, NOW - 20).await;

    WorkflowInduction::new(logs.path().to_path_buf())
        .run(&client, NOW)
        .await
        .unwrap();
    let queued = read_drafts(logs.path());
    assert_eq!(queued.len(), 1);
    let signature = queued[0].shape_signature.clone();

    let outcome = reject_workflow(logs.path(), NAME, NOW + 300).await.unwrap();
    assert_eq!(
        outcome,
        RejectOutcome::Rejected {
            shape_signature: signature.clone()
        }
    );
    assert!(
        read_drafts(logs.path()).is_empty(),
        "reject removes the draft"
    );
    let rejected = read_rejected(logs.path());
    assert_eq!(
        rejected,
        vec![RejectedSignature {
            shape_signature: signature,
            rejected_at: rfc3339(NOW + 300),
            source_plan_ids: vec!["a".to_string(), "b".to_string()],
        }],
        "the ledger carries the RFC3339 timestamp and evidence trail"
    );

    let rerun = WorkflowInduction::new(logs.path().to_path_buf())
        .run(&client, NOW)
        .await
        .unwrap();
    assert_eq!(rerun.drafts_written, 0, "quarantine is permanent (AC#4)");
    assert!(read_drafts(logs.path()).is_empty());
}

#[tokio::test]
async fn workflow_induction_gate_reject_is_idempotent() {
    let logs = TempDir::new().unwrap();
    write_queue(logs.path(), &[draft(NAME, SIG)]);

    let first = reject_workflow(logs.path(), NAME, NOW).await.unwrap();
    assert_eq!(
        first,
        RejectOutcome::Rejected {
            shape_signature: SIG.to_string()
        }
    );

    write_queue(logs.path(), &[draft(NAME, SIG)]);
    let second = reject_workflow(logs.path(), NAME, NOW + 60).await.unwrap();
    assert_eq!(
        second,
        RejectOutcome::AlreadyRejected {
            shape_signature: SIG.to_string()
        }
    );
    assert_eq!(
        read_rejected(logs.path()).len(),
        1,
        "the ledger row is never duplicated"
    );
    assert!(read_drafts(logs.path()).is_empty());
}

#[tokio::test]
async fn workflow_induction_gate_unknown_draft_errors_both_ops() {
    let logs = TempDir::new().unwrap();
    let vault = TempDir::new().unwrap();

    let confirm = confirm_workflow(logs.path(), vault.path(), "ghost", NOW)
        .await
        .unwrap_err();
    assert!(matches!(
        confirm,
        InductionError::UnknownDraft(name) if name == "ghost"
    ));

    let reject = reject_workflow(logs.path(), "ghost", NOW)
        .await
        .unwrap_err();
    assert!(matches!(
        reject,
        InductionError::UnknownDraft(name) if name == "ghost"
    ));
    assert!(
        !logs
            .path()
            .join("workflow-induction-rejected.json")
            .exists(),
        "an unknown reject never creates the ledger"
    );
}

#[tokio::test]
async fn workflow_induction_gate_reject_refuses_to_clobber_corrupt_ledger() {
    let logs = TempDir::new().unwrap();
    write_queue(logs.path(), &[draft(NAME, SIG)]);
    std::fs::write(
        logs.path().join("workflow-induction-rejected.json"),
        "{ not json",
    )
    .unwrap();

    let error = reject_workflow(logs.path(), NAME, NOW).await.unwrap_err();
    assert!(matches!(error, InductionError::Json(_)));
    assert_eq!(
        read_drafts(logs.path()).len(),
        1,
        "a failed ledger read leaves the queue untouched"
    );
    assert_eq!(
        std::fs::read_to_string(logs.path().join("workflow-induction-rejected.json")).unwrap(),
        "{ not json",
        "the corrupt ledger is never overwritten"
    );
}
