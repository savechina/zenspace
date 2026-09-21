//! Success-criteria measurements for the spec's SC success criteria (T185).
//!
//! # Functionality
//! Computes SC-002's first-attempt success rate — the share of completed
//! (archived) inbox notes that finished distillation on their FIRST attempt —
//! as an *observation* over the real audit sink (`<logs>/audit.jsonl`
//! `loop.note.archived` lines and their `attempts` field). The observation
//! carries its sample size `n` and an explicit [`InsufficientSample`] state
//! below [`MIN_SAMPLE`], mirroring [`super::baselines`].
//!
//! # Source choice (most authoritative evidence)
//! The per-note `attempts` field on `loop.note.archived` is used, NOT the
//! `loop-attempts.json` retry ledger or the cycle report's aggregate
//! counters:
//! - the retry ledger only holds *unfinished* business — entries for notes
//!   that later succeeded are indistinguishable from still-pending ones and
//!   carry no completion event;
//! - `LoopCycleReport` counters (`archived_count`, `quarantined_count`) are
//!   per-cycle aggregates with no attempt dimension;
//! - the archive audit line is emitted exactly once per completed note
//!   (FR-018 reversibility record), so it IS the completion ledger, and the
//!   `attempts` field records the retry-ledger depth at completion time
//!   (prior failed cycles + the successful one).
//!
//! # Default behavior
//! A missing log file, an unreadable directory, or a corrupt line degrades to
//! insufficient/`None` — never a panic, never a fabricated percentage.
//! Historical lines written before the `attempts` field existed carry no
//! attempt data; they are counted in [`FirstAttemptSuccess::legacy_lines_skipped`]
//! and excluded from `n` rather than assumed first-attempt.
//!
//! # Interaction
//! Read by `zen discover report` (additive `success_criteria` section). Same
//! audit file and field-extractor discipline as [`super::baselines`],
//! [`super::orchestration_stats`] and [`super::decision_audit`]. Budget
//! deferrals (pending-pool requeues) never increment the retry ledger, so a
//! deferred-then-archived note still counts as first-attempt — a deferral is
//! not a failed attempt.

use std::fs;
use std::io::{self, BufRead};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::baselines::{InsufficientSample, MIN_SAMPLE, read_field_u64};

/// SC-002 first-attempt success observation over one audit log.
///
/// `first_attempt_success_pct` is `None` in exactly one case: `n` is below
/// [`MIN_SAMPLE`] — then `insufficient` is `Some` and the percentage is
/// withheld, never defaulted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirstAttemptSuccess {
    /// Percentage (0.0–100.0) of completed notes archived on their first
    /// attempt; `None` when `n < MIN_SAMPLE`.
    pub first_attempt_success_pct: Option<f64>,
    /// Completed (archived) notes carrying attempt data — the denominator.
    pub n: usize,
    /// Notes archived with `attempts == 1` — the numerator.
    pub first_attempt: usize,
    /// `Some` when `n < MIN_SAMPLE` — the percentage is withheld.
    pub insufficient: Option<InsufficientSample>,
    /// Archived lines without an `attempts` field (written before the
    /// recording existed); excluded from `n`, never assumed first-attempt.
    pub legacy_lines_skipped: usize,
    /// Audit line kind + field the observation is derived from.
    pub sources: Vec<String>,
    /// Caveats about what the observation covers.
    pub notes: Vec<String>,
}

fn sources() -> Vec<String> {
    vec!["loop.note.archived.attempts".to_string()]
}

fn notes() -> Vec<String> {
    vec![
        "window = the whole audit.jsonl (no time slice); attempts = prior failed cycles + the successful one"
            .to_string(),
        "budget-deferred notes (pending pool) are not failed attempts and count as first-attempt once archived"
            .to_string(),
        "quarantined notes never complete, so they are not in the denominator (SC-002 measures completed notes)"
            .to_string(),
    ]
}

/// Compute SC-002's `first_attempt_success_pct` from `<logs_dir>/audit.jsonl`.
///
/// Pure and fail-open: a missing file, unreadable directory, or corrupt line
/// degrades to an insufficient observation — never a panic, never a fabricated
/// number below [`MIN_SAMPLE`].
pub fn compute_first_attempt_success(logs_dir: &Path) -> FirstAttemptSuccess {
    let audit_path = logs_dir.join("audit.jsonl");
    let file = match fs::File::open(&audit_path) {
        Ok(f) => f,
        Err(_) => {
            return FirstAttemptSuccess {
                first_attempt_success_pct: None,
                n: 0,
                first_attempt: 0,
                insufficient: Some(InsufficientSample {
                    n: 0,
                    required: MIN_SAMPLE,
                }),
                legacy_lines_skipped: 0,
                sources: sources(),
                notes: notes(),
            };
        }
    };

    let mut n = 0usize;
    let mut first_attempt = 0usize;
    let mut legacy_lines_skipped = 0usize;

    for line in io::BufReader::new(file).lines().map_while(Result::ok) {
        if !line.contains("\"kind\":\"loop.note.archived\"") {
            continue;
        }
        match read_field_u64(&line, "attempts") {
            Some(attempts) => {
                n += 1;
                if attempts <= 1 {
                    first_attempt += 1;
                }
            }
            None => legacy_lines_skipped += 1,
        }
    }

    let insufficient = (n < MIN_SAMPLE).then_some(InsufficientSample {
        n,
        required: MIN_SAMPLE,
    });
    let pct = insufficient
        .is_none()
        .then(|| first_attempt as f64 / n as f64 * 100.0);

    FirstAttemptSuccess {
        first_attempt_success_pct: pct,
        n,
        first_attempt,
        insufficient,
        legacy_lines_skipped,
        sources: sources(),
        notes: notes(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn write_audit(dir: &Path, lines: &[&str]) {
        let mut content = String::new();
        for line in lines {
            content.push_str(line);
            content.push('\n');
        }
        fs::write(dir.join("audit.jsonl"), content).unwrap();
    }

    fn archived_line(name: &str, attempts: Option<u64>) -> String {
        match attempts {
            Some(a) => format!(
                r#"{{"kind":"loop.note.archived","cycle_id":"c-1","source":"/inbox/{name}","dest":"/archive/{name}","checksum":"h","attempts":{a}}}"#
            ),
            None => format!(
                r#"{{"kind":"loop.note.archived","cycle_id":"c-1","source":"/inbox/{name}","dest":"/archive/{name}","checksum":"h"}}"#
            ),
        }
    }

    #[test]
    fn missing_audit_file_is_insufficient_not_error() {
        let dir = tmpdir();
        let m = compute_first_attempt_success(dir.path());
        assert!(m.first_attempt_success_pct.is_none());
        assert_eq!(m.n, 0);
        let ins = m.insufficient.expect("insufficient");
        assert_eq!(ins.n, 0);
        assert_eq!(ins.required, MIN_SAMPLE);
        assert_eq!(m.sources, vec!["loop.note.archived.attempts"]);
    }

    #[test]
    fn computes_percentage_and_n_at_or_above_minimum_sample() {
        let dir = tmpdir();
        let mut lines: Vec<String> = (0..27)
            .map(|i| archived_line(&format!("a{i}.md"), Some(1)))
            .collect();
        lines.extend((0..3).map(|i| archived_line(&format!("b{i}.md"), Some(2 + i as u64))));
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_audit(dir.path(), &refs);

        let m = compute_first_attempt_success(dir.path());
        assert_eq!(m.n, 30);
        assert_eq!(m.first_attempt, 27);
        assert!(m.insufficient.is_none());
        let pct = m.first_attempt_success_pct.expect("observed");
        assert!((pct - 90.0).abs() < 1e-9, "27/30 = 90%, got {pct}");
    }

    #[test]
    fn below_minimum_sample_withholds_the_percentage() {
        let dir = tmpdir();
        let lines: Vec<String> = (0..MIN_SAMPLE - 1)
            .map(|i| archived_line(&format!("a{i}.md"), Some(1)))
            .collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_audit(dir.path(), &refs);

        let m = compute_first_attempt_success(dir.path());
        assert_eq!(m.n, MIN_SAMPLE - 1);
        assert!(m.first_attempt_success_pct.is_none());
        let ins = m.insufficient.expect("insufficient");
        assert_eq!(ins.n, MIN_SAMPLE - 1);
    }

    #[test]
    fn legacy_lines_without_attempts_are_excluded_not_assumed() {
        let dir = tmpdir();
        let mut lines: Vec<String> = (0..30)
            .map(|i| archived_line(&format!("a{i}.md"), Some(1)))
            .collect();
        lines.extend((0..5).map(|i| archived_line(&format!("old{i}.md"), None)));
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_audit(dir.path(), &refs);

        let m = compute_first_attempt_success(dir.path());
        assert_eq!(m.n, 30, "legacy lines never enter the denominator");
        assert_eq!(m.legacy_lines_skipped, 5);
        assert!((m.first_attempt_success_pct.unwrap() - 100.0).abs() < 1e-9);
    }

    #[test]
    fn corrupt_and_unrelated_lines_are_skipped_without_panicking() {
        let dir = tmpdir();
        let valid = archived_line("ok.md", Some(1));
        let lines = vec![
            "not json at all",
            "}}}}invalid{{{",
            r#"{"kind":"loop.turn.review","intent_category":"Query"}"#,
            r#"{"kind":"loop.note.archived","source":"/inbox/x.md"}"#,
            &valid,
        ];
        write_audit(dir.path(), &lines);

        let m = compute_first_attempt_success(dir.path());
        assert_eq!(m.n, 1);
        assert_eq!(m.first_attempt, 1);
        assert_eq!(m.legacy_lines_skipped, 1);
        assert!(m.first_attempt_success_pct.is_none());
        assert!(m.insufficient.is_some());
    }

    #[test]
    fn round_trips_through_json() {
        let dir = tmpdir();
        let lines: Vec<String> = (0..30)
            .map(|i| archived_line(&format!("a{i}.md"), Some(1)))
            .collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_audit(dir.path(), &refs);

        let m = compute_first_attempt_success(dir.path());
        let json = serde_json::to_string(&m).unwrap();
        let back: FirstAttemptSuccess = serde_json::from_str(&json).unwrap();
        assert_eq!(back.first_attempt_success_pct, m.first_attempt_success_pct);
        assert_eq!(back.n, 30);
    }
}
