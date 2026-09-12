use std::fs;
use std::path::Path;

use zen_vault::distill::aggregate_orchestration;

fn write_audit(dir: &Path, lines: &[&str]) {
    let path = dir.join("audit.jsonl");
    let mut content = String::new();
    for line in lines {
        content.push_str(line);
        content.push('\n');
    }
    fs::write(&path, content).unwrap();
}

#[test]
fn orchestration_stats_full_sections() {
    let dir = tempfile::tempdir().unwrap();
    write_audit(
        dir.path(),
        &[
            r#"{"kind":"loop.turn.review","session_id":"s1","agent":"Sisyphus","intent_signal":"explore","intent_category":"Query","intent_source":"Llm","intent_confidence":0.9,"intent_acl":"private","intent_llm_outcome":"ok","intent_llm_ms":120,"plan_approved":true,"delivery_ready":true,"feedback_rounds":0,"failed_attempts":0}"#,
            r#"{"kind":"loop.turn.review","session_id":"s2","agent":"Sisyphus","intent_signal":"act","intent_category":"Action","intent_source":"Keyword","intent_confidence":0.8,"intent_acl":"private","intent_llm_outcome":"ok","intent_llm_ms":90,"plan_approved":false,"delivery_ready":false,"feedback_rounds":2,"failed_attempts":1}"#,
            r#"{"kind":"loop.turn.review","session_id":"s3","agent":"Sisyphus","intent_signal":"chat","intent_category":"Conversation","intent_source":"Llm","intent_confidence":0.95,"intent_acl":"public","intent_llm_outcome":"timeout","intent_llm_ms":2000,"plan_approved":true,"delivery_ready":true,"feedback_rounds":0,"failed_attempts":0}"#,
            r#"{"kind":"gateway.turn.started","ts":"2026-09-12T10:00:00Z","turnId":"t1","sessionId":"s1","agent":"Sisyphus"}"#,
            r#"{"kind":"gateway.turn.completed","ts":"2026-09-12T10:00:05Z","turnId":"t1","sessionId":"s1","outcome":"completed"}"#,
            r#"{"kind":"gateway.turn.started","ts":"2026-09-12T10:01:00Z","turnId":"t2","sessionId":"s2","agent":"Sisyphus"}"#,
            r#"{"kind":"gateway.turn.completed","ts":"2026-09-12T10:01:10Z","turnId":"t2","sessionId":"s2","outcome":"cancelled"}"#,
            r#"{"kind":"loop.delegate.gates","parent":"Sisyphus","depth":1,"batch_width":2,"gates":[{"agent":"Hephaestus","independent":true,"consumer_decision":true,"bounded":true,"worth_it":true},{"agent":"Explore","independent":false,"consumer_decision":true,"bounded":true,"worth_it":true}]}"#,
            r#"{"kind":"loop.delegate.gates","parent":"Sisyphus","depth":1,"batch_width":1,"gates":[{"agent":"Hephaestus","independent":true,"consumer_decision":true,"bounded":false,"worth_it":false}]}"#,
            r#"{"kind":"loop.plan.completed","plan":"research","plan_id":"p1","tasks_total":3,"tasks_ok":3,"tasks_failed":0,"tasks_skipped":0,"plan_approved":true,"delivery_ready":true,"duration_ms":6000}"#,
            r#"{"kind":"loop.plan.completed","plan":"refactor","plan_id":"p2","tasks_total":2,"tasks_ok":1,"tasks_failed":1,"tasks_skipped":0,"plan_approved":true,"delivery_ready":false,"duration_ms":4000}"#,
        ],
    );

    let stats = aggregate_orchestration(dir.path()).unwrap();

    assert_eq!(stats.turn_review.turns, 3);
    assert_eq!(stats.turn_review.delivery_not_ready, 1);
    assert_eq!(stats.turn_review.plan_vetoed, 1);
    assert_eq!(stats.turn_review.feedback_rounds_total, 2);
    assert_eq!(stats.turn_review.intent_llm_outcomes.len(), 2);

    assert_eq!(stats.gateway.turns_started, 2);
    assert_eq!(stats.gateway.turns_completed, 2);

    assert_eq!(stats.delegate_gates.events, 2);
    assert_eq!(stats.delegate_gates.batched, 1);
    assert_eq!(stats.delegate_gates.blocked_independent, 1);
    assert_eq!(stats.delegate_gates.blocked_unbounded, 1);
    assert_eq!(stats.delegate_gates.blocked_not_worth, 1);
    assert_eq!(stats.delegate_gates.target_distribution.len(), 2);

    assert_eq!(stats.plan_completed.plans, 2);
    assert_eq!(stats.plan_completed.tasks_ok, 4);
    assert_eq!(stats.plan_completed.tasks_failed, 1);
    assert_eq!(stats.plan_completed.avg_duration_ms, 5000);
    assert_eq!(stats.plan_completed.delivery_not_ready, 1);

    let display = stats.to_string();
    assert!(display.contains("[turn.review]"));
    assert!(display.contains("[gateway]"));
    assert!(display.contains("[delegate.gates]"));
    assert!(display.contains("[plan.completed]"));
    assert!(display.contains("Query"));
    assert!(display.contains("Hephaestus"));
}

#[test]
fn orchestration_stats_empty_audit_file() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("audit.jsonl"), "").unwrap();
    let stats = aggregate_orchestration(dir.path()).unwrap();
    assert_eq!(stats.turn_review.turns, 0);
    assert_eq!(stats.gateway.turns_started, 0);
    assert_eq!(stats.delegate_gates.events, 0);
    assert_eq!(stats.plan_completed.plans, 0);
}

#[test]
fn orchestration_stats_serialization_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    write_audit(
        dir.path(),
        &[
            r#"{"kind":"loop.turn.review","intent_category":"Query","intent_source":"Llm","delivery_ready":true,"plan_approved":true,"feedback_rounds":0,"intent_llm_outcome":"ok","intent_llm_ms":100}"#,
            r#"{"kind":"gateway.turn.started","turnId":"t1","sessionId":"s1"}"#,
        ],
    );
    let stats = aggregate_orchestration(dir.path()).unwrap();
    let json = serde_json::to_string(&stats).unwrap();
    let deserialized: zen_vault::distill::OrchestrationStats = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.turn_review.turns, 1);
    assert_eq!(deserialized.gateway.turns_started, 1);
}
