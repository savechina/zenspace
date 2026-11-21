//! T016 (007 US4/FR-009): the `admission.list` agent tool renders the SAME
//! store rows the CLI reads. Drives the real tool against a real `state.db`
//! (migration 016 applied by `SqliteClient::open`) and compares every field
//! to `AdmissionRepo::list_decisions` directly.

use rig_compose::tool::Tool;
use serde_json::json;
use zen_agents::admission_list::{AdmissionListTool, DEFAULT_LIMIT, MAX_LIMIT, MIN_LIMIT};
use zen_repo::{AdmissionRepo, DecisionRecord, DispositionFilter};

async fn insert(
    repo: &AdmissionRepo<'_>,
    id: &str,
    disposition: &str,
    sender: &str,
    content: &str,
    created_at: i64,
) {
    repo.record_decision(DecisionRecord {
        id,
        source: "qqbot",
        event_id: Some(id),
        content_hash: id,
        sender,
        thread_key: "thread",
        content,
        disposition,
        matched_rule: Some("allowlist"),
        scope: Some("chat"),
        accept_mode: "observe",
        strength: Some("explicit"),
        policy_version: "v1",
        reason: None,
        created_at,
    })
    .await
    .unwrap();
}

fn assert_row_parity(rendered: &serde_json::Value, row: &zen_repo::DecisionRow) {
    assert_eq!(rendered["id"], json!(row.id));
    assert_eq!(rendered["source"], json!(row.source));
    assert_eq!(rendered["disposition"], json!(row.disposition));
    assert_eq!(rendered["sender"], json!(row.sender));
    assert_eq!(rendered["snippet"], json!(row.content));
    assert_eq!(rendered["matched_rule"], json!(row.matched_rule));
    assert_eq!(rendered["scope"], json!(row.scope));
    assert_eq!(rendered["mode"], json!(row.accept_mode));
    assert_eq!(rendered["created_at"], json!(row.created_at));
    assert_eq!(rendered["reason"], json!(row.reason));
}

#[tokio::test]
async fn admission_list_output_matches_repo_rows() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.db");
    let client = zen_repo::SqliteClient::open(&db_path).await.unwrap();
    let repo = AdmissionRepo::new(&client);
    insert(
        &repo,
        "w1",
        "would_accept",
        "Ada",
        "deploy the staging build",
        300,
    )
    .await;
    insert(
        &repo,
        "w2",
        "would_accept",
        "Lin",
        "check the invoices",
        200,
    )
    .await;
    insert(
        &repo,
        "w3",
        "would_accept",
        "Kim",
        "summarize the thread",
        100,
    )
    .await;
    insert(&repo, "r1", "rejected", "Mallory", "drop tables", 90).await;

    let tool = AdmissionListTool::new(Some(db_path.clone()));
    let out = tool
        .invoke(json!({ "status": "would_accept", "limit": 10 }))
        .await
        .unwrap();

    let expected = repo
        .list_decisions(
            DispositionFilter {
                disposition: Some("would_accept".to_string()),
                since: None,
            },
            10,
        )
        .await
        .unwrap();
    let rendered = out["rows"].as_array().unwrap();
    assert_eq!(rendered.len(), expected.len());
    assert_eq!(out["count"], json!(expected.len()));
    assert_eq!(out["status"], "would_accept");
    assert_eq!(out["limit"], 10);
    for (rendered_row, repo_row) in rendered.iter().zip(expected.iter()) {
        assert_row_parity(rendered_row, repo_row);
    }

    let all = tool.invoke(json!({})).await.unwrap();
    let expected_all = repo
        .list_decisions(DispositionFilter::default(), DEFAULT_LIMIT)
        .await
        .unwrap();
    assert_eq!(all["count"], json!(expected_all.len()));
    assert_eq!(all["limit"], DEFAULT_LIMIT);
    assert_eq!(all["rows"].as_array().unwrap().len(), 4);
}

#[tokio::test]
async fn admission_list_clamps_limit_and_rejects_unknown_status() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.db");
    let client = zen_repo::SqliteClient::open(&db_path).await.unwrap();
    let repo = AdmissionRepo::new(&client);
    for i in 0..3 {
        insert(&repo, &format!("d{i}"), "would_accept", "Ada", "deploy", i).await;
    }
    let tool = AdmissionListTool::new(Some(db_path));

    let low = tool.invoke(json!({ "limit": 0 })).await.unwrap();
    assert_eq!(low["limit"], MIN_LIMIT);
    let high = tool.invoke(json!({ "limit": 9_999 })).await.unwrap();
    assert_eq!(high["limit"], MAX_LIMIT);

    let bad = tool.invoke(json!({ "status": "granted" })).await.unwrap();
    assert!(
        bad["error"]
            .as_str()
            .unwrap()
            .contains("outside the vocabulary"),
        "{bad}"
    );
    assert_eq!(bad["admission.list"], true);

    let bad_limit = tool.invoke(json!({ "limit": "ten" })).await.unwrap();
    assert!(
        bad_limit["error"].as_str().unwrap().contains("integer"),
        "{bad_limit}"
    );
}

#[tokio::test]
async fn admission_list_missing_ledger_is_honest_empty_not_created() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.db");
    let tool = AdmissionListTool::new(Some(db_path.clone()));
    let out = tool.invoke(json!({})).await.unwrap();
    assert_eq!(out["count"], 0);
    assert_eq!(out["rows"].as_array().unwrap().len(), 0);
    assert!(
        out["note"]
            .as_str()
            .unwrap()
            .contains("state.db not present"),
        "{out}"
    );
    assert!(
        !db_path.exists(),
        "a read-only query must never create the daemon's database"
    );
}
