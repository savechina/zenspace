//! W3.2 offline half: competence posterior sufficient statistics are a pure
//! function of the labeled trajectories — no thresholds, no activation.

use zen_vault::distill::decision_audit::{DecisionRecord, competence_statistics};

fn record(kind: &str, rung: &str, decision: Option<&str>, label: Option<&str>) -> DecisionRecord {
    DecisionRecord {
        id: format!("{kind}-{rung}-{decision:?}-{label:?}"),
        timestamp: None,
        kind: kind.to_string(),
        rung: rung.to_string(),
        confidence: Some(0.5),
        decision: decision.map(str::to_string),
        label: label.map(str::to_string),
        input_excerpt: None,
    }
}

#[test]
fn empty_cell_yields_unlabeled_uniform_prior() {
    let stats = competence_statistics(&[], "intent", "L1");
    assert_eq!(stats.labeled, 0);
    assert_eq!(stats.successes, 0);
    assert_eq!(stats.failures, 0);
    assert_eq!(stats.alpha, 1.0);
    assert_eq!(stats.beta, 1.0);
    assert_eq!(stats.mean, 0.5);
}

#[test]
fn labeled_cell_counts_successes_and_failures() {
    let records = vec![
        record("intent", "L1", Some("Query"), Some("Query")),
        record("intent", "L1", Some("Query"), Some("Query")),
        record("intent", "L1", Some("Action"), Some("Query")),
    ];
    let stats = competence_statistics(&records, "intent", "L1");
    assert_eq!(stats.labeled, 3);
    assert_eq!(stats.successes, 2);
    assert_eq!(stats.failures, 1);
    assert_eq!(stats.alpha, 3.0);
    assert_eq!(stats.beta, 2.0);
    assert_eq!(stats.mean, 0.6);
}

#[test]
fn unlabeled_records_are_excluded() {
    let records = vec![
        record("intent", "L1", Some("Query"), Some("Query")),
        record("intent", "L1", Some("Query"), None),
        record("intent", "L1", None, Some("Query")),
    ];
    let stats = competence_statistics(&records, "intent", "L1");
    assert_eq!(stats.labeled, 1);
    assert_eq!(stats.successes, 1);
    assert_eq!(stats.failures, 0);
}

#[test]
fn mismatched_kind_or_rung_is_ignored() {
    let records = vec![
        record("intent", "L1", Some("Query"), Some("Query")),
        record("review", "L1", Some("approved"), Some("approved")),
        record("intent", "L2", Some("Query"), Some("Action")),
    ];
    let stats = competence_statistics(&records, "intent", "L1");
    assert_eq!(stats.kind, "intent");
    assert_eq!(stats.rung, "L1");
    assert_eq!(stats.labeled, 1);
    assert_eq!(stats.successes, 1);
    assert_eq!(stats.failures, 0);
}

#[test]
fn statistics_are_deterministic() {
    let records = vec![
        record("intent", "L1", Some("Query"), Some("Query")),
        record("intent", "L1", Some("Action"), Some("Query")),
    ];
    let first = competence_statistics(&records, "intent", "L1");
    let second = competence_statistics(&records, "intent", "L1");
    assert_eq!(first, second);
}
