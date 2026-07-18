use crate::types::{Sensitivity, Task};

// ---------------------------------------------------------------------------
// ReviewContext — carries escalation-relevant state into Zeus
// ---------------------------------------------------------------------------

/// Context passed to [`ZeusEscalation::final_review`] so it can decide whether
/// to escalate based on sensitivity, budget, and rejection history.
#[derive(Debug, Clone)]
pub struct ReviewContext {
    /// Sensitivity level of the task / deliverable.
    pub sensitivity: Sensitivity,
    /// Estimated token budget for the task.
    pub token_budget: usize,
    /// Number of times Hermes rejected the deliverable.
    pub hermes_rejections: u8,
}

impl ReviewContext {
    pub fn new(sensitivity: Sensitivity, token_budget: usize, hermes_rejections: u8) -> Self {
        Self {
            sensitivity,
            token_budget,
            hermes_rejections,
        }
    }

    /// Build a context from a [`Task`] with sensible defaults:
    /// - sensitivity → `Public`
    /// - token_budget → `task.user_input.len() * 4`
    /// - hermes_rejections → `0`
    pub fn from_task(task: &Task) -> Self {
        Self {
            sensitivity: Sensitivity::Public,
            token_budget: task.user_input.len() * 4,
            hermes_rejections: 0,
        }
    }

    /// Parse sensitivity from task metadata, falling back to `Public`.
    pub fn from_task_with_metadata(task: &Task, hermes_rejections: u8) -> Self {
        let sensitivity = task
            .metadata
            .get("sensitivity")
            .map(|s| match s.as_str() {
                "Confidential" => Sensitivity::Confidential,
                "Private" => Sensitivity::Private,
                _ => Sensitivity::Public,
            })
            .unwrap_or(Sensitivity::Public);

        Self {
            sensitivity,
            token_budget: task.user_input.len() * 4,
            hermes_rejections,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ZeusFinding {
    pub finding_type: ZeusFindingType,
    pub description: String,
    pub requires_escalation: bool,
}

#[derive(Debug, Clone)]
pub enum ZeusFindingType {
    ValueMisalignment,
    GoalConformanceFailure,
    UnrecoverableConflict,
}

#[derive(Debug)]
pub struct ZeusReview {
    pub findings: Vec<ZeusFinding>,
    pub approved: bool,
    pub escalated: bool,
    pub reason: String,
    pub athena_shield: Option<String>,
}

pub struct ZeusEscalation;

impl ZeusEscalation {
    pub fn new() -> Self {
        Self
    }

    pub fn final_review(&self, ctx: &ReviewContext) -> ZeusReview {
        let should_escalate = ctx.sensitivity == Sensitivity::Confidential
            || ctx.token_budget > 100_000
            || ctx.hermes_rejections >= 2;

        if !should_escalate {
            return ZeusReview {
                findings: Vec::new(),
                approved: true,
                escalated: false,
                reason: "Low-risk task — auto-approved".to_string(),
                athena_shield: None,
            };
        }

        let has_deadlock_indicator = ctx.hermes_rejections >= 2;
        let has_budget_risk = ctx.token_budget > 100_000;
        let has_sensitivity_risk = ctx.sensitivity == Sensitivity::Confidential;

        let approved = !has_deadlock_indicator;

        let mut findings = Vec::new();
        if has_deadlock_indicator {
            findings.push(ZeusFinding {
                finding_type: ZeusFindingType::UnrecoverableConflict,
                description: format!(
                    "Hermes rejected {} times — deadlock indicator",
                    ctx.hermes_rejections
                ),
                requires_escalation: true,
            });
        }
        if has_budget_risk {
            findings.push(ZeusFinding {
                finding_type: ZeusFindingType::GoalConformanceFailure,
                description: format!(
                    "Token budget {} exceeds 100K threshold",
                    ctx.token_budget
                ),
                requires_escalation: false,
            });
        }
        if has_sensitivity_risk {
            findings.push(ZeusFinding {
                finding_type: ZeusFindingType::ValueMisalignment,
                description: "Confidential sensitivity requires Zeus escalation".to_string(),
                requires_escalation: true,
            });
        }

        let reason = if !approved {
            "Deadlock detected — escalated to Zeus".to_string()
        } else if has_sensitivity_risk && has_budget_risk {
            "High-risk review passed (confidential + budget)".to_string()
        } else if has_sensitivity_risk {
            "High-risk review passed (confidential sensitivity)".to_string()
        } else {
            "High-risk review passed (budget threshold)".to_string()
        };

        ZeusReview {
            findings,
            approved,
            escalated: true,
            reason,
            athena_shield: if !approved {
                Some(self.generate_athena_shield(ctx))
            } else {
                None
            },
        }
    }

    pub fn check_value_alignment(
        &self,
        _deliverable: &str,
        _shared_vision: &str,
    ) -> Vec<ZeusFinding> {
        Vec::new()
    }

    pub fn assess_goal_conformance(
        &self,
        _deliverable: &str,
        _goals: &[String],
    ) -> Vec<ZeusFinding> {
        Vec::new()
    }

    pub fn generate_athena_shield(&self, ctx: &ReviewContext) -> String {
        let mut sections = vec![
            "# Athena Shield — Human Handoff Guide".to_string(),
            String::new(),
            "## Escalation Context".to_string(),
            format!("- Sensitivity: {}", ctx.sensitivity),
            format!("- Token budget: {}", ctx.token_budget),
            format!("- Hermes rejections: {}", ctx.hermes_rejections),
            String::new(),
            "## Failed Attempts".to_string(),
        ];

        if ctx.hermes_rejections >= 2 {
            sections.push(format!(
                "- Hermes rejected {} times — possible deadlock",
                ctx.hermes_rejections
            ));
        }
        if ctx.token_budget > 100_000 {
            sections.push(format!(
                "- Token budget {} exceeded 100K threshold",
                ctx.token_budget
            ));
        }
        if ctx.sensitivity == Sensitivity::Confidential {
            sections.push("- Confidential sensitivity requires human review".to_string());
        }

        sections.push(String::new());
        sections.push("## Recommended Actions".to_string());
        sections.push("- Review task requirements".to_string());
        sections.push("- Consult domain expert".to_string());
        sections.push("- Consider alternative approach".to_string());

        sections.join("\n")
    }

    pub fn should_escalate(
        &self,
        sensitivity: Sensitivity,
        hermes_rejections: u8,
        token_budget: usize,
    ) -> bool {
        sensitivity == Sensitivity::Confidential || hermes_rejections >= 2 || token_budget > 100_000
    }
}

impl Default for ZeusEscalation {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Sensitivity, TaskType};

    #[test]
    fn low_risk_task_auto_approved() {
        let zeus = ZeusEscalation::new();
        let ctx = ReviewContext::new(Sensitivity::Public, 50_000, 0);
        let review = zeus.final_review(&ctx);
        assert!(review.approved);
        assert!(!review.escalated);
        assert!(review.athena_shield.is_none());
        assert!(review.findings.is_empty());
    }

    #[test]
    fn confidential_task_escalated() {
        let zeus = ZeusEscalation::new();
        let ctx = ReviewContext::new(Sensitivity::Confidential, 50_000, 0);
        let review = zeus.final_review(&ctx);
        assert!(review.approved);
        assert!(review.escalated);
        assert!(review.findings.iter().any(|f| matches!(
            f.finding_type,
            ZeusFindingType::ValueMisalignment
        )));
    }

    #[test]
    fn budget_over_100k_escalated() {
        let zeus = ZeusEscalation::new();
        let ctx = ReviewContext::new(Sensitivity::Public, 150_000, 0);
        let review = zeus.final_review(&ctx);
        assert!(review.approved);
        assert!(review.escalated);
        assert!(review.findings.iter().any(|f| matches!(
            f.finding_type,
            ZeusFindingType::GoalConformanceFailure
        )));
    }

    #[test]
    fn hermes_rejects_2_plus_times_blocked() {
        let zeus = ZeusEscalation::new();
        let ctx = ReviewContext::new(Sensitivity::Public, 50_000, 2);
        let review = zeus.final_review(&ctx);
        assert!(!review.approved);
        assert!(review.escalated);
        assert!(review.athena_shield.is_some());
        assert!(review.findings.iter().any(|f| matches!(
            f.finding_type,
            ZeusFindingType::UnrecoverableConflict
        )));
    }

    #[test]
    fn hermes_3_rejections_also_blocked() {
        let zeus = ZeusEscalation::new();
        let ctx = ReviewContext::new(Sensitivity::Private, 20_000, 3);
        let review = zeus.final_review(&ctx);
        assert!(!review.approved);
        assert!(review.escalated);
    }

    #[test]
    fn confidential_with_budget_escalated_approved() {
        let zeus = ZeusEscalation::new();
        let ctx = ReviewContext::new(Sensitivity::Confidential, 150_000, 0);
        let review = zeus.final_review(&ctx);
        assert!(review.approved);
        assert!(review.escalated);
        assert!(review.reason.contains("confidential + budget"));
    }

    #[test]
    fn confidential_with_deadlock_blocked() {
        let zeus = ZeusEscalation::new();
        let ctx = ReviewContext::new(Sensitivity::Confidential, 150_000, 2);
        let review = zeus.final_review(&ctx);
        assert!(!review.approved);
        assert!(review.escalated);
        assert!(review.athena_shield.is_some());
    }

    #[test]
    fn should_escalate_checks_all_three_triggers() {
        let zeus = ZeusEscalation::new();
        assert!(zeus.should_escalate(Sensitivity::Confidential, 0, 50_000));
        assert!(zeus.should_escalate(Sensitivity::Public, 2, 50_000));
        assert!(zeus.should_escalate(Sensitivity::Public, 0, 150_000));
        assert!(!zeus.should_escalate(Sensitivity::Public, 0, 50_000));
        assert!(!zeus.should_escalate(Sensitivity::Private, 1, 50_000));
    }

    #[test]
    fn generate_athena_shield_contains_context() {
        let zeus = ZeusEscalation::new();
        let ctx = ReviewContext::new(Sensitivity::Confidential, 150_000, 2);
        let shield = zeus.generate_athena_shield(&ctx);
        assert!(shield.contains("Athena Shield"));
        assert!(shield.contains("Confidential"));
        assert!(shield.contains("150000"));
        assert!(shield.contains("2 times"));
    }

    #[test]
    fn from_task_defaults_to_public() {
        let task = Task::new("test task", 0.5, TaskType::Code);
        let ctx = ReviewContext::from_task(&task);
        assert_eq!(ctx.sensitivity, Sensitivity::Public);
        assert_eq!(ctx.token_budget, task.user_input.len() * 4);
        assert_eq!(ctx.hermes_rejections, 0);
    }

    #[test]
    fn from_task_with_metadata_reads_sensitivity() {
        let mut task = Task::new("test task", 0.5, TaskType::Code);
        task.metadata
            .insert("sensitivity".to_string(), "Confidential".to_string());
        let ctx = ReviewContext::from_task_with_metadata(&task, 1);
        assert_eq!(ctx.sensitivity, Sensitivity::Confidential);
        assert_eq!(ctx.hermes_rejections, 1);
    }
}
