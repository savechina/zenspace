//! Offline replay scorer for the exploration policy (T140) — zero-execution
//! scoring over the recorded discovery tree.
//!
//! The logging discipline in [`super::discovery_tree`] records every attempt
//! (typed `Policy`/`NodeKind`/`Outcome`, real `parent_id` links, append-only
//! `logs/discovery-tree.jsonl`) but nothing reads it back — a write-only
//! primitive. This module is the consumer: it replays the recorded tree and
//! scores each observed policy's **in-sample** outcome record (validated vs
//! rejected counts and rate) with **zero execution** — no LLM calls, no
//! network, no filesystem writes beyond the report.
//!
//! # ⚠️ The in-sample caveat (read this first)
//!
//! In-sample replay **overfits by construction**. A policy that happened to
//! be chosen on easy attempts looks better than one that was tried on hard
//! attempts; the recorded tree is the product of the very selection logic
//! under evaluation, so the scores are circular, not causal. This scorer is
//! an **advisory signal only** — it MUST NOT auto-switch the production
//! policy. Auto-switching on in-sample data would be a new uncalibrated
//! decision, exactly what V13-A.3 forbids. The [`ReplayReport::advisory_only`]
//! field is always `true` and the [`std::fmt::Display`] output repeats the
//! caveat on every render.
//!
//! # What counts as an attempt
//!
//! Only **terminal** outcomes are scored: [`Outcome::Validated`] and
//! [`Outcome::Rejected`]. [`Outcome::Pending`] and [`Outcome::Superseded`]
//! are not attempts with an answer — they cannot tell us whether the policy
//! succeeded or failed, so they are counted nowhere. A policy whose recorded
//! nodes are all pending/superseded has `attempts = 0` and `rate = None` and
//! is **not** a selection candidate ("a policy with no recorded attempts must
//! not be chosen on the strength of a prior").
//!
//! # Selection and the never-worse invariant
//!
//! The chosen policy is the argmax over the validated rate among
//! `{observed policies with ≥1 terminal attempt} ∪ {incumbent}`. The
//! incumbent is always a candidate **when it has recorded terminal attempts**
//! (reverify nodes carry `Policy::Incumbent` with validated/rejected
//! outcomes); ties prefer the incumbent (conservatism). Because the incumbent
//! is in the candidate set, the reported choice is **never worse than the
//! incumbent in-sample** — `chosen_rate ≥ incumbent_rate` always holds when
//! the report is not insufficient. This invariant is asserted in tests.
//!
//! # Honest insufficiency (mirrors `decision_audit`'s `labels_required`)
//!
//! When the tree cannot support a comparison, the scorer reports
//! `insufficient = true` and names what is missing — it never fabricates a
//! recommendation or a number:
//!
//! | state | `missing` |
//! |-------|-----------|
//! | empty tree | `no discovery-tree records` |
//! | no terminal outcomes anywhere | `no completed outcomes (validated or rejected)` |
//! | incumbent has no terminal attempts | `no completed outcomes for the incumbent policy` |
//! | only the incumbent has terminal attempts | `no alternative policy with completed outcomes` |
//!
//! The last two are the honest answer when the never-worse guarantee cannot
//! be stated (no incumbent baseline) or there is nothing to compare against
//! (no challenger) — the same discipline as `decision_audit.rs` refusing to
//! print calibration numbers without labels.
//!
//! # API shape
//!
//! [`score`] is a pure function over loaded records (testable without I/O);
//! [`score_from_log`] is the thin wrapper that loads via
//! [`DiscoveryTree::load`] (fail-open: a missing or corrupt log yields an
//! empty tree, hence an insufficient report — never an error).

use std::path::Path;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::discovery_tree::{DiscoveryNode, DiscoveryTree, INCUMBENT_SLUG, Outcome, Policy};

/// In-sample outcome record for one policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyRecord {
    /// The selector this record describes.
    pub policy: Policy,
    /// Terminal attempts (validated + rejected). Pending/superseded nodes
    /// are not attempts with an answer and are not counted.
    pub attempts: usize,
    /// Terminal attempts that validated.
    pub validated: usize,
    /// Terminal attempts that were rejected.
    pub rejected: usize,
    /// In-sample validated rate `validated / attempts`; `None` when
    /// `attempts == 0` (no completed outcomes — never a fabricated number).
    pub rate: Option<f64>,
}

/// Full replay snapshot for the `zen discover report` `replay` section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayReport {
    /// True when the recorded tree cannot support a comparison — see
    /// [`ReplayReport::missing`]. When true, `chosen`/`chosen_rate`/
    /// `incumbent_rate`/`never_worse` are `None`/`false`.
    pub insufficient: bool,
    /// What is missing, when `insufficient` (e.g. `no completed outcomes
    /// (validated or rejected)`). `None` when sufficient.
    pub missing: Option<String>,
    /// Per-policy in-sample records for every policy observed in the tree,
    /// sorted by rate descending (ties by policy name). Policies with zero
    /// terminal attempts appear with `rate: None`.
    pub policies: Vec<PolicyRecord>,
    /// The recommended policy: argmax validated rate over
    /// `{observed policies with ≥1 terminal attempt} ∪ {incumbent}`, ties
    /// preferring the incumbent. `None` when `insufficient`.
    pub chosen: Option<Policy>,
    /// In-sample validated rate of the chosen policy; `None` when
    /// `insufficient`.
    pub chosen_rate: Option<f64>,
    /// In-sample validated rate of the incumbent policy; `None` when
    /// `insufficient`.
    pub incumbent_rate: Option<f64>,
    /// The never-worse invariant: `Some(true)` when sufficient and
    /// `chosen_rate ≥ incumbent_rate` (always true by construction — the
    /// incumbent is a candidate). `None` when `insufficient` (no selection
    /// was made, so the property is vacuous).
    pub never_worse: Option<bool>,
    /// Always `true`: in-sample replay overfits by construction and must not
    /// auto-switch the production policy. Surfaced so machine consumers
    /// cannot mistake the recommendation for a calibrated decision.
    pub advisory_only: bool,
    /// Snapshot wall-clock time (RFC 3339).
    pub generated_at: String,
}

/// Score the recorded discovery tree (pure — no I/O).
///
/// See the module doc for the attempt definition, the selection rule, the
/// never-worse invariant, and the insufficiency states.
pub fn score(nodes: &[DiscoveryNode]) -> ReplayReport {
    let generated_at = Utc::now().to_rfc3339();

    // Terminal attempts only: validated/rejected. Pending/superseded nodes
    // have no answer and are not scored.
    let terminal: Vec<&DiscoveryNode> = nodes
        .iter()
        .filter(|n| matches!(n.outcome, Outcome::Validated | Outcome::Rejected))
        .collect();

    let missing = if nodes.is_empty() {
        Some("no discovery-tree records".to_string())
    } else if terminal.is_empty() {
        Some("no completed outcomes (validated or rejected)".to_string())
    } else {
        None
    };

    // Per-policy terminal counts over every observed policy (a policy with
    // zero terminal attempts gets attempts=0, rate=None). Accumulated in a
    // Vec — `Policy` deliberately has no `Hash` derive, so no HashMap key.
    let mut counts: Vec<(Policy, usize, usize)> = Vec::new(); // (policy, validated, rejected)
    for node in nodes {
        match counts.iter_mut().find(|(p, _, _)| *p == node.policy) {
            Some((_, validated, rejected)) => match node.outcome {
                Outcome::Validated => *validated += 1,
                Outcome::Rejected => *rejected += 1,
                Outcome::Pending | Outcome::Superseded => {}
            },
            None => {
                let (validated, rejected) = match node.outcome {
                    Outcome::Validated => (1, 0),
                    Outcome::Rejected => (0, 1),
                    Outcome::Pending | Outcome::Superseded => (0, 0),
                };
                counts.push((node.policy, validated, rejected));
            }
        }
    }

    let mut policies: Vec<PolicyRecord> = counts
        .iter()
        .map(|(policy, validated, rejected)| {
            let attempts = validated + rejected;
            PolicyRecord {
                policy: *policy,
                attempts,
                validated: *validated,
                rejected: *rejected,
                rate: (attempts > 0).then(|| *validated as f64 / attempts as f64),
            }
        })
        .collect();
    // Deterministic display order: rate descending, ties by policy name.
    policies.sort_by(|a, b| {
        b.rate
            .partial_cmp(&a.rate)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| policy_name(&a.policy).cmp(policy_name(&b.policy)))
    });

    // Insufficiency beyond the empty/no-terminal states: the never-worse
    // guarantee needs an incumbent baseline, and a comparison needs a
    // challenger. Both are named explicitly rather than fabricating a choice.
    let incumbent_attempts = counts
        .iter()
        .find(|(policy, _, _)| *policy == Policy::Incumbent)
        .map(|(_, validated, rejected)| validated + rejected)
        .unwrap_or(0);
    let challenger_attempts: usize = counts
        .iter()
        .filter(|(policy, _, _)| *policy != Policy::Incumbent)
        .map(|(_, validated, rejected)| validated + rejected)
        .sum();
    let missing = missing.or_else(|| {
        if incumbent_attempts == 0 {
            Some("no completed outcomes for the incumbent policy".to_string())
        } else if challenger_attempts == 0 {
            Some("no alternative policy with completed outcomes".to_string())
        } else {
            None
        }
    });

    let (chosen, chosen_rate, incumbent_rate, never_worse) = match &missing {
        Some(_) => (None, None, None, None),
        None => {
            // Candidate set: policies with ≥1 terminal attempt. The
            // incumbent is guaranteed present here (incumbent_attempts > 0).
            let mut candidates: Vec<(Policy, f64)> = counts
                .iter()
                .filter(|(_, validated, rejected)| validated + rejected > 0)
                .map(|(policy, validated, rejected)| {
                    (*policy, *validated as f64 / (*validated + *rejected) as f64)
                })
                .collect();
            // Deterministic iteration: ties among non-incumbent policies
            // resolve to the alphabetically-first policy name.
            candidates.sort_by_key(|(policy, _)| policy_name(policy).to_string());
            let mut best = candidates[0];
            for (policy, rate) in candidates.iter().skip(1) {
                if *rate > best.1 || (*rate == best.1 && *policy == Policy::Incumbent) {
                    best = (*policy, *rate);
                }
            }
            let incumbent_rate = counts
                .iter()
                .find(|(policy, _, _)| *policy == Policy::Incumbent)
                .map(|(_, validated, rejected)| *validated as f64 / (*validated + *rejected) as f64)
                .expect("incumbent has terminal attempts when sufficient");
            let never_worse = best.1 >= incumbent_rate;
            (
                Some(best.0),
                Some(best.1),
                Some(incumbent_rate),
                Some(never_worse),
            )
        }
    };

    ReplayReport {
        insufficient: missing.is_some(),
        missing,
        policies,
        chosen,
        chosen_rate,
        incumbent_rate,
        never_worse,
        advisory_only: true,
        generated_at,
    }
}

/// Load the discovery tree from `<logs_dir>/discovery-tree.jsonl` and score
/// it. Fail-open: a missing or corrupt log yields an empty tree, hence an
/// insufficient report — never an error (the tree is advisory, not
/// load-bearing).
pub fn score_from_log(logs_dir: &Path) -> ReplayReport {
    let path = super::discovery_tree::discovery_tree_path(logs_dir);
    score(&DiscoveryTree::load(&path))
}

/// Stable display name for a policy (serde snake_case, matching the JSONL
/// records).
fn policy_name(policy: &Policy) -> &'static str {
    match policy {
        Policy::GapDriven => "gap_driven",
        Policy::IlluminationOrdered => "illumination_ordered",
        Policy::SteppingStone => "stepping_stone",
        Policy::ArenaLoss => "arena_loss",
        Policy::Incumbent => INCUMBENT_SLUG,
    }
}

impl std::fmt::Display for ReplayReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.insufficient {
            writeln!(
                f,
                "[replay] insufficient=true  missing={}",
                self.missing.as_deref().unwrap_or("unknown")
            )?;
        } else {
            let chosen = self.chosen.map(|p| policy_name(&p)).unwrap_or("none");
            writeln!(
                f,
                "[replay] chosen={chosen}  rate={:.4}  incumbent_rate={:.4}  never_worse={}",
                self.chosen_rate.unwrap_or(0.0),
                self.incumbent_rate.unwrap_or(0.0),
                self.never_worse.unwrap_or(false),
            )?;
        }
        for record in &self.policies {
            match record.rate {
                Some(rate) => writeln!(
                    f,
                    "  {:<20} attempts={} validated={} rejected={} rate={rate:.4}",
                    policy_name(&record.policy),
                    record.attempts,
                    record.validated,
                    record.rejected,
                )?,
                None => writeln!(
                    f,
                    "  {:<20} attempts=0 (no completed outcomes)",
                    policy_name(&record.policy),
                )?,
            }
        }
        writeln!(
            f,
            "  advisory: in-sample replay overfits by construction — do not auto-switch the production policy"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::distill::discovery_tree::DiscoveryNode;

    /// Build a terminal node for a policy with the given outcome.
    fn terminal(policy: Policy, outcome: Outcome) -> DiscoveryNode {
        DiscoveryNode {
            id: uuid::Uuid::now_v7().to_string(),
            ts: chrono::Utc::now().to_rfc3339(),
            parent_id: None,
            node_kind: crate::distill::discovery_tree::NodeKind::Reverify,
            hypothesis_slug: "h".to_string(),
            policy,
            outcome,
            falsifier: None,
            evidence_refs: Vec::new(),
        }
    }

    fn pending(policy: Policy) -> DiscoveryNode {
        DiscoveryNode {
            id: uuid::Uuid::now_v7().to_string(),
            ts: chrono::Utc::now().to_rfc3339(),
            parent_id: None,
            node_kind: crate::distill::discovery_tree::NodeKind::Incubation,
            hypothesis_slug: "h".to_string(),
            policy,
            outcome: Outcome::Pending,
            falsifier: None,
            evidence_refs: Vec::new(),
        }
    }

    fn rate_of(report: &ReplayReport, policy: Policy) -> Option<f64> {
        report
            .policies
            .iter()
            .find(|r| r.policy == policy)
            .and_then(|r| r.rate)
    }

    #[test]
    fn better_policy_beats_incumbent() {
        // gap_driven: 3/4 validated (0.75) vs incumbent: 1/2 (0.50).
        let nodes = vec![
            terminal(Policy::GapDriven, Outcome::Validated),
            terminal(Policy::GapDriven, Outcome::Validated),
            terminal(Policy::GapDriven, Outcome::Validated),
            terminal(Policy::GapDriven, Outcome::Rejected),
            terminal(Policy::Incumbent, Outcome::Validated),
            terminal(Policy::Incumbent, Outcome::Rejected),
        ];
        let report = score(&nodes);
        assert!(!report.insufficient);
        assert_eq!(report.chosen, Some(Policy::GapDriven));
        assert!((report.chosen_rate.unwrap() - 0.75).abs() < 1e-9);
        assert!((report.incumbent_rate.unwrap() - 0.5).abs() < 1e-9);
        assert_eq!(report.never_worse, Some(true));
    }

    #[test]
    fn worse_policy_reports_incumbent() {
        // gap_driven: 1/4 (0.25) vs incumbent: 2/3 (0.667).
        let nodes = vec![
            terminal(Policy::GapDriven, Outcome::Validated),
            terminal(Policy::GapDriven, Outcome::Rejected),
            terminal(Policy::GapDriven, Outcome::Rejected),
            terminal(Policy::GapDriven, Outcome::Rejected),
            terminal(Policy::Incumbent, Outcome::Validated),
            terminal(Policy::Incumbent, Outcome::Validated),
            terminal(Policy::Incumbent, Outcome::Rejected),
        ];
        let report = score(&nodes);
        assert!(!report.insufficient);
        assert_eq!(report.chosen, Some(Policy::Incumbent));
        assert_eq!(report.never_worse, Some(true));
    }

    #[test]
    fn tie_prefers_incumbent() {
        // Both 1/2 (0.50): conservatism keeps the incumbent.
        let nodes = vec![
            terminal(Policy::GapDriven, Outcome::Validated),
            terminal(Policy::GapDriven, Outcome::Rejected),
            terminal(Policy::Incumbent, Outcome::Validated),
            terminal(Policy::Incumbent, Outcome::Rejected),
        ];
        let report = score(&nodes);
        assert!(!report.insufficient);
        assert_eq!(report.chosen, Some(Policy::Incumbent));
        assert_eq!(report.never_worse, Some(true));
    }

    #[test]
    fn empty_tree_is_insufficient() {
        let report = score(&[]);
        assert!(report.insufficient);
        assert_eq!(report.missing.as_deref(), Some("no discovery-tree records"));
        assert_eq!(report.chosen, None);
        assert_eq!(report.never_worse, None);
        assert!(report.policies.is_empty());
    }

    #[test]
    fn no_completed_outcomes_is_insufficient() {
        // Only pending nodes: no terminal attempt anywhere.
        let nodes = vec![
            pending(Policy::GapDriven),
            pending(Policy::Incumbent),
            pending(Policy::ArenaLoss),
        ];
        let report = score(&nodes);
        assert!(report.insufficient);
        assert_eq!(
            report.missing.as_deref(),
            Some("no completed outcomes (validated or rejected)")
        );
        assert_eq!(report.chosen, None);
        // Observed policies still reported with attempts=0, rate=None.
        assert_eq!(report.policies.len(), 3);
        assert!(
            report
                .policies
                .iter()
                .all(|r| r.attempts == 0 && r.rate.is_none())
        );
    }

    #[test]
    fn only_incumbent_observable_is_insufficient() {
        let nodes = vec![
            terminal(Policy::Incumbent, Outcome::Validated),
            terminal(Policy::Incumbent, Outcome::Rejected),
        ];
        let report = score(&nodes);
        assert!(report.insufficient);
        assert_eq!(
            report.missing.as_deref(),
            Some("no alternative policy with completed outcomes")
        );
        assert_eq!(report.chosen, None);
    }

    #[test]
    fn incumbent_without_terminal_attempts_is_insufficient() {
        // A challenger has terminal attempts but the incumbent baseline is
        // only ever pending — the never-worse guarantee cannot be stated.
        let nodes = vec![
            terminal(Policy::GapDriven, Outcome::Validated),
            terminal(Policy::GapDriven, Outcome::Rejected),
            pending(Policy::Incumbent),
        ];
        let report = score(&nodes);
        assert!(report.insufficient);
        assert_eq!(
            report.missing.as_deref(),
            Some("no completed outcomes for the incumbent policy")
        );
        assert_eq!(report.chosen, None);
    }

    #[test]
    fn zero_attempt_policy_is_not_chosen() {
        // arena_loss has only pending nodes: it must not be chosen on the
        // strength of a prior, even though it is observed.
        let nodes = vec![
            terminal(Policy::GapDriven, Outcome::Validated),
            terminal(Policy::GapDriven, Outcome::Rejected),
            terminal(Policy::Incumbent, Outcome::Validated),
            terminal(Policy::Incumbent, Outcome::Rejected),
            pending(Policy::ArenaLoss),
        ];
        let report = score(&nodes);
        assert!(!report.insufficient);
        assert_eq!(report.chosen, Some(Policy::Incumbent)); // tie → incumbent
        let arena = report
            .policies
            .iter()
            .find(|r| r.policy == Policy::ArenaLoss)
            .unwrap();
        assert_eq!(arena.attempts, 0);
        assert_eq!(arena.rate, None);
    }

    #[test]
    fn never_worse_invariant_holds_across_synthetic_matrix() {
        // Exhaustive small matrix over validated/rejected counts for one
        // challenger vs the incumbent: whenever the report is sufficient,
        // chosen_rate >= incumbent_rate.
        for challenger_v in 0..=4 {
            for challenger_r in 0..=4 {
                for incumbent_v in 1..=4 {
                    for incumbent_r in 0..=4 {
                        let nodes = build_matrix_nodes(
                            challenger_v,
                            challenger_r,
                            incumbent_v,
                            incumbent_r,
                        );
                        let report = score(&nodes);
                        if !report.insufficient {
                            assert_eq!(
                                report.never_worse,
                                Some(true),
                                "never-worse violated: challenger {challenger_v}/{challenger_r} incumbent {incumbent_v}/{incumbent_r}"
                            );
                            let chosen_rate = report.chosen_rate.unwrap();
                            let incumbent_rate = report.incumbent_rate.unwrap();
                            assert!(
                                chosen_rate >= incumbent_rate - 1e-12,
                                "chosen {chosen_rate} < incumbent {incumbent_rate}"
                            );
                        }
                    }
                }
            }
        }
    }

    fn build_matrix_nodes(
        challenger_v: usize,
        challenger_r: usize,
        incumbent_v: usize,
        incumbent_r: usize,
    ) -> Vec<DiscoveryNode> {
        let mut nodes = Vec::new();
        for _ in 0..challenger_v {
            nodes.push(terminal(Policy::GapDriven, Outcome::Validated));
        }
        for _ in 0..challenger_r {
            nodes.push(terminal(Policy::GapDriven, Outcome::Rejected));
        }
        for _ in 0..incumbent_v {
            nodes.push(terminal(Policy::Incumbent, Outcome::Validated));
        }
        for _ in 0..incumbent_r {
            nodes.push(terminal(Policy::Incumbent, Outcome::Rejected));
        }
        nodes
    }

    #[test]
    fn score_from_log_loads_and_scores() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = super::super::discovery_tree::discovery_tree_path(dir.path());
        DiscoveryTree::append(&path, &terminal(Policy::GapDriven, Outcome::Validated)).unwrap();
        DiscoveryTree::append(&path, &terminal(Policy::GapDriven, Outcome::Rejected)).unwrap();
        DiscoveryTree::append(&path, &terminal(Policy::Incumbent, Outcome::Validated)).unwrap();
        DiscoveryTree::append(&path, &terminal(Policy::Incumbent, Outcome::Rejected)).unwrap();

        let report = score_from_log(dir.path());
        assert!(!report.insufficient);
        assert_eq!(report.chosen, Some(Policy::Incumbent)); // tie → incumbent
        assert_eq!(report.never_worse, Some(true));
    }

    #[test]
    fn score_from_log_missing_file_is_insufficient_not_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let report = score_from_log(dir.path());
        assert!(report.insufficient);
        assert_eq!(report.missing.as_deref(), Some("no discovery-tree records"));
    }

    #[test]
    fn report_renders_and_round_trips_through_json() {
        let nodes = vec![
            terminal(Policy::GapDriven, Outcome::Validated),
            terminal(Policy::GapDriven, Outcome::Validated),
            terminal(Policy::GapDriven, Outcome::Rejected),
            terminal(Policy::Incumbent, Outcome::Validated),
            terminal(Policy::Incumbent, Outcome::Rejected),
        ];
        let report = score(&nodes);
        // Display renders the section header + the advisory caveat.
        let display = report.to_string();
        assert!(display.contains("[replay]"));
        assert!(display.contains("advisory"));
        assert!(display.contains("never_worse=true"));

        // JSON round-trip preserves the recommendation and the invariant.
        let json = serde_json::to_string(&report).unwrap();
        let back: ReplayReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back.insufficient, report.insufficient);
        assert_eq!(back.chosen, report.chosen);
        assert_eq!(back.chosen_rate, report.chosen_rate);
        assert_eq!(back.incumbent_rate, report.incumbent_rate);
        assert_eq!(back.never_worse, report.never_worse);
        assert!(back.advisory_only);
        assert_eq!(back.policies.len(), report.policies.len());
        assert_eq!(
            rate_of(&back, Policy::GapDriven),
            rate_of(&report, Policy::GapDriven)
        );
    }

    #[test]
    fn insufficient_report_renders_missing_not_a_number() {
        let report = score(&[]);
        let display = report.to_string();
        assert!(display.contains("insufficient=true"));
        assert!(display.contains("no discovery-tree records"));
        assert!(!display.contains("rate="));
        assert!(!display.contains("never_worse="));
    }
}
