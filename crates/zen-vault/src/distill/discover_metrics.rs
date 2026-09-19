//! RSI health metrics for the Discover Loop (PD-04, G7-minimal).
//!
//! [`aggregate`] folds per-cycle [`LoopCycleReport`]s into a single
//! [`DiscoverMetrics`] snapshot: hypothesis velocity plus promotion, rejection,
//! and precipitation rates. The module is path-agnostic — callers pass the
//! ZenPaths-resolved logs dir to [`load_reports`]; no `~/.zen` / `.zentest`
//! literals live here.
//!
//! # Rate definitions
//!
//! * `hypothesis_velocity` = Σ hypotheses_generated / window_cycles
//! * `promotion_rate` = Σ hypotheses_validated / Σ hypotheses_generated
//! * `rejection_rate` = Σ hypotheses_rejected / Σ decided,
//!   where decided = validated + rejected
//! * `precipitation_rate` = Σ skills_precipitated / window_cycles
//!
//! All rates are 0.0 when their denominator is 0 (including empty history).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::types::LoopCycleReport;

/// Errors from RSI metrics file I/O.
#[derive(Debug, Error)]
pub enum DiscoverMetricsError {
    /// Filesystem error (missing dir is NOT an error — see [`load_reports`]).
    #[error("rsi metrics I/O error: {0}")]
    Io(#[from] io::Error),
    /// JSON serialization/deserialization error.
    #[error("rsi metrics JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Recurrent self-improvement health snapshot over a report window.
///
/// All rates are unitless ratios in `0.0..=1.0` except
/// [`DiscoverMetrics::hypothesis_velocity`], which is hypotheses per cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverMetrics {
    /// Number of cycle reports in the aggregation window.
    pub window_cycles: usize,
    /// Mean hypotheses generated per cycle.
    pub hypothesis_velocity: f64,
    /// Validated / generated (0.0 when nothing generated).
    pub promotion_rate: f64,
    /// Rejected / decided, decided = validated + rejected (0.0 when undecided).
    pub rejection_rate: f64,
    /// Mean skills precipitated per cycle.
    pub precipitation_rate: f64,
    /// Snapshot wall-clock time (RFC 3339).
    pub generated_at: String,
}

/// Aggregate cycle reports into RSI health metrics.
///
/// # Arguments
///
/// * `reports` — Cycle reports in any order (order-insensitive).
///
/// # Returns
///
/// Zero metrics (no panic) for empty input.
///
/// # Examples
///
/// ```
/// use zen_vault::distill::discover_metrics::aggregate;
/// use zen_vault::distill::types::LoopCycleReport;
///
/// let m = aggregate(&[]);
/// assert_eq!(m.window_cycles, 0);
/// assert_eq!(m.promotion_rate, 0.0);
/// ```
pub fn aggregate(reports: &[LoopCycleReport]) -> DiscoverMetrics {
    let now = Utc::now().to_rfc3339();
    if reports.is_empty() {
        return DiscoverMetrics {
            window_cycles: 0,
            hypothesis_velocity: 0.0,
            promotion_rate: 0.0,
            rejection_rate: 0.0,
            precipitation_rate: 0.0,
            generated_at: now,
        };
    }
    let n = reports.len() as f64;
    let generated: usize = reports.iter().map(|r| r.hypotheses_generated).sum();
    let validated: usize = reports.iter().map(|r| r.hypotheses_validated).sum();
    let rejected: usize = reports.iter().map(|r| r.hypotheses_rejected).sum();
    let precipitated: usize = reports.iter().map(|r| r.skills_precipitated).sum();
    let decided = (validated + rejected) as f64;
    DiscoverMetrics {
        window_cycles: reports.len(),
        hypothesis_velocity: generated as f64 / n,
        promotion_rate: if generated == 0 {
            0.0
        } else {
            validated as f64 / generated as f64
        },
        rejection_rate: if decided == 0.0 {
            0.0
        } else {
            rejected as f64 / decided
        },
        precipitation_rate: precipitated as f64 / n,
        generated_at: now,
    }
}

/// Per-cycle snapshot filename prefix read by [`load_reports`].
pub const REPORT_FILE_PREFIX: &str = "loop-report-";

/// How many most-recent cycle reports [`load_reports`] returns.
///
/// One report is written per discover cycle (daily), each embedding the
/// cycle's full gap vector; without a window the aggregate re-reads and
/// re-parses unbounded history. Metrics therefore roll over the most
/// recent [`REPORT_WINDOW`] cycles.
pub const REPORT_WINDOW: usize = 90;

/// Load `loop-report-*.json` in `dir`, sorted by filename, newest last.
///
/// A missing directory yields an empty vec (fresh state is not an error);
/// a malformed file is an error so corruption never silently skews rates.
/// At most the [`REPORT_WINDOW`] most recent reports are returned — older
/// cycles age out of the aggregate.
///
/// # Arguments
///
/// * `dir` — Caller-resolved logs dir (e.g. from ZenPaths).
///
/// # Errors
///
/// Returns [`DiscoverMetricsError`] on I/O (other than missing dir) or JSON failure.
pub fn load_reports(dir: &Path) -> Result<Vec<LoopCycleReport>, DiscoverMetricsError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(DiscoverMetricsError::Io(e)),
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().map(|ext| ext == "json").unwrap_or(false)
                && path
                    .file_name()
                    .map(|name| name.to_string_lossy().starts_with(REPORT_FILE_PREFIX))
                    .unwrap_or(false)
        })
        .collect();
    paths.sort();
    if paths.len() > REPORT_WINDOW {
        paths.drain(..paths.len() - REPORT_WINDOW);
    }
    let mut reports: Vec<LoopCycleReport> = Vec::with_capacity(paths.len());
    for path in paths {
        let content = fs::read_to_string(&path)?;
        reports.push(serde_json::from_str(&content)?);
    }
    // zen_loop overwrites a single last-report per cycle (PD-06 fusion).
    // Upsert it by cycle_id so metrics see live cycles even when per-cycle
    // report files are absent, without double-counting the newest cycle.
    let last = dir.join("loop-last-report.json");
    if let Ok(content) = fs::read_to_string(&last)
        && let Ok(report) = serde_json::from_str::<LoopCycleReport>(&content)
    {
        match reports
            .iter_mut()
            .find(|existing| existing.cycle_id == report.cycle_id)
        {
            Some(existing) => *existing = report,
            None => reports.push(report),
        }
    }
    Ok(reports)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::distill::types::LoopCycleReport;

    fn report(
        generated: usize,
        validated: usize,
        rejected: usize,
        precipitated: usize,
    ) -> LoopCycleReport {
        LoopCycleReport {
            hypotheses_generated: generated,
            hypotheses_validated: validated,
            hypotheses_rejected: rejected,
            skills_precipitated: precipitated,
            ..Default::default()
        }
    }

    #[test]
    fn aggregate_computes_exact_rates() {
        let reports = vec![report(4, 2, 1, 3), report(2, 1, 1, 0), report(0, 0, 0, 0)];
        let m = aggregate(&reports);
        assert_eq!(m.window_cycles, 3);
        // velocity = 6/3
        assert!((m.hypothesis_velocity - 2.0).abs() < 1e-9);
        // promotion = 3/6
        assert!((m.promotion_rate - 0.5).abs() < 1e-9);
        // rejection = 2/5
        assert!((m.rejection_rate - 0.4).abs() < 1e-9);
        // precipitation = 3/3
        assert!((m.precipitation_rate - 1.0).abs() < 1e-9);
        assert!(!m.generated_at.is_empty());
    }

    #[test]
    fn aggregate_empty_history_is_zero() {
        let m = aggregate(&[]);
        assert_eq!(m.window_cycles, 0);
        assert_eq!(m.hypothesis_velocity, 0.0);
        assert_eq!(m.promotion_rate, 0.0);
        assert_eq!(m.rejection_rate, 0.0);
        assert_eq!(m.precipitation_rate, 0.0);
    }

    #[test]
    fn load_missing_dir_is_empty_not_error() {
        let missing = Path::new("/definitely/not/here/rsi-test-logs");
        let reports = load_reports(missing).unwrap();
        assert!(reports.is_empty());
    }

    #[test]
    fn load_reports_windows_to_most_recent() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..92 {
            let r = report(i, 0, 0, 0);
            let path = dir.path().join(format!("loop-report-{i:04}.json"));
            fs::write(&path, serde_json::to_string_pretty(&r).unwrap()).unwrap();
        }
        let loaded = load_reports(dir.path()).unwrap();
        assert_eq!(loaded.len(), REPORT_WINDOW);
        // Oldest cycles age out; the most recent stay, newest last.
        assert_eq!(loaded.first().unwrap().hypotheses_generated, 2);
        assert_eq!(loaded.last().unwrap().hypotheses_generated, 91);
    }

    #[test]
    fn load_and_aggregate_preserves_values() {
        let dir = tempfile::tempdir().unwrap();
        let reports = [report(4, 2, 1, 3), report(2, 2, 0, 1)];
        for (i, r) in reports.iter().enumerate() {
            let path = dir.path().join(format!("loop-report-{i}.json"));
            fs::write(&path, serde_json::to_string_pretty(r).unwrap()).unwrap();
        }
        // A non-report JSON file must be ignored.
        fs::write(dir.path().join("other.json"), "{}").unwrap();
        let loaded = load_reports(dir.path()).unwrap();
        assert_eq!(loaded.len(), 2);
        let m = aggregate(&loaded);
        assert_eq!(m.window_cycles, 2);
        assert!((m.promotion_rate - 4.0 / 6.0).abs() < 1e-9);
    }
}
