//! Priority scoring and reinforcement/decay tracking for the self-learning
//! evolution engine.
//!
//! Implements DESIGN.md §8.4 (priority scoring formula) and §8.3.3
//! (reinforcement/decay mechanism).
//!
//! ## §8.4 Priority Scoring Formula
//!
//! ```text
//! priority_score = belief.posterior
//!                × commitment.expected_value.ev
//!                × (1 / commitment.discipline_streak.max(1))
//! ```
//!
//! High-confidence + high-EV + recently-stalled commitments get system
//! attention first. Top-5 by `priority_score` are injected into the
//! `SessionJournaler` prompt.
//!
//! ## §8.3.3 Reinforcement / Decay
//!
//! `ReinforcementTracker` records retrieval hit-counts per entity.
//! Frequently-retrieved M3 entities get wiki priority.
//! Unretrieved M2 episodes become compression candidates after 90 days.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::belief::Belief;
use crate::commitment::{Commitment, CommitmentState};

// ─── Errors ─────────────────────────────────────────────────────────────

/// Errors specific to the priority scoring module.
#[derive(Debug, Error)]
pub enum PriorityError {
    /// I/O error during reinforcement tracker persistence.
    #[error("reinforcement tracker I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON serialization/deserialization error.
    #[error("reinforcement tracker JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

// ─── Priority scoring types ─────────────────────────────────────────────

/// A computed priority score for a (belief, commitment) pair.
///
/// Represents how much system attention a particular commitment deserves,
/// factoring in belief confidence, expected value, and execution discipline.
#[derive(Debug, Clone, PartialEq)]
pub struct PriorityScore {
    /// The belief ID driving this priority.
    pub belief_id: String,
    /// The belief's proposition (for human readability).
    pub belief_statement: String,
    /// The commitment's slug identifier.
    pub commitment_slug: String,
    /// The commitment's "what" text.
    pub commitment_text: String,
    /// The computed priority score (higher = more attention).
    pub score: f64,
    /// The belief's posterior probability.
    pub posterior: f64,
    /// The expected value used in the calculation.
    pub expected_value: f64,
    /// The commitment's discipline streak.
    pub streak: u32,
}

// ─── §8.4 Priority Scoring ─────────────────────────────────────────────

/// Compute priority scores for all (belief, commitment) pairs where the
/// commitment is in a validated or executing state.
///
/// Uses a default expected value of 1.0 for all commitments since
/// `Commitment` does not store EV directly. For precise EV data from
/// `Decision.expected_value`, use [`compute_priority_scores_with_ev`].
///
/// Formula: `posterior × ev × (1 / max(streak, 1))`
pub fn compute_priority_scores(
    beliefs: &[Belief],
    commitments: &[Commitment],
) -> Vec<PriorityScore> {
    let default_ev: HashMap<String, f64> = HashMap::new();
    compute_priority_scores_with_ev(beliefs, commitments, &default_ev)
}

/// Compute priority scores with external expected-value data.
///
/// The `ev_map` maps commitment IDs to their expected values (typically
/// sourced from `Decision.expected_value.ev`). Commitments not present
/// in the map default to EV = 1.0.
///
/// Only validated/executing commitments with positive EV are included.
pub fn compute_priority_scores_with_ev(
    beliefs: &[Belief],
    commitments: &[Commitment],
    ev_map: &HashMap<String, f64>,
) -> Vec<PriorityScore> {
    let active: Vec<&Commitment> = commitments
        .iter()
        .filter(|c| {
            matches!(
                c.state,
                CommitmentState::Validated | CommitmentState::Executing
            )
        })
        .collect();

    let mut scores: Vec<PriorityScore> = Vec::new();

    for belief in beliefs {
        for commitment in &active {
            let ev = ev_map.get(&commitment.id).copied().unwrap_or(1.0);

            // Skip zero/negative EV — no point allocating attention.
            if ev <= 0.0 {
                continue;
            }

            let streak = commitment.discipline_streak;
            let streak_divisor = (streak.max(1)) as f64;
            let score = belief.posterior * ev * (1.0 / streak_divisor);

            scores.push(PriorityScore {
                belief_id: belief.id.clone(),
                belief_statement: belief.proposition.clone(),
                commitment_slug: commitment.slug(),
                commitment_text: commitment.what.clone(),
                score,
                posterior: belief.posterior,
                expected_value: ev,
                streak,
            });
        }
    }

    // Descending by score, stable for determinism.
    scores.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    scores
}

/// Return the top-N priority scores.
///
/// Convenience wrapper around [`compute_priority_scores`].
pub fn top_n_by_priority(
    beliefs: &[Belief],
    commitments: &[Commitment],
    n: usize,
) -> Vec<PriorityScore> {
    compute_priority_scores(beliefs, commitments)
        .into_iter()
        .take(n)
        .collect()
}

/// Format a list of priority scores into a human-readable prompt section
/// suitable for injection into the `SessionJournaler` prompt.
///
/// Returns an empty string when `scores` is empty.
pub fn format_priority_for_prompt(scores: &[PriorityScore]) -> String {
    if scores.is_empty() {
        return String::new();
    }

    let mut out = String::from("## Priority Commitments (Evolution Engine)\n\n");
    out.push_str("The following belief-commitment pairs deserve attention this session:\n\n");

    for (i, s) in scores.iter().enumerate() {
        out.push_str(&format!(
            "{}. **[{}] {}** (score: {:.4})\n   \
             Belief: \"{}\" (posterior: {:.1}%)\n   \
             Expected Value: {:.2} | Discipline streak: {}\n\n",
            i + 1,
            s.commitment_slug,
            s.commitment_text,
            s.score,
            s.belief_statement,
            s.posterior * 100.0,
            s.expected_value,
            s.streak,
        ));
    }

    out
}

// ─── §8.3.3 Reinforcement / Decay ──────────────────────────────────────

/// Persisted hit-count entry for reinforcement tracking.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct ReinforcementEntry {
    /// How many times this entity has been retrieved.
    hit_count: u32,
    /// Date of the most recent retrieval (YYYY-MM-DD).
    last_accessed: NaiveDate,
}

/// On-disk schema for the reinforcement sidecar.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
struct ReinforcementFile {
    entities: HashMap<String, ReinforcementEntry>,
}

/// Tracks retrieval hit-counts for reinforcement/decay decisions.
///
/// Persists to a JSON sidecar file (`memories/.reinforcement.json`).
///
/// - **Frequently-retrieved** M3 entities get wiki priority.
/// - **Unretrieved** M2 episodes become compression candidates after 90 days.
pub struct ReinforcementTracker {
    path: PathBuf,
    counts: HashMap<String, u32>,
    last_accessed: HashMap<String, NaiveDate>,
}

impl ReinforcementTracker {
    /// Create or load a tracker backed by the given JSON path.
    ///
    /// If the file exists, its contents are loaded. Otherwise a fresh
    /// in-memory state is created.
    pub fn new(storage_path: PathBuf) -> Self {
        Self::load_from_path(&storage_path).unwrap_or_else(|_| Self {
            path: storage_path,
            counts: HashMap::new(),
            last_accessed: HashMap::new(),
        })
    }

    /// Record a retrieval event for the given entity.
    ///
    /// Increments the hit-count and updates `last_accessed` to today.
    /// Automatically persists to disk.
    pub fn record_retrieval(&mut self, entity_id: &str) -> Result<(), PriorityError> {
        let today = Utc::now().date_naive();
        let count = self.counts.entry(entity_id.to_string()).or_insert(0);
        *count += 1;
        self.last_accessed.insert(entity_id.to_string(), today);
        self.save()
    }

    /// Get the current hit-count for an entity (0 if never retrieved).
    pub fn get_hit_count(&self, entity_id: &str) -> u32 {
        self.counts.get(entity_id).copied().unwrap_or(0)
    }

    /// Get entities not retrieved within the given number of days.
    ///
    /// These are candidates for compression / archival (§8.3.3).
    pub fn get_stale_episodes(&self, days_threshold: u32) -> Vec<String> {
        let cutoff = Utc::now().date_naive() - Duration::days(days_threshold as i64);
        self.last_accessed
            .iter()
            .filter(|entry| entry.1 < &cutoff)
            .map(|entry| entry.0.clone())
            .collect()
    }

    /// Get entities that have been retrieved at least `min_hits` times.
    ///
    /// Frequently-retrieved entities are candidates for wiki promotion.
    pub fn get_frequent_entities(&self, min_hits: u32) -> Vec<String> {
        self.counts
            .iter()
            .filter(|entry| *entry.1 >= min_hits)
            .map(|entry| entry.0.clone())
            .collect()
    }

    /// Persist the current state to the JSON sidecar file.
    pub fn save(&self) -> Result<(), PriorityError> {
        let file = ReinforcementFile {
            entities: self
                .counts
                .iter()
                .map(|(id, &count)| {
                    let date = self
                        .last_accessed
                        .get(id)
                        .copied()
                        .unwrap_or_else(|| Utc::now().date_naive());
                    (
                        id.clone(),
                        ReinforcementEntry {
                            hit_count: count,
                            last_accessed: date,
                        },
                    )
                })
                .collect(),
        };

        // Ensure parent directory exists.
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string_pretty(&file)?;
        fs::write(&self.path, json)?;
        Ok(())
    }

    /// Load state from a JSON file. Returns `Err` if the file is
    /// missing or malformed (callers typically fall back to defaults).
    fn load_from_path(path: &Path) -> Result<Self, PriorityError> {
        let content = fs::read_to_string(path)?;
        let file: ReinforcementFile = serde_json::from_str(&content)?;

        let mut counts = HashMap::new();
        let mut last_accessed = HashMap::new();

        for (id, entry) in file.entities {
            counts.insert(id.clone(), entry.hit_count);
            last_accessed.insert(id, entry.last_accessed);
        }

        Ok(Self {
            path: path.to_path_buf(),
            counts,
            last_accessed,
        })
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::belief::Belief;
    use crate::commitment::{Commitment, CommitmentState};
    use tempfile::tempdir;

    // ── helpers ──────────────────────────────────────────────────────

    fn make_belief(id: &str, posterior: f64) -> Belief {
        let mut b = Belief::new(id.into(), format!("proposition {id}"), "test".into());
        b.posterior = posterior;
        b
    }

    fn make_commitment(id: &str, state: CommitmentState, streak: u32) -> Commitment {
        let mut c = Commitment::new(&format!("commitment {id}"));
        c.id = id.to_string();
        c.state = state;
        c.discipline_streak = streak;
        c
    }

    // ── compute_priority_scores ──────────────────────────────────────

    #[test]
    fn test_empty_inputs_produce_no_scores() {
        let scores = compute_priority_scores(&[], &[]);
        assert!(scores.is_empty());
    }

    #[test]
    fn test_only_validated_executing_commitments_included() {
        let beliefs = vec![make_belief("b1", 0.8)];
        let commitments = vec![
            make_commitment("c1", CommitmentState::Drafted, 0),
            make_commitment("c2", CommitmentState::Validated, 0),
            make_commitment("c3", CommitmentState::Executing, 0),
            make_commitment("c4", CommitmentState::Completed, 0),
            make_commitment("c5", CommitmentState::Abandoned, 0),
        ];
        let scores = compute_priority_scores(&beliefs, &commitments);
        assert_eq!(scores.len(), 2); // Only c2 and c3
    }

    #[test]
    fn test_formula_basic_no_ev_map() {
        // posterior=0.8, ev=1.0 (default), streak=0 → divisor=1
        let beliefs = vec![make_belief("b1", 0.8)];
        let commitments = vec![make_commitment("c1", CommitmentState::Executing, 0)];
        let scores = compute_priority_scores(&beliefs, &commitments);
        assert_eq!(scores.len(), 1);
        assert!((scores[0].score - 0.8).abs() < 1e-10);
    }

    #[test]
    fn test_formula_streak_divisor() {
        // posterior=0.9, ev=1.0, streak=3 → score = 0.9 / 3 = 0.3
        let beliefs = vec![make_belief("b1", 0.9)];
        let commitments = vec![make_commitment("c1", CommitmentState::Validated, 3)];
        let scores = compute_priority_scores(&beliefs, &commitments);
        assert!((scores[0].score - 0.3).abs() < 1e-10);
        assert_eq!(scores[0].streak, 3);
    }

    #[test]
    fn test_formula_with_ev_map() {
        let mut ev_map = HashMap::new();
        ev_map.insert("c1".to_string(), 5.0);
        // posterior=0.7, ev=5.0, streak=1 → score = 0.7 * 5.0 / 1 = 3.5
        let beliefs = vec![make_belief("b1", 0.7)];
        let commitments = vec![make_commitment("c1", CommitmentState::Executing, 1)];
        let scores = compute_priority_scores_with_ev(&beliefs, &commitments, &ev_map);
        assert_eq!(scores.len(), 1);
        assert!((scores[0].score - 3.5).abs() < 1e-10);
        assert!((scores[0].expected_value - 5.0).abs() < 1e-10);
    }

    #[test]
    fn test_zero_ev_excluded() {
        let mut ev_map = HashMap::new();
        ev_map.insert("c1".to_string(), 0.0);
        let beliefs = vec![make_belief("b1", 0.9)];
        let commitments = vec![make_commitment("c1", CommitmentState::Validated, 0)];
        let scores = compute_priority_scores_with_ev(&beliefs, &commitments, &ev_map);
        assert!(scores.is_empty());
    }

    #[test]
    fn test_negative_ev_excluded() {
        let mut ev_map = HashMap::new();
        ev_map.insert("c1".to_string(), -3.0);
        let beliefs = vec![make_belief("b1", 0.9)];
        let commitments = vec![make_commitment("c1", CommitmentState::Executing, 0)];
        let scores = compute_priority_scores_with_ev(&beliefs, &commitments, &ev_map);
        assert!(scores.is_empty());
    }

    #[test]
    fn test_sorted_descending_by_score() {
        let beliefs = vec![make_belief("b1", 0.5), make_belief("b2", 0.9)];
        let commitments = vec![make_commitment("c1", CommitmentState::Validated, 1)];
        let scores = compute_priority_scores(&beliefs, &commitments);
        assert_eq!(scores.len(), 2);
        // b2 (0.9) should rank above b1 (0.5)
        assert!(scores[0].score > scores[1].score);
        assert_eq!(scores[0].belief_id, "b2");
    }

    #[test]
    fn test_all_fields_populated() {
        let mut belief = make_belief("b1", 0.85);
        belief.proposition = "jwt-works".into();
        let mut c = make_commitment("c1", CommitmentState::Executing, 2);
        c.what = "ship jwt".into();

        let beliefs = vec![belief];
        let commitments = vec![c];
        let scores = compute_priority_scores(&beliefs, &commitments);
        let s = &scores[0];

        assert_eq!(s.belief_id, "b1");
        assert_eq!(s.belief_statement, "jwt-works");
        assert_eq!(s.commitment_text, "ship jwt");
        assert!((s.posterior - 0.85).abs() < 1e-10);
        assert!((s.expected_value - 1.0).abs() < 1e-10);
        assert_eq!(s.streak, 2);
    }

    // ── top_n_by_priority ────────────────────────────────────────────

    #[test]
    fn test_top_n_limits_output() {
        let beliefs = vec![
            make_belief("b1", 0.9),
            make_belief("b2", 0.8),
            make_belief("b3", 0.7),
        ];
        let commitments = vec![make_commitment("c1", CommitmentState::Executing, 0)];
        let top = top_n_by_priority(&beliefs, &commitments, 2);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].belief_id, "b1");
        assert_eq!(top[1].belief_id, "b2");
    }

    #[test]
    fn test_top_n_zero_returns_empty() {
        let scores = top_n_by_priority(&[], &[], 5);
        assert!(scores.is_empty());
    }

    // ── format_priority_for_prompt ───────────────────────────────────

    #[test]
    fn test_format_empty_scores() {
        assert_eq!(format_priority_for_prompt(&[]), "");
    }

    #[test]
    fn test_format_contains_header() {
        let beliefs = vec![make_belief("b1", 0.8)];
        let commitments = vec![make_commitment("c1", CommitmentState::Validated, 1)];
        let scores = compute_priority_scores(&beliefs, &commitments);
        let prompt = format_priority_for_prompt(&scores);
        assert!(prompt.contains("## Priority Commitments (Evolution Engine)"));
        assert!(prompt.contains("commitment c1"));
    }

    // ── ReinforcementTracker ─────────────────────────────────────────

    #[test]
    fn test_tracker_new_creates_fresh_state() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tracker.json");
        let tracker = ReinforcementTracker::new(path);
        assert_eq!(tracker.get_hit_count("missing"), 0);
    }

    #[test]
    fn test_tracker_record_retrieval_increments() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tracker.json");
        let mut tracker = ReinforcementTracker::new(path);

        tracker.record_retrieval("entity-1").unwrap();
        assert_eq!(tracker.get_hit_count("entity-1"), 1);

        tracker.record_retrieval("entity-1").unwrap();
        assert_eq!(tracker.get_hit_count("entity-1"), 2);

        tracker.record_retrieval("entity-2").unwrap();
        assert_eq!(tracker.get_hit_count("entity-2"), 1);
    }

    #[test]
    fn test_tracker_persistence_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tracker.json");

        {
            let mut tracker = ReinforcementTracker::new(path.clone());
            tracker.record_retrieval("e1").unwrap();
            tracker.record_retrieval("e1").unwrap();
            tracker.record_retrieval("e2").unwrap();
        }

        let loaded = ReinforcementTracker::new(path);
        assert_eq!(loaded.get_hit_count("e1"), 2);
        assert_eq!(loaded.get_hit_count("e2"), 1);
        assert_eq!(loaded.get_hit_count("e3"), 0);
    }

    #[test]
    fn test_get_frequent_entities() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tracker.json");
        let mut tracker = ReinforcementTracker::new(path);

        for _ in 0..5 {
            tracker.record_retrieval("frequent").unwrap();
        }
        tracker.record_retrieval("rare").unwrap();

        let frequent = tracker.get_frequent_entities(3);
        assert!(frequent.contains(&"frequent".to_string()));
        assert!(!frequent.contains(&"rare".to_string()));
    }

    #[test]
    fn test_get_stale_episodes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tracker.json");

        // Manually write a file with an old date.
        let old_date = Utc::now().date_naive() - Duration::days(100);
        let file = ReinforcementFile {
            entities: HashMap::from([(
                "stale-entity".to_string(),
                ReinforcementEntry {
                    hit_count: 3,
                    last_accessed: old_date,
                },
            )]),
        };
        let json = serde_json::to_string_pretty(&file).unwrap();
        fs::write(&path, json).unwrap();

        let tracker = ReinforcementTracker::new(path);
        let stale = tracker.get_stale_episodes(90);
        assert!(stale.contains(&"stale-entity".to_string()));
    }

    #[test]
    fn test_get_stale_episodes_excludes_recent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tracker.json");
        let mut tracker = ReinforcementTracker::new(path);

        tracker.record_retrieval("fresh").unwrap();
        let stale = tracker.get_stale_episodes(90);
        assert!(stale.is_empty());
    }

    #[test]
    fn test_get_frequent_entities_empty_when_below_threshold() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tracker.json");
        let mut tracker = ReinforcementTracker::new(path);

        tracker.record_retrieval("low-count").unwrap();
        let frequent = tracker.get_frequent_entities(5);
        assert!(frequent.is_empty());
    }

    #[test]
    fn test_format_scorecard_contains_all_fields() {
        let score = PriorityScore {
            belief_id: "b1".into(),
            belief_statement: "rust is fast".into(),
            commitment_slug: "ship-rust".into(),
            commitment_text: "ship the rust module".into(),
            score: 1.2345,
            posterior: 0.9,
            expected_value: 2.0,
            streak: 1,
        };
        let prompt = format_priority_for_prompt(&[score]);
        assert!(prompt.contains("ship-rust"));
        assert!(prompt.contains("rust is fast"));
        assert!(prompt.contains("90.0%"));
        assert!(prompt.contains("1.2345"));
    }
}
