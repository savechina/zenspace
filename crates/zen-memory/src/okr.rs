use std::fmt;

use chrono::{Duration, Utc};

use crate::commitment::{Commitment, CommitmentState};

/// Key performance indicators for commitment tracking.
///
/// Derived from DESIGN.md §15.2:
/// "commitment_completion_rate = (commitments with >=1 milestone achieved within review_at window)
///  / (total commitments). Must trend up over 90-day rolling window."
#[derive(Debug, Clone)]
pub struct CommitmentOkr {
    pub total_commitments: usize,
    pub completed: usize,
    pub abandoned: usize,
    pub active: usize,
    pub overdue: usize,
    pub completion_rate: f64,
    pub window_days: u32,
}

impl CommitmentOkr {
    pub fn is_below_target(&self) -> bool {
        self.completion_rate < 0.5
    }
}

impl fmt::Display for CommitmentOkr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CommitmentOkr({window}d): {total} total, {completed} completed, \
             {abandoned} abandoned, {active} active, {overdue} overdue — \
             rate = {rate:.1}%",
            window = self.window_days,
            total = self.total_commitments,
            completed = self.completed,
            abandoned = self.abandoned,
            active = self.active,
            overdue = self.overdue,
            rate = self.completion_rate * 100.0,
        )
    }
}

/// Compute commitment completion KPI over a rolling window.
///
/// Filters commitments created within the last `window_days` days and
/// classifies each one.
///
/// - **completed**: state is `Completed` or has >= 1 milestone achieved (`.completed == true`)
/// - **abandoned**: state is `Abandoned`
/// - **overdue**: past `review_at` with zero milestones achieved
/// - **active**: everything else (drafted / validated / executing / reviewing)
///
/// `completion_rate = completed / total` (0.0 when total == 0).
pub fn compute_commitment_completion_rate(
    commitments: &[Commitment],
    window_days: u32,
) -> CommitmentOkr {
    let cutoff = Utc::now() - Duration::days(window_days as i64);
    let cutoff_date = cutoff.date_naive();

    let filtered: Vec<&Commitment> = commitments
        .iter()
        .filter(|c| c.created_at.date_naive() >= cutoff_date)
        .collect();

    let total = filtered.len();
    let mut completed = 0usize;
    let mut abandoned = 0usize;
    let mut active = 0usize;
    let mut overdue = 0usize;

    for c in &filtered {
        match c.state {
            CommitmentState::Abandoned => {
                abandoned += 1;
            }
            CommitmentState::Completed => {
                completed += 1;
            }
            _ => {
                let has_achieved_milestone = c.milestones.iter().any(|m| m.completed);
                if has_achieved_milestone {
                    completed += 1;
                } else if c.is_overdue() {
                    overdue += 1;
                } else {
                    active += 1;
                }
            }
        }
    }

    let completion_rate = if total > 0 {
        completed as f64 / total as f64
    } else {
        0.0
    };

    CommitmentOkr {
        total_commitments: total,
        completed,
        abandoned,
        active,
        overdue,
        completion_rate,
        window_days,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commitment::{Milestone, StopLossLine};
    use chrono::Utc;

    fn make_commitment(what: &str, days_ago: i64) -> Commitment {
        let created = Utc::now() - Duration::days(days_ago);
        Commitment {
            id: uuid::Uuid::new_v4().to_string(),
            what: what.to_string(),
            state: CommitmentState::Drafted,
            by_when: None,
            review_at: None,
            next_action: String::new(),
            two_minute_rule: false,
            milestones: Vec::new(),
            stop_loss: StopLossLine::default(),
            discipline_streak: 0,
            sustained_value_plan: None,
            retrospective: None,
            execution_checklist: Default::default(),
            source_journal: None,
            created_at: created,
            updated_at: created,
            closed_at: None,
        }
    }

    #[test]
    fn test_empty_commitments() {
        let kpi = compute_commitment_completion_rate(&[], 90);
        assert_eq!(kpi.total_commitments, 0);
        assert_eq!(kpi.completed, 0);
        assert_eq!(kpi.abandoned, 0);
        assert_eq!(kpi.active, 0);
        assert_eq!(kpi.overdue, 0);
        assert_eq!(kpi.completion_rate, 0.0);
        assert_eq!(kpi.window_days, 90);
    }

    #[test]
    fn test_all_completed() {
        let mut c1 = make_commitment("done 1", 5);
        c1.state = CommitmentState::Completed;
        let mut c2 = make_commitment("done 2", 10);
        c2.state = CommitmentState::Completed;

        let kpi = compute_commitment_completion_rate(&[c1, c2], 30);
        assert_eq!(kpi.total_commitments, 2);
        assert_eq!(kpi.completed, 2);
        assert_eq!(kpi.abandoned, 0);
        assert_eq!(kpi.completion_rate, 1.0);
        assert!(!kpi.is_below_target());
    }

    #[test]
    fn test_all_overdue() {
        let mut c1 = make_commitment("late 1", 5);
        c1.review_at = Some(Utc::now().date_naive() - Duration::days(1));
        let mut c2 = make_commitment("late 2", 10);
        c2.review_at = Some(Utc::now().date_naive() - Duration::days(3));

        let kpi = compute_commitment_completion_rate(&[c1, c2], 30);
        assert_eq!(kpi.total_commitments, 2);
        assert_eq!(kpi.overdue, 2);
        assert_eq!(kpi.completed, 0);
        assert_eq!(kpi.completion_rate, 0.0);
        assert!(kpi.is_below_target());
    }

    #[test]
    fn test_mixed_states() {
        let mut completed = make_commitment("finished", 5);
        completed.state = CommitmentState::Completed;

        let mut abandoned = make_commitment("given up", 8);
        abandoned.state = CommitmentState::Abandoned;

        let mut overdue = make_commitment("late work", 10);
        overdue.review_at = Some(Utc::now().date_naive() - Duration::days(2));

        let active = make_commitment("in progress", 3);

        let mut old = make_commitment("old task", 200);
        old.state = CommitmentState::Completed;

        let kpi =
            compute_commitment_completion_rate(&[completed, abandoned, overdue, active, old], 90);
        assert_eq!(kpi.total_commitments, 4);
        assert_eq!(kpi.completed, 1);
        assert_eq!(kpi.abandoned, 1);
        assert_eq!(kpi.overdue, 1);
        assert_eq!(kpi.active, 1);
        assert!((kpi.completion_rate - 0.25).abs() < f64::EPSILON);
    }

    #[test]
    fn test_milestone_achieved_counts_as_completed() {
        let mut c = make_commitment("has milestone", 5);
        c.milestones = vec![Milestone {
            description: "step 1".into(),
            target_date: None,
            completed: true,
            completed_at: Some(Utc::now()),
        }];

        let kpi = compute_commitment_completion_rate(&[c], 30);
        assert_eq!(kpi.completed, 1);
        assert_eq!(kpi.completion_rate, 1.0);
    }

    #[test]
    fn test_excludes_outside_window() {
        let inside = make_commitment("recent", 10);
        let outside = make_commitment("old", 200);

        let kpi = compute_commitment_completion_rate(&[inside, outside], 30);
        assert_eq!(kpi.total_commitments, 1);
    }

    #[test]
    fn test_display_format() {
        let kpi = CommitmentOkr {
            total_commitments: 10,
            completed: 4,
            abandoned: 1,
            active: 3,
            overdue: 2,
            completion_rate: 0.4,
            window_days: 90,
        };
        let s = kpi.to_string();
        assert!(s.contains("90d"));
        assert!(s.contains("40.0%"));
        assert!(s.contains("10 total"));
    }
}
