//! Vendor System-1 model evaluation gate (T174): measure, never assume.
//!
//! The L1 calibrated decision layer (V13 增补 A) must work with local models
//! alone, and adopting an external "System-1" model vendor (Jev/TypeSafe)
//! without measuring it on zen's *own* decision workload would repeat the
//! uncalibrated-heuristic failure this codebase just removed. This module is
//! the gate: a runnable offline harness that evaluates **any** candidate
//! model endpoint supplied at run time (a local Ollama model today, the
//! vendor model when early access lands) against the three named decision
//! workloads — intent 4-way, correction binary, review verdict — on four
//! independently reportable axes:
//!
//! 1. **cardinality** — the workload's distinct decision heads vs the
//!    vendor's documented 255-choice cap ([`CARDINALITY_CAP`], D29).
//! 2. **latency under load** — p50/p95 per call + error rate, measured with
//!    bounded concurrency ([`DEFAULT_MAX_CONCURRENT`], the established
//!    `max_concurrent`-style batch pattern; never unbounded spawns).
//! 3. **calibration** — ECE over the **labeled** records of the candidate's
//!    outputs, reusing [`super::decision_audit::ece`] and the same
//!    "refuse to print a number without labels" discipline
//!    (`labels_required`).
//! 4. **local-first compatibility** — the candidate must run locally (no
//!    cloud egress); a hard requirement, not a preference (V13-A.3).
//!
//! # Honesty invariants (V13-A.3)
//!
//! * No preset commitment: the candidate is a run-time parameter
//!   (`provider` + `model`), never a hard-coded vendor name or SDK.
//! * No fabricated verdicts: an unreachable model, a missing model name, an
//!   empty dataset, or a workload without records/labels produces an explicit
//!   [`EvalState::NotEvaluated`] / [`EvalState::InsufficientData`] state —
//!   never a panic, never a silent `pass`.
//! * The aggregate verdict is `pass` only when every axis passes **and** data
//!   was sufficient for every workload.
//! * Latency and calibration are reported without invented thresholds: the
//!   only gated criteria are the documented ones (cardinality cap, local-first
//!   hard requirement, endpoint reachability, label presence). ECE is a
//!   measurement for human review — the τ thresholds T168-T171 must derive
//!   from labeled data, which is exactly why `labels_required` blocks a pass.
//!
//! # Data reuse
//!
//! The harness reuses the existing decision dataset
//! (`<logs>/decision-audit/dataset.jsonl` via
//! [`super::decision_audit::load_dataset`]) — it does not invent a new one.
//! Prompts are derived from each [`DecisionRecord`]'s fields
//! (kind/rung/confidence/decision): the audit sink does not retain the
//! original turn inputs, so the candidate is asked to verify each recorded
//! decision against the workload's choice set. This limitation is surfaced in
//! the report's `note`, never hidden.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Semaphore;

use super::decision_audit::{DecisionAuditError, DecisionRecord, load_dataset};
use zen_core::config::ZenConfig;
use zen_provider::ProviderInstance;

/// The vendor's documented cardinality cap (D29: "基数上限 255").
pub const CARDINALITY_CAP: usize = 255;

/// Default bounded concurrency for the latency-under-load measurement
/// (matches the established `[agentic.delegate].max_concurrent` default).
pub const DEFAULT_MAX_CONCURRENT: usize = 4;

/// Upper clamp for [`DEFAULT_MAX_CONCURRENT`] (matches the delegate clamp).
pub const MAX_CONCURRENT_CLAMP: usize = 8;

/// Directory under `<logs>/` where evaluation reports are persisted.
pub const EVAL_DIR: &str = "vendor-eval";

/// Intent 4-way choice set (001 A.3 `IntentCategory`).
pub const INTENT_CHOICES: [&str; 4] = ["Query", "Action", "System", "Conversation"];

/// Correction binary choice set (is-user-correction).
pub const CORRECTION_CHOICES: [&str; 2] = ["yes", "no"];

/// Review verdict binary choice set (SemanticReviewer approve/veto).
pub const REVIEW_CHOICES: [&str; 2] = ["approve", "veto"];

/// Errors from vendor-eval I/O.
#[derive(Debug, Error)]
pub enum VendorEvalError {
    /// Filesystem error (missing files are NOT errors — see [`latest_report`]).
    #[error("vendor eval I/O error: {0}")]
    Io(#[from] io::Error),
    /// JSON serialization/deserialization error.
    #[error("vendor eval JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// Error from the shared decision-audit data store.
    #[error("decision audit error: {0}")]
    DecisionAudit(#[from] DecisionAuditError),
}

/// Top-level evaluation state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvalState {
    /// The evaluation ran and every workload was fully measured.
    Evaluated,
    /// The evaluation did not run (no model, unreachable candidate, empty
    /// dataset, provider not configured).
    NotEvaluated,
    /// The evaluation ran but some workload lacked records or labels.
    InsufficientData,
}

/// Per-workload evaluation state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkloadState {
    /// The workload had records and the candidate was called.
    Evaluated,
    /// The workload had no records in the decision dataset.
    InsufficientData,
}

/// Per-axis / per-workload verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AxisVerdict {
    /// The axis (or workload) satisfies its documented criteria.
    Pass,
    /// A documented criterion failed (cardinality over cap, cloud egress,
    /// unreachable endpoint).
    Fail,
    /// The axis could not be judged (no records, no labels) — never a pass.
    NotGated,
}

/// Aggregate verdict across all workloads and axes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AggregateVerdict {
    /// Every axis passes and data was sufficient for every workload.
    Pass,
    /// A documented criterion failed (cardinality over cap, cloud egress).
    Fail,
    /// The evaluation did not run.
    NotEvaluated,
    /// The evaluation ran but data was insufficient (missing records/labels).
    InsufficientData,
}

/// The three named decision workloads (T174).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkloadKind {
    /// Intent 4-way classification.
    Intent,
    /// Is-user-correction binary.
    Correction,
    /// Review verdict binary (approve/veto).
    Review,
}

impl WorkloadKind {
    /// The decision-audit kind string this workload selects on.
    pub fn as_str(&self) -> &'static str {
        match self {
            WorkloadKind::Intent => "intent",
            WorkloadKind::Correction => "correction",
            WorkloadKind::Review => "review",
        }
    }

    /// The candidate's allowed choice set for this workload.
    pub fn choices(&self) -> &'static [&'static str] {
        match self {
            WorkloadKind::Intent => &INTENT_CHOICES,
            WorkloadKind::Correction => &CORRECTION_CHOICES,
            WorkloadKind::Review => &REVIEW_CHOICES,
        }
    }

    /// All three workloads, in report order.
    pub fn all() -> [WorkloadKind; 3] {
        [
            WorkloadKind::Intent,
            WorkloadKind::Correction,
            WorkloadKind::Review,
        ]
    }
}

/// Cardinality axis: distinct decision heads vs the 255 cap.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CardinalityAxis {
    /// Distinct decision values observed in the workload's records.
    pub observed: usize,
    /// The documented cap ([`CARDINALITY_CAP`]).
    pub cap: usize,
    /// `observed <= cap`.
    pub fits: bool,
    /// The observed distinct choices.
    pub choices: Vec<String>,
}

/// Latency-under-load axis.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LatencyAxis {
    /// Total candidate calls attempted.
    pub calls: usize,
    /// Calls that errored at the endpoint.
    pub errors: usize,
    /// `errors / calls`.
    pub error_rate: f64,
    /// Median call latency in milliseconds (over successful calls).
    pub p50_ms: f64,
    /// 95th-percentile call latency in milliseconds.
    pub p95_ms: f64,
    /// The bounded concurrency used for the measurement.
    pub max_concurrent: usize,
}

/// Calibration axis over the candidate's outputs on labeled records.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CalibrationAxis {
    /// Labeled records in the workload.
    pub labeled: usize,
    /// Labeled records with a parseable, in-choice-set candidate output.
    pub usable: usize,
    /// True when no `(confidence, correct)` pair exists — ECE is `None` and
    /// the report MUST say "labels required" rather than print a number.
    pub labels_required: bool,
    /// ECE over the candidate's outputs; `None` when `labels_required`.
    pub ece: Option<f64>,
    /// Accuracy over usable labeled records.
    pub accuracy: Option<f64>,
    /// Mean candidate confidence over usable labeled records.
    pub mean_confidence: Option<f64>,
}

/// Local-first compatibility axis (candidate-level, hard requirement).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalFirstAxis {
    /// True when the candidate endpoint is loopback (no cloud egress).
    pub local: bool,
    /// Why: the base_url observed, or the absence of one.
    pub reason: String,
}

/// Per-workload evaluation result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkloadEval {
    /// Workload name (`"intent"` / `"correction"` / `"review"`).
    pub workload: String,
    /// Whether the workload had records and was measured.
    pub state: WorkloadState,
    /// Why the workload is insufficient (absent records), when applicable.
    pub reason: Option<String>,
    /// Cardinality axis (always present).
    pub cardinality: CardinalityAxis,
    /// Latency axis; `None` when the workload had no records.
    pub latency: Option<LatencyAxis>,
    /// Calibration axis; `None` when the workload had no records.
    pub calibration: Option<CalibrationAxis>,
    /// Per-workload verdict.
    pub verdict: AxisVerdict,
}

/// Full vendor-evaluation report (the `vendor_eval` section of
/// `zen discover report`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VendorEvalReport {
    /// Candidate model name (run-time parameter).
    pub model: String,
    /// Candidate provider name (run-time parameter).
    pub provider: String,
    /// Snapshot wall-clock time (RFC 3339).
    pub generated_at: String,
    /// Top-level evaluation state.
    pub state: EvalState,
    /// Local-first axis (candidate-level).
    pub local_first: LocalFirstAxis,
    /// Per-workload results.
    pub workloads: Vec<WorkloadEval>,
    /// Aggregate verdict.
    pub aggregate: AggregateVerdict,
    /// Human-readable note (not-evaluated reason, prompt-shape limitation).
    pub note: Option<String>,
}

/// One parsed candidate output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateOutput {
    /// The choice the candidate selected.
    pub choice: String,
    /// The candidate's confidence in `0.0..=1.0`.
    pub confidence: f64,
}

/// Abstraction over the candidate endpoint so tests can inject a fake.
#[async_trait]
pub trait CandidateCaller: Send + Sync {
    /// Send one decision prompt and return the raw text, or an error string.
    async fn call(&self, prompt: &str) -> Result<String, String>;
}

/// Production caller: routes through zen-provider's existing router
/// ([`ProviderInstance::complete`]) on a blocking thread — the established
/// `spawn_blocking` pattern that keeps the sync provider impls out of the
/// async context (the nested-`Runtime` panic hazard).
pub struct RouterCandidateCaller {
    instance: ProviderInstance,
}

impl RouterCandidateCaller {
    /// Wrap a provider instance (from `DefaultRouter::provider_instance`).
    pub fn new(instance: ProviderInstance) -> Self {
        Self { instance }
    }
}

#[async_trait]
impl CandidateCaller for RouterCandidateCaller {
    async fn call(&self, prompt: &str) -> Result<String, String> {
        let instance = self.instance.clone();
        let prompt = prompt.to_string();
        tokio::task::spawn_blocking(move || {
            instance.complete(&prompt, &zen_core::config::ModelOptions::default())
        })
        .await
        .map_err(|e| format!("candidate call task join failed: {e}"))?
        .map_err(|e| e.to_string())
    }
}

/// Full gate entry: load config + dataset, build the router, run every
/// workload, and return the report.
///
/// Every failure is fail-open and explicit: a missing model name, an empty
/// dataset, an unconfigured provider, or an unreachable candidate yields a
/// [`VendorEvalReport`] with [`EvalState::NotEvaluated`] and a readable
/// `note` — never a panic, never a silent `pass`.
pub async fn evaluate(
    config: &ZenConfig,
    dir: &Path,
    provider: &str,
    model: &str,
    max_concurrent: usize,
) -> Result<VendorEvalReport, VendorEvalError> {
    let max_concurrent = max_concurrent.clamp(1, MAX_CONCURRENT_CLAMP);

    if model.trim().is_empty() {
        return Ok(not_evaluated(
            model.to_string(),
            provider.to_string(),
            "no model name supplied".to_string(),
        ));
    }

    let records = load_dataset(dir)?;
    if records.is_empty() {
        return Ok(not_evaluated(
            model.to_string(),
            provider.to_string(),
            "decision dataset is empty — run `zen discover report` or a zen-loop cycle to populate logs/decision-audit/dataset.jsonl".to_string(),
        ));
    }

    let router = zen_provider::DefaultRouter::from_config_override(config, provider, model);
    let Some(instance) = router.provider_instance(provider) else {
        return Ok(not_evaluated(
            model.to_string(),
            provider.to_string(),
            format!("provider '{provider}' is not configured or its key cannot be resolved"),
        ));
    };

    let local_first = local_first_for(&instance);
    let caller: Arc<dyn CandidateCaller> = Arc::new(RouterCandidateCaller::new(instance));
    Ok(evaluate_with_caller(
        provider,
        model,
        local_first,
        caller,
        &records,
        max_concurrent,
    )
    .await)
}

/// Core gate: run every workload against an injected caller (testable).
pub async fn evaluate_with_caller(
    provider: &str,
    model: &str,
    local_first: LocalFirstAxis,
    caller: Arc<dyn CandidateCaller>,
    records: &[DecisionRecord],
    max_concurrent: usize,
) -> VendorEvalReport {
    let max_concurrent = max_concurrent.clamp(1, MAX_CONCURRENT_CLAMP);

    let mut workloads = Vec::new();
    for workload in WorkloadKind::all() {
        let kind_records: Vec<DecisionRecord> = records
            .iter()
            .filter(|r| r.kind == workload.as_str())
            .cloned()
            .collect();
        workloads
            .push(evaluate_workload(workload, &kind_records, caller.clone(), max_concurrent).await);
    }

    let total_calls: usize = workloads
        .iter()
        .filter_map(|w| w.latency.as_ref())
        .map(|l| l.calls)
        .sum();
    let total_errors: usize = workloads
        .iter()
        .filter_map(|w| w.latency.as_ref())
        .map(|l| l.errors)
        .sum();
    if total_calls > 0 && total_errors == total_calls {
        return not_evaluated(
            model.to_string(),
            provider.to_string(),
            format!("candidate unreachable: {total_errors}/{total_calls} calls errored"),
        );
    }

    let note = Some(
        "prompts are derived from decision records (kind/rung/confidence/decision), not the original turn inputs — the audit sink does not retain them; the candidate is asked to verify each recorded decision against the workload's choices"
            .to_string(),
    );
    VendorEvalReport::evaluated(
        model.to_string(),
        provider.to_string(),
        local_first,
        workloads,
        note,
    )
}

/// Run one workload: cardinality + bounded-concurrency latency + calibration.
async fn evaluate_workload(
    workload: WorkloadKind,
    records: &[DecisionRecord],
    caller: Arc<dyn CandidateCaller>,
    max_concurrent: usize,
) -> WorkloadEval {
    if records.is_empty() {
        return WorkloadEval {
            workload: workload.as_str().to_string(),
            state: WorkloadState::InsufficientData,
            reason: Some(format!(
                "no records of kind '{}' in the decision dataset",
                workload.as_str()
            )),
            cardinality: CardinalityAxis::default(),
            latency: None,
            calibration: None,
            verdict: AxisVerdict::NotGated,
        };
    }

    let cardinality = cardinality_for(records);

    let prompts: Vec<String> = records.iter().map(|r| build_prompt(workload, r)).collect();
    let semaphore = Arc::new(Semaphore::new(max_concurrent));
    let mut handles = Vec::with_capacity(prompts.len());
    for prompt in prompts {
        let sem = semaphore.clone();
        let caller = caller.clone();
        handles.push(tokio::spawn(async move {
            // The semaphore is never closed, so acquisition only fails if the
            // runtime is shutting down — treat that as "no call made" rather
            // than panicking a background task.
            let Ok(_permit) = sem.acquire_owned().await else {
                return (Err("semaphore closed".to_string()), Duration::ZERO);
            };
            let start = Instant::now();
            let result = caller.call(&prompt).await;
            let elapsed = start.elapsed();
            (result, elapsed)
        }));
    }

    let mut latencies = Vec::new();
    let mut errors = 0usize;
    let mut outputs: Vec<Option<CandidateOutput>> = Vec::with_capacity(records.len());
    for handle in handles {
        match handle.await {
            Ok((Ok(text), elapsed)) => {
                latencies.push(elapsed);
                outputs.push(
                    parse_candidate_output(&text)
                        .filter(|o| workload.choices().contains(&o.choice.as_str())),
                );
            }
            Ok((Err(_), _)) => {
                errors += 1;
                outputs.push(None);
            }
            Err(_) => {
                errors += 1;
                outputs.push(None);
            }
        }
    }

    let latency = LatencyAxis {
        calls: records.len(),
        errors,
        error_rate: errors as f64 / records.len() as f64,
        p50_ms: percentile_ms(&latencies, 0.50),
        p95_ms: percentile_ms(&latencies, 0.95),
        max_concurrent,
    };

    let calibration = calibration_axis(records, &outputs);
    let verdict = workload_verdict(&cardinality, &latency, &calibration);

    WorkloadEval {
        workload: workload.as_str().to_string(),
        state: WorkloadState::Evaluated,
        reason: None,
        cardinality,
        latency: Some(latency),
        calibration: Some(calibration),
        verdict,
    }
}

/// Count the workload's distinct decision heads and check the 255 cap.
pub fn cardinality_for(records: &[DecisionRecord]) -> CardinalityAxis {
    let mut choices: Vec<String> = records.iter().filter_map(|r| r.decision.clone()).collect();
    choices.sort();
    choices.dedup();
    let observed = choices.len();
    CardinalityAxis {
        observed,
        cap: CARDINALITY_CAP,
        fits: observed <= CARDINALITY_CAP,
        choices,
    }
}

/// Build the decision prompt for one record of a workload.
///
/// The candidate sees the recorded decision and is asked to verify it against
/// the workload's choice set — the verification shape is the most relevant
/// System-1 use, and the report's `note` documents that original turn inputs
/// are not retained by the audit sink.
pub fn build_prompt(workload: WorkloadKind, record: &DecisionRecord) -> String {
    let choices = workload.choices().join(", ");
    let decision = record.decision.as_deref().unwrap_or("<none>");
    let confidence = record
        .confidence
        .map(|c| format!("{c:.3}"))
        .unwrap_or_else(|| "<none>".to_string());
    format!(
        "You are evaluating a decision record from zen's decision audit.\n\
         Decision kind: {}\n\
         Decision rung: {}\n\
         Recorded confidence: {confidence}\n\
         Recorded decision: {decision}\n\
         Task: choose the correct {} category from: {choices}.\n\
         Respond with JSON only: {{\"choice\": \"<one of the choices>\", \"confidence\": <0.0 to 1.0>}}",
        record.kind,
        record.rung,
        workload.as_str(),
    )
}

/// Parse the candidate's JSON output, tolerating prose-wrapped JSON.
///
/// `None` when no JSON object is present, `choice`/`confidence` are missing,
/// or the confidence is outside `0.0..=1.0` — never a fabricated default.
pub fn parse_candidate_output(text: &str) -> Option<CandidateOutput> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(&text[start..=end]).ok()?;
    let choice = value.get("choice")?.as_str()?.to_string();
    let confidence = value.get("confidence")?.as_f64()?;
    if !(0.0..=1.0).contains(&confidence) {
        return None;
    }
    Some(CandidateOutput { choice, confidence })
}

/// Determine local-first compatibility from the provider instance.
///
/// Local means loopback (localhost / 127.* / ::1 / 0.0.0.0) — the same
/// loopback-only rule the Ollama provider uses to decide proxy bypass. A
/// provider with no base_url (cloud) or a non-loopback base_url fails the
/// hard local-first requirement.
pub fn local_first_for(instance: &ProviderInstance) -> LocalFirstAxis {
    match instance.base_url() {
        Some(url) if is_loopback_url(url) => LocalFirstAxis {
            local: true,
            reason: format!("base_url {url} is loopback — no cloud egress"),
        },
        Some(url) => LocalFirstAxis {
            local: false,
            reason: format!("base_url {url} is not loopback — cloud egress"),
        },
        None => LocalFirstAxis {
            local: false,
            reason: "provider exposes no base_url (cloud provider)".to_string(),
        },
    }
}

/// Persist an evaluation report under `<dir>/vendor-eval/`.
pub fn save_report(dir: &Path, report: &VendorEvalReport) -> Result<PathBuf, VendorEvalError> {
    let eval_dir = dir.join(EVAL_DIR);
    fs::create_dir_all(&eval_dir)?;
    let slug = format!(
        "{}-{}",
        sanitize(&report.model),
        Utc::now().format("%Y%m%d-%H%M%S")
    );
    let path = eval_dir.join(format!("{slug}.json"));
    fs::write(&path, serde_json::to_string_pretty(report)?)?;
    Ok(path)
}

/// Read the newest persisted evaluation report, if any.
///
/// A missing `vendor-eval/` directory yields `Ok(None)` — "no evaluation has
/// been run" is a first-class state, not an error.
pub fn latest_report(dir: &Path) -> Result<Option<VendorEvalReport>, VendorEvalError> {
    let eval_dir = dir.join(EVAL_DIR);
    let entries = match fs::read_dir(&eval_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(VendorEvalError::Io(e)),
    };
    let mut files: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
        .collect();
    files.sort_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());
    match files.last() {
        Some(entry) => {
            let raw = fs::read_to_string(entry.path())?;
            Ok(Some(serde_json::from_str(&raw)?))
        }
        None => Ok(None),
    }
}

/// Construct an explicit "not evaluated" report (readable state, never a
/// fabricated verdict).
pub fn not_evaluated(model: String, provider: String, reason: String) -> VendorEvalReport {
    VendorEvalReport {
        model,
        provider,
        generated_at: Utc::now().to_rfc3339(),
        state: EvalState::NotEvaluated,
        local_first: LocalFirstAxis {
            local: false,
            reason: "not determined — evaluation did not run".to_string(),
        },
        workloads: Vec::new(),
        aggregate: AggregateVerdict::NotEvaluated,
        note: Some(reason),
    }
}

impl VendorEvalReport {
    /// Build an evaluated report, deriving state and aggregate from the
    /// workloads and the local-first axis.
    fn evaluated(
        model: String,
        provider: String,
        local_first: LocalFirstAxis,
        workloads: Vec<WorkloadEval>,
        note: Option<String>,
    ) -> Self {
        let state = if workloads.is_empty() {
            EvalState::NotEvaluated
        } else if workloads.iter().any(|w| w.verdict == AxisVerdict::NotGated) {
            EvalState::InsufficientData
        } else {
            EvalState::Evaluated
        };
        let aggregate = compute_aggregate(&state, &local_first, &workloads);
        Self {
            model,
            provider,
            generated_at: Utc::now().to_rfc3339(),
            state,
            local_first,
            workloads,
            aggregate,
            note,
        }
    }
}

/// Aggregate verdict: `pass` only when every axis passes and data was
/// sufficient for every workload.
fn compute_aggregate(
    state: &EvalState,
    local_first: &LocalFirstAxis,
    workloads: &[WorkloadEval],
) -> AggregateVerdict {
    match state {
        EvalState::NotEvaluated => AggregateVerdict::NotEvaluated,
        EvalState::InsufficientData => AggregateVerdict::InsufficientData,
        EvalState::Evaluated => {
            if !local_first.local {
                return AggregateVerdict::Fail;
            }
            if workloads.iter().any(|w| w.verdict == AxisVerdict::Fail) {
                return AggregateVerdict::Fail;
            }
            AggregateVerdict::Pass
        }
    }
}

/// Per-workload verdict: cardinality must fit the cap, the endpoint must have
/// responded at least once, and calibration must have labels.
fn workload_verdict(
    cardinality: &CardinalityAxis,
    latency: &LatencyAxis,
    calibration: &CalibrationAxis,
) -> AxisVerdict {
    if !cardinality.fits {
        return AxisVerdict::Fail;
    }
    if latency.errors >= latency.calls {
        return AxisVerdict::Fail;
    }
    if calibration.labels_required {
        return AxisVerdict::NotGated;
    }
    AxisVerdict::Pass
}

/// Calibration over labeled records with parseable, in-choice-set outputs.
fn calibration_axis(
    records: &[DecisionRecord],
    outputs: &[Option<CandidateOutput>],
) -> CalibrationAxis {
    let labeled = records.iter().filter(|r| r.label.is_some()).count();
    let mut pairs = Vec::new();
    for (record, output) in records.iter().zip(outputs) {
        let (Some(label), Some(output)) = (&record.label, output) else {
            continue;
        };
        pairs.push((output.confidence, label == &output.choice));
    }
    let usable = pairs.len();
    let labels_required = usable == 0;
    let (ece, accuracy, mean_confidence) = if pairs.is_empty() {
        (None, None, None)
    } else {
        (
            Some(super::decision_audit::ece(&pairs)),
            Some(pairs.iter().filter(|(_, correct)| *correct).count() as f64 / pairs.len() as f64),
            Some(pairs.iter().map(|(c, _)| c).sum::<f64>() / pairs.len() as f64),
        )
    };
    CalibrationAxis {
        labeled,
        usable,
        labels_required,
        ece,
        accuracy,
        mean_confidence,
    }
}

/// p-th percentile of call latencies in milliseconds (0.0 when empty).
fn percentile_ms(latencies: &[Duration], p: f64) -> f64 {
    if latencies.is_empty() {
        return 0.0;
    }
    let mut ms: Vec<f64> = latencies.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((ms.len() - 1) as f64 * p).round() as usize;
    ms[idx.min(ms.len() - 1)]
}

/// Loopback-only rule, mirroring the Ollama provider's proxy-bypass check.
fn is_loopback_url(base_url: &str) -> bool {
    let rest = base_url
        .strip_prefix("http://")
        .or_else(|| base_url.strip_prefix("https://"))
        .unwrap_or(base_url);
    let host = rest.split(['/', ':']).next().unwrap_or("");
    let host = host.trim_matches(|c| c == '[' || c == ']');
    host == "localhost" || host == "::1" || host.starts_with("127.") || host == "0.0.0.0"
}

/// Model names may contain `/` or `:` — keep the persisted filename safe.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect()
}

impl std::fmt::Display for VendorEvalReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "[vendor-eval] model={} provider={} state={:?} aggregate={:?}",
            self.model, self.provider, self.state, self.aggregate
        )?;
        writeln!(
            f,
            "  local-first: local={} ({})",
            self.local_first.local, self.local_first.reason
        )?;
        for w in &self.workloads {
            writeln!(
                f,
                "  {}: state={:?} verdict={:?}",
                w.workload, w.state, w.verdict
            )?;
            writeln!(
                f,
                "    cardinality: observed={} cap={} fits={}",
                w.cardinality.observed, w.cardinality.cap, w.cardinality.fits
            )?;
            if let Some(l) = &w.latency {
                writeln!(
                    f,
                    "    latency: calls={} errors={} error_rate={:.3} p50={:.1}ms p95={:.1}ms (max_concurrent={})",
                    l.calls, l.errors, l.error_rate, l.p50_ms, l.p95_ms, l.max_concurrent
                )?;
            }
            if let Some(c) = &w.calibration {
                match c.ece {
                    Some(ece) => writeln!(
                        f,
                        "    calibration: labeled={} usable={} ece={ece:.4} acc={:.4} mean_conf={:.4}",
                        c.labeled,
                        c.usable,
                        c.accuracy.unwrap_or(0.0),
                        c.mean_confidence.unwrap_or(0.0)
                    )?,
                    None => writeln!(
                        f,
                        "    calibration: labeled={} usable={} labels required",
                        c.labeled, c.usable
                    )?,
                }
            }
            if let Some(reason) = &w.reason {
                writeln!(f, "    {reason}")?;
            }
        }
        if let Some(note) = &self.note {
            writeln!(f, "  note: {note}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use zen_core::config::ProviderConfig;
    use zen_provider::providers::OllamaProvider;

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn record(
        id: &str,
        kind: &str,
        rung: &str,
        confidence: Option<f64>,
        decision: Option<&str>,
        label: Option<&str>,
    ) -> DecisionRecord {
        DecisionRecord {
            id: id.to_string(),
            timestamp: None,
            kind: kind.to_string(),
            rung: rung.to_string(),
            confidence,
            decision: decision.map(str::to_string),
            label: label.map(str::to_string),
        }
    }

    fn minimal_config() -> ZenConfig {
        let mut providers = HashMap::new();
        providers.insert(
            "ollama".to_string(),
            ProviderConfig {
                provider_type: Some("ollama".into()),
                default_model: Some("test-model".into()),
                ..ProviderConfig::default()
            },
        );
        ZenConfig {
            default_provider: Some("ollama".into()),
            providers,
            ..ZenConfig::default()
        }
    }

    fn local_true() -> LocalFirstAxis {
        LocalFirstAxis {
            local: true,
            reason: "test".to_string(),
        }
    }

    /// Fake caller that tracks peak concurrency and answers per workload.
    struct FakeCaller {
        max_concurrent: Arc<AtomicUsize>,
        current: Arc<AtomicUsize>,
        delay: Duration,
    }

    impl FakeCaller {
        fn new(delay: Duration) -> Self {
            Self {
                max_concurrent: Arc::new(AtomicUsize::new(0)),
                current: Arc::new(AtomicUsize::new(0)),
                delay,
            }
        }
    }

    #[async_trait]
    impl CandidateCaller for FakeCaller {
        async fn call(&self, prompt: &str) -> Result<String, String> {
            let now = self.current.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_concurrent.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            self.current.fetch_sub(1, Ordering::SeqCst);
            let choice = if prompt.contains("correction") {
                "yes"
            } else if prompt.contains("review") {
                "approve"
            } else {
                "Query"
            };
            Ok(format!(r#"{{"choice":"{choice}","confidence":0.9}}"#))
        }
    }

    struct ErrorCaller;

    #[async_trait]
    impl CandidateCaller for ErrorCaller {
        async fn call(&self, _prompt: &str) -> Result<String, String> {
            Err("connection refused".to_string())
        }
    }

    fn write_dataset(dir: &Path, lines: &[&str]) {
        let dataset_dir = dir.join("decision-audit");
        fs::create_dir_all(&dataset_dir).unwrap();
        let mut content = String::new();
        for line in lines {
            content.push_str(line);
            content.push('\n');
        }
        fs::write(
            dataset_dir.join(super::super::decision_audit::DATASET_FILE),
            content,
        )
        .unwrap();
    }

    #[test]
    fn cardinality_counts_distinct_decisions() {
        let records = vec![
            record("a", "intent", "llm", Some(0.9), Some("Query"), None),
            record("b", "intent", "llm", Some(0.9), Some("Query"), None),
            record("c", "intent", "keyword", Some(0.7), Some("Action"), None),
        ];
        let axis = cardinality_for(&records);
        assert_eq!(axis.observed, 2);
        assert_eq!(axis.cap, CARDINALITY_CAP);
        assert!(axis.fits);
        assert_eq!(
            axis.choices,
            vec!["Action".to_string(), "Query".to_string()]
        );
    }

    #[test]
    fn cardinality_over_cap_fails() {
        let records: Vec<DecisionRecord> = (0..CARDINALITY_CAP + 1)
            .map(|i| {
                record(
                    &format!("id{i}"),
                    "intent",
                    "llm",
                    Some(0.9),
                    Some(&format!("Choice{i}")),
                    None,
                )
            })
            .collect();
        let axis = cardinality_for(&records);
        assert_eq!(axis.observed, CARDINALITY_CAP + 1);
        assert!(!axis.fits);
    }

    #[tokio::test]
    async fn empty_dataset_is_not_evaluated() {
        let dir = tmpdir();
        let report = evaluate(&minimal_config(), dir.path(), "ollama", "test-model", 4)
            .await
            .unwrap();
        assert_eq!(report.state, EvalState::NotEvaluated);
        assert_eq!(report.aggregate, AggregateVerdict::NotEvaluated);
        assert!(report.note.as_deref().unwrap().contains("empty"));
    }

    #[tokio::test]
    async fn missing_model_name_is_not_evaluated() {
        let dir = tmpdir();
        write_dataset(
            dir.path(),
            &[r#"{"id":"a","kind":"intent","rung":"llm","confidence":0.9,"decision":"Query"}"#],
        );
        let report = evaluate(&minimal_config(), dir.path(), "ollama", "  ", 4)
            .await
            .unwrap();
        assert_eq!(report.state, EvalState::NotEvaluated);
        assert!(report.note.as_deref().unwrap().contains("no model name"));
    }

    #[tokio::test]
    async fn unconfigured_provider_is_not_evaluated() {
        let dir = tmpdir();
        write_dataset(
            dir.path(),
            &[r#"{"id":"a","kind":"intent","rung":"llm","confidence":0.9,"decision":"Query"}"#],
        );
        let report = evaluate(&minimal_config(), dir.path(), "no-such-provider", "m", 4)
            .await
            .unwrap();
        assert_eq!(report.state, EvalState::NotEvaluated);
        assert!(report.note.as_deref().unwrap().contains("not configured"));
    }

    #[tokio::test]
    async fn unreachable_candidate_is_not_evaluated() {
        let records = vec![record("a", "intent", "llm", Some(0.9), Some("Query"), None)];
        let report = evaluate_with_caller(
            "ollama",
            "test-model",
            local_true(),
            Arc::new(ErrorCaller),
            &records,
            4,
        )
        .await;
        assert_eq!(report.state, EvalState::NotEvaluated);
        assert_eq!(report.aggregate, AggregateVerdict::NotEvaluated);
        assert!(report.note.as_deref().unwrap().contains("unreachable"));
    }

    #[tokio::test]
    async fn missing_workload_is_insufficient_not_pass() {
        let records = vec![record(
            "a",
            "intent",
            "llm",
            Some(0.9),
            Some("Query"),
            Some("Query"),
        )];
        let caller = FakeCaller::new(Duration::from_millis(1));
        let report = evaluate_with_caller(
            "ollama",
            "test-model",
            local_true(),
            Arc::new(caller),
            &records,
            4,
        )
        .await;
        assert_eq!(report.state, EvalState::InsufficientData);
        assert_eq!(report.aggregate, AggregateVerdict::InsufficientData);
        let correction = report
            .workloads
            .iter()
            .find(|w| w.workload == "correction")
            .unwrap();
        assert_eq!(correction.state, WorkloadState::InsufficientData);
        assert_eq!(correction.verdict, AxisVerdict::NotGated);
        assert!(correction.reason.as_deref().unwrap().contains("no records"));
    }

    #[tokio::test]
    async fn ece_only_emitted_with_labels() {
        let records = vec![record("a", "intent", "llm", Some(0.9), Some("Query"), None)];
        let caller = FakeCaller::new(Duration::from_millis(1));
        let report = evaluate_with_caller(
            "ollama",
            "test-model",
            local_true(),
            Arc::new(caller),
            &records,
            4,
        )
        .await;
        let intent = report
            .workloads
            .iter()
            .find(|w| w.workload == "intent")
            .unwrap();
        let cal = intent.calibration.as_ref().unwrap();
        assert!(cal.labels_required);
        assert!(cal.ece.is_none());
        assert!(cal.accuracy.is_none());
        // Display must say "labels required", never print a number.
        let display = report.to_string();
        assert!(display.contains("labels required"));
        assert!(!display.contains("ece="));
    }

    #[tokio::test]
    async fn calibration_computed_over_labeled_records() {
        // Candidate answers "Query" for every intent prompt: record a (label
        // Query) is correct, record b (label Action) is wrong. Pairs
        // (0.9, true), (0.9, false) → ECE = |0.5 − 0.9| = 0.4.
        let records = vec![
            record(
                "a",
                "intent",
                "llm",
                Some(0.9),
                Some("Query"),
                Some("Query"),
            ),
            record(
                "b",
                "intent",
                "llm",
                Some(0.9),
                Some("Action"),
                Some("Action"),
            ),
        ];
        let caller = FakeCaller::new(Duration::from_millis(1));
        let report = evaluate_with_caller(
            "ollama",
            "test-model",
            local_true(),
            Arc::new(caller),
            &records,
            4,
        )
        .await;
        let intent = report
            .workloads
            .iter()
            .find(|w| w.workload == "intent")
            .unwrap();
        let cal = intent.calibration.as_ref().unwrap();
        assert_eq!(cal.labeled, 2);
        assert_eq!(cal.usable, 2);
        assert!(!cal.labels_required);
        assert!((cal.ece.unwrap() - 0.4).abs() < 1e-9);
        assert!((cal.accuracy.unwrap() - 0.5).abs() < 1e-9);
    }

    #[tokio::test]
    async fn bounded_concurrency_is_respected() {
        let records: Vec<DecisionRecord> = (0..20)
            .map(|i| {
                record(
                    &format!("id{i}"),
                    "intent",
                    "llm",
                    Some(0.9),
                    Some("Query"),
                    Some("Query"),
                )
            })
            .collect();
        let caller = FakeCaller::new(Duration::from_millis(20));
        let max_seen = caller.max_concurrent.clone();
        let report = evaluate_with_caller(
            "ollama",
            "test-model",
            local_true(),
            Arc::new(caller),
            &records,
            4,
        )
        .await;
        let intent = report
            .workloads
            .iter()
            .find(|w| w.workload == "intent")
            .unwrap();
        let latency = intent.latency.as_ref().unwrap();
        assert_eq!(latency.calls, 20);
        assert_eq!(latency.errors, 0);
        assert_eq!(latency.max_concurrent, 4);
        assert!(max_seen.load(Ordering::SeqCst) <= 4);
    }

    #[tokio::test]
    async fn local_first_rejection_fails_the_gate() {
        // All three workloads present and labeled so the only failing axis is
        // local-first.
        let records = vec![
            record(
                "a",
                "intent",
                "llm",
                Some(0.9),
                Some("Query"),
                Some("Query"),
            ),
            record(
                "b",
                "correction",
                "llm",
                Some(0.9),
                Some("yes"),
                Some("yes"),
            ),
            record(
                "c",
                "review",
                "llm",
                Some(0.9),
                Some("approve"),
                Some("approve"),
            ),
        ];
        let caller = FakeCaller::new(Duration::from_millis(1));
        let report = evaluate_with_caller(
            "openai",
            "vendor-model",
            LocalFirstAxis {
                local: false,
                reason: "base_url https://api.example.com is not loopback — cloud egress"
                    .to_string(),
            },
            Arc::new(caller),
            &records,
            4,
        )
        .await;
        assert_eq!(report.state, EvalState::Evaluated);
        assert_eq!(report.aggregate, AggregateVerdict::Fail);
        assert!(
            report
                .workloads
                .iter()
                .all(|w| w.verdict == AxisVerdict::Pass)
        );
    }

    #[tokio::test]
    async fn all_axes_pass_yields_pass() {
        let records = vec![
            record(
                "a",
                "intent",
                "llm",
                Some(0.9),
                Some("Query"),
                Some("Query"),
            ),
            record(
                "b",
                "correction",
                "llm",
                Some(0.9),
                Some("yes"),
                Some("yes"),
            ),
            record(
                "c",
                "review",
                "llm",
                Some(0.9),
                Some("approve"),
                Some("approve"),
            ),
        ];
        let caller = FakeCaller::new(Duration::from_millis(1));
        let report = evaluate_with_caller(
            "ollama",
            "test-model",
            local_true(),
            Arc::new(caller),
            &records,
            4,
        )
        .await;
        assert_eq!(report.state, EvalState::Evaluated);
        assert_eq!(report.aggregate, AggregateVerdict::Pass);
    }

    #[test]
    fn report_round_trips_through_json() {
        let records = vec![
            record(
                "a",
                "intent",
                "llm",
                Some(0.9),
                Some("Query"),
                Some("Query"),
            ),
            record(
                "b",
                "correction",
                "llm",
                Some(0.9),
                Some("yes"),
                Some("yes"),
            ),
            record(
                "c",
                "review",
                "llm",
                Some(0.9),
                Some("approve"),
                Some("approve"),
            ),
        ];
        let caller = FakeCaller::new(Duration::from_millis(1));
        let report = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(evaluate_with_caller(
                "ollama",
                "test-model",
                local_true(),
                Arc::new(caller),
                &records,
                4,
            ));
        let json = serde_json::to_string(&report).unwrap();
        let back: VendorEvalReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back.aggregate, report.aggregate);
        assert_eq!(back.state, report.state);
        assert_eq!(back.workloads.len(), 3);
        assert_eq!(back.local_first.local, report.local_first.local);
        // Display renders the section header.
        assert!(report.to_string().contains("[vendor-eval]"));
    }

    #[test]
    fn parse_candidate_output_handles_prose_wrapped_json() {
        let out =
            parse_candidate_output(r#"Here is the answer: {"choice":"Query","confidence":0.85}"#)
                .unwrap();
        assert_eq!(out.choice, "Query");
        assert!((out.confidence - 0.85).abs() < 1e-9);
        assert!(parse_candidate_output("no json here").is_none());
        assert!(parse_candidate_output(r#"{"choice":"Query"}"#).is_none());
        assert!(parse_candidate_output(r#"{"choice":"Query","confidence":1.5}"#).is_none());
    }

    #[test]
    fn build_prompt_includes_choices_and_record() {
        let r = record("a", "intent", "llm", Some(0.9), Some("Query"), None);
        let prompt = build_prompt(WorkloadKind::Intent, &r);
        assert!(prompt.contains("Query, Action, System, Conversation"));
        assert!(prompt.contains("intent"));
        assert!(prompt.contains("llm"));
        assert!(prompt.contains("Query"));
    }

    #[test]
    fn local_first_for_detects_loopback() {
        let ollama = ProviderInstance::Ollama(OllamaProvider::new(
            "http://127.0.0.1:11434".to_string(),
            "test".to_string(),
        ));
        assert!(local_first_for(&ollama).local);
        let mock = ProviderInstance::Mock(zen_provider::MockProvider::default());
        assert!(!local_first_for(&mock).local);
        assert!(local_first_for(&mock).reason.contains("no base_url"));
    }

    #[test]
    fn save_and_latest_report_round_trip() {
        let dir = tmpdir();
        let report = not_evaluated(
            "vendor-model".to_string(),
            "ollama".to_string(),
            "test".to_string(),
        );
        let path = save_report(dir.path(), &report).unwrap();
        assert!(path.exists());
        let latest = latest_report(dir.path()).unwrap().unwrap();
        assert_eq!(latest.model, "vendor-model");
        assert_eq!(latest.state, EvalState::NotEvaluated);
        // Missing directory is a first-class "no evaluation" state.
        let empty_dir = tmpdir();
        assert!(latest_report(empty_dir.path()).unwrap().is_none());
    }
}
