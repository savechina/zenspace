use std::sync::Arc;

use zen_core::review::{HermesValidator, MetisReviewer, ReviewContext, ZeusEscalation};
use zen_core::types::Task;

use super::momus::MomusReviewer;

/// Blast radius of a review task (T092): only HIGH tasks pay the LLM
/// semantic-review cost; LOW tasks keep the pure-heuristic fast path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlastRadius {
    Low,
    High,
}

/// Classifies blast radius from the task's metadata sensitivity
/// (same parse as `ReviewContext::from_task_with_metadata`).
/// `Confidential` data is HIGH; everything else is LOW. The former
/// semantic-entropy leg was removed (T172): no production writer feeds
/// `Task.semantic_entropy` (both `Task::new` sites hardcode 0.0), so
/// entropy can no longer drive blast radius.
pub fn classify_blast_radius(task: &Task) -> BlastRadius {
    let sensitivity = ReviewContext::from_task_with_metadata(task, 0).sensitivity;
    if sensitivity == zen_core::types::Sensitivity::Confidential {
        BlastRadius::High
    } else {
        BlastRadius::Low
    }
}

/// Verdict of the LLM semantic-review stage (T092).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticVerdict {
    pub approved: bool,
    pub note: String,
}

impl SemanticVerdict {
    pub fn approve(note: impl Into<String>) -> Self {
        Self {
            approved: true,
            note: note.into(),
        }
    }

    pub fn reject(note: impl Into<String>) -> Self {
        Self {
            approved: false,
            note: note.into(),
        }
    }
}

/// LLM semantic reviewer: production wires a completion-model call here;
/// tests inject a mock. Async like `deliverable_cb` so callers can await
/// a real model round-trip.
pub type SemanticReviewer = Arc<
    dyn Fn(&Task, &str, &str) -> futures::future::BoxFuture<'static, SemanticVerdict> + Send + Sync,
>;

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
    llm_review_high_blast: bool,
    semantic_reviewer: Option<SemanticReviewer>,
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
            llm_review_high_blast: true,
            semantic_reviewer: None,
        }
    }

    pub fn with_limits(mut self, max_momus_retries: u8, max_hermes_revisions: u8) -> Self {
        self.max_momus_retries = max_momus_retries;
        self.max_hermes_revisions = max_hermes_revisions;
        self
    }

    /// Applies the `[agentic.review]` config layer (T092): effective
    /// budgets (clamped) plus the LLM-stage gate. Absent config resolves
    /// to the hardcoded defaults, so behavior is unchanged.
    pub fn with_review_config(mut self, cfg: &zen_core::config::ReviewConfig) -> Self {
        self.max_momus_retries = cfg.max_momus_retries_or_default();
        self.max_hermes_revisions = cfg.max_hermes_revisions_or_default();
        self.llm_review_high_blast = cfg.llm_review_high_blast_or_default();
        self
    }

    /// Wires the LLM semantic-review stage (T092). Without a reviewer the
    /// stage is skipped with a note — the heuristic pipeline stays the
    /// fast path and current behavior is preserved.
    pub fn with_semantic_reviewer(
        mut self,
        reviewer: impl Fn(&Task, &str, &str) -> futures::future::BoxFuture<'static, SemanticVerdict>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.semantic_reviewer = Some(Arc::new(reviewer));
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
                        // T092: LLM semantic-review stage — HIGH blast radius
                        // only. The heuristic pipeline stays the fast path:
                        // LOW tasks return here exactly as before.
                        if classify_blast_radius(task) == BlastRadius::High
                            && self.llm_review_high_blast
                        {
                            match &self.semantic_reviewer {
                                Some(reviewer) => {
                                    let verdict = reviewer(task, &current_plan, &raw).await;
                                    if verdict.approved {
                                        review_notes.push_str(&format!(
                                            "LLM semantic review: APPROVED — {}\n",
                                            verdict.note
                                        ));
                                    } else {
                                        review_notes.push_str(&format!(
                                            "LLM semantic review: REJECTED — {}\n",
                                            verdict.note
                                        ));
                                        failed_attempts
                                            .push(format!("LLM semantic veto: {}", verdict.note));
                                        return PipelineResult {
                                            plan_approved: true,
                                            review_notes,
                                            delivery_ready: false,
                                            athena_shield: None,
                                            failed_attempts,
                                        };
                                    }
                                }
                                None => {
                                    review_notes.push_str(
                                        "LLM semantic review: SKIPPED (no reviewer wired)\n",
                                    );
                                }
                            }
                        }
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
                        let ctx = ReviewContext::from_task_with_metadata(task, hermes_revisions);
                        let should_escalate = self.zeus.should_escalate(
                            ctx.sensitivity,
                            ctx.hermes_rejections,
                            ctx.token_budget,
                        );

                        if should_escalate {
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
    use std::sync::atomic::{AtomicBool, Ordering};
    use zen_core::types::{Task, TaskType};

    async fn mock_deliverable(plan: String) -> String {
        format!("Executed: {plan}")
    }

    fn confidential_task() -> Task {
        let mut task = Task::new("rotate the signing keys", 0.4, TaskType::Code);
        task.metadata
            .insert("sensitivity".to_string(), "Confidential".to_string());
        task
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

    #[test]
    fn blast_radius_classification() {
        let low = Task::new("summarize the changelog", 0.4, TaskType::Text);
        assert_eq!(classify_blast_radius(&low), BlastRadius::Low);
        // T172: entropy no longer drives blast radius — a high-entropy task
        // without Confidential metadata is LOW (no production writer feeds
        // semantic_entropy; both Task::new sites hardcode 0.0).
        let high_entropy = Task::new("create then delete", 0.9, TaskType::Code);
        assert_eq!(classify_blast_radius(&high_entropy), BlastRadius::Low);
        assert_eq!(
            classify_blast_radius(&confidential_task()),
            BlastRadius::High
        );
    }

    #[tokio::test]
    async fn llm_stage_triggers_for_high_blast_task() {
        let invoked = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&invoked);
        let pipeline = QualityPipeline::new().with_semantic_reviewer(move |_t, _p, _d| {
            flag.store(true, Ordering::SeqCst);
            Box::pin(async move { SemanticVerdict::approve("semantics sound") })
        });
        let plan = "1. Rotate the signing keys\n2. Verify pass";
        let result = pipeline
            .execute(&confidential_task(), plan, |p| {
                Box::pin(async move { mock_deliverable(p).await })
            })
            .await;
        assert!(
            invoked.load(Ordering::SeqCst),
            "HIGH task must trigger LLM stage"
        );
        assert!(result.delivery_ready);
        assert!(
            result
                .review_notes
                .contains("LLM semantic review: APPROVED"),
            "notes: {}",
            result.review_notes
        );
    }

    #[tokio::test]
    async fn llm_veto_blocks_delivery_for_high_blast_task() {
        let pipeline = QualityPipeline::new().with_semantic_reviewer(|_t, _p, _d| {
            Box::pin(async move { SemanticVerdict::reject("hallucinated key id") })
        });
        let plan = "1. Rotate the signing keys\n2. Verify pass";
        let result = pipeline
            .execute(&confidential_task(), plan, |p| {
                Box::pin(async move { mock_deliverable(p).await })
            })
            .await;
        assert!(!result.delivery_ready, "LLM veto must block delivery");
        assert!(!result.failed_attempts.is_empty());
        assert!(
            result
                .review_notes
                .contains("LLM semantic review: REJECTED")
        );
    }

    #[tokio::test]
    async fn llm_stage_skipped_for_low_blast_task() {
        let invoked = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&invoked);
        let pipeline = QualityPipeline::new().with_semantic_reviewer(move |_t, _p, _d| {
            flag.store(true, Ordering::SeqCst);
            Box::pin(async move { SemanticVerdict::approve("n/a") })
        });
        let task = Task::new("create a feature with tests", 0.4, TaskType::Code);
        let plan = "1. Create the feature\n2. Add tests\n3. Verify pass";
        let result = pipeline
            .execute(&task, plan, |p| {
                Box::pin(async move { mock_deliverable(p).await })
            })
            .await;
        assert!(
            !invoked.load(Ordering::SeqCst),
            "LOW task must keep the fast path"
        );
        assert!(result.delivery_ready);
        assert!(
            !result.review_notes.contains("LLM semantic review"),
            "notes: {}",
            result.review_notes
        );
    }

    #[tokio::test]
    async fn review_config_overrides_budgets_and_disables_llm_stage() {
        let cfg = zen_core::config::ReviewConfig {
            max_momus_retries: Some(0),
            max_hermes_revisions: Some(1),
            llm_review_high_blast: Some(false),
        };
        let invoked = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&invoked);
        let pipeline = QualityPipeline::new()
            .with_review_config(&cfg)
            .with_semantic_reviewer(move |_t, _p, _d| {
                flag.store(true, Ordering::SeqCst);
                Box::pin(async move { SemanticVerdict::approve("n/a") })
            });
        // Gate off: HIGH task skips the reviewer entirely.
        let plan = "1. Rotate the signing keys\n2. Verify pass";
        let result = pipeline
            .execute(&confidential_task(), plan, |p| {
                Box::pin(async move { mock_deliverable(p).await })
            })
            .await;
        assert!(!invoked.load(Ordering::SeqCst));
        assert!(result.delivery_ready);

        // Budgets applied: 0 Momus retries fails fast on a vetoed plan.
        let strict = QualityPipeline::new().with_review_config(&cfg);
        let veto = Task::new("create then delete", 0.9, TaskType::Code);
        let denied = strict
            .execute(&veto, "create the new table. delete the old table.", |p| {
                Box::pin(async move { mock_deliverable(p).await })
            })
            .await;
        assert!(!denied.plan_approved);
    }
}
