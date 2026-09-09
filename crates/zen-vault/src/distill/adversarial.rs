//! GAN-style adversarial arena for distill capabilities (PD-04 addendum).
//!
//! Same testcase corpus, multiple contestants, mechanical judge. The
//! incumbent [`ZenDistill`] (current `correlate` + `generate_from_gaps`)
//! fights [`NaiveBaseline`] (same structure minus normalization and
//! dedup) and any external agent CLI via [`CliContestant`]. Cases the
//! incumbent loses become [`HypothesisSlug`]s (kind `LlmFailure`, status
//! `Exploring`) saved to the hypotheses dir — the 4am promotion worker
//! stages them, closing the self-improve loop.
//!
//! Judge metrics are fully mechanical (no LLM): determinism, coverage
//! (v2: correlate groups covered, so dedup is rewarded),
//! dedup/economy, isolation, parsimony, validity. Ties keep the incumbent.
//!
//! External protocol ([`CliContestant`]): the program receives
//! `{"case_id": "...", "gaps": [...]}` on stdin and must print
//! `{"opportunities": [...], "hypotheses": [...]}` (same schemas as
//! [`Opportunity`] / [`HypothesisSlug`]) on stdout within the timeout.
//! Example: `zen discover arena --external codex:"codex exec --skip-git-repo-check"`.
//! Live external runs are manual (auth/cost/latency); tests use fakes.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::DateTime;
use serde::{Deserialize, Serialize};

use super::correlation::{Opportunity, correlate};
use super::hypothesis::generate_from_gaps;
use super::types::{GapKind, GapRecord, HypothesisSlug, HypothesisStatus};

/// Name of the incumbent contestant. Ties keep this title.
pub const INCUMBENT: &str = "zen-distill";

/// Arena report filename: `logs/adversarial-<cycle>.json`.
pub const ARENA_REPORT_PREFIX: &str = "adversarial-";

/// One fixed testcase: gaps plus ground-truth ranges.
#[derive(Debug, Clone)]
pub struct EvalCase {
    /// Stable case id (also used in loss-hypothesis slugs).
    pub id: &'static str,
    /// What capability this case probes.
    pub description: &'static str,
    /// Input gaps (fixed ids for run-to-run determinism).
    pub gaps: Vec<GapRecord>,
    /// Expected opportunity count range for `correlate`.
    pub expected_opps: (usize, usize),
    /// Expected hypothesis count range for `hypothesize`.
    pub expected_slugs: (usize, usize),
}

/// A distill implementation under test.
pub trait Contestant {
    /// Stable contestant name used in reports.
    fn name(&self) -> String;
    /// Cluster gaps into opportunities.
    fn correlate(&self, gaps: &[GapRecord]) -> Vec<Opportunity>;
    /// Draft hypotheses from gaps.
    fn hypothesize(&self, gaps: &[GapRecord]) -> Vec<HypothesisSlug>;
}

/// The current zen-vault implementation (title holder).
pub struct ZenDistill;

impl Contestant for ZenDistill {
    fn name(&self) -> String {
        INCUMBENT.to_string()
    }

    fn correlate(&self, gaps: &[GapRecord]) -> Vec<Opportunity> {
        correlate(gaps)
    }

    fn hypothesize(&self, gaps: &[GapRecord]) -> Vec<HypothesisSlug> {
        generate_from_gaps(gaps)
    }
}

/// Ablation baseline: same fallback structure, minus normalization and
/// dedup, fixed 0.5 confidence, no exploration prompts. Expected to lose
/// wherever normalization/dedup/prompts matter.
pub struct NaiveBaseline;

impl NaiveBaseline {
    fn key_for(gap: &GapRecord) -> String {
        if let Some(entity) = gap.subject_entity.as_deref() {
            return format!("raw:{entity}");
        }
        if let Some(path) = gap.subject_path.as_deref() {
            return format!("path:{path}");
        }
        format!("id:{}", gap.id)
    }
}

impl Contestant for NaiveBaseline {
    fn name(&self) -> String {
        "naive-baseline".to_string()
    }

    fn correlate(&self, gaps: &[GapRecord]) -> Vec<Opportunity> {
        let mut groups: HashMap<String, Vec<GapRecord>> = HashMap::new();
        for gap in gaps {
            groups
                .entry(Self::key_for(gap))
                .or_default()
                .push(gap.clone());
        }
        let mut keys: Vec<String> = groups.keys().cloned().collect();
        keys.sort();
        keys.into_iter()
            .map(|key| {
                let member_gaps = groups.remove(&key).unwrap_or_default();
                Opportunity {
                    id: format!("naive-opp-{}", key.replace([':', ' ', '/'], "-")),
                    entity_key: key.clone(),
                    score: member_gaps.len() as f64,
                    reason: format!("naive group of {}", member_gaps.len()),
                    member_gaps,
                }
            })
            .collect()
    }

    fn hypothesize(&self, gaps: &[GapRecord]) -> Vec<HypothesisSlug> {
        gaps.iter()
            .map(|gap| HypothesisSlug {
                slug: format!("naive-{}", Self::key_for(gap).replace([':', ' ', '/'], "-")),
                hypothesis: format!("naive guess: {}", gap.detail),
                gap_kind: gap.kind,
                confidence: 0.5,
                status: HypothesisStatus::Hypothesis,
                exploration_prompt: None,
                evidence_refs: Vec::new(),
                created_from: gap.id.clone(),
            })
            .collect()
    }
}

/// External agent CLI following the stdin/stdout JSON protocol.
pub struct CliContestant {
    /// Report name (e.g. `codex`, `hermes`).
    pub name: String,
    /// Program to execute (e.g. `codex`).
    pub program: String,
    /// Extra argv (e.g. `["exec"]`).
    pub args: Vec<String>,
    /// Kill the child after this many seconds.
    pub timeout_secs: u64,
}

#[derive(Serialize)]
struct ArenaRequest<'a> {
    case_id: &'a str,
    gaps: &'a [GapRecord],
}

#[derive(Deserialize)]
struct ArenaResponse {
    opportunities: Vec<Opportunity>,
    hypotheses: Vec<HypothesisSlug>,
}

impl Contestant for CliContestant {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn correlate(&self, gaps: &[GapRecord]) -> Vec<Opportunity> {
        self.round_trip("probe", gaps)
            .map(|r| r.opportunities)
            .unwrap_or_default()
    }

    fn hypothesize(&self, gaps: &[GapRecord]) -> Vec<HypothesisSlug> {
        self.round_trip("probe", gaps)
            .map(|r| r.hypotheses)
            .unwrap_or_default()
    }
}

impl CliContestant {
    fn round_trip(&self, case_id: &str, gaps: &[GapRecord]) -> Result<ArenaResponse> {
        let input = serde_json::to_string(&ArenaRequest { case_id, gaps })?;
        let mut child = Command::new(&self.program)
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("spawn external contestant {}", self.name))?;
        child
            .stdin
            .take()
            .context("open contestant stdin")?
            .write_all(input.as_bytes())?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let out = child.wait_with_output();
            let _ = tx.send(out);
        });
        let output = rx
            .recv_timeout(Duration::from_secs(self.timeout_secs))
            .context("external contestant timed out")??;
        if !output.status.success() {
            anyhow::bail!("external contestant {} exited non-zero", self.name);
        }
        Ok(serde_json::from_slice(&output.stdout)?)
    }
}

/// Per-phase mechanical scores, each 0.0..=1.0.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PhaseScore {
    /// Two runs serialize identically (corpus ids are fixed).
    pub determinism: f64,
    /// Correlate: input gaps referenced by outputs. Hypothesize (v2):
    /// correlate groups with at least one hypothesis.
    pub coverage: f64,
    /// Correlate-only: bare gaps (no entity/path) stand alone.
    pub isolation: f64,
    /// Correlate-only: output count inside the case ground-truth range.
    pub parsimony: f64,
    /// Hypothesize-only: unique slugs over total.
    pub dedup: f64,
    /// Hypothesize-only: output count inside the case ground-truth range.
    pub economy: f64,
    /// Hypothesize-only: non-blank slug plus exploration prompt present.
    pub validity: f64,
}

impl PhaseScore {
    fn mean(fields: &[f64]) -> f64 {
        fields.iter().sum::<f64>() / fields.len().max(1) as f64
    }

    /// Mean of the correlate-applicable metrics.
    pub fn correlate_total(&self) -> f64 {
        Self::mean(&[
            self.determinism,
            self.coverage,
            self.isolation,
            self.parsimony,
        ])
    }

    /// Mean of the hypothesize-applicable metrics.
    pub fn hypothesize_total(&self) -> f64 {
        Self::mean(&[
            self.determinism,
            self.coverage,
            self.dedup,
            self.economy,
            self.validity,
        ])
    }
}

/// Judge verdict for one contestant on one case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContestantVerdict {
    /// Contestant name.
    pub contestant: String,
    /// Correlate-phase scores.
    pub correlate: PhaseScore,
    /// Hypothesize-phase scores.
    pub hypothesize: PhaseScore,
    /// correlate_total + hypothesize_total.
    pub total: f64,
}

/// Judge verdict for one case across contestants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseResult {
    /// EvalCase id.
    pub case_id: String,
    /// Winner name (ties keep [`INCUMBENT`]).
    pub winner: String,
    /// Per-contestant verdicts.
    pub verdicts: Vec<ContestantVerdict>,
}

/// Full arena outcome, persisted as `logs/adversarial-<cycle>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdversarialReport {
    /// Cycle id (caller-provided, e.g. date).
    pub cycle_id: String,
    /// Per-case results.
    pub cases: Vec<CaseResult>,
    /// Cases won by [`INCUMBENT`].
    pub zen_wins: usize,
    /// Total cases run.
    pub total_cases: usize,
}

fn in_range(value: usize, range: (usize, usize)) -> f64 {
    if value >= range.0 && value <= range.1 {
        return 1.0;
    }
    let dist = if value < range.0 {
        range.0 - value
    } else {
        value - range.1
    };
    1.0 / (1.0 + dist as f64)
}

fn gap_identities(gaps: &[GapRecord]) -> HashSet<String> {
    gaps.iter().map(|gap| gap.id.clone()).collect()
}

/// Serialize-compare two outputs; serialization failure counts as mismatch.
fn same_output<T: Serialize>(first: &T, second: &T) -> bool {
    serde_json::to_string(first).ok() == serde_json::to_string(second).ok()
}

fn referenced_ids(opps: &[Opportunity]) -> HashSet<String> {
    opps.iter()
        .flat_map(|opp| opp.member_gaps.iter().map(|gap| gap.id.clone()))
        .collect()
}

fn judge_correlate(contestant: &dyn Contestant, case: &EvalCase) -> (PhaseScore, Vec<Opportunity>) {
    let first = contestant.correlate(&case.gaps);
    let second = contestant.correlate(&case.gaps);
    let determinism = f64::from(same_output(&first, &second));
    let wanted = gap_identities(&case.gaps);
    let got = referenced_ids(&first);
    let coverage = if wanted.is_empty() {
        1.0
    } else {
        got.intersection(&wanted).count() as f64 / wanted.len() as f64
    };
    let bare_ids: HashSet<String> = case
        .gaps
        .iter()
        .filter(|gap| gap.subject_entity.is_none() && gap.subject_path.is_none())
        .map(|gap| gap.id.clone())
        .collect();
    let mut violations = 0;
    for opp in &first {
        let bare_inside = opp
            .member_gaps
            .iter()
            .filter(|gap| bare_ids.contains(&gap.id))
            .count();
        if bare_inside > 0 && opp.member_gaps.len() > bare_inside {
            violations += 1;
        }
        if bare_inside > 1 {
            let distinct: HashSet<&str> =
                opp.member_gaps.iter().map(|gap| gap.id.as_str()).collect();
            if distinct.len() > 1 {
                violations += 1;
            }
        }
    }
    let isolation = 1.0 / (1.0 + violations as f64);
    let parsimony = in_range(first.len(), case.expected_opps);
    (
        PhaseScore {
            determinism,
            coverage,
            isolation,
            parsimony,
            ..Default::default()
        },
        first,
    )
}

fn judge_hypothesize(
    contestant: &dyn Contestant,
    case: &EvalCase,
    groups: &[HashSet<String>],
) -> (PhaseScore, Vec<HypothesisSlug>) {
    let first = contestant.hypothesize(&case.gaps);
    let second = contestant.hypothesize(&case.gaps);
    let determinism = f64::from(same_output(&first, &second));
    // v2: coverage counts correlate groups (not raw gaps) with at least one
    // hypothesis, so deduped outputs are rewarded instead of punished.
    let got: HashSet<String> = first.iter().map(|slug| slug.created_from.clone()).collect();
    let coverage = if groups.is_empty() {
        f64::from(first.is_empty())
    } else {
        groups
            .iter()
            .filter(|group| group.iter().any(|id| got.contains(id)))
            .count() as f64
            / groups.len() as f64
    };
    let unique: HashSet<&str> = first.iter().map(|slug| slug.slug.as_str()).collect();
    let dedup = if first.is_empty() {
        1.0
    } else {
        unique.len() as f64 / first.len() as f64
    };
    let economy = in_range(first.len(), case.expected_slugs);
    let valid = first
        .iter()
        .filter(|slug| !slug.slug.trim().is_empty() && slug.exploration_prompt.is_some())
        .count();
    let validity = if first.is_empty() {
        1.0
    } else {
        valid as f64 / first.len() as f64
    };
    (
        PhaseScore {
            determinism,
            coverage,
            dedup,
            economy,
            validity,
            ..Default::default()
        },
        first,
    )
}

/// Judge every contestant on one case. Ties keep [`INCUMBENT`].
pub fn judge_case(contestants: &[&dyn Contestant], case: &EvalCase) -> CaseResult {
    let mut verdicts = Vec::with_capacity(contestants.len());
    for contestant in contestants {
        let (correlate, opps) = judge_correlate(*contestant, case);
        let groups: Vec<HashSet<String>> = opps
            .iter()
            .map(|opp| opp.member_gaps.iter().map(|gap| gap.id.clone()).collect())
            .collect();
        let (hypothesize, _) = judge_hypothesize(*contestant, case, &groups);
        let total = correlate.correlate_total() + hypothesize.hypothesize_total();
        verdicts.push(ContestantVerdict {
            contestant: contestant.name(),
            correlate,
            hypothesize,
            total,
        });
    }
    let mut winner = INCUMBENT.to_string();
    let mut best = verdicts
        .iter()
        .find(|verdict| verdict.contestant == INCUMBENT)
        .map(|verdict| verdict.total)
        .unwrap_or(f64::MIN);
    for verdict in &verdicts {
        if verdict.contestant != INCUMBENT && verdict.total > best {
            best = verdict.total;
            winner = verdict.contestant.clone();
        }
    }
    CaseResult {
        case_id: case.id.to_string(),
        winner,
        verdicts,
    }
}

/// Fixed corpus gap with deterministic id and timestamp.
fn gap(kind: GapKind, id: &str, detail: &str) -> GapRecord {
    let mut gap = GapRecord::new(kind, "arena-corpus", detail);
    gap.id = format!("arena-gap-{id}");
    gap.detected_at =
        DateTime::from_timestamp(1_786_000_000, 0).expect("arena corpus timestamp valid");
    gap
}

/// The shared testcase corpus every contestant faces.
pub fn corpus() -> Vec<EvalCase> {
    vec![
        EvalCase {
            id: "alias-collapse",
            description: "case-variant entity spellings must collapse to one cluster",
            gaps: vec![
                gap(GapKind::OrphanEntity, "ac1", "orphan: Cache").with_entity("Cache"),
                gap(GapKind::OrphanEntity, "ac2", "orphan: cache").with_entity("cache"),
                gap(GapKind::OrphanEntity, "ac3", "orphan: CACHE").with_entity("CACHE"),
                gap(GapKind::OrphanEntity, "ac4", "orphan: Router").with_entity("Router"),
            ],
            expected_opps: (2, 2),
            expected_slugs: (2, 2),
        },
        EvalCase {
            id: "bare-singletons",
            description: "gaps with neither entity nor path must never merge",
            gaps: vec![
                gap(GapKind::LlmFailure, "bs1", "llm down"),
                gap(GapKind::LlmFailure, "bs2", "llm down again"),
                gap(GapKind::OrphanEntity, "bs3", "orphan: Cache").with_entity("Cache"),
            ],
            expected_opps: (3, 3),
            expected_slugs: (2, 3),
        },
        EvalCase {
            id: "mixed-domains",
            description: "graph, judgment, and process gaps stay separated",
            gaps: vec![
                gap(GapKind::OrphanEntity, "md1", "orphan: Cache").with_entity("Cache"),
                gap(GapKind::DecisionBlocked, "md2", "decision blocked: launch")
                    .with_entity("Launch"),
                gap(GapKind::IngestNeverConsolidated, "md3", "stale: inbox/n.md")
                    .with_path("vault/inbox/n.md"),
                gap(GapKind::CommitmentOverdue, "md4", "overdue: habit-run")
                    .with_entity("habit-run"),
            ],
            expected_opps: (4, 4),
            expected_slugs: (1, 4),
        },
        EvalCase {
            id: "empty",
            description: "empty input yields empty output without panic",
            gaps: Vec::new(),
            expected_opps: (0, 0),
            expected_slugs: (0, 0),
        },
        EvalCase {
            id: "stale-ingest-cluster",
            description: "same-path stale ingests cluster into one opportunity",
            gaps: vec![
                gap(GapKind::IngestNeverConsolidated, "si1", "stale: raw/x.md")
                    .with_path("vault/raw/x.md"),
                gap(
                    GapKind::IngestNeverConsolidated,
                    "si2",
                    "stale: raw/x.md again",
                )
                .with_path("vault/raw/x.md"),
                gap(
                    GapKind::IngestNeverConsolidated,
                    "si3",
                    "stale: raw/x.md third",
                )
                .with_path("vault/raw/x.md"),
            ],
            expected_opps: (1, 1),
            expected_slugs: (1, 3),
        },
    ]
}

/// Run the full corpus across contestants, persist the report, and judge
/// mechanically. Pure regression gate (PD-06): losses are recorded in the
/// report only — nothing is staged back into the hypothesis pipeline.
pub fn run_arena(
    contestants: &[&dyn Contestant],
    logs_dir: &Path,
    cycle_id: &str,
) -> Result<AdversarialReport> {
    let mut cases = Vec::new();
    for case in corpus() {
        cases.push(judge_case(contestants, &case));
    }
    let zen_wins = cases
        .iter()
        .filter(|result| result.winner == INCUMBENT)
        .count();
    let report = AdversarialReport {
        cycle_id: cycle_id.to_string(),
        zen_wins,
        total_cases: cases.len(),
        cases,
    };
    fs::create_dir_all(logs_dir)
        .with_context(|| format!("create logs dir: {}", logs_dir.display()))?;
    fs::write(
        logs_dir.join(format!("{ARENA_REPORT_PREFIX}{cycle_id}.json")),
        serde_json::to_string_pretty(&report)?,
    )
    .with_context(|| format!("write adversarial report {cycle_id}"))?;

    tracing::info!(
        cycle = %cycle_id,
        zen_wins,
        total = report.total_cases,
        "arena regression gate complete"
    );
    Ok(report)
}

/// Write path for an arena report (mirrors [`ARENA_REPORT_PREFIX`]).
pub fn report_path(logs_dir: &Path, cycle_id: &str) -> PathBuf {
    logs_dir.join(format!("{ARENA_REPORT_PREFIX}{cycle_id}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn contestants<'a>() -> Vec<&'a dyn Contestant> {
        vec![&ZenDistill, &NaiveBaseline]
    }

    fn zen_total(result: &CaseResult) -> f64 {
        result
            .verdicts
            .iter()
            .find(|v| v.contestant == INCUMBENT)
            .map(|v| v.total)
            .unwrap_or(-1.0)
    }

    #[test]
    fn corpus_cases_have_fixed_ids() {
        for case in corpus() {
            for gap in &case.gaps {
                assert!(gap.id.starts_with("arena-gap-"), "volatile id: {}", gap.id);
            }
        }
    }

    #[test]
    fn zen_is_deterministic_across_runs() {
        let zen = ZenDistill;
        for case in corpus() {
            let a = serde_json::to_string(&zen.correlate(&case.gaps)).unwrap();
            let b = serde_json::to_string(&zen.correlate(&case.gaps)).unwrap();
            assert_eq!(a, b, "nondeterministic correlate on {}", case.id);
            let c = serde_json::to_string(&zen.hypothesize(&case.gaps)).unwrap();
            let d = serde_json::to_string(&zen.hypothesize(&case.gaps)).unwrap();
            assert_eq!(c, d, "nondeterministic hypothesize on {}", case.id);
        }
    }

    #[test]
    fn zen_beats_naive_on_alias_collapse() {
        let case = corpus()
            .into_iter()
            .find(|c| c.id == "alias-collapse")
            .unwrap();
        let result = judge_case(&[&ZenDistill, &NaiveBaseline], &case);
        assert_eq!(result.winner, INCUMBENT);
        let naive: f64 = result
            .verdicts
            .iter()
            .find(|v| v.contestant == "naive-baseline")
            .map(|v| v.total)
            .unwrap();
        assert!(zen_total(&result) > naive);
    }

    #[test]
    fn bare_singletons_documents_filter_policy() {
        let case = corpus()
            .into_iter()
            .find(|c| c.id == "bare-singletons")
            .unwrap();
        let result = judge_case(&[&ZenDistill, &NaiveBaseline], &case);
        let verdict = |name: &str| {
            result
                .verdicts
                .iter()
                .find(|v| v.contestant == name)
                .unwrap()
                .clone()
        };
        let zen = verdict(INCUMBENT);
        let naive = verdict("naive-baseline");
        // Bare gaps always stand alone under zen (uuid-v7 id fallback).
        assert_eq!(zen.correlate.isolation, 1.0);
        // Zen filters ineligible kinds (LlmFailure) out of hypothesize;
        // naive emits for every gap. Whether filtering is correct is an
        // RSI question — the arena stages the divergence either way.
        assert!(zen.hypothesize.coverage < 1.0);
        assert_eq!(naive.hypothesize.coverage, 1.0);
        assert_eq!(result.verdicts.len(), 2);
    }

    #[test]
    fn v2_coverage_rewards_dedup() {
        // alias-collapse: zen emits 2 slugs for 2 groups (deduped from 4
        // gaps) — v2 coverage counts groups, so dedup scores 1.0.
        let case = corpus()
            .into_iter()
            .find(|c| c.id == "alias-collapse")
            .unwrap();
        let result = judge_case(&[&ZenDistill, &NaiveBaseline], &case);
        let zen = result
            .verdicts
            .iter()
            .find(|v| v.contestant == INCUMBENT)
            .unwrap();
        assert_eq!(zen.hypothesize.coverage, 1.0);
    }

    #[test]
    fn run_arena_persists_report() {
        let dir = TempDir::new().unwrap();
        let logs = dir.path().join("logs");
        let report = run_arena(&contestants(), &logs, "test-cycle").unwrap();
        assert_eq!(report.total_cases, corpus().len());
        assert!(report_path(&logs, "test-cycle").is_file());
    }

    #[test]
    fn cli_contestant_runs_protocol_with_fake() {
        let contestant = CliContestant {
            name: "fake".to_string(),
            program: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                "cat >/dev/null; printf '{\"opportunities\":[],\"hypotheses\":[]}'".to_string(),
            ],
            timeout_secs: 10,
        };
        let gaps = vec![gap(GapKind::OrphanEntity, "f1", "orphan: Foo").with_entity("Foo")];
        assert!(contestant.correlate(&gaps).is_empty());
        assert!(contestant.hypothesize(&gaps).is_empty());
    }

    #[test]
    fn cli_contestant_times_out() {
        let contestant = CliContestant {
            name: "slow".to_string(),
            program: "/bin/sleep".to_string(),
            args: vec!["30".to_string()],
            timeout_secs: 1,
        };
        // correlate swallows transport errors into empty output.
        assert!(contestant.correlate(&[]).is_empty());
    }
}
