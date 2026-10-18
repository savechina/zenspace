//! W3 calibration-queue shadow observers (spec D13): audit-line shape, the
//! shadow-never-mutates pins (T168 pattern), and proof the observers ignore
//! `thresholds.json` while gating nothing.

use zen_agents::decision::{
    LadderDecision, LadderTrace, QualitySignal, SHADOW_RUNG, estimate_quality_escalation,
    load_intent_l1_competence, run_bayesian_escalation_shadow, run_minority_sentinel_shadow,
    run_qe_cascade_shadow,
};
use zen_agents::intent::{Acl, Intent, IntentCategory, IntentSource, LlmOutcome, LlmTelemetry};
use zen_core::paths::ZenPaths;
use zen_vault::distill::decision_audit::{DecisionRecord, competence_statistics};

fn test_paths() -> (tempfile::TempDir, ZenPaths) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let paths = ZenPaths::for_testing(tmp.path().to_path_buf());
    (tmp, paths)
}

fn audit_entries(paths: &ZenPaths) -> Vec<serde_json::Value> {
    let path = paths.logs().join("audit.jsonl");
    match std::fs::read_to_string(path) {
        Ok(content) => content
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn record(
    id: &str,
    kind: &str,
    rung: &str,
    decision: Option<&str>,
    label: Option<&str>,
) -> DecisionRecord {
    DecisionRecord {
        id: id.to_string(),
        timestamp: None,
        kind: kind.to_string(),
        rung: rung.to_string(),
        confidence: Some(0.5),
        decision: decision.map(str::to_string),
        label: label.map(str::to_string),
        input_excerpt: None,
    }
}

fn production_ladder_decision() -> LadderDecision {
    LadderDecision {
        intent: Intent {
            category: IntentCategory::Query,
            agent: "Explore".to_string(),
            signal: "search".to_string(),
            acl: Acl::ReadOnly,
            confidence: 0.9,
            source: IntentSource::Llm,
        },
        telemetry: LlmTelemetry {
            outcome: LlmOutcome::L1Resolved,
            elapsed_ms: 0,
        },
        trace: Some(LadderTrace {
            rung: "L1",
            gate: Some(0.7),
            confidence: 0.9,
            latency_ms: 12,
        }),
    }
}

#[test]
fn qe_cascade_shadow_emits_heuristic_line() {
    let (_tmp, paths) = test_paths();
    let signal = QualitySignal {
        has_error: true,
        response_chars: 500,
    };
    run_qe_cascade_shadow(&paths, "s-qe", &signal, Some(true));

    let entries = audit_entries(&paths);
    assert_eq!(entries.len(), 1, "one observation line");
    let entry = &entries[0];
    assert_eq!(entry["kind"], "loop.decision");
    assert_eq!(entry["decision"], "shadow");
    assert_eq!(entry["decision_kind"], "qe_cascade");
    assert_eq!(entry["rung"], SHADOW_RUNG);
    assert_eq!(entry["session_id"], "s-qe");
    assert_eq!(entry["calibrated"], false);
    assert_eq!(entry["heuristic"], true);
    assert_eq!(entry["has_error"], true);
    assert_eq!(entry["response_chars"], 500);
    assert_eq!(entry["predicted_escalate"], true);
    assert_eq!(entry["actual_escalated"], true);
    assert_eq!(entry["agrees_with_actual"], true);
    assert!(entry["score"].as_f64().unwrap() >= 0.5);
}

#[test]
fn qe_heuristic_bands_are_deterministic() {
    let bands = [
        (true, 10_000, 0.9_f32),
        (false, 0, 0.85),
        (false, 39, 0.7),
        (false, 199, 0.4),
        (false, 200, 0.15),
    ];
    for (has_error, response_chars, expected) in bands {
        let score = estimate_quality_escalation(&QualitySignal {
            has_error,
            response_chars,
        });
        assert_eq!(
            score, expected,
            "heuristic band ({has_error}, {response_chars})"
        );
    }
}

#[test]
fn bayesian_escalation_shadow_emits_comparison() {
    let (_tmp, paths) = test_paths();
    let records = vec![
        record("a", "intent", "L1", Some("Query"), Some("Query")),
        record("b", "intent", "L1", Some("Action"), Some("Query")),
        record("c", "intent", "L1", Some("Query"), None),
    ];
    let stats = competence_statistics(&records, "intent", "L1");
    assert_eq!(stats.labeled, 2);

    run_bayesian_escalation_shadow(&paths, "s-bayes", &stats, "L1", false);

    let entries = audit_entries(&paths);
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry["kind"], "loop.decision");
    assert_eq!(entry["decision_kind"], "bayesian_escalation");
    assert_eq!(entry["rung"], SHADOW_RUNG);
    assert_eq!(entry["calibrated"], false);
    assert_eq!(entry["heuristic"], true);
    assert_eq!(entry["labeled"], 2);
    assert_eq!(entry["successes"], 1);
    assert_eq!(entry["failures"], 1);
    assert_eq!(entry["alpha"], 2.0);
    assert_eq!(entry["beta"], 2.0);
    assert_eq!(entry["production_rung"], "L1");
    assert_eq!(entry["production_escalated"], false);
    assert_eq!(entry["predicted_escalate"], false);
    assert_eq!(entry["agrees_with_actual"], true);
}

#[test]
fn bayesian_predicts_escalation_when_competence_is_low() {
    let (_tmp, paths) = test_paths();
    let records = vec![
        record("a", "intent", "L1", Some("Action"), Some("Query")),
        record("b", "intent", "L1", Some("System"), Some("Query")),
    ];
    let stats = competence_statistics(&records, "intent", "L1");
    assert_eq!(stats.successes, 0);
    assert_eq!(stats.failures, 2);
    assert_eq!(stats.alpha, 1.0);
    assert_eq!(stats.beta, 3.0);
    assert_eq!(stats.mean, 0.25);

    run_bayesian_escalation_shadow(&paths, "s-bayes-low", &stats, "L2", false);
    let entries = audit_entries(&paths);
    assert_eq!(entries[0]["predicted_escalate"], true);
    assert_eq!(entries[0]["agrees_with_actual"], false);
}

#[test]
fn load_intent_l1_competence_reads_dataset_and_fails_open() {
    let (_tmp, paths) = test_paths();
    let empty = load_intent_l1_competence(&paths);
    assert_eq!(empty.labeled, 0);
    assert_eq!(empty.mean, 0.5);

    let dir = paths.logs().join("decision-audit");
    std::fs::create_dir_all(&dir).unwrap();
    let records = [
        record("a", "intent", "L1", Some("Query"), Some("Query")),
        record("b", "intent", "L1", Some("Action"), Some("Query")),
    ];
    let mut body = String::new();
    for record in &records {
        body.push_str(&serde_json::to_string(record).unwrap());
        body.push('\n');
    }
    std::fs::write(dir.join("dataset.jsonl"), body).unwrap();

    let loaded = load_intent_l1_competence(&paths);
    assert_eq!(loaded.labeled, 2);
    assert_eq!(loaded.successes, 1);
    assert_eq!(loaded.failures, 1);
}

#[test]
fn minority_sentinel_records_dormant_once() {
    let (_tmp, paths) = test_paths();
    run_minority_sentinel_shadow(&paths, "s-sentinel", 0);
    run_minority_sentinel_shadow(&paths, "s-sentinel", 0);

    let entries = audit_entries(&paths);
    assert_eq!(entries.len(), 1, "dormant observation is one-time");
    let entry = &entries[0];
    assert_eq!(entry["decision_kind"], "minority_sentinel");
    assert_eq!(entry["rung"], SHADOW_RUNG);
    assert_eq!(entry["dormant"], true);
    assert_eq!(entry["reason"], "no_quorum");
    assert_eq!(entry["quorum"], 0);
}

#[test]
fn observers_ignore_thresholds_json_and_gate_nothing() {
    let (_tmp, paths) = test_paths();
    let dir = paths.logs().join("decision-audit");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("thresholds.json"),
        r#"{"intent_l1":0.95,"review_escalate":0.1,"correction_l1":0.95,"citation_l1":0.95}"#,
    )
    .unwrap();

    run_qe_cascade_shadow(
        &paths,
        "s",
        &QualitySignal {
            has_error: false,
            response_chars: 5000,
        },
        None,
    );
    let stats = competence_statistics(&[], "intent", "L1");
    run_bayesian_escalation_shadow(&paths, "s", &stats, "L2", true);

    let entries = audit_entries(&paths);
    assert_eq!(
        entries.len(),
        2,
        "observers fire regardless of thresholds.json"
    );
    for entry in &entries {
        assert_eq!(entry["rung"], SHADOW_RUNG);
        assert_eq!(entry["calibrated"], false);
    }
}

#[test]
fn shadow_observation_does_not_mutate_production_decision() {
    let (_tmp, paths) = test_paths();
    let production = production_ladder_decision();
    let before_intent = production.intent.clone();
    let before_trace = production.trace.clone().unwrap();

    run_qe_cascade_shadow(
        &paths,
        "s",
        &QualitySignal {
            has_error: false,
            response_chars: 5000,
        },
        Some(false),
    );
    let stats = competence_statistics(&[], "intent", "L1");
    run_bayesian_escalation_shadow(&paths, "s", &stats, "L1", false);
    run_minority_sentinel_shadow(&paths, "s", 3);

    assert_eq!(production.intent.category, before_intent.category);
    assert_eq!(production.intent.agent, before_intent.agent);
    assert_eq!(production.intent.signal, before_intent.signal);
    assert_eq!(production.intent.acl, before_intent.acl);
    assert_eq!(production.intent.confidence, before_intent.confidence);
    assert_eq!(production.intent.source, before_intent.source);
    let after_trace = production.trace.clone().unwrap();
    assert_eq!(after_trace.rung, before_trace.rung);
    assert_eq!(after_trace.confidence, before_trace.confidence);
    assert_eq!(after_trace.latency_ms, before_trace.latency_ms);
    assert_eq!(after_trace.gate, before_trace.gate);
}
