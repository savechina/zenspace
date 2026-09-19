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
use std::path::{Path, PathBuf};

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

// ---------------------------------------------------------------------------
// Threshold calibration (T173 → T168-T171): labels in, operating points out
// ---------------------------------------------------------------------------

/// Subdirectory holding the label interface and the calibration artefact.
pub const DECISION_AUDIT_DIR: &str = "decision-audit";

/// Calibrated operating points consumed by the decision gates. This is the
/// filename `zen-agents::decision::DecisionThresholds::load` reads.
pub const THRESHOLDS_FILE: &str = "thresholds.json";

/// Minimum labeled records before ANY operating point may be emitted. Below
/// this the calibration refuses rather than fitting noise — a threshold chosen
/// from a handful of labels is worse than a closed gate.
pub const MIN_LABELS_FOR_CALIBRATION: usize = 100;

/// Minimum precision on the accepted set for a candidate threshold.
pub const MIN_ACCEPTED_PRECISION: f64 = 0.95;

/// Minimum share of labeled records a candidate threshold must accept, so a
/// "perfect" threshold that accepts nothing cannot masquerade as calibrated.
pub const MIN_ACCEPTED_COVERAGE: f64 = 0.10;

/// Minimum precision improvement over the base rate required to conclude that
/// confidence carries any signal at all. V13-A.3's "never gate on verbalized
/// confidence alone" (arXiv 2601.07767) is enforced here: a confidence that
/// does not separate correct from incorrect decisions yields NO threshold,
/// however high its numbers look.
pub const MIN_PRECISION_LIFT: f64 = 0.05;

/// Rounding scale used when persisting a threshold (4 decimals).
pub const THRESHOLD_SCALE: f64 = 10_000.0;

/// The objective a calibrated operating point must satisfy.
#[derive(Debug, Clone, Copy)]
pub struct CalibrationTarget {
    pub min_precision: f64,
    pub min_coverage: f64,
    pub min_labels: usize,
    pub min_precision_lift: f64,
}

impl Default for CalibrationTarget {
    fn default() -> Self {
        Self {
            min_precision: MIN_ACCEPTED_PRECISION,
            min_coverage: MIN_ACCEPTED_COVERAGE,
            min_labels: MIN_LABELS_FOR_CALIBRATION,
            min_precision_lift: MIN_PRECISION_LIFT,
        }
    }
}

/// One candidate operating point: accepting every decision at or above
/// `threshold` yields this precision and coverage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThresholdPoint {
    pub threshold: f32,
    pub precision: f64,
    pub coverage: f64,
    pub accepted: usize,
}

/// A selected, defensible operating point with its risk audit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibratedThreshold {
    /// Threshold field name in `thresholds.json` (e.g. `"intent_l1"`).
    pub field: String,
    /// Source decision kind the sample came from.
    pub kind: String,
    /// Source rung the sample came from.
    pub rung: String,
    pub threshold: f32,
    pub precision: f64,
    pub coverage: f64,
    /// Precision of the gate being *closed* (accept everything) — the
    /// baseline the lift is measured against.
    pub base_rate: f64,
    pub precision_lift: f64,
    /// ECE over the accepted set at the selected threshold.
    pub ece: Option<f64>,
    pub labeled: usize,
    pub accepted: usize,
}

/// Why a threshold was — or was not — selected.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ThresholdOutcome {
    /// A defensible operating point was selected.
    Selected(CalibratedThreshold),
    /// Not enough labeled records.
    InsufficientLabels { labeled: usize, required: usize },
    /// Confidence does not separate correct from incorrect decisions, so no
    /// gate may be derived from it (V13-A.3).
    UninformativeConfidence {
        base_rate: f64,
        best_precision: f64,
        required_lift: f64,
    },
    /// No candidate met both the precision target and the coverage floor.
    CannotMeetTarget {
        best: Option<ThresholdPoint>,
        required_precision: f64,
        required_coverage: f64,
    },
}

impl ThresholdOutcome {
    /// The selected operating point, if any.
    pub fn selected(&self) -> Option<&CalibratedThreshold> {
        match self {
            Self::Selected(threshold) => Some(threshold),
            _ => None,
        }
    }

    /// Short machine-readable state name.
    pub fn state(&self) -> &'static str {
        match self {
            Self::Selected(_) => "selected",
            Self::InsufficientLabels { .. } => "insufficient_labels",
            Self::UninformativeConfidence { .. } => "uninformative_confidence",
            Self::CannotMeetTarget { .. } => "cannot_meet_target",
        }
    }
}

/// Which `(kind, rung)` sample calibrates which threshold field.
///
/// The rung filter is load-bearing: an `intent` record produced by the LLM or
/// keyword rung carries a *different* confidence than the L1 rung's, so
/// calibrating `intent_l1` from it would fit the wrong signal entirely.
#[derive(Debug, Clone, Copy)]
pub struct ThresholdSource {
    pub field: &'static str,
    pub kind: &'static str,
    pub rung: &'static str,
}

/// The four gates the ladder exposes, each with its calibration sample.
pub const THRESHOLD_SOURCES: &[ThresholdSource] = &[
    ThresholdSource {
        field: "intent_l1",
        kind: "intent",
        rung: "L1",
    },
    ThresholdSource {
        field: "correction_l1",
        kind: "correction",
        rung: "L1",
    },
    ThresholdSource {
        field: "citation_l1",
        kind: "citation",
        rung: "L1",
    },
    ThresholdSource {
        field: "review_escalate",
        kind: "review",
        rung: "L1",
    },
];

/// Calibration outcome for one threshold field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldCalibration {
    pub field: String,
    pub kind: String,
    pub rung: String,
    /// Labeled records in this field's sample.
    pub labeled: usize,
    pub outcome: ThresholdOutcome,
}

impl FieldCalibration {
    pub fn state(&self) -> &'static str {
        self.outcome.state()
    }
}

/// Select an operating point from `(confidence, correct)` pairs.
///
/// The objective is the Trust-or-Escalate one: among thresholds meeting the
/// precision target, prefer the one that accepts the most decisions. A
/// threshold below which the gate would accept a wrong decision is a
/// mis-accept, so precision-on-accepted is the quantity that matters — not
/// aggregate accuracy.
pub fn select_threshold(pairs: &[(f64, bool)], target: CalibrationTarget) -> ThresholdOutcome {
    let labeled = pairs.len();
    if labeled < target.min_labels {
        return ThresholdOutcome::InsufficientLabels {
            labeled,
            required: target.min_labels,
        };
    }

    let correct = pairs.iter().filter(|(_, correct)| *correct).count();
    let base_rate = correct as f64 / labeled as f64;

    let mut candidates: Vec<f32> = pairs
        .iter()
        .map(|(confidence, _)| *confidence as f32)
        .collect();
    candidates.sort_by(|a, b| a.total_cmp(b));
    candidates.dedup();

    let mut points: Vec<ThresholdPoint> = Vec::new();
    for threshold in candidates {
        let accepted: Vec<&(f64, bool)> = pairs
            .iter()
            .filter(|(confidence, _)| *confidence as f32 >= threshold)
            .collect();
        if accepted.is_empty() {
            continue;
        }
        let accepted_correct = accepted.iter().filter(|(_, correct)| *correct).count();
        points.push(ThresholdPoint {
            threshold,
            precision: accepted_correct as f64 / accepted.len() as f64,
            coverage: accepted.len() as f64 / labeled as f64,
            accepted: accepted.len(),
        });
    }

    let best_precision = points
        .iter()
        .map(|point| point.precision)
        .fold(f64::NEG_INFINITY, f64::max);
    if !best_precision.is_finite() || best_precision - base_rate < target.min_precision_lift {
        return ThresholdOutcome::UninformativeConfidence {
            base_rate,
            best_precision: if best_precision.is_finite() {
                best_precision
            } else {
                0.0
            },
            required_lift: target.min_precision_lift,
        };
    }

    let eligible: Vec<&ThresholdPoint> = points
        .iter()
        .filter(|point| {
            point.precision >= target.min_precision && point.coverage >= target.min_coverage
        })
        .collect();

    let Some(chosen) = eligible
        .iter()
        .copied()
        // Max coverage; ties resolved by the lowest threshold (iteration order
        // is ascending, so strict `>` keeps the first/most-accepting point),
        // which keeps selection deterministic.
        .fold(None::<&ThresholdPoint>, |best, point| match best {
            Some(current) if current.coverage >= point.coverage => Some(current),
            _ => Some(point),
        })
    else {
        let best = points
            .iter()
            .max_by(|a, b| a.precision.total_cmp(&b.precision))
            .cloned();
        return ThresholdOutcome::CannotMeetTarget {
            best,
            required_precision: target.min_precision,
            required_coverage: target.min_coverage,
        };
    };

    let accepted_pairs: Vec<(f64, bool)> = pairs
        .iter()
        .filter(|(confidence, _)| *confidence as f32 >= chosen.threshold)
        .copied()
        .collect();

    ThresholdOutcome::Selected(CalibratedThreshold {
        field: String::new(),
        kind: String::new(),
        rung: String::new(),
        threshold: chosen.threshold,
        precision: chosen.precision,
        coverage: chosen.coverage,
        base_rate,
        precision_lift: chosen.precision - base_rate,
        ece: Some(ece(&accepted_pairs)),
        labeled,
        accepted: chosen.accepted,
    })
}

/// Calibrate one threshold field from its `(kind, rung)` sample.
pub fn calibrate_field(
    records: &[DecisionRecord],
    source: &ThresholdSource,
    target: CalibrationTarget,
) -> FieldCalibration {
    let pairs: Vec<(f64, bool)> = records
        .iter()
        .filter(|record| record.kind == source.kind && record.rung == source.rung)
        .filter_map(|record| {
            let confidence = record.confidence?;
            let correct = is_correct(record)?;
            Some((confidence, correct))
        })
        .collect();

    let labeled = pairs.len();
    let outcome = select_threshold(&pairs, target);
    let outcome = match outcome {
        ThresholdOutcome::Selected(mut selected) => {
            selected.field = source.field.to_string();
            selected.kind = source.kind.to_string();
            selected.rung = source.rung.to_string();
            ThresholdOutcome::Selected(selected)
        }
        other => other,
    };

    FieldCalibration {
        field: source.field.to_string(),
        kind: source.kind.to_string(),
        rung: source.rung.to_string(),
        labeled,
        outcome,
    }
}

/// Calibrate every threshold field the ladder exposes.
pub fn calibrate(records: &[DecisionRecord], target: CalibrationTarget) -> Vec<FieldCalibration> {
    THRESHOLD_SOURCES
        .iter()
        .map(|source| calibrate_field(records, source, target))
        .collect()
}

/// Path of the calibration artefact under `<logs>/decision-audit/`.
pub fn thresholds_path(logs_dir: &Path) -> PathBuf {
    logs_dir.join(DECISION_AUDIT_DIR).join(THRESHOLDS_FILE)
}

/// Merge-write selected operating points into the calibration artefact.
///
/// Merge semantics are deliberate: a field that produced no defensible
/// threshold this run keeps its previous value, so recalibrating one gate can
/// never silently close (or open) an unrelated one. The write is atomic
/// (tmp + rename), matching the repo's artefact-write discipline.
///
/// Only [`ThresholdOutcome::Selected`] fields are written — a refusal is never
/// persisted as a number.
pub fn write_thresholds(
    logs_dir: &Path,
    calibrations: &[FieldCalibration],
) -> Result<PathBuf, DecisionAuditError> {
    let path = thresholds_path(logs_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut root: serde_json::Map<String, serde_json::Value> = match std::fs::read_to_string(&path)
    {
        Ok(raw) => serde_json::from_str::<serde_json::Value>(&raw)
            .ok()
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default(),
        Err(_) => serde_json::Map::new(),
    };

    for calibration in calibrations {
        if let Some(selected) = calibration.outcome.selected() {
            // Persist to 4 decimals. `serde_json::Value` stores numbers as f64,
            // so an f32 0.95 would otherwise be written as 0.949999988079071 —
            // more precision than a confidence gate needs, and unreadable to
            // the human who has to audit the artefact.
            let rounded =
                (f64::from(selected.threshold) * THRESHOLD_SCALE).round() / THRESHOLD_SCALE;
            root.insert(calibration.field.clone(), serde_json::json!(rounded));
        }
    }

    let body = serde_json::to_string_pretty(&serde_json::Value::Object(root))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body.as_bytes())?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
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
    // ---- threshold calibration (T173 -> T168-T171) ----

    /// Labeled record whose label equals its decision — i.e. a correct
    /// decision, which is what a calibration sample grades against.
    fn calibration_record(
        kind: &str,
        rung: &str,
        confidence: f64,
        decision: &str,
    ) -> DecisionRecord {
        record(
            &format!("{kind}-{rung}-{confidence}-{decision}"),
            kind,
            rung,
            Some(confidence),
            Some(decision),
            Some(decision),
        )
    }

    /// Confidence that separates correct from incorrect decisions is the
    /// precondition for any gate at all.
    fn informative_pairs() -> Vec<(f64, bool)> {
        let mut pairs: Vec<(f64, bool)> = (0..80).map(|_| (0.95, true)).collect();
        pairs.extend((0..5).map(|_| (0.30, true)));
        pairs.extend((0..15).map(|_| (0.30, false)));
        pairs
    }

    #[test]
    fn calibration_refuses_below_the_minimum_sample() {
        let pairs: Vec<(f64, bool)> = (0..10).map(|_| (0.99, true)).collect();
        let outcome = select_threshold(&pairs, CalibrationTarget::default());
        assert_eq!(outcome.state(), "insufficient_labels");
        assert!(outcome.selected().is_none());
    }

    #[test]
    fn calibration_selects_the_widest_accepting_threshold_meeting_precision() {
        let outcome = select_threshold(&informative_pairs(), CalibrationTarget::default());
        let selected = outcome.selected().expect("a defensible threshold");

        assert_eq!(selected.threshold, 0.95);
        assert_eq!(selected.precision, 1.0);
        assert!((selected.coverage - 0.8).abs() < 1e-9);
        assert!(selected.precision_lift >= MIN_PRECISION_LIFT);
        assert!(selected.ece.is_some());
    }

    #[test]
    fn calibration_refuses_uninformative_confidence() {
        // Correctness is independent of confidence: half the confident
        // decisions are right, half the unconfident ones too. A gate derived
        // from this would be a coin flip wearing a number (V13-A.3 /
        // arXiv 2601.07767).
        let mut pairs: Vec<(f64, bool)> = (0..50).map(|i| (0.9, i % 2 == 0)).collect();
        pairs.extend((0..50).map(|i| (0.2, i % 2 == 0)));

        let outcome = select_threshold(&pairs, CalibrationTarget::default());
        assert_eq!(outcome.state(), "uninformative_confidence");
        assert!(outcome.selected().is_none());
    }

    #[test]
    fn calibration_refuses_a_perfect_gate_that_accepts_almost_nothing() {
        // The 0.99 candidate is perfectly precise but covers 2.5% of the
        // sample, below the coverage floor: high precision bought by refusing
        // to decide is not calibration.
        let mut pairs: Vec<(f64, bool)> = (0..5).map(|_| (0.99, true)).collect();
        pairs.extend((0..100).map(|_| (0.20, true)));
        pairs.extend((0..95).map(|_| (0.20, false)));

        let outcome = select_threshold(&pairs, CalibrationTarget::default());
        assert_eq!(outcome.state(), "cannot_meet_target");
        assert!(outcome.selected().is_none());
    }

    #[test]
    fn calibrate_field_filters_to_its_own_kind_and_rung() {
        let source = ThresholdSource {
            field: "intent_l1",
            kind: "intent",
            rung: "L1",
        };
        // The 80 confident decisions are correct; the 20 timid ones are wrong.
        // Label != decision there, which is what makes the confidence signal
        // measurable at all.
        let mut records: Vec<DecisionRecord> = (0..MIN_LABELS_FOR_CALIBRATION)
            .map(|i| {
                let (confidence, correct) = if i < 80 { (0.95, true) } else { (0.30, false) };
                record(
                    &format!("intent-L1-{i}"),
                    "intent",
                    "L1",
                    Some(confidence),
                    Some("Query"),
                    Some(if correct { "Query" } else { "Action" }),
                )
            })
            .collect();
        // A different rung and a different kind must not contaminate the sample.
        records.push(calibration_record("intent", "llm", 0.99, "Query"));
        records.push(calibration_record("correction", "L1", 0.99, "yes"));

        let calibration = calibrate_field(&records, &source, CalibrationTarget::default());
        assert_eq!(calibration.field, "intent_l1");
        assert_eq!(calibration.labeled, MIN_LABELS_FOR_CALIBRATION);
        let selected = calibration.outcome.selected().expect("selected");
        assert_eq!(selected.kind, "intent");
        assert_eq!(selected.rung, "L1");
        assert_eq!(selected.threshold, 0.95);
    }

    #[test]
    fn calibrate_field_reports_insufficient_when_the_sample_is_absent() {
        let records = vec![calibration_record("intent", "llm", 0.9, "Query")];
        let calibration = calibrate(&records, CalibrationTarget::default());
        assert_eq!(calibration.len(), THRESHOLD_SOURCES.len());
        assert!(
            calibration
                .iter()
                .all(|field| field.state() == "insufficient_labels"),
            "every gate must report insufficient rather than a number"
        );
    }

    #[test]
    fn write_thresholds_merges_and_never_persists_a_refusal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let logs = dir.path();

        // Pre-existing artefact: an unrelated gate plus a stale value for one
        // we are about to recalibrate.
        let dir_path = logs.join(DECISION_AUDIT_DIR);
        std::fs::create_dir_all(&dir_path).expect("mkdir");
        std::fs::write(
            thresholds_path(logs),
            r#"{"citation_l1": 0.42, "review_escalate": 0.11}"#,
        )
        .expect("seed");

        let records: Vec<DecisionRecord> = (0..MIN_LABELS_FOR_CALIBRATION)
            .map(|i| {
                let (confidence, correct) = if i < 80 { (0.95, true) } else { (0.30, false) };
                record(
                    &format!("intent-L1-{i}"),
                    "intent",
                    "L1",
                    Some(confidence),
                    Some("Query"),
                    Some(if correct { "Query" } else { "Action" }),
                )
            })
            .collect();
        let calibrations = calibrate(&records, CalibrationTarget::default());

        let path = write_thresholds(logs, &calibrations).expect("write");
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json");

        assert_eq!(
            written["intent_l1"], 0.95,
            "the calibrated field is written"
        );
        assert_eq!(
            written["citation_l1"], 0.42,
            "an unrecalibrated field keeps its previous value"
        );
        assert_eq!(written["review_escalate"], 0.11);
        assert!(
            written.get("correction_l1").is_none(),
            "a refusal must never be persisted as a number"
        );
    }
}
