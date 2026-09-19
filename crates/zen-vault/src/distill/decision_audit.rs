//! Decision-audit harness (T173): collect, label, measure — never fabricate.
//!
//! The L1 calibrated decision layer (V13 增补 A, T168-T171) needs every
//! threshold τ calibrated on a rolling labeled audit set, plus drift
//! monitoring, before any threshold is legitimate. This module is that
//! harness: it COLLECTS decision records from the existing audit sink
//! (`<logs>/audit.jsonl`), MERGES human-supplied ground-truth labels, and
//! MEASURES calibration (ECE / Brier) and drift. It never invents labels,
//! thresholds, or "well-calibrated" claims — the honest "labels required"
//! state is the expected state today (the V13 §6 baseline was reset to zero
//! after T118).
//!
//! # Data flow
//!
//! 1. [`extract_dataset`] reads `<logs>/audit.jsonl`, extracts one
//!    [`DecisionRecord`] per decision from `loop.turn.review` lines (and,
//!    when T168 lands, `loop.decision` lines), and writes them to
//!    `<logs>/decision-audit/dataset.jsonl`. Record ids are stable sha256
//!    digests of the raw audit line, so re-extraction is idempotent.
//! 2. A sibling `<logs>/decision-audit/labels.jsonl` holds ground truth
//!    keyed by record id (`{"id": "...", "label": "..."}`). Labels are
//!    *absent* unless supplied; [`load_dataset`] / [`analyze`] merge them
//!    when present.
//! 3. [`compute`] produces a [`CalibrationReport`]: per decision kind and
//!    per rung, ECE + Brier over **labeled records only**. A kind with zero
//!    labeled records reports `labels_required` — never a number.
//! 4. Drift monitoring compares the last [`DRIFT_WINDOW_DAYS`] against the
//!    preceding window (confidence distribution + rung mix per kind) and
//!    flags divergence beyond the documented tolerances
//!    [`CONFIDENCE_DRIFT_TOLERANCE`] / [`RUNG_MIX_DRIFT_TOLERANCE`].
//!
//! # Calibration discipline (V13-A.3)
//!
//! * Probe/logit readouts are preferred over verbalized confidence
//!   (ECE 0.044 vs 0.093) — the harness measures whatever confidence the
//!   audit line records; it does not gate on it.
//! * The harness is read-only analytics: it never gates, blocks, or mutates
//!   a decision. It writes only its own `decision-audit/` data store.
//! * Empty inputs are first-class: a fresh install has no labels and no
//!   history, so "labels required" and empty drift windows are the common
//!   path, not an error.
//!
//! # Drift tolerances (documented, not decision thresholds)
//!
//! These tolerances are the drift *detector's* stated sensitivity — they are
//! NOT the τ thresholds T168-T171 must derive from labeled data. They are
//! deliberately coarse so a fresh, sparse dataset does not alarm.
//!
//! * [`CONFIDENCE_DRIFT_TOLERANCE`] = 0.1 — |Δ mean confidence| beyond this
//!   flags confidence drift.
//! * [`RUNG_MIX_DRIFT_TOLERANCE`] = 0.2 — any rung's share moving beyond
//!   20 percentage points flags rung-mix drift.

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::orchestration_stats::IntentDist;

/// Errors from decision-audit I/O.
#[derive(Debug, Error)]
pub enum DecisionAuditError {
    /// Filesystem error (missing file is NOT an error — see [`extract_dataset`]).
    #[error("decision audit I/O error: {0}")]
    Io(#[from] io::Error),
    /// JSON serialization/deserialization error.
    #[error("decision audit JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// ECE bin count (equal-width over `[0, 1]`).
pub const ECE_BINS: usize = 10;

/// Drift window length in days: the recent window is the last
/// [`DRIFT_WINDOW_DAYS`] days; the prior window is the [`DRIFT_WINDOW_DAYS`]
/// days before that.
pub const DRIFT_WINDOW_DAYS: i64 = 7;

/// Confidence-drift tolerance: |Δ mean confidence| between the recent and
/// prior windows beyond this flags a drift signal.
pub const CONFIDENCE_DRIFT_TOLERANCE: f64 = 0.1;

/// Rung-mix drift tolerance: any rung's share (percentage points) moving
/// beyond this between windows flags a drift signal.
pub const RUNG_MIX_DRIFT_TOLERANCE: f64 = 0.2;

/// Dataset file name under `<logs>/decision-audit/`.
pub const DATASET_FILE: &str = "dataset.jsonl";

/// Labels file name under `<logs>/decision-audit/`.
pub const LABELS_FILE: &str = "labels.jsonl";

/// One extracted decision record.
///
/// The ground-truth [`DecisionRecord::label`] is *absent* unless supplied
/// via `labels.jsonl` — the harness never fabricates it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionRecord {
    /// Stable record id — sha256 of the raw audit line (first 16 hex chars).
    pub id: String,
    /// RFC3339 timestamp from the audit line's `ts` field, if present.
    pub timestamp: Option<String>,
    /// Decision kind, e.g. `"intent"` (from `loop.turn.review`) or the
    /// `decision_kind` carried by a `loop.decision` line.
    pub kind: String,
    /// The rung that decided, e.g. `"keyword"` / `"llm"` (lowercased
    /// `intent_source` for `loop.turn.review`; the `rung` field for
    /// `loop.decision`).
    pub rung: String,
    /// Recorded confidence in `0.0..=1.0`, if present.
    pub confidence: Option<f64>,
    /// The value the decision chose (e.g. `intent_category`), used to derive
    /// correctness against the label. Absent when the audit line carries no
    /// choice.
    pub decision: Option<String>,
    /// Ground-truth label, merged from `labels.jsonl` when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// One ground-truth label entry from `labels.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabelEntry {
    /// Record id (must match a [`DecisionRecord::id`]).
    pub id: String,
    /// Ground-truth label for the decision (e.g. the correct intent category).
    pub label: String,
}

/// Per-rung calibration metrics over labeled records.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RungCalibration {
    /// Rung name (e.g. `"keyword"`, `"llm"`).
    pub rung: String,
    /// Records of this rung in the dataset.
    pub records: usize,
    /// Records of this rung with a merged label.
    pub labeled: usize,
    /// True when no labeled records exist — ECE/Brier are `None` and the
    /// report MUST say "labels required" rather than print a number.
    pub labels_required: bool,
    /// Expected Calibration Error over labeled records; `None` when
    /// `labels_required`.
    pub ece: Option<f64>,
    /// Brier score over labeled records; `None` when `labels_required`.
    pub brier: Option<f64>,
    /// Mean recorded confidence over labeled records.
    pub mean_confidence: Option<f64>,
    /// Accuracy (correct / labeled) over labeled records.
    pub accuracy: Option<f64>,
}

/// Per-kind calibration section.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KindCalibration {
    /// Decision kind (e.g. `"intent"`).
    pub kind: String,
    /// Records of this kind in the dataset.
    pub records: usize,
    /// Records of this kind with a merged label.
    pub labeled: usize,
    /// True when no labeled records exist for this kind.
    pub labels_required: bool,
    /// Per-rung metrics.
    pub rungs: Vec<RungCalibration>,
}

/// Per-kind drift signal between the recent and prior windows.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KindDrift {
    /// Decision kind.
    pub kind: String,
    /// Records with timestamps in the recent window (last
    /// [`DRIFT_WINDOW_DAYS`] days).
    pub recent_records: usize,
    /// Records with timestamps in the prior window (the
    /// [`DRIFT_WINDOW_DAYS`] days before that).
    pub prior_records: usize,
    /// Mean confidence in the recent window.
    pub recent_mean_confidence: Option<f64>,
    /// Mean confidence in the prior window.
    pub prior_mean_confidence: Option<f64>,
    /// True when |Δ mean confidence| exceeds [`CONFIDENCE_DRIFT_TOLERANCE`].
    pub confidence_drift: bool,
    /// Rung distribution (share %) in the recent window.
    pub recent_rungs: Vec<IntentDist>,
    /// Rung distribution (share %) in the prior window.
    pub prior_rungs: Vec<IntentDist>,
    /// True when any rung's share moved beyond
    /// [`RUNG_MIX_DRIFT_TOLERANCE`] percentage points.
    pub rung_mix_drift: bool,
    /// Human-readable note for empty/insufficient windows (first-class case).
    pub note: Option<String>,
}

/// Full calibration + drift snapshot for the `zen discover report`
/// `calibration` section.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CalibrationReport {
    /// Total records in the dataset.
    pub records: usize,
    /// Records with a merged label.
    pub labeled: usize,
    /// Per-kind calibration metrics.
    pub kinds: Vec<KindCalibration>,
    /// Per-kind drift signals.
    pub drift: Vec<KindDrift>,
    /// Snapshot wall-clock time (RFC 3339).
    pub generated_at: String,
}

/// Extract decision records from `<dir>/audit.jsonl` into
/// `<dir>/decision-audit/dataset.jsonl`.
///
/// One record per decision from `loop.turn.review` lines (kind `"intent"`,
/// rung = lowercased `intent_source`, confidence = `intent_confidence`,
/// decision = `intent_category`) and, when present, `loop.decision` lines
/// (kind = `decision_kind`, rung = `rung`, confidence = `confidence`,
/// decision = `choice`). Corrupt lines and lines without a rung are skipped
/// (never panic). A missing audit file yields an empty dataset (fresh state
/// is not an error). Record ids are stable sha256 digests of the raw line,
/// so re-extraction is idempotent.
///
/// # Errors
///
/// Returns [`DecisionAuditError`] on I/O (other than missing audit file) or
/// JSON failure while writing the dataset.
pub fn extract_dataset(dir: &Path) -> Result<Vec<DecisionRecord>, DecisionAuditError> {
    let audit_path = dir.join("audit.jsonl");
    let file = match fs::File::open(&audit_path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(DecisionAuditError::Io(e)),
    };

    let reader = io::BufReader::new(file);
    let mut records = Vec::new();
    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(_) => continue,
        };
        if let Some(record) = record_from_line(&line) {
            records.push(record);
        }
    }

    let out_dir = dir.join("decision-audit");
    fs::create_dir_all(&out_dir)?;
    let out_path = out_dir.join(DATASET_FILE);
    let mut out = fs::File::create(&out_path)?;
    for record in &records {
        writeln!(out, "{}", serde_json::to_string(record)?)?;
    }
    Ok(records)
}

/// Load ground-truth labels from `<dir>/decision-audit/labels.jsonl`.
///
/// A missing labels file yields an empty map (no labels is the honest
/// default). Corrupt lines are skipped.
pub fn load_labels(dir: &Path) -> Result<HashMap<String, String>, DecisionAuditError> {
    let path = dir.join("decision-audit").join(LABELS_FILE);
    let file = match fs::File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => return Err(DecisionAuditError::Io(e)),
    };
    let reader = io::BufReader::new(file);
    let mut labels = HashMap::new();
    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(_) => continue,
        };
        if let Ok(entry) = serde_json::from_str::<LabelEntry>(&line) {
            labels.insert(entry.id, entry.label);
        }
    }
    Ok(labels)
}

/// Load the dataset from `<dir>/decision-audit/dataset.jsonl` and merge
/// labels from `labels.jsonl` by record id.
///
/// A missing dataset yields an empty vec. Corrupt lines are skipped.
pub fn load_dataset(dir: &Path) -> Result<Vec<DecisionRecord>, DecisionAuditError> {
    let path = dir.join("decision-audit").join(DATASET_FILE);
    let file = match fs::File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(DecisionAuditError::Io(e)),
    };
    let labels = load_labels(dir)?;
    let reader = io::BufReader::new(file);
    let mut records = Vec::new();
    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(_) => continue,
        };
        if let Ok(mut record) = serde_json::from_str::<DecisionRecord>(&line) {
            if let Some(label) = labels.get(&record.id) {
                record.label = Some(label.clone());
            }
            records.push(record);
        }
    }
    Ok(records)
}

/// Full harness entry: refresh the dataset from the audit sink, merge labels,
/// and compute the calibration + drift report.
///
/// Read-only with respect to decisions: the only write is the harness's own
/// `decision-audit/dataset.jsonl` data store.
pub fn analyze(dir: &Path) -> Result<CalibrationReport, DecisionAuditError> {
    let records = extract_dataset(dir)?;
    let labels = load_labels(dir)?;
    let mut merged = records;
    for record in &mut merged {
        if let Some(label) = labels.get(&record.id) {
            record.label = Some(label.clone());
        }
    }
    Ok(compute(&merged))
}

/// Compute the calibration + drift report from records (pure, no I/O).
///
/// Calibration metrics are computed over **labeled records only**; a kind or
/// rung with zero labeled records reports `labels_required` and `None`
/// metrics — never a fabricated number.
pub fn compute(records: &[DecisionRecord]) -> CalibrationReport {
    let now = Utc::now().to_rfc3339();
    let labeled = records.iter().filter(|r| r.label.is_some()).count();

    let mut by_kind: HashMap<String, Vec<&DecisionRecord>> = HashMap::new();
    for record in records {
        by_kind.entry(record.kind.clone()).or_default().push(record);
    }
    let mut kind_names: Vec<String> = by_kind.keys().cloned().collect();
    kind_names.sort();
    let kinds: Vec<KindCalibration> = kind_names
        .iter()
        .map(|kind| calibration_for_kind(kind, &by_kind[kind]))
        .collect();

    let drift = drift_for_kinds(records);

    CalibrationReport {
        records: records.len(),
        labeled,
        kinds,
        drift,
        generated_at: now,
    }
}

/// Derive correctness from the label vs the recorded decision.
///
/// `None` when either the label or the decision value is absent — such a
/// record cannot be scored and is excluded from calibration.
fn is_correct(record: &DecisionRecord) -> Option<bool> {
    match (&record.label, &record.decision) {
        (Some(label), Some(decision)) => Some(label == decision),
        _ => None,
    }
}

fn calibration_for_kind(kind: &str, records: &[&DecisionRecord]) -> KindCalibration {
    let mut by_rung: HashMap<String, Vec<&DecisionRecord>> = HashMap::new();
    for record in records {
        by_rung
            .entry(record.rung.clone())
            .or_default()
            .push(*record);
    }
    let mut rung_names: Vec<String> = by_rung.keys().cloned().collect();
    rung_names.sort();
    let rungs: Vec<RungCalibration> = rung_names
        .iter()
        .map(|rung| calibration_for_rung(rung, &by_rung[rung]))
        .collect();
    let labeled = records.iter().filter(|r| r.label.is_some()).count();
    KindCalibration {
        kind: kind.to_string(),
        records: records.len(),
        labeled,
        labels_required: labeled == 0,
        rungs,
    }
}

fn calibration_for_rung(rung: &str, records: &[&DecisionRecord]) -> RungCalibration {
    let labeled_records: Vec<&DecisionRecord> = records
        .iter()
        .filter(|r| r.label.is_some())
        .copied()
        .collect();
    let pairs: Vec<(f64, bool)> = labeled_records
        .iter()
        .filter_map(|record| {
            let confidence = record.confidence?;
            let correct = is_correct(record)?;
            Some((confidence, correct))
        })
        .collect();
    let labels_required = labeled_records.is_empty();
    let (ece, brier, mean_confidence, accuracy) = if pairs.is_empty() {
        (None, None, None, None)
    } else {
        (
            Some(ece(&pairs)),
            Some(brier(&pairs)),
            Some(mean_conf(&pairs)),
            Some(accuracy(&pairs)),
        )
    };
    RungCalibration {
        rung: rung.to_string(),
        records: records.len(),
        labeled: labeled_records.len(),
        labels_required,
        ece,
        brier,
        mean_confidence,
        accuracy,
    }
}

/// Expected Calibration Error over `(confidence, correct)` pairs.
///
/// Records are binned into [`ECE_BINS`] equal-width bins over `[0, 1]`;
/// ECE = Σ_b (n_b / N) · |acc_b − conf_b|. Perfectly calibrated ≈ 0;
/// systematically overconfident > 0.
///
/// `pub(crate)` so the T174 vendor-eval gate (`distill::vendor_eval`)
/// reuses the same calculation over the candidate's outputs instead of
/// duplicating it (Constitution XI — reuse over novelty).
pub(crate) fn ece(pairs: &[(f64, bool)]) -> f64 {
    let mut bins = vec![(0usize, 0.0f64, 0usize); ECE_BINS];
    for (confidence, correct) in pairs {
        let idx = ((confidence * ECE_BINS as f64).floor() as usize).min(ECE_BINS - 1);
        bins[idx].0 += 1;
        bins[idx].1 += confidence;
        if *correct {
            bins[idx].2 += 1;
        }
    }
    let n = pairs.len() as f64;
    bins.iter()
        .map(|(count, conf_sum, correct_count)| {
            if *count == 0 {
                return 0.0;
            }
            let acc = *correct_count as f64 / *count as f64;
            let conf = conf_sum / *count as f64;
            (*count as f64 / n) * (acc - conf).abs()
        })
        .sum()
}

/// Binary Brier score over `(confidence, correct)` pairs:
/// mean of `(confidence − y)²` with `y = 1` when correct else `0`.
fn brier(pairs: &[(f64, bool)]) -> f64 {
    pairs
        .iter()
        .map(|(confidence, correct)| {
            let y = if *correct { 1.0 } else { 0.0 };
            (confidence - y).powi(2)
        })
        .sum::<f64>()
        / pairs.len() as f64
}

fn mean_conf(pairs: &[(f64, bool)]) -> f64 {
    pairs.iter().map(|(c, _)| c).sum::<f64>() / pairs.len() as f64
}

fn accuracy(pairs: &[(f64, bool)]) -> f64 {
    pairs.iter().filter(|(_, correct)| *correct).count() as f64 / pairs.len() as f64
}

fn drift_for_kinds(records: &[DecisionRecord]) -> Vec<KindDrift> {
    let now = Utc::now();
    let recent_start = now - Duration::days(DRIFT_WINDOW_DAYS);
    let prior_start = now - Duration::days(2 * DRIFT_WINDOW_DAYS);

    let mut by_kind: HashMap<String, Vec<&DecisionRecord>> = HashMap::new();
    for record in records {
        by_kind.entry(record.kind.clone()).or_default().push(record);
    }
    let mut kind_names: Vec<String> = by_kind.keys().cloned().collect();
    kind_names.sort();
    kind_names
        .iter()
        .map(|kind| {
            let (recent, prior) = split_windows(&by_kind[kind], recent_start, prior_start);
            drift_for_kind(kind, &recent, &prior)
        })
        .collect()
}

/// Split records into (recent, prior) windows by timestamp.
///
/// Records without a parseable RFC3339 timestamp are excluded from drift
/// (they cannot be windowed — no fabrication).
fn split_windows<'a>(
    records: &[&'a DecisionRecord],
    recent_start: DateTime<Utc>,
    prior_start: DateTime<Utc>,
) -> (Vec<&'a DecisionRecord>, Vec<&'a DecisionRecord>) {
    let mut recent = Vec::new();
    let mut prior = Vec::new();
    for record in records {
        let Some(ts) = record
            .timestamp
            .as_deref()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|dt| dt.with_timezone(&Utc))
        else {
            continue;
        };
        if ts >= recent_start {
            recent.push(*record);
        } else if ts >= prior_start {
            prior.push(*record);
        }
    }
    (recent, prior)
}

fn drift_for_kind(kind: &str, recent: &[&DecisionRecord], prior: &[&DecisionRecord]) -> KindDrift {
    let recent_conf: Vec<f64> = recent.iter().filter_map(|r| r.confidence).collect();
    let prior_conf: Vec<f64> = prior.iter().filter_map(|r| r.confidence).collect();
    let recent_mean = mean(&recent_conf);
    let prior_mean = mean(&prior_conf);
    let confidence_drift = match (recent_mean, prior_mean) {
        (Some(a), Some(b)) => (a - b).abs() > CONFIDENCE_DRIFT_TOLERANCE,
        _ => false,
    };
    let recent_rungs = rung_distribution(recent);
    let prior_rungs = rung_distribution(prior);
    let rung_mix_drift = rung_mix_shifted(&recent_rungs, &prior_rungs);
    let note = if recent.is_empty() && prior.is_empty() {
        Some("no timestamped records in either window".to_string())
    } else if recent.is_empty() || prior.is_empty() {
        Some("insufficient data: one window empty".to_string())
    } else {
        None
    };
    KindDrift {
        kind: kind.to_string(),
        recent_records: recent.len(),
        prior_records: prior.len(),
        recent_mean_confidence: recent_mean,
        prior_mean_confidence: prior_mean,
        confidence_drift,
        recent_rungs,
        prior_rungs,
        rung_mix_drift,
        note,
    }
}

fn mean(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<f64>() / values.len() as f64)
    }
}

fn rung_distribution(records: &[&DecisionRecord]) -> Vec<IntentDist> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for record in records {
        *counts.entry(record.rung.clone()).or_insert(0) += 1;
    }
    let total = records.len();
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

/// True when any rung's share (percentage points) moved beyond
/// [`RUNG_MIX_DRIFT_TOLERANCE`] between the two windows.
fn rung_mix_shifted(recent: &[IntentDist], prior: &[IntentDist]) -> bool {
    let mut names: Vec<&str> = recent
        .iter()
        .map(|d| d.name.as_str())
        .chain(prior.iter().map(|d| d.name.as_str()))
        .collect();
    names.sort_unstable();
    names.dedup();
    names.iter().any(|name| {
        let r = recent
            .iter()
            .find(|d| d.name == *name)
            .map(|d| d.pct)
            .unwrap_or(0.0);
        let p = prior
            .iter()
            .find(|d| d.name == *name)
            .map(|d| d.pct)
            .unwrap_or(0.0);
        (r - p).abs() > RUNG_MIX_DRIFT_TOLERANCE * 100.0
    })
}

/// Extract one record from a raw audit line, or `None` when the line is not
/// a decision line or lacks a rung.
fn record_from_line(line: &str) -> Option<DecisionRecord> {
    if line.contains("\"kind\":\"loop.turn.review\"") {
        let rung = read_field_str(line, "intent_source")?.to_lowercase();
        Some(DecisionRecord {
            id: stable_id(line),
            timestamp: read_field_str(line, "ts"),
            kind: "intent".to_string(),
            rung,
            confidence: read_field_f64(line, "intent_confidence"),
            decision: read_field_str(line, "intent_category"),
            label: None,
        })
    } else if line.contains("\"kind\":\"loop.decision\"") {
        // T168 will emit these (`rung`, `latency_ms`, `confidence`); support
        // them defensively so the harness is ready when the ladder lands.
        let rung = read_field_str(line, "rung")?;
        Some(DecisionRecord {
            id: stable_id(line),
            timestamp: read_field_str(line, "ts"),
            kind: read_field_str(line, "decision_kind").unwrap_or_else(|| "decision".to_string()),
            rung,
            confidence: read_field_f64(line, "confidence"),
            decision: read_field_str(line, "choice"),
            label: None,
        })
    } else {
        None
    }
}

/// Stable record id: first 16 hex chars of the sha256 of the raw line.
fn stable_id(line: &str) -> String {
    let digest = Sha256::digest(line.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    hex[..16].to_string()
}

/// Extract `"key":"value"` from a raw JSON line.
fn read_field_str(line: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = line.find(&needle)?;
    let value_start = start + needle.len();
    let rest = &line[value_start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Extract a JSON number field from a raw JSON line.
fn read_field_f64(line: &str, key: &str) -> Option<f64> {
    let needle = format!("\"{key}\":");
    let start = line.find(&needle)?;
    let value_start = start + needle.len();
    let rest = &line[value_start..].trim_start();
    let end = rest
        .find(|c: char| [',', '}'].contains(&c))
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    rest[..end].parse().ok()
}

impl std::fmt::Display for CalibrationReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "[calibration] records={} labeled={}",
            self.records, self.labeled
        )?;
        for kind in &self.kinds {
            writeln!(
                f,
                "  {}: records={} labeled={}",
                kind.kind, kind.records, kind.labeled
            )?;
            for rung in &kind.rungs {
                match (rung.ece, rung.brier) {
                    (Some(ece), Some(brier)) => writeln!(
                        f,
                        "    rung {:<10} labeled={} ece={ece:.4} brier={brier:.4} mean_conf={:.4} acc={:.4}",
                        rung.rung,
                        rung.labeled,
                        rung.mean_confidence.unwrap_or(0.0),
                        rung.accuracy.unwrap_or(0.0),
                    )?,
                    _ => writeln!(
                        f,
                        "    rung {:<10} labeled={} labels required",
                        rung.rung, rung.labeled
                    )?,
                }
            }
        }
        for drift in &self.drift {
            writeln!(
                f,
                "  drift {}: recent={} prior={} confidence_drift={} rung_mix_drift={}",
                drift.kind,
                drift.recent_records,
                drift.prior_records,
                drift.confidence_drift,
                drift.rung_mix_drift,
            )?;
            if let Some(note) = &drift.note {
                writeln!(f, "    {note}")?;
            }
        }
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

    fn review_line(category: &str, source: &str, confidence: f64, ts: &str) -> String {
        format!(
            r#"{{"kind":"loop.turn.review","session_id":"s1","agent":"Sisyphus","intent_signal":"q","intent_category":"{category}","intent_source":"{source}","intent_confidence":{confidence},"intent_acl":"public","intent_llm_outcome":"ok","intent_llm_ms":100,"plan_approved":true,"delivery_ready":true,"feedback_rounds":0,"failed_attempts":0,"ts":"{ts}"}}"#
        )
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

    fn record_with_ts(kind: &str, rung: &str, confidence: f64, ts: &str) -> DecisionRecord {
        DecisionRecord {
            id: format!("{kind}-{rung}-{confidence}-{ts}"),
            timestamp: Some(ts.to_string()),
            kind: kind.to_string(),
            rung: rung.to_string(),
            confidence: Some(confidence),
            decision: None,
            label: None,
        }
    }

    #[test]
    fn extract_dataset_from_turn_review_lines() {
        let dir = tmpdir();
        write_audit(
            dir.path(),
            &[
                &review_line("Query", "Llm", 0.9, "2026-09-10T10:00:00Z"),
                &review_line("Action", "Keyword", 0.7, "2026-09-11T10:00:00Z"),
                "not json",
                r#"{"kind":"gateway.turn.started","turnId":"t1"}"#,
            ],
        );
        let records = extract_dataset(dir.path()).unwrap();
        assert_eq!(records.len(), 2);
        // dataset.jsonl written under decision-audit/
        let dataset_path = dir.path().join("decision-audit").join(DATASET_FILE);
        assert!(dataset_path.exists());
        // stable, distinct ids
        assert_ne!(records[0].id, records[1].id);
        // rung lowercased from intent_source
        assert_eq!(records[0].rung, "llm");
        assert_eq!(records[1].rung, "keyword");
        // kind + confidence + decision value
        assert_eq!(records[0].kind, "intent");
        assert_eq!(records[0].confidence, Some(0.9));
        assert_eq!(records[0].decision.as_deref(), Some("Query"));
        // timestamp preserved
        assert_eq!(
            records[0].timestamp.as_deref(),
            Some("2026-09-10T10:00:00Z")
        );
        // no labels in the extracted dataset
        assert!(records[0].label.is_none());
    }

    #[test]
    fn extract_dataset_missing_audit_is_empty() {
        let dir = tmpdir();
        let records = extract_dataset(dir.path()).unwrap();
        assert!(records.is_empty());
    }

    #[test]
    fn extract_dataset_skips_lines_without_rung() {
        let dir = tmpdir();
        write_audit(
            dir.path(),
            &[
                r#"{"kind":"loop.turn.review","intent_category":"Query","delivery_ready":true}"#,
                &review_line("Query", "Llm", 0.9, "2026-09-10T10:00:00Z"),
            ],
        );
        let records = extract_dataset(dir.path()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].rung, "llm");
    }

    #[test]
    fn labels_merge_by_record_id() {
        let dir = tmpdir();
        write_audit(
            dir.path(),
            &[&review_line("Query", "Llm", 0.9, "2026-09-10T10:00:00Z")],
        );
        let records = extract_dataset(dir.path()).unwrap();
        let id = records[0].id.clone();
        let labels_path = dir.path().join("decision-audit").join(LABELS_FILE);
        fs::create_dir_all(labels_path.parent().unwrap()).unwrap();
        fs::write(&labels_path, format!(r#"{{"id":"{id}","label":"Query"}}"#)).unwrap();
        let merged = load_dataset(dir.path()).unwrap();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].label.as_deref(), Some("Query"));
    }

    #[test]
    fn ece_brier_perfectly_calibrated_is_zero() {
        let pairs = vec![
            (1.0, true),
            (1.0, true),
            (1.0, true),
            (1.0, true),
            (1.0, true),
            (0.0, false),
            (0.0, false),
            (0.0, false),
            (0.0, false),
            (0.0, false),
        ];
        assert!(ece(&pairs) < 1e-9);
        assert!(brier(&pairs) < 1e-9);
    }

    #[test]
    fn ece_brier_overconfident_is_positive() {
        // 5× (0.9, wrong) + 5× (0.1, right): hand-computed ECE = 0.9,
        // Brier = 0.81.
        let pairs = vec![
            (0.9, false),
            (0.9, false),
            (0.9, false),
            (0.9, false),
            (0.9, false),
            (0.1, true),
            (0.1, true),
            (0.1, true),
            (0.1, true),
            (0.1, true),
        ];
        assert!((ece(&pairs) - 0.9).abs() < 1e-9);
        assert!((brier(&pairs) - 0.81).abs() < 1e-9);
    }

    #[test]
    fn ece_brier_mid_vector_hand_computed() {
        // 4× (0.5, right) + 6× (0.5, wrong): bin 5 conf 0.5 acc 0.4 →
        // ECE = 0.1; Brier = 0.25.
        let pairs = vec![
            (0.5, true),
            (0.5, true),
            (0.5, true),
            (0.5, true),
            (0.5, false),
            (0.5, false),
            (0.5, false),
            (0.5, false),
            (0.5, false),
            (0.5, false),
        ];
        assert!((ece(&pairs) - 0.1).abs() < 1e-9);
        assert!((brier(&pairs) - 0.25).abs() < 1e-9);
    }

    #[test]
    fn zero_labels_reports_labels_required() {
        let records = vec![
            record("a", "intent", "llm", Some(0.9), Some("Query"), None),
            record("b", "intent", "keyword", Some(0.7), Some("Action"), None),
        ];
        let report = compute(&records);
        assert_eq!(report.labeled, 0);
        let kind = report.kinds.iter().find(|k| k.kind == "intent").unwrap();
        assert!(kind.labels_required);
        for rung in &kind.rungs {
            assert!(rung.labels_required);
            assert!(rung.ece.is_none());
            assert!(rung.brier.is_none());
        }
        // Display must say "labels required", never print a number.
        let display = report.to_string();
        assert!(display.contains("labels required"));
        assert!(!display.contains("ece="));
    }

    #[test]
    fn calibration_uses_labels_only() {
        // One labeled record (correct) + one unlabeled record: metrics must
        // be computed over the labeled record only.
        let records = vec![
            record(
                "a",
                "intent",
                "llm",
                Some(0.9),
                Some("Query"),
                Some("Query"),
            ),
            record("b", "intent", "llm", Some(0.9), Some("Query"), None),
        ];
        let report = compute(&records);
        let kind = report.kinds.iter().find(|k| k.kind == "intent").unwrap();
        assert_eq!(kind.records, 2);
        assert_eq!(kind.labeled, 1);
        let rung = kind.rungs.iter().find(|r| r.rung == "llm").unwrap();
        assert_eq!(rung.records, 2);
        assert_eq!(rung.labeled, 1);
        assert!(!rung.labels_required);
        // Single correct record at conf 0.9: ECE = |1.0 − 0.9| = 0.1,
        // Brier = (0.9 − 1)² = 0.01.
        assert!((rung.ece.unwrap() - 0.1).abs() < 1e-9);
        assert!((rung.brier.unwrap() - 0.01).abs() < 1e-9);
    }

    #[test]
    fn drift_stable_windows_no_signal() {
        let now = Utc::now();
        let recent_ts = (now - Duration::days(1)).to_rfc3339();
        let prior_ts = (now - Duration::days(10)).to_rfc3339();
        let records = vec![
            record_with_ts("intent", "llm", 0.7, &recent_ts),
            record_with_ts("intent", "llm", 0.7, &recent_ts),
            record_with_ts("intent", "llm", 0.7, &prior_ts),
            record_with_ts("intent", "llm", 0.7, &prior_ts),
        ];
        let report = compute(&records);
        let drift = report.drift.iter().find(|d| d.kind == "intent").unwrap();
        assert_eq!(drift.recent_records, 2);
        assert_eq!(drift.prior_records, 2);
        assert!(!drift.confidence_drift);
        assert!(!drift.rung_mix_drift);
        assert!(drift.note.is_none());
    }

    #[test]
    fn drift_shifted_windows_flag_signal() {
        let now = Utc::now();
        let recent_ts = (now - Duration::days(1)).to_rfc3339();
        let prior_ts = (now - Duration::days(10)).to_rfc3339();
        let records = vec![
            record_with_ts("intent", "llm", 0.9, &recent_ts),
            record_with_ts("intent", "llm", 0.9, &recent_ts),
            record_with_ts("intent", "keyword", 0.5, &prior_ts),
            record_with_ts("intent", "keyword", 0.5, &prior_ts),
        ];
        let report = compute(&records);
        let drift = report.drift.iter().find(|d| d.kind == "intent").unwrap();
        // |0.9 − 0.5| = 0.4 > 0.1 tolerance.
        assert!(drift.confidence_drift);
        // llm share 100% vs 0% → |100 − 0| = 100 > 20 pp tolerance.
        assert!(drift.rung_mix_drift);
    }

    #[test]
    fn drift_empty_windows_is_first_class() {
        let records = vec![record("a", "intent", "llm", Some(0.9), None, None)];
        let report = compute(&records);
        let drift = report.drift.iter().find(|d| d.kind == "intent").unwrap();
        assert_eq!(drift.recent_records, 0);
        assert_eq!(drift.prior_records, 0);
        assert!(!drift.confidence_drift);
        assert!(!drift.rung_mix_drift);
        assert!(drift.note.is_some());
    }

    #[test]
    fn report_round_trips_through_json() {
        let records = vec![record(
            "a",
            "intent",
            "llm",
            Some(0.9),
            Some("Query"),
            Some("Query"),
        )];
        let report = compute(&records);
        let json = serde_json::to_string(&report).unwrap();
        let back: CalibrationReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back.records, 1);
        assert_eq!(back.labeled, 1);
        assert_eq!(back.kinds[0].rungs[0].ece, report.kinds[0].rungs[0].ece);
        // Display renders the section header.
        assert!(report.to_string().contains("[calibration]"));
    }
}
