//! D10: heterogeneous-accuracy aggregation weights (frontier adoption wave,
//! 2026-10-14).
//!
//! # Functionality
//! Fan-out batches run several sub-agents (models) whose reliability differs.
//! The "Beyond Majority Voting" result (arXiv:2510.01499) is that a
//! heterogeneous-accuracy aggregation strictly beats the best single model;
//! the corollary is that a merge/synthesis should let the more reliable
//! models lead. This module derives a per-agent success rate from the
//! `loop.delegate.gates` audit traces the delegation tool already writes and
//! exposes it as an **ordering** signal.
//!
//! # User impact
//! Once a caller injects the weights ([`DelegateTaskTool::with_weights`]),
//! a multi-result batch is returned with higher-accuracy agents first. No
//! result is ever dropped or rewritten — only order changes — and a batch of
//! exactly one result is never touched.
//!
//! # Default behavior
//! **Cold start (no audit records) yields an empty weight map, which leaves
//! ordering byte-identical to today.** The production orchestrator does not
//! inject weights yet (out of this change's file scope), so the default
//! install is unchanged; activation is a one-line caller opt-in.
//!
//! # Interaction
//! Weights are read-side only: this module never mutates the audit log and
//! never feeds routing. Debate rounds stay refuted (spec C7) — aggregation is
//! vote/merge by ordering, with zero interaction rounds.
//!
//! [`DelegateTaskTool::with_weights`]: crate::delegate_task::DelegateTaskTool::with_weights

use std::collections::HashMap;
use std::path::Path;

/// Evidence window: the last `WEIGHT_WINDOW` gate observations feed the
/// success rates (D10 `N = 100`). Deliberately a constant, not a config key:
/// T192 forbids a dormant/never-read config surface, and the value is the
/// recorded design constant.
pub const WEIGHT_WINDOW: usize = 100;

/// Laplace smoothing prior (`weight = (successes + α) / (trials + 2α)`).
/// `α = 1` is the standard add-one rule and keeps an agent with zero visible
/// trials at the 0.5 neutral point rather than 0 or 1.
const LAPLACE_PRIOR: f32 = 1.0;

/// Neutral weight for an agent absent from the map (Laplace prior at zero
/// trials). Using the same constant keeps "unknown" and "no evidence"
/// consistent.
const NEUTRAL_WEIGHT: f32 = 0.5;

/// One per-task observation extracted from a `loop.delegate.gates` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateObservation {
    /// Delegated agent (the "model" for weighting purposes).
    pub agent: String,
    /// Whether this observation counts as a success.
    pub ok: bool,
}

/// Parse `loop.delegate.{outcome,gates}` lines from an audit JSONL body into
/// per-task observations, in file order. Unknown lines and malformed JSON are
/// skipped (fail-open — a corrupt audit line must never abort weight
/// computation).
///
/// # Success derivation
/// `loop.delegate.outcome` (post-exec, written at each task's completion)
/// carries the real verdict: ok == its `error` field is absent. The
/// pre-spawn `loop.delegate.gates` line has no post-execution knowledge and
/// is used ONLY as the fallback for batches predating the outcome line: its
/// `bounded` admit/deny stands in for ok (documented deviation). Both kinds
/// merge in file order into one observation stream.
pub fn parse_gate_observations(audit_jsonl: &str) -> Vec<GateObservation> {
    let mut merged: Vec<(usize, GateObservation)> = Vec::new();
    for (line_no, line) in audit_jsonl.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match value.get("kind").and_then(|k| k.as_str()) {
            Some("loop.delegate.outcome") => {
                let Some(agent) = value.get("agent").and_then(|a| a.as_str()) else {
                    continue;
                };
                let ok = value.get("ok").and_then(|b| b.as_bool()).unwrap_or(false);
                merged.push((
                    line_no,
                    GateObservation {
                        agent: agent.to_string(),
                        ok,
                    },
                ));
            }
            Some("loop.delegate.gates") => {
                let Some(gates) = value.get("gates").and_then(|g| g.as_array()) else {
                    continue;
                };
                for gate in gates {
                    let Some(agent) = gate.get("agent").and_then(|a| a.as_str()) else {
                        continue;
                    };
                    let ok = gate
                        .get("bounded")
                        .and_then(|b| b.as_bool())
                        .unwrap_or(false);
                    merged.push((
                        line_no,
                        GateObservation {
                            agent: agent.to_string(),
                            ok,
                        },
                    ));
                }
            }
            _ => continue,
        }
    }
    merged.sort_by_key(|(line_no, _)| *line_no);
    merged.into_iter().map(|(_, obs)| obs).collect()
}

/// Laplace-smoothed success rate per agent over the last [`WEIGHT_WINDOW`]
/// observations (a single global window in file order). Agents with no
/// observations are simply absent from the map.
pub fn success_rates(observations: &[GateObservation]) -> HashMap<String, f32> {
    let window = if observations.len() > WEIGHT_WINDOW {
        &observations[observations.len() - WEIGHT_WINDOW..]
    } else {
        observations
    };
    let mut totals: HashMap<&str, (u32, u32)> = HashMap::new();
    for observation in window {
        let entry = totals.entry(observation.agent.as_str()).or_insert((0, 0));
        entry.1 += 1;
        if observation.ok {
            entry.0 += 1;
        }
    }
    totals
        .into_iter()
        .map(|(agent, (successes, trials))| {
            let rate = (successes as f32 + LAPLACE_PRIOR) / (trials as f32 + 2.0 * LAPLACE_PRIOR);
            (agent.to_string(), rate)
        })
        .collect()
}

/// Read `loop.delegate.gates` observations from `<logs_dir>/audit.jsonl`.
/// A missing/unreadable log yields an empty vector (cold start).
pub fn load_observations(logs_dir: &Path) -> Vec<GateObservation> {
    match std::fs::read_to_string(logs_dir.join("audit.jsonl")) {
        Ok(body) => parse_gate_observations(&body),
        Err(_) => Vec::new(),
    }
}

/// Load the per-agent weight map from `<logs_dir>/audit.jsonl`. Empty when
/// the log is absent or holds no gate lines.
pub fn load_weights(logs_dir: &Path) -> HashMap<String, f32> {
    success_rates(&load_observations(logs_dir))
}

/// Reorder a multi-result batch by descending agent weight.
///
/// Contract (D10):
/// - **Ordering only** — entries are permuted, never dropped or rewritten.
/// - **Stable** — equal weights and unknown agents retain their input order,
///   so a uniform map (or a cold-start empty map) leaves the batch untouched.
/// - Callers must not apply this to a single-result batch; the delegation tool
///   never does.
pub fn order_values_by_weight(results: &mut [serde_json::Value], weights: &HashMap<String, f32>) {
    if weights.is_empty() || results.len() < 2 {
        return;
    }
    // `sort_by` is stable, so ties keep input order (deterministic).
    results.sort_by(|a, b| {
        let wa = weight_of(a, weights);
        let wb = weight_of(b, weights);
        wb.partial_cmp(&wa).unwrap_or(std::cmp::Ordering::Equal)
    });
}

fn weight_of(value: &serde_json::Value, weights: &HashMap<String, f32>) -> f32 {
    value
        .get("agent")
        .and_then(|a| a.as_str())
        .and_then(|agent| weights.get(agent).copied())
        .unwrap_or(NEUTRAL_WEIGHT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate_line(entries: &[(&str, bool)]) -> String {
        let gates: Vec<serde_json::Value> = entries
            .iter()
            .map(|(agent, bounded)| {
                serde_json::json!({
                    "agent": agent,
                    "prompt_chars": 10,
                    "independent": true,
                    "consumer_decision": true,
                    "bounded": bounded,
                    "worth_it": true,
                    "inherited_notes": 0,
                    "inherited_chars": 0,
                    "skill_hits": 0,
                })
            })
            .collect();
        serde_json::json!({
            "kind": "loop.delegate.gates",
            "parent": "Sisyphus",
            "depth": 1,
            "batch_width": entries.len(),
            "gates": gates,
        })
        .to_string()
    }

    #[test]
    fn parses_gate_entries_and_ignores_other_kinds_and_garbage() {
        let body = format!(
            "{}\n{}\nnot json\n{}",
            gate_line(&[("Explore", true), ("Librarian", true)]),
            r#"{"kind":"loop.turn.review","delivery_ready":true}"#,
            gate_line(&[("Explore", false)]),
        );
        let observations = parse_gate_observations(&body);
        assert_eq!(observations.len(), 3, "both gate lines, garbage skipped");
        assert_eq!(observations[0].agent, "Explore");
        assert!(observations[0].ok, "bounded=true is an ok observation");
        assert_eq!(observations[1].agent, "Librarian");
        assert!(
            !observations[2].ok,
            "bounded=false is a failure observation"
        );
    }

    #[test]
    fn laplace_smoothed_rates_match_fixed_fixture() {
        // Explore: 3 bounded (ok).  => (3+1)/(3+2) = 0.8
        // Hephaestus: 2 bounded, 3 unbounded => (2+1)/(5+2) = 3/7
        let mut entries: Vec<(&str, bool)> = Vec::new();
        entries.extend([("Explore", true)].repeat(3));
        entries.extend([("Hephaestus", true)].repeat(2));
        entries.extend([("Hephaestus", false)].repeat(3));
        let observations = parse_gate_observations(&gate_line(&entries));
        let rates = success_rates(&observations);

        assert!((rates["Explore"] - 0.8).abs() < 1e-6, "{rates:?}");
        assert!(
            (rates["Hephaestus"] - (3.0 / 7.0)).abs() < 1e-6,
            "add-one smoothing: {rates:?}"
        );
        assert!(
            !rates.contains_key("Absent"),
            "agents with no observations are absent (neutral at lookup)"
        );
    }

    #[test]
    fn window_keeps_only_the_last_n_observations() {
        // 120 ok observations of old data for Explore, then 100 failures for
        // Hephaestus: the window keeps only the last 100 (all failures), so
        // Explore falls out entirely.
        let mut entries: Vec<(&str, bool)> = Vec::new();
        entries.extend([("Explore", true)].repeat(120));
        entries.extend([("Hephaestus", false)].repeat(100));
        let observations = parse_gate_observations(&gate_line(&entries));
        assert_eq!(observations.len(), 220);

        let rates = success_rates(&observations);
        assert!(
            !rates.contains_key("Explore"),
            "the last {} observations exclude the older Explore successes",
            WEIGHT_WINDOW
        );
        assert!(
            (rates["Hephaestus"] - 1.0 / 102.0).abs() < 1e-6,
            "{rates:?}"
        );
    }

    fn result(agent: &str) -> serde_json::Value {
        serde_json::json!({ "agent": agent, "response": "..." })
    }

    #[test]
    fn cold_start_leaves_order_byte_identical() {
        let mut results = vec![result("A"), result("B"), result("C")];
        let before = results.clone();
        order_values_by_weight(&mut results, &HashMap::new());
        assert_eq!(results, before, "empty weights must not reorder anything");
    }

    #[test]
    fn ordering_ranks_higher_weight_first_and_never_drops() {
        let mut results = vec![result("Low"), result("High"), result("Mid")];
        let weights = HashMap::from([
            ("Low".to_string(), 0.1),
            ("High".to_string(), 0.9),
            ("Mid".to_string(), 0.5),
        ]);
        order_values_by_weight(&mut results, &weights);
        let agents: Vec<&str> = results
            .iter()
            .map(|r| r["agent"].as_str().unwrap())
            .collect();
        assert_eq!(agents, vec!["High", "Mid", "Low"]);
        assert_eq!(results.len(), 3, "ordering never drops results");
    }

    #[test]
    fn ties_and_unknown_agents_keep_input_order() {
        let mut results = vec![result("Unknown"), result("A"), result("B")];
        let weights = HashMap::from([
            ("A".to_string(), 0.7),
            ("B".to_string(), 0.7),
            ("C".to_string(), 0.7),
        ]);
        order_values_by_weight(&mut results, &weights);
        let agents: Vec<&str> = results
            .iter()
            .map(|r| r["agent"].as_str().unwrap())
            .collect();
        // Unknown (0.5) sorts below the 0.7 ties; A/B remain in input order.
        assert_eq!(agents, vec!["A", "B", "Unknown"]);
    }

    #[test]
    fn single_result_is_never_reordered() {
        let mut results = vec![result("Low")];
        let weights = HashMap::from([("Low".to_string(), 0.01)]);
        let before = results.clone();
        order_values_by_weight(&mut results, &weights);
        assert_eq!(results, before);
    }

    #[test]
    fn missing_log_is_a_cold_start() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(load_weights(dir.path()).is_empty());
    }

    #[test]
    fn loads_weights_from_audit_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("audit.jsonl"),
            format!(
                "{}\n{}\n",
                gate_line(&[("Explore", true), ("Explore", true)]),
                gate_line(&[("Explore", false)]),
            ),
        )
        .expect("write audit");
        let weights = load_weights(dir.path());
        // 2 ok / 3 trials => (2+1)/(3+2) = 0.6
        assert!((weights["Explore"] - 0.6).abs() < 1e-6, "{weights:?}");
    }
}
