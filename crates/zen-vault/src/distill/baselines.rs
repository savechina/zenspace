//! Falsifiable-baseline observations for the V13-A.3 decision layer.
//!
//! # Functionality
//! Computes the four V13-A.3 baselines — intent p95 latency, LLM-intent call
//! rate, routing-distribution parity, and the review cascade's frontier-call
//! reduction — as *observations* over the real audit sink
//! (`<logs>/audit.jsonl`). Each baseline carries its observed value (or
//! `None`), its sample size `n`, an explicit insufficiency state, and the
//! audit line kinds it was derived from. Nothing here compares against the
//! V13-A.3 target numbers: the comparison is a presentation concern for the
//! orchestrator that wires this surface.
//!
//! # User impact
//! A reviewer can finally tell whether the L1 ladder is helping: the intent
//! p95 and LLM-intent call rate are computed from real traffic, and a
//! baseline with fewer than [`MIN_SAMPLE`] observations reports
//! [`InsufficientSample`] instead of a number — a baseline computed from
//! three data points is worse than none.
//!
//! # Default behavior
//! A missing log file, an unreadable directory, or a corrupt line degrades
//! to insufficient/`None` — never a panic, never a fabricated number. The
//! review-cascade frontier-call baseline is *not derivable* from any emitted
//! `loop.decision` review line's `escalated` field (T170)
//! with that reason stated.
//!
//! # Interaction
//! Reads the same `audit.jsonl` as [`super::orchestration_stats`] and
//! [`super::decision_audit`], with the same lightweight field-extractor
//! discipline (no full JSON tree per line). It does not call into
//! `aggregate_orchestration` because that aggregator does not expose the
//! per-baseline sample sizes or the `loop.decision` shadow lines this module
//! needs; the parsing approach is matched, not forked.

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Minimum sample size before a baseline is reported as trustworthy.
///
/// Justification: the 95th percentile of fewer than 30 samples is dominated
/// by single outliers (with n = 20 the p95 is the 19th-highest value — one
/// slow call moves it arbitrarily), and a call-rate over fewer than 30 turns
/// has a binomial standard error of roughly 5.5 percentage points at the 10%
/// baseline (`sqrt(0.1 * 0.9 / 30)`). 30 is the conventional small-sample
/// floor for percentile/rate estimation; below it the honest output is
/// [`InsufficientSample`], not a number.
pub const MIN_SAMPLE: usize = 30;

/// Why the review-cascade frontier-call baseline cannot be observed.
///
/// Explicit small-sample state: `n` observations exist but [`MIN_SAMPLE`] are
/// required before the value may be reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InsufficientSample {
    /// Observed sample size.
    pub n: usize,
    /// Minimum sample size required ([`MIN_SAMPLE`]).
    pub required: usize,
}

/// One baseline observation: the value (or `None`), its sample size, and why
/// it is not trustworthy when it is not.
///
/// `value` is `None` in exactly two cases, distinguished by the other fields:
/// * `insufficient` is `Some` — the sample is below [`MIN_SAMPLE`]; the value
///   is withheld, never defaulted.
/// * `reason` is `Some` — the baseline is not derivable from real emitted
///   fields at all (insufficient rather than assumed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Baseline<T> {
    /// Observed value; `None` when insufficient or not derivable.
    pub value: Option<T>,
    /// Sample size the observation is based on.
    pub n: usize,
    /// `Some` when `n < MIN_SAMPLE` — the value is withheld.
    pub insufficient: Option<InsufficientSample>,
    /// Why the value is `None` when it is not an insufficiency (not derivable).
    pub reason: Option<String>,
    /// Which audit line kinds (and fields) the observation came from.
    pub sources: Vec<String>,
}

impl<T> Baseline<T> {
    fn insufficient(n: usize, sources: Vec<String>) -> Self {
        Self {
            value: None,
            n,
            insufficient: Some(InsufficientSample {
                n,
                required: MIN_SAMPLE,
            }),
            reason: None,
            sources,
        }
    }

    fn observed(value: T, n: usize, sources: Vec<String>) -> Self {
        Self {
            value: Some(value),
            n,
            insufficient: None,
            reason: None,
            sources,
        }
    }
}

/// One routing-source bucket of the distribution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceCount {
    /// `intent_source` value as emitted: `Llm` / `Keyword` / `Fallback` / `L1`.
    pub source: String,
    /// Turns routed through this source.
    pub count: usize,
}

/// Shadow-mode L1↔production agreement — the direct parity observation.
///
/// Derived from the shadow `loop.decision` lines' `agree` field (the ladder
/// variant carries no `agree` and is excluded). `rate` is withheld below
/// [`MIN_SAMPLE`] shadow observations, consistent with the baseline rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowAgreement {
    /// Shadow observations where L1 agreed with the production decision.
    pub agreed: usize,
    /// Shadow observations total.
    pub total: usize,
    /// `agreed / total`; `None` when `total < MIN_SAMPLE`.
    pub rate: Option<f64>,
}

/// Observed routing distribution for the routing-distribution-parity baseline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingDistribution {
    /// Per-source turn counts from `loop.turn.review.intent_source`.
    pub sources: Vec<SourceCount>,
    /// Total turns with an `intent_source` field.
    pub total: usize,
    /// Shadow-mode L1↔production agreement, when any shadow lines exist.
    pub shadow_agreement: Option<ShadowAgreement>,
}

/// Full falsifiable-baseline snapshot over one audit log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Baselines {
    /// V13-A.3 "intent p95 < 500ms": p95 of `intent_llm_ms` over
    /// `loop.turn.review` lines. Covers LLM-attempted turns only (see
    /// [`Baselines::notes`]).
    pub intent_p95_ms: Baseline<u64>,
    /// V13-A.3 "LLM-intent call rate < 10%": fraction of `loop.turn.review`
    /// turns where an LLM classification call was actually attempted
    /// (`ok` / `low_confidence` / `timeout` / `error`). `skipped`,
    /// `unavailable` and `l1_resolved` are not calls.
    pub llm_intent_call_rate: Baseline<f64>,
    /// V13-A.3 "routing-distribution parity": the `intent_source` distribution
    /// plus the shadow L1↔production agreement rate.
    pub routing_distribution: Baseline<RoutingDistribution>,
    /// V13-A.3 "review cascade −70% frontier-call": not derivable from any
    /// `loop.decision` review line's `escalated` field (T170); insufficient
    /// until that emit exists in the log.
    pub review_frontier_call_rate: Baseline<f64>,
    /// Cross-cutting caveats about what the observations do and do not cover.
    pub notes: Vec<String>,
}

/// Extract `"key":"value"` from a raw JSON line (same discipline as
/// `orchestration_stats` / `decision_audit` — no full JSON tree per line).
fn read_field_str(line: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = line.find(&needle)?;
    let value_start = start + needle.len();
    let rest = &line[value_start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

pub(super) fn read_field_u64(line: &str, key: &str) -> Option<u64> {
    let needle = format!("\"{key}\":");
    let start = line.find(&needle)?;
    let value_start = start + needle.len();
    let rest = &line[value_start..].trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    rest[..end].parse().ok()
}

fn read_field_bool(line: &str, key: &str) -> Option<bool> {
    if line.contains(&format!("\"{key}\":true")) {
        return Some(true);
    }
    if line.contains(&format!("\"{key}\":false")) {
        return Some(false);
    }
    None
}

/// Percentile from an ascending-sorted slice (0.0–1.0), same formula as
/// `orchestration_stats::percentile`.
fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
}

/// True when the `intent_llm_outcome` means an LLM classification call was
/// actually attempted. `skipped` (fail-fast availability gate), `unavailable`
/// (config-level resolution failure before any call) and `l1_resolved` (L1
/// served, LLM never invoked) are NOT calls.
fn is_llm_call(outcome: &str) -> bool {
    matches!(outcome, "ok" | "low_confidence" | "timeout" | "error")
}

/// The all-insufficient snapshot for a missing/unreadable audit log.
fn empty_baselines() -> Baselines {
    Baselines {
        intent_p95_ms: Baseline::insufficient(
            0,
            vec!["loop.turn.review.intent_llm_ms".to_string()],
        ),
        llm_intent_call_rate: Baseline::insufficient(
            0,
            vec!["loop.turn.review.intent_llm_outcome".to_string()],
        ),
        routing_distribution: Baseline::insufficient(
            0,
            vec![
                "loop.turn.review.intent_source".to_string(),
                "loop.decision.agree".to_string(),
            ],
        ),
        review_frontier_call_rate: Baseline::insufficient(
            0,
            vec!["loop.decision(decision_kind=review).escalated".to_string()],
        ),
        notes: Vec::new(),
    }
}

/// Compute the four V13-A.3 baselines from `<logs_dir>/audit.jsonl`.
///
/// A missing log file, an unreadable directory, or a corrupt line degrades to
/// insufficient/`None` — never a panic, never a fabricated number. Each
/// baseline reports its observed value (or `None`), its sample size `n`, and
/// an explicit [`InsufficientSample`] below [`MIN_SAMPLE`].
pub fn compute(logs_dir: &Path) -> Baselines {
    let audit_path = logs_dir.join("audit.jsonl");
    let file = match fs::File::open(&audit_path) {
        Ok(f) => f,
        Err(_) => return empty_baselines(),
    };

    let reader = io::BufReader::new(file);
    let mut llm_latencies: Vec<u64> = Vec::new();
    let mut llm_calls: usize = 0;
    let mut outcome_turns: usize = 0;
    let mut source_counts: HashMap<String, usize> = HashMap::new();
    let mut source_turns: usize = 0;
    let mut shadow_agreed: usize = 0;
    let mut shadow_total: usize = 0;
    let mut review_total: usize = 0;
    let mut review_escalated: usize = 0;

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.contains("\"kind\":\"loop.turn.review\"") {
            if let Some(ms) = read_field_u64(&line, "intent_llm_ms") {
                llm_latencies.push(ms);
            }
            if let Some(outcome) = read_field_str(&line, "intent_llm_outcome") {
                outcome_turns += 1;
                if is_llm_call(&outcome) {
                    llm_calls += 1;
                }
            }
            if let Some(source) = read_field_str(&line, "intent_source") {
                *source_counts.entry(source).or_insert(0) += 1;
                source_turns += 1;
            }
        } else if line.contains("\"kind\":\"loop.decision\"") {
            // Shadow variant carries `agree`; the ladder variant does not.
            if let Some(agree) = read_field_bool(&line, "agree") {
                shadow_total += 1;
                if agree {
                    shadow_agreed += 1;
                }
            }
            // T170 review cascade: `escalated` records whether the frontier
            // judge was consulted, which is what this baseline counts.
            if read_field_str(&line, "decision_kind").as_deref() == Some("review")
                && let Some(escalated) = read_field_bool(&line, "escalated")
            {
                review_total += 1;
                if escalated {
                    review_escalated += 1;
                }
            }
        }
    }

    let mut notes = Vec::new();
    if !llm_latencies.is_empty() {
        notes.push(
            "intent p95 covers LLM-attempted turns only (intent_llm_ms); L1-served turns \
             record latency_ms on loop.decision and are not merged (a different rung's latency)"
                .to_string(),
        );
    }

    let intent_p95_ms = if llm_latencies.len() < MIN_SAMPLE {
        Baseline::insufficient(
            llm_latencies.len(),
            vec!["loop.turn.review.intent_llm_ms".to_string()],
        )
    } else {
        let mut sorted = llm_latencies;
        sorted.sort_unstable();
        Baseline::observed(
            percentile(&sorted, 0.95),
            sorted.len(),
            vec!["loop.turn.review.intent_llm_ms".to_string()],
        )
    };

    let llm_intent_call_rate = if outcome_turns < MIN_SAMPLE {
        Baseline::insufficient(
            outcome_turns,
            vec!["loop.turn.review.intent_llm_outcome".to_string()],
        )
    } else {
        Baseline::observed(
            llm_calls as f64 / outcome_turns as f64,
            outcome_turns,
            vec!["loop.turn.review.intent_llm_outcome".to_string()],
        )
    };

    let routing_distribution = if source_turns < MIN_SAMPLE {
        Baseline::insufficient(
            source_turns,
            vec![
                "loop.turn.review.intent_source".to_string(),
                "loop.decision.agree".to_string(),
            ],
        )
    } else {
        let mut sources: Vec<SourceCount> = source_counts
            .iter()
            .map(|(source, &count)| SourceCount {
                source: source.clone(),
                count,
            })
            .collect();
        sources.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.source.cmp(&b.source)));
        let shadow_agreement = if shadow_total == 0 {
            None
        } else {
            Some(ShadowAgreement {
                agreed: shadow_agreed,
                total: shadow_total,
                rate: (shadow_total >= MIN_SAMPLE)
                    .then(|| shadow_agreed as f64 / shadow_total as f64),
            })
        };
        Baseline::observed(
            RoutingDistribution {
                sources,
                total: source_turns,
                shadow_agreement,
            },
            source_turns,
            vec![
                "loop.turn.review.intent_source".to_string(),
                "loop.decision.agree".to_string(),
            ],
        )
    };

    let review_frontier_call_rate = if review_total < MIN_SAMPLE {
        Baseline::insufficient(
            review_total,
            vec!["loop.decision(decision_kind=review).escalated".to_string()],
        )
    } else {
        Baseline::observed(
            review_escalated as f64 / review_total as f64,
            review_total,
            vec!["loop.decision(decision_kind=review).escalated".to_string()],
        )
    };

    Baselines {
        intent_p95_ms,
        llm_intent_call_rate,
        routing_distribution,
        review_frontier_call_rate,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn write_audit(dir: &Path, lines: &[&str]) {
        let path = dir.join("audit.jsonl");
        let mut content = String::new();
        for line in lines {
            content.push_str(line);
            content.push('\n');
        }
        fs::write(&path, content).unwrap();
    }

    /// A `loop.turn.review` line carrying the intent fields this module reads.
    fn review_line(outcome: &str, ms: u64, source: &str) -> String {
        format!(
            r#"{{"kind":"loop.turn.review","session_id":"s","agent":"Sisyphus","intent_signal":"q","intent_category":"Query","intent_source":"{source}","intent_confidence":0.8,"intent_acl":"public","intent_llm_outcome":"{outcome}","intent_llm_ms":{ms},"plan_approved":true,"delivery_ready":true,"feedback_rounds":0,"failed_attempts":0}}"#
        )
    }

    /// A shadow `loop.decision` line (carries `agree`).
    fn review_decision_line(escalated: bool) -> String {
        format!(
            "{{\"kind\":\"loop.decision\",\"decision\":\"review\",\"decision_kind\":\"review\",\"rung\":\"L1\",\"escalated\":{escalated},\"confidence\":0.9,\"choice\":\"approved\"}}"
        )
    }

    fn ladder_decision_line() -> String {
        "{\"kind\":\"loop.decision\",\"decision\":\"intent\",\"decision_kind\":\"intent\",\"rung\":\"L1\",\"confidence\":0.9,\"choice\":\"Query\"}".to_string()
    }

    fn shadow_line(agree: bool) -> String {
        format!(
            r#"{{"kind":"loop.decision","decision":"intent","session_id":"s","production_rung":"L2","production_source":"Llm","production_choice":"Query","l1_rung":"L1","l1_choice":"Query","l1_confidence":0.9,"agree":{agree},"l1_latency_ms":10}}"#
        )
    }

    /// A ladder `loop.decision` line (no `agree` field).
    fn ladder_line() -> &'static str {
        r#"{"kind":"loop.decision","decision":"intent","session_id":"s","rung":"L1","gate":0.5,"gate_fired":true,"confidence":0.9,"latency_ms":12,"choice":"Query","agent":"Momus","signal":"review-quality"}"#
    }

    fn assert_insufficient_zero<T>(baseline: &Baseline<T>) {
        assert!(baseline.value.is_none());
        assert_eq!(baseline.n, 0);
        let ins = baseline.insufficient.as_ref().expect("insufficient");
        assert_eq!(ins.n, 0);
        assert_eq!(ins.required, MIN_SAMPLE);
    }

    #[test]
    fn empty_log_yields_every_baseline_insufficient_with_n_zero() {
        let dir = tmpdir();
        let b = compute(dir.path());
        assert_insufficient_zero(&b.intent_p95_ms);
        assert_insufficient_zero(&b.llm_intent_call_rate);
        assert_insufficient_zero(&b.routing_distribution);
        assert_insufficient_zero(&b.review_frontier_call_rate);
        assert!(b.notes.is_empty());
    }

    #[test]
    fn missing_audit_file_is_insufficient_not_error() {
        let dir = tmpdir();
        let b = compute(dir.path());
        assert_insufficient_zero(&b.intent_p95_ms);
        assert_insufficient_zero(&b.llm_intent_call_rate);
    }

    #[test]
    fn known_intent_llm_ms_yields_expected_p95() {
        let dir = tmpdir();
        // 30 samples, values 100..=129. p95 index = round(29 * 0.95) = 28 →
        // sorted[28] = 128.
        let lines: Vec<String> = (0..30).map(|i| review_line("ok", 100 + i, "Llm")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_audit(dir.path(), &refs);
        let b = compute(dir.path());
        assert_eq!(b.intent_p95_ms.value, Some(128));
        assert_eq!(b.intent_p95_ms.n, 30);
        assert!(b.intent_p95_ms.insufficient.is_none());
        assert_eq!(
            b.intent_p95_ms.sources,
            vec!["loop.turn.review.intent_llm_ms"]
        );
    }

    #[test]
    fn llm_call_rate_counts_only_actual_calls() {
        let dir = tmpdir();
        // 18 real calls + 12 non-calls (skipped / l1_resolved / unavailable):
        // rate must be 18/30 = 0.6, proving non-calls never count.
        let mut lines = Vec::new();
        for _ in 0..18 {
            lines.push(review_line("ok", 100, "Llm"));
        }
        for _ in 0..4 {
            lines.push(review_line("skipped", 0, "Keyword"));
        }
        for _ in 0..4 {
            lines.push(review_line("l1_resolved", 0, "L1"));
        }
        for _ in 0..4 {
            lines.push(review_line("unavailable", 0, "Keyword"));
        }
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_audit(dir.path(), &refs);
        let b = compute(dir.path());
        let rate = b.llm_intent_call_rate.value.expect("rate observed");
        assert!((rate - 0.6).abs() < 1e-9);
        assert_eq!(b.llm_intent_call_rate.n, 30);
        assert!(b.llm_intent_call_rate.insufficient.is_none());
    }

    #[test]
    fn routing_distribution_counts_sources_and_shadow_agreement() {
        let dir = tmpdir();
        let mut lines = Vec::new();
        for _ in 0..12 {
            lines.push(review_line("ok", 100, "Llm"));
        }
        for _ in 0..10 {
            lines.push(review_line("skipped", 0, "Keyword"));
        }
        for _ in 0..5 {
            lines.push(review_line("skipped", 0, "Fallback"));
        }
        for _ in 0..3 {
            lines.push(review_line("l1_resolved", 0, "L1"));
        }
        for _ in 0..15 {
            lines.push(shadow_line(true));
        }
        for _ in 0..15 {
            lines.push(shadow_line(false));
        }
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_audit(dir.path(), &refs);
        let b = compute(dir.path());
        let dist = b.routing_distribution.value.expect("distribution observed");
        assert_eq!(dist.total, 30);
        let llm = dist.sources.iter().find(|s| s.source == "Llm").unwrap();
        assert_eq!(llm.count, 12);
        let keyword = dist.sources.iter().find(|s| s.source == "Keyword").unwrap();
        assert_eq!(keyword.count, 10);
        let fallback = dist
            .sources
            .iter()
            .find(|s| s.source == "Fallback")
            .unwrap();
        assert_eq!(fallback.count, 5);
        let l1 = dist.sources.iter().find(|s| s.source == "L1").unwrap();
        assert_eq!(l1.count, 3);
        let shadow = dist.shadow_agreement.expect("shadow observed");
        assert_eq!(shadow.total, 30);
        assert_eq!(shadow.agreed, 15);
        assert!((shadow.rate.unwrap() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn ladder_decision_lines_do_not_count_as_shadow() {
        let dir = tmpdir();
        let mut lines: Vec<String> = (0..30).map(|i| review_line("ok", 100 + i, "Llm")).collect();
        lines.push(ladder_line().to_string());
        lines.push(shadow_line(true));
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_audit(dir.path(), &refs);
        let b = compute(dir.path());
        let dist = b.routing_distribution.value.expect("distribution observed");
        let shadow = dist.shadow_agreement.expect("shadow observed");
        assert_eq!(shadow.total, 1, "ladder line (no agree) must not count");
        assert_eq!(shadow.agreed, 1);
    }

    #[test]
    fn malformed_and_absent_lines_are_skipped_without_panicking() {
        let dir = tmpdir();
        let valid = review_line("ok", 100, "Llm");
        let lines = vec![
            "not json at all",
            "}}}}invalid{{{",
            r#"{"kind":"loop.turn.review","intent_category":"Query"}"#,
            &valid,
        ];
        write_audit(dir.path(), &lines);
        let b = compute(dir.path());
        // Only the one well-formed line contributes.
        assert_eq!(b.intent_p95_ms.n, 1);
        assert_eq!(b.llm_intent_call_rate.n, 1);
        assert_eq!(b.routing_distribution.n, 1);
        assert!(b.intent_p95_ms.value.is_none());
        assert!(b.intent_p95_ms.insufficient.is_some());
    }

    #[test]
    fn below_minimum_sample_reports_insufficient_not_a_value() {
        let dir = tmpdir();
        let lines: Vec<String> = (0..5).map(|i| review_line("ok", 100 + i, "Llm")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_audit(dir.path(), &refs);
        let b = compute(dir.path());
        assert!(b.intent_p95_ms.value.is_none());
        let ins = b.intent_p95_ms.insufficient.expect("insufficient");
        assert_eq!(ins.n, 5);
        assert_eq!(ins.required, MIN_SAMPLE);
        assert!(b.llm_intent_call_rate.value.is_none());
        assert!(b.routing_distribution.value.is_none());
    }

    #[test]
    fn frontier_call_rate_counts_only_review_decisions() {
        let dir = tmpdir();
        let mut lines: Vec<String> = (0..30).map(|i| review_line("ok", 100 + i, "Llm")).collect();
        // MIN_SAMPLE review decisions, 9 of which escalated to the frontier
        // judge.
        for i in 0..MIN_SAMPLE {
            lines.push(review_decision_line(i < 9));
        }
        // A non-review decision line must not enter this sample.
        lines.push(ladder_decision_line());
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_audit(dir.path(), &refs);

        let b = compute(dir.path());
        assert_eq!(b.review_frontier_call_rate.n, MIN_SAMPLE);
        let rate = b.review_frontier_call_rate.value.expect("observed");
        assert!((rate - 0.3).abs() < 1e-9, "9 of 30 escalated, got {rate}");
    }

    #[test]
    fn frontier_call_rate_needs_the_escalation_field() {
        // Without the T170 emit there is nothing to observe: the baseline must
        // report insufficient, never assume the frontier was not called.
        let dir = tmpdir();
        let lines: Vec<String> = (0..30).map(|i| review_line("ok", 100 + i, "Llm")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_audit(dir.path(), &refs);
        let b = compute(dir.path());
        assert!(b.review_frontier_call_rate.value.is_none());
        assert_eq!(b.review_frontier_call_rate.n, 0);
        assert!(b.review_frontier_call_rate.insufficient.is_some());
    }

    #[test]
    fn shadow_rate_is_withheld_below_minimum_sample() {
        let dir = tmpdir();
        let mut lines: Vec<String> = (0..30).map(|i| review_line("ok", 100 + i, "Llm")).collect();
        lines.push(shadow_line(true));
        lines.push(shadow_line(false));
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_audit(dir.path(), &refs);
        let b = compute(dir.path());
        let dist = b.routing_distribution.value.expect("distribution observed");
        let shadow = dist.shadow_agreement.expect("shadow observed");
        assert_eq!(shadow.total, 2);
        assert_eq!(shadow.agreed, 1);
        assert!(shadow.rate.is_none(), "rate withheld below MIN_SAMPLE");
    }

    #[test]
    fn baselines_round_trip_through_json() {
        let dir = tmpdir();
        let lines: Vec<String> = (0..30).map(|i| review_line("ok", 100 + i, "Llm")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_audit(dir.path(), &refs);
        let b = compute(dir.path());
        let json = serde_json::to_string(&b).unwrap();
        let back: Baselines = serde_json::from_str(&json).unwrap();
        assert_eq!(back.intent_p95_ms.value, b.intent_p95_ms.value);
        assert_eq!(back.llm_intent_call_rate.n, 30);
        assert_eq!(back.routing_distribution.n, 30);
    }
}
