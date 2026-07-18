use zen_core::types::Task;

use zen_core::review::{HermesValidator, MetisReviewer, ReviewContext, ZeusEscalation};

use super::momus::MomusReviewer;

#[derive(Debug)]
pub struct PipelineResult {
    pub plan_approved: bool,
    pub review_notes: String,
    pub delivery_ready: bool,
    pub athena_shield: Option<String>,
    pub failed_attempts: Vec<String>,
}

pub struct QualityPipeline {
    metis: MetisReviewer,
    momus: MomusReviewer,
    hermes: HermesValidator,
    zeus: ZeusEscalation,
    max_momus_retries: u8,
    max_hermes_revisions: u8,
}

impl QualityPipeline {
    pub fn new() -> Self {
        Self {
            metis: MetisReviewer::new(),
            momus: MomusReviewer::new(),
            hermes: HermesValidator::new(),
            zeus: ZeusEscalation::new(),
            max_momus_retries: 2,
            max_hermes_revisions: 1,
        }
    }

    pub fn with_limits(mut self, max_momus_retries: u8, max_hermes_revisions: u8) -> Self {
        self.max_momus_retries = max_momus_retries;
        self.max_hermes_revisions = max_hermes_revisions;
        self
    }

    pub async fn execute(
        &self,
        task: &Task,
        plan: &str,
        deliverable_cb: impl Fn(String) -> futures::future::BoxFuture<'static, String>,
    ) -> PipelineResult {
        let mut review_notes = String::new();
        let mut failed_attempts = Vec::new();
        let current_plan = plan.to_string();

        let metis_review = self.metis.review_plan(task, &current_plan);
        if !metis_review.findings.is_empty() {
            review_notes.push_str(&format!(
                "Metis suggestions ({count}): {details}\n",
                count = metis_review.findings.len(),
                details = metis_review
                    .findings
                    .iter()
                    .map(|f| f.description.clone())
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        review_notes.push_str(&format!(
            "Metis optimization score: {score}\n",
            score = metis_review.optimization_score
        ));

        for attempt in 0..=self.max_momus_retries {
            let momus_review = self.momus.gate_review(task, &current_plan);

            if momus_review.approved {
                review_notes.push_str("Momus gate: APPROVED\n");
                let mut task_with_revision = task.clone();
                task_with_revision
                    .metadata
                    .insert("revision_count".to_string(), "0".to_string());
                let raw = deliverable_cb(current_plan.clone()).await;
                let mut hermes_revisions = 0u8;

                loop {
                    let hermes_validation =
                        self.hermes.validate_deliverable(&task_with_revision, &raw);

                    if self.hermes.can_push(&hermes_validation) {
                        review_notes.push_str("Hermes validation: DELIVERY READY\n");
                        return PipelineResult {
                            plan_approved: true,
                            review_notes,
                            delivery_ready: true,
                            athena_shield: None,
                            failed_attempts,
                        };
                    }

                    hermes_revisions += 1;
                    if hermes_revisions > self.max_hermes_revisions {
                        let is_high_risk = task.semantic_entropy > 0.8;
                        let ctx = ReviewContext::from_task_with_metadata(task, hermes_revisions);
                        let should_escalate = self.zeus.should_escalate(
                            ctx.sensitivity,
                            ctx.hermes_rejections,
                            ctx.token_budget,
                        );

                        if should_escalate || is_high_risk {
                            review_notes.push_str("Hermes deadlock detected, escalating to Zeus\n");
                            let zeus_review = self.zeus.final_review(&ctx);
                            if let Some(shield) = zeus_review.athena_shield {
                                return PipelineResult {
                                    plan_approved: true,
                                    review_notes,
                                    delivery_ready: zeus_review.approved,
                                    athena_shield: Some(shield),
                                    failed_attempts,
                                };
                            }
                            return PipelineResult {
                                plan_approved: true,
                                review_notes,
                                delivery_ready: zeus_review.approved,
                                athena_shield: None,
                                failed_attempts,
                            };
                        }

                        review_notes
                            .push_str("Hermes revision limit reached, returning deliverable\n");
                        return PipelineResult {
                            plan_approved: true,
                            review_notes,
                            delivery_ready: false,
                            athena_shield: None,
                            failed_attempts,
                        };
                    }

                    review_notes.push_str(&format!(
                        "Hermes revision {}/{}: {}\n",
                        hermes_revisions,
                        self.max_hermes_revisions,
                        hermes_validation
                            .findings
                            .iter()
                            .map(|f| f.description.clone())
                            .collect::<Vec<_>>()
                            .join("; ")
                    ));
                }
            } else {
                review_notes.push_str(&format!(
                    "Momus gate: REJECTED (attempt {}) — {}\n",
                    attempt + 1,
                    momus_review.veto_reason.as_deref().unwrap_or("unknown")
                ));
                failed_attempts.push(format!(
                    "Momus veto {}: {}",
                    attempt + 1,
                    momus_review.veto_reason.as_deref().unwrap_or("unknown")
                ));

                if attempt >= self.max_momus_retries {
                    let ctx = ReviewContext::from_task_with_metadata(task, 0);
                    let zeus_review = self.zeus.final_review(&ctx);
                    return PipelineResult {
                        plan_approved: false,
                        review_notes,
                        delivery_ready: false,
                        athena_shield: zeus_review.athena_shield,
                        failed_attempts,
                    };
                }
            }
        }

        PipelineResult {
            plan_approved: false,
            review_notes,
            delivery_ready: false,
            athena_shield: None,
            failed_attempts,
        }
    }

    pub fn metis(&self) -> &MetisReviewer {
        &self.metis
    }

    pub fn momus(&self) -> &MomusReviewer {
        &self.momus
    }

    pub fn hermes(&self) -> &HermesValidator {
        &self.hermes
    }

    pub fn zeus(&self) -> &ZeusEscalation {
        &self.zeus
    }
}

impl Default for QualityPipeline {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zen_core::types::{Task, TaskType};

    async fn mock_deliverable(plan: String) -> String {
        format!("Executed: {plan}")
    }

    #[tokio::test]
    async fn pipeline_happy_path() {
        let pipeline = QualityPipeline::new();
        let task = Task::new("create a feature with tests", 0.4, TaskType::Code);
        let plan = "1. Create the feature\n2. Add tests\n3. Verify pass";

        let result = pipeline
            .execute(&task, plan, |p| {
                Box::pin(async move { mock_deliverable(p).await })
            })
            .await;

        assert!(result.plan_approved);
        assert!(result.delivery_ready);
        assert!(result.athena_shield.is_none());
        assert!(result.review_notes.contains("Momus gate: APPROVED"));
        assert!(
            result
                .review_notes
                .contains("Hermes validation: DELIVERY READY")
        );
    }

    #[tokio::test]
    async fn pipeline_momus_veto_escalates_to_zeus() {
        let pipeline = QualityPipeline::new().with_limits(0, 1);
        let task = Task::new("create then delete", 0.9, TaskType::Code);
        let plan = "create the new table. delete the old table. verify it works";

        let result = pipeline
            .execute(&task, plan, |p| {
                Box::pin(async move { mock_deliverable(p).await })
            })
            .await;

        assert!(!result.plan_approved);
        assert!(!result.delivery_ready);
        assert!(!result.failed_attempts.is_empty());
        assert!(result.review_notes.contains("Momus gate: REJECTED"));
    }

    #[tokio::test]
    async fn pipeline_hermes_loopback_on_bad_deliverable() {
        let pipeline = QualityPipeline::new().with_limits(2, 0);
        let task = Task::new("implement authentication middleware", 0.5, TaskType::Code);
        let plan = "1. Implement auth middleware\n2. Write tests";

        let result = pipeline
            .execute(&task, plan, |_p| {
                Box::pin(async move { "bad deliverable with no auth middleware".to_string() })
            })
            .await;

        assert!(result.plan_approved);
        assert!(result.review_notes.contains("Momus gate: APPROVED"));
        assert!(
            result.review_notes.contains("Hermes revision")
                || result.review_notes.contains("DELIVERY READY")
                || result.review_notes.contains("deadlock")
                || result.review_notes.contains("limit reached"),
            "Expected Hermes revision/limit/deadlock in review notes: {}",
            result.review_notes
        );
    }
}
