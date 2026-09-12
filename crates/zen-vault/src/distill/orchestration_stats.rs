//! Orchestration telemetry aggregator for `zen discover report`.
//!
//! Parses `audit.jsonl` and produces section-level counters matching the
//! output of `bin/orchestration-stats`. Sections:
//!
//! - **turn.review** — intent routing distributions, delivery-ready rate,
//!   plan veto rate, feedback rounds, intent LLM outcome + latency p50/p95
//! - **gateway** — turn started/completed counters
//! - **delegate.gates** — delegation gate events, batch ratio, blocked-by
//!   reason counts, depth distribution, target agent distribution
//! - **plan.completed** — plan outcomes (ok/failed/skipped), avg duration,
//!   delivery-not-ready rate
//!
//! Path-agnostic: callers pass the resolved logs dir. Corrupt lines are
//! skipped; missing files produce zeroed sections (not an error).

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Errors from audit log I/O.
#[derive(Debug)]
pub enum AuditError {
    /// Filesystem error (missing file is NOT an error — see [`aggregate_orchestration`]).
    Io(io::Error),
}

impl std::fmt::Display for AuditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuditError::Io(e) => write!(f, "audit log I/O error: {e}"),
        }
    }
}

impl std::error::Error for AuditError {}

/// Count + percentage for one bucket in a distribution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IntentDist {
    pub name: String,
    pub count: usize,
    pub pct: f64,
}

/// Aggregated `[turn.review]` section.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TurnReviewStats {
    pub turns: usize,
    pub delivery_not_ready: usize,
    pub delivery_not_ready_pct: f64,
    pub plan_vetoed: usize,
    pub feedback_rounds_total: u64,
    pub intent_categories: Vec<IntentDist>,
    pub intent_sources: Vec<IntentDist>,
    pub intent_llm_outcomes: Vec<IntentDist>,
    pub intent_llm_ms_p50: u64,
    pub intent_llm_ms_p95: u64,
}

/// Aggregated `[gateway]` section.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GatewayStats {
    pub turns_started: usize,
    pub turns_completed: usize,
}

/// Aggregated `[delegate.gates]` section.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DelegateGatesStats {
    pub events: usize,
    pub batched: usize,
    pub batched_pct: f64,
    pub blocked_independent: usize,
    pub blocked_unbounded: usize,
    pub blocked_consumer: usize,
    pub blocked_not_worth: usize,
    pub depth_distribution: Vec<IntentDist>,
    pub target_distribution: Vec<IntentDist>,
}

/// Aggregated `[plan.completed]` section.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlanCompletedStats {
    pub plans: usize,
    pub tasks_ok: u64,
    pub tasks_failed: u64,
    pub tasks_skipped: u64,
    pub avg_duration_ms: u64,
    pub delivery_not_ready: usize,
    pub delivery_not_ready_pct: f64,
}

/// Full orchestration telemetry snapshot — all four sections.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OrchestrationStats {
    pub turn_review: TurnReviewStats,
    pub gateway: GatewayStats,
    pub delegate_gates: DelegateGatesStats,
    pub plan_completed: PlanCompletedStats,
}

// Lightweight JSON field extractors that work on raw lines without full
// serde deserialization — avoids allocating a Value tree per line.

/// Extract `"key":"value"` from a raw JSON line.
fn read_field_str(line: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = line.find(&needle)?;
    let value_start = start + needle.len();
    let rest = &line[value_start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn read_field_u64(line: &str, key: &str) -> Option<u64> {
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

fn has_bool_false(line: &str, key: &str) -> bool {
    line.contains(&format!("\"{key}\":false"))
}

fn distribution(counts: &HashMap<String, usize>, total: usize) -> Vec<IntentDist> {
    let mut items: Vec<IntentDist> = counts
        .iter()
        .map(|(name, &count)| IntentDist {
            name: name.clone(),
            count,
            pct: if total > 0 {
                count as f64 / total as f64 * 100.0
            } else {
                0.0
            },
        })
        .collect();
    items.sort_by_key(|b| std::cmp::Reverse(b.count));
    items
}

/// Percentile from an ascending-sorted slice (0.0–1.0).
fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
}

/// Aggregate orchestration telemetry from an `audit.jsonl` file.
///
/// Returns a zeroed snapshot if the file does not exist. Corrupt lines are
/// skipped (never panic). Fields absent from a line are silently ignored.
pub fn aggregate_orchestration(dir: &Path) -> Result<OrchestrationStats, AuditError> {
    let audit_path = dir.join("audit.jsonl");
    let file = match fs::File::open(&audit_path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(OrchestrationStats::default()),
        Err(e) => return Err(AuditError::Io(e)),
    };

    let reader = io::BufReader::new(file);
    let mut stats = OrchestrationStats::default();

    let mut cat_counts: HashMap<String, usize> = HashMap::new();
    let mut src_counts: HashMap<String, usize> = HashMap::new();
    let mut outcome_counts: HashMap<String, usize> = HashMap::new();
    let mut llm_latencies: Vec<u64> = Vec::new();
    let mut depth_counts: HashMap<String, usize> = HashMap::new();
    let mut target_counts: HashMap<String, usize> = HashMap::new();

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(_) => continue,
        };

        if line.contains("\"kind\":\"loop.turn.review\"") {
            stats.turn_review.turns += 1;

            if let Some(cat) = read_field_str(&line, "intent_category") {
                *cat_counts.entry(cat).or_insert(0) += 1;
            }
            if let Some(src) = read_field_str(&line, "intent_source") {
                *src_counts.entry(src).or_insert(0) += 1;
            }
            if let Some(outcome) = read_field_str(&line, "intent_llm_outcome") {
                *outcome_counts.entry(outcome).or_insert(0) += 1;
            }
            if let Some(ms) = read_field_u64(&line, "intent_llm_ms") {
                llm_latencies.push(ms);
            }
            if has_bool_false(&line, "delivery_ready") {
                stats.turn_review.delivery_not_ready += 1;
            }
            if has_bool_false(&line, "plan_approved") {
                stats.turn_review.plan_vetoed += 1;
            }
            if let Some(fb) = read_field_u64(&line, "feedback_rounds") {
                stats.turn_review.feedback_rounds_total += fb;
            }
        } else if line.contains("\"kind\":\"gateway.turn.started\"") {
            stats.gateway.turns_started += 1;
        } else if line.contains("\"kind\":\"gateway.turn.completed\"") {
            stats.gateway.turns_completed += 1;
        } else if line.contains("\"kind\":\"loop.delegate.gates\"") {
            stats.delegate_gates.events += 1;

            if let Some(w) = read_field_u64(&line, "batch_width")
                && w > 1
            {
                stats.delegate_gates.batched += 1;
            }
            if let Some(d) = read_field_u64(&line, "depth") {
                *depth_counts.entry(d.to_string()).or_insert(0) += 1;
            }
            if has_bool_false(&line, "independent") {
                stats.delegate_gates.blocked_independent += 1;
            }
            if has_bool_false(&line, "bounded") {
                stats.delegate_gates.blocked_unbounded += 1;
            }
            if has_bool_false(&line, "consumer_decision") {
                stats.delegate_gates.blocked_consumer += 1;
            }
            if has_bool_false(&line, "worth_it") {
                stats.delegate_gates.blocked_not_worth += 1;
            }

            // Extract "agent":"<name>" entries from the nested gates array.
            if let Some(gates_start) = line.find("\"gates\":[") {
                let gates_section = &line[gates_start..];
                let mut pos = 0;
                while pos < gates_section.len() {
                    if let Some(offset) = gates_section[pos..].find("\"agent\":\"") {
                        let agent_start = pos + offset + 9;
                        if let Some(end) = gates_section[agent_start..].find('"') {
                            let agent = &gates_section[agent_start..agent_start + end];
                            *target_counts.entry(agent.to_string()).or_insert(0) += 1;
                        }
                        pos = agent_start;
                    } else {
                        break;
                    }
                }
            }
        } else if line.contains("\"kind\":\"loop.plan.completed\"") {
            stats.plan_completed.plans += 1;

            if let Some(ok) = read_field_u64(&line, "tasks_ok") {
                stats.plan_completed.tasks_ok += ok;
            }
            if let Some(fail) = read_field_u64(&line, "tasks_failed") {
                stats.plan_completed.tasks_failed += fail;
            }
            if let Some(skip) = read_field_u64(&line, "tasks_skipped") {
                stats.plan_completed.tasks_skipped += skip;
            }
            if let Some(dur) = read_field_u64(&line, "duration_ms") {
                stats.plan_completed.avg_duration_ms += dur;
            }
            if has_bool_false(&line, "delivery_ready") {
                stats.plan_completed.delivery_not_ready += 1;
            }
        }
    }

    let total_turns = stats.turn_review.turns;
    stats.turn_review.delivery_not_ready_pct = if total_turns > 0 {
        stats.turn_review.delivery_not_ready as f64 / total_turns as f64 * 100.0
    } else {
        0.0
    };
    stats.turn_review.intent_categories = distribution(&cat_counts, total_turns);
    stats.turn_review.intent_sources = distribution(&src_counts, total_turns);
    stats.turn_review.intent_llm_outcomes = distribution(&outcome_counts, total_turns);

    llm_latencies.sort_unstable();
    stats.turn_review.intent_llm_ms_p50 = percentile(&llm_latencies, 0.5);
    stats.turn_review.intent_llm_ms_p95 = percentile(&llm_latencies, 0.95);

    let total_events = stats.delegate_gates.events;
    stats.delegate_gates.batched_pct = if total_events > 0 {
        stats.delegate_gates.batched as f64 / total_events as f64 * 100.0
    } else {
        0.0
    };
    stats.delegate_gates.depth_distribution = distribution(&depth_counts, total_events);
    stats.delegate_gates.target_distribution = distribution(&target_counts, total_events);

    if stats.plan_completed.plans > 0 {
        stats.plan_completed.avg_duration_ms /= stats.plan_completed.plans as u64;
        stats.plan_completed.delivery_not_ready_pct =
            stats.plan_completed.delivery_not_ready as f64 / stats.plan_completed.plans as f64
                * 100.0;
    }

    Ok(stats)
}

impl std::fmt::Display for OrchestrationStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tr = &self.turn_review;
        writeln!(
            f,
            "[turn.review] turns={}  delivery_not_ready={}({:.0}%)  plan_vetoed={}  feedback_rounds_total={}",
            tr.turns,
            tr.delivery_not_ready,
            tr.delivery_not_ready_pct,
            tr.plan_vetoed,
            tr.feedback_rounds_total,
        )?;
        for d in &tr.intent_categories {
            writeln!(f, "  intent {:<14} {:>4} ({:.0}%)", d.name, d.count, d.pct)?;
        }
        for d in &tr.intent_sources {
            writeln!(f, "  source {:<14} {:>4}", d.name, d.count)?;
        }
        for d in &tr.intent_llm_outcomes {
            writeln!(f, "  llm_outcome {:<14} {:>4}", d.name, d.count)?;
        }
        if !tr.intent_llm_outcomes.is_empty() {
            writeln!(
                f,
                "  intent_llm_ms p50={}  p95={}",
                tr.intent_llm_ms_p50, tr.intent_llm_ms_p95
            )?;
        }

        writeln!(f)?;

        let gw = &self.gateway;
        writeln!(
            f,
            "[gateway] turns_started={}  turns_completed={}",
            gw.turns_started, gw.turns_completed
        )?;

        writeln!(f)?;

        let dg = &self.delegate_gates;
        writeln!(
            f,
            "[delegate.gates] events={}  batched(width>1)={}({:.0}%)",
            dg.events, dg.batched, dg.batched_pct,
        )?;
        writeln!(
            f,
            "  blocked: independent={} unbounded={} consumer={} not_worth={}",
            dg.blocked_independent, dg.blocked_unbounded, dg.blocked_consumer, dg.blocked_not_worth,
        )?;
        for d in &dg.depth_distribution {
            writeln!(f, "  depth {}: {}", d.name, d.count)?;
        }
        for d in &dg.target_distribution {
            writeln!(f, "  target {:<12} {}", d.name, d.count)?;
        }

        writeln!(f)?;

        let pc = &self.plan_completed;
        writeln!(
            f,
            "[plan.completed] plans={}  tasks ok={} failed={} skipped={}  avg_duration={}ms  not_ready={}({:.0}%)",
            pc.plans,
            pc.tasks_ok,
            pc.tasks_failed,
            pc.tasks_skipped,
            pc.avg_duration_ms,
            pc.delivery_not_ready,
            pc.delivery_not_ready_pct,
        )?;

        Ok(())
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

    #[test]
    fn empty_dir_produces_zeroed_stats() {
        let dir = tmpdir();
        let stats = aggregate_orchestration(dir.path()).unwrap();
        assert_eq!(stats.turn_review.turns, 0);
        assert_eq!(stats.gateway.turns_started, 0);
        assert_eq!(stats.delegate_gates.events, 0);
        assert_eq!(stats.plan_completed.plans, 0);
    }

    #[test]
    fn missing_file_is_not_error() {
        let dir = tmpdir();
        let stats = aggregate_orchestration(dir.path()).unwrap();
        assert_eq!(stats.turn_review.turns, 0);
    }

    #[test]
    fn turn_review_basic_counts() {
        let dir = tmpdir();
        write_audit(
            dir.path(),
            &[
                r#"{"kind":"loop.turn.review","intent_category":"Query","intent_source":"Llm","delivery_ready":true,"plan_approved":true,"feedback_rounds":0,"intent_llm_outcome":"ok","intent_llm_ms":100}"#,
                r#"{"kind":"loop.turn.review","intent_category":"Action","intent_source":"Keyword","delivery_ready":false,"plan_approved":false,"feedback_rounds":1,"intent_llm_outcome":"timeout","intent_llm_ms":500}"#,
                r#"{"kind":"loop.turn.review","intent_category":"Query","intent_source":"Llm","delivery_ready":true,"plan_approved":true,"feedback_rounds":0,"intent_llm_outcome":"ok","intent_llm_ms":200}"#,
            ],
        );
        let stats = aggregate_orchestration(dir.path()).unwrap();
        assert_eq!(stats.turn_review.turns, 3);
        assert_eq!(stats.turn_review.delivery_not_ready, 1);
        assert!((stats.turn_review.delivery_not_ready_pct - 33.3).abs() < 0.1);
        assert_eq!(stats.turn_review.plan_vetoed, 1);
        assert_eq!(stats.turn_review.feedback_rounds_total, 1);

        assert_eq!(stats.turn_review.intent_categories.len(), 2);
        let query = stats
            .turn_review
            .intent_categories
            .iter()
            .find(|d| d.name == "Query")
            .unwrap();
        assert_eq!(query.count, 2);

        assert_eq!(stats.turn_review.intent_llm_outcomes.len(), 2);
        let ok = stats
            .turn_review
            .intent_llm_outcomes
            .iter()
            .find(|d| d.name == "ok")
            .unwrap();
        assert_eq!(ok.count, 2);

        assert_eq!(stats.turn_review.intent_llm_ms_p50, 200);
        assert_eq!(stats.turn_review.intent_llm_ms_p95, 500);
    }

    #[test]
    fn gateway_counts() {
        let dir = tmpdir();
        write_audit(
            dir.path(),
            &[
                r#"{"kind":"gateway.turn.started","turnId":"t1","sessionId":"s1"}"#,
                r#"{"kind":"gateway.turn.completed","turnId":"t1","sessionId":"s1","outcome":"completed"}"#,
                r#"{"kind":"gateway.turn.started","turnId":"t2","sessionId":"s2"}"#,
            ],
        );
        let stats = aggregate_orchestration(dir.path()).unwrap();
        assert_eq!(stats.gateway.turns_started, 2);
        assert_eq!(stats.gateway.turns_completed, 1);
    }

    #[test]
    fn delegate_gates_counts() {
        let dir = tmpdir();
        write_audit(
            dir.path(),
            &[
                r#"{"kind":"loop.delegate.gates","parent":"Sisyphus","depth":1,"batch_width":2,"gates":[{"agent":"Hephaestus","independent":true,"consumer_decision":true,"bounded":true,"worth_it":true},{"agent":"Explore","independent":false,"consumer_decision":true,"bounded":true,"worth_it":true}]}"#,
                r#"{"kind":"loop.delegate.gates","parent":"Sisyphus","depth":1,"batch_width":1,"gates":[{"agent":"Hephaestus","independent":true,"consumer_decision":true,"bounded":false,"worth_it":false}]}"#,
            ],
        );
        let stats = aggregate_orchestration(dir.path()).unwrap();
        assert_eq!(stats.delegate_gates.events, 2);
        assert_eq!(stats.delegate_gates.batched, 1);
        assert!((stats.delegate_gates.batched_pct - 50.0).abs() < 0.1);
        assert_eq!(stats.delegate_gates.blocked_independent, 1);
        assert_eq!(stats.delegate_gates.blocked_unbounded, 1);
        assert_eq!(stats.delegate_gates.blocked_not_worth, 1);
        assert_eq!(stats.delegate_gates.blocked_consumer, 0);

        assert_eq!(stats.delegate_gates.target_distribution.len(), 2);
        let heph = stats
            .delegate_gates
            .target_distribution
            .iter()
            .find(|d| d.name == "Hephaestus")
            .unwrap();
        assert_eq!(heph.count, 2);
    }

    #[test]
    fn plan_completed_counts() {
        let dir = tmpdir();
        write_audit(
            dir.path(),
            &[
                r#"{"kind":"loop.plan.completed","plan":"research","tasks_total":3,"tasks_ok":3,"tasks_failed":0,"tasks_skipped":0,"delivery_ready":true,"duration_ms":6000}"#,
                r#"{"kind":"loop.plan.completed","plan":"refactor","tasks_total":2,"tasks_ok":1,"tasks_failed":1,"tasks_skipped":0,"delivery_ready":false,"duration_ms":4000}"#,
            ],
        );
        let stats = aggregate_orchestration(dir.path()).unwrap();
        assert_eq!(stats.plan_completed.plans, 2);
        assert_eq!(stats.plan_completed.tasks_ok, 4);
        assert_eq!(stats.plan_completed.tasks_failed, 1);
        assert_eq!(stats.plan_completed.tasks_skipped, 0);
        assert_eq!(stats.plan_completed.avg_duration_ms, 5000);
        assert_eq!(stats.plan_completed.delivery_not_ready, 1);
        assert!((stats.plan_completed.delivery_not_ready_pct - 50.0).abs() < 0.1);
    }

    #[test]
    fn corrupt_lines_are_skipped() {
        let dir = tmpdir();
        write_audit(
            dir.path(),
            &[
                "not json at all",
                r#"{"kind":"loop.turn.review","intent_category":"Query","intent_source":"Llm","delivery_ready":true,"plan_approved":true,"feedback_rounds":0,"intent_llm_outcome":"ok","intent_llm_ms":50}"#,
                "}}}}invalid{{{",
            ],
        );
        let stats = aggregate_orchestration(dir.path()).unwrap();
        assert_eq!(stats.turn_review.turns, 1);
    }

    #[test]
    fn unknown_kinds_are_ignored() {
        let dir = tmpdir();
        write_audit(
            dir.path(),
            &[
                r#"{"kind":"unknown.event","payload":"ignored"}"#,
                r#"{"kind":"loop.turn.review","intent_category":"System","intent_source":"Keyword","delivery_ready":true,"plan_approved":true,"feedback_rounds":0,"intent_llm_outcome":"ok","intent_llm_ms":30}"#,
            ],
        );
        let stats = aggregate_orchestration(dir.path()).unwrap();
        assert_eq!(stats.turn_review.turns, 1);
        assert_eq!(stats.gateway.turns_started, 0);
    }

    #[test]
    fn display_matches_script_format() {
        let dir = tmpdir();
        write_audit(
            dir.path(),
            &[
                r#"{"kind":"loop.turn.review","intent_category":"Query","intent_source":"Llm","delivery_ready":true,"plan_approved":true,"feedback_rounds":0,"intent_llm_outcome":"ok","intent_llm_ms":100}"#,
                r#"{"kind":"gateway.turn.started","turnId":"t1","sessionId":"s1"}"#,
                r#"{"kind":"gateway.turn.completed","turnId":"t1","sessionId":"s1","outcome":"completed"}"#,
                r#"{"kind":"loop.delegate.gates","parent":"Sisyphus","depth":1,"batch_width":1,"gates":[{"agent":"Hephaestus","independent":true,"consumer_decision":true,"bounded":true,"worth_it":true}]}"#,
                r#"{"kind":"loop.plan.completed","plan":"test","tasks_total":1,"tasks_ok":1,"tasks_failed":0,"tasks_skipped":0,"delivery_ready":true,"duration_ms":1000}"#,
            ],
        );
        let stats = aggregate_orchestration(dir.path()).unwrap();
        let display = stats.to_string();
        assert!(display.contains("[turn.review]"));
        assert!(display.contains("[gateway]"));
        assert!(display.contains("[delegate.gates]"));
        assert!(display.contains("[plan.completed]"));
    }
}
