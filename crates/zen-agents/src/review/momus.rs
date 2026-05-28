use zen_core::types::Task;

#[derive(Debug, Clone)]
pub struct MomusFinding {
    pub finding_type: MomusFindingType,
    pub description: String,
    pub is_blocking: bool,
}

#[derive(Debug, Clone)]
pub enum MomusFindingType {
    PlanInconsistency,
    UntestableAcceptanceCriteria,
    MissingVerifiability,
    AmbiguousScope,
}

#[derive(Debug)]
pub struct MomusReview {
    pub findings: Vec<MomusFinding>,
    pub approved: bool,
    pub veto_reason: Option<String>,
}

pub struct MomusReviewer;

impl MomusReviewer {
    pub fn new() -> Self {
        Self
    }

    pub fn gate_review(&self, task: &Task, plan: &str) -> MomusReview {
        let mut findings = Vec::new();
        findings.extend(self.check_plan_consistency(plan));
        findings.extend(self.validate_acceptance_criteria(plan));
        findings.extend(self.assess_verifiability(plan));

        let blocked = findings.iter().any(|f| f.is_blocking);
        let veto_reason = if blocked {
            Some(
                findings
                    .iter()
                    .filter(|f| f.is_blocking)
                    .map(|f| format!("[{:?}] {}", f.finding_type, f.description))
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        } else {
            None
        };

        tracing::debug!(
            task_id = %task.id,
            findings = findings.len(),
            blocked,
            "MomusReview completed"
        );

        MomusReview {
            findings,
            approved: !blocked,
            veto_reason,
        }
    }

    pub fn check_plan_consistency(&self, plan: &str) -> Vec<MomusFinding> {
        let mut findings = Vec::new();
        if plan.trim().is_empty() {
            return findings;
        }

        let lower = plan.to_lowercase();
        let sentences: Vec<&str> = plan
            .split(['.', '\n'])
            .filter(|s| !s.trim().is_empty())
            .collect();

        let has_create = ["create", "add", "new", "build"]
            .iter()
            .any(|t| lower.contains(t));
        let has_delete = ["delete", "remove", "destroy", "drop"]
            .iter()
            .any(|t| lower.contains(t));
        if has_create && has_delete && sentences.len() > 1 {
            findings.push(MomusFinding {
                finding_type: MomusFindingType::PlanInconsistency,
                description: "Plan contains both creation and deletion actions without ordering".to_string(),
                is_blocking: true,
            });
        }

        let ambiguous_refs = ["it will", "this should", "the above should", "that must"]
            .iter()
            .any(|t| lower.contains(t));
        if ambiguous_refs && sentences.len() < 3 {
            findings.push(MomusFinding {
                finding_type: MomusFindingType::PlanInconsistency,
                description: "Ambiguous reference detected without sufficient context".to_string(),
                is_blocking: false,
            });
        }

        if plan.to_lowercase().contains("step") {
            let step_pattern = plan.to_lowercase().matches("step").count();
            if step_pattern > 5 && sentences.len() < step_pattern {
                findings.push(MomusFinding {
                    finding_type: MomusFindingType::PlanInconsistency,
                    description: "Possible circular step referencing detected".to_string(),
                    is_blocking: true,
                });
            }
        }

        findings
    }

    pub fn validate_acceptance_criteria(&self, plan: &str) -> Vec<MomusFinding> {
        let mut findings = Vec::new();
        if plan.trim().is_empty() {
            return findings;
        }

        let lower = plan.to_lowercase();
        let has_criteria = ["verify", "validate", "test", "check", "assert", "confirm"]
            .iter()
            .any(|t| lower.contains(t));
        let has_metrics = ["percent", "count", "score", "threshold", "minimum", "maximum", "less than", "more than"]
            .iter()
            .any(|t| lower.contains(t));

        let word_count = plan.split_whitespace().count();
        if !has_criteria && word_count > 10 {
            findings.push(MomusFinding {
                finding_type: MomusFindingType::UntestableAcceptanceCriteria,
                description: "No verification or testing steps found in plan".to_string(),
                is_blocking: true,
            });
        }

        if !has_metrics && !lower.contains("complete") {
            findings.push(MomusFinding {
                finding_type: MomusFindingType::UntestableAcceptanceCriteria,
                description: "No measurable acceptance criteria detected".to_string(),
                is_blocking: false,
            });
        }

        findings
    }

    pub fn assess_verifiability(&self, plan: &str) -> Vec<MomusFinding> {
        let mut findings = Vec::new();
        if plan.trim().is_empty() {
            return findings;
        }

        let lower = plan.to_lowercase();
        let lines: Vec<&str> = plan.lines().filter(|l| !l.trim().is_empty()).collect();

        let lines_without_subject = lines.iter().filter(|l| {
            let trimmed = l.trim();
            trimmed.starts_with("and") || trimmed.starts_with("or") || trimmed.starts_with("then")
        }).count();

        if lines_without_subject > lines.len() / 2 && lines.len() > 2 {
            findings.push(MomusFinding {
                finding_type: MomusFindingType::MissingVerifiability,
                description: "Majority of steps lack clear subject — hard to verify independently".to_string(),
                is_blocking: true,
            });
        }

        let has_implicit_state = lower.contains("updated") && !lower.contains("update");
        if has_implicit_state {
            findings.push(MomusFinding {
                finding_type: MomusFindingType::MissingVerifiability,
                description: "Implied state changes without explicit update action".to_string(),
                is_blocking: false,
            });
        }

        findings
    }

    pub fn can_veto(&self, review: &MomusReview) -> bool {
        review.findings.iter().any(|f| f.is_blocking)
    }
}

impl Default for MomusReviewer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zen_core::types::{Task, TaskType};

    #[test]
    fn momus_approves_simple_plans() {
        let reviewer = MomusReviewer::new();
        let task = Task::new("simple task", 0.3, TaskType::Code);
        let review = reviewer.gate_review(&task, "simple plan");
        assert!(review.approved);
        assert!(review.veto_reason.is_none());
    }

    #[test]
    fn momus_veto_on_plan_with_issues() {
        let reviewer = MomusReviewer::new();
        let task = Task::new("test task", 0.5, TaskType::Code);
        let review = reviewer.gate_review(&task, "create the new table. delete the old table. verify it works");
        assert!(!review.approved);
        assert!(review.veto_reason.is_some());
    }

    #[test]
    fn momus_can_veto_returns_false_when_no_blocking_findings() {
        let reviewer = MomusReviewer::new();
        let task = Task::new("test", 0.5, TaskType::Text);
        let review = reviewer.gate_review(&task, "test plan");
        assert!(!reviewer.can_veto(&review));
    }
}
