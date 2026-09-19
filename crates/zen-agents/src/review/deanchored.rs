//! T170: de-anchored semantic judge + calibrated escalation cascade.
//!
//! # Functionality
//! The HIGH-blast-radius delivery review (T092) becomes a cascade. A local
//! judge first **commits its own answer to the task before seeing the draft**,
//! then scores the draft against that commitment — the de-anchoring technique
//! from arXiv 2607.05904, where a self-judge that committed first moved
//! false positives from 0.719 to 0.012. The frontier judge is consulted only
//! when the local verdict's confidence falls below the calibrated escalation
//! threshold τ.
//!
//! # User impact
//! With a calibrated τ the frontier review call — the expensive, cloud-bound
//! hop — is skipped whenever the local judge is confident, which is the
//! V13-A.3 "cloud semantic-review calls −70% at equal-or-better veto
//! precision" baseline. Judgement stops inheriting the draft's framing.
//!
//! # Default behavior
//! **Absent τ the local phase is skipped entirely** and the frontier judge
//! runs exactly as it did before T170 — one call, byte-identical prompt,
//! unchanged cost. No threshold is invented here (V13-A.3): τ comes from the
//! T173 calibration artefact or the `[agentic.review] escalate_threshold`
//! override.
//!
//! # Interaction
//! Fail-open at every hop: a missing local model, a provider error or an
//! unparsable reply escalates to the frontier judge rather than vetoing
//! delivery. The local hop routes through sensitivity enforcement in local
//! mode, so it cannot reach a cloud provider.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::{debug, warn};
use zen_core::paths::ZenPaths;
use zen_core::types::{Sensitivity, Task};
use zen_provider::{DefaultRouter, TaskRequirements};

use super::pipeline::SemanticVerdict;

/// Characters of the draft shown to a judge — unchanged from the pre-T170
/// frontier prompt so the escalation path stays byte-identical.
const DRAFT_CHARS: usize = 4000;

/// One model round-trip. Injectable so the cascade and the de-anchoring
/// invariant are testable without a live provider.
#[async_trait]
pub trait JudgeModel: Send + Sync {
    async fn complete(&self, prompt: &str) -> Result<String, String>;
}

/// Production [`JudgeModel`] over the existing router.
pub struct RouterJudgeModel {
    router: DefaultRouter,
    sensitivity: Sensitivity,
    max_tokens: u32,
}

impl RouterJudgeModel {
    /// `sensitivity` drives routing enforcement: pass
    /// [`Sensitivity::Private`] for the local hop (local-only) and the
    /// session's own sensitivity for the frontier hop.
    pub fn new(router: &DefaultRouter, sensitivity: Sensitivity, max_tokens: u32) -> Self {
        Self {
            router: router.clone(),
            sensitivity,
            max_tokens,
        }
    }
}

#[async_trait]
impl JudgeModel for RouterJudgeModel {
    async fn complete(&self, prompt: &str) -> Result<String, String> {
        let requirements = TaskRequirements {
            max_tokens: Some(self.max_tokens),
            sensitivity: self.sensitivity,
            preferred_model: None,
            budget_limit: None,
        };
        let router = self.router.clone();
        let prompt = prompt.to_string();
        // spawn_blocking guard (same hazard as intent::llm_classify): route()
        // and call() are sync and OllamaProvider constructs a nested tokio
        // Runtime inside them, which panics on an async worker thread. Running
        // off-runtime keeps the local path failing open instead of panicking.
        tokio::task::spawn_blocking(move || {
            use zen_provider::LlmRouter as _;
            router
                .route(&requirements)
                .and_then(|provider| router.call(provider, &prompt))
        })
        .await
        .map_err(|error| format!("judge task join failed: {error}"))?
        .map_err(|error| error.to_string())
    }
}

/// The local judge's structured verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalJudgement {
    pub approved: bool,
    pub note: String,
    pub confidence: f32,
}

/// What the cascade decided, and which rung produced it.
///
/// `local_*` record the local phase's own verdict whenever it ran, *including*
/// when the case escalated. That is deliberate: calibrating the escalation
/// threshold needs the local judge's confidence paired with whether its
/// verdict was right, which is exactly the sample the escalated cases carry.
#[derive(Debug, Clone)]
pub struct JudgeTrace {
    /// True when the frontier judge was consulted.
    pub escalated: bool,
    /// Rung that produced the final verdict: `"L1"` (local) or `"L2"` (frontier).
    pub final_rung: &'static str,
    /// The local phase's confidence, when the local phase ran.
    pub local_confidence: Option<f32>,
    /// The local phase's own verdict, when the local phase ran.
    pub local_approved: Option<bool>,
}

/// A verdict plus the trace of how it was reached.
#[derive(Debug, Clone)]
pub struct JudgedVerdict {
    pub verdict: SemanticVerdict,
    pub trace: JudgeTrace,
}

/// The de-anchored judge cascade.
pub struct DeAnchoredJudge {
    frontier: Arc<dyn JudgeModel>,
    local: Option<Arc<dyn JudgeModel>>,
    escalate_below: Option<f32>,
}

impl DeAnchoredJudge {
    /// `escalate_below` is the calibrated τ. `None` means "no operating point
    /// has been calibrated", which disables the local phase entirely so the
    /// frontier path behaves exactly as it did before T170.
    pub fn new(
        frontier: Arc<dyn JudgeModel>,
        local: Option<Arc<dyn JudgeModel>>,
        escalate_below: Option<f32>,
    ) -> Self {
        Self {
            frontier,
            local,
            escalate_below,
        }
    }

    /// De-anchored local judgement: commit an answer first, then score the
    /// draft against that commitment.
    ///
    /// The commit prompt deliberately excludes the draft — that exclusion *is*
    /// the de-anchoring, and a test pins it.
    pub async fn local_judgement(
        &self,
        task: &Task,
        deliverable: &str,
    ) -> Result<LocalJudgement, String> {
        let local = self
            .local
            .as_ref()
            .ok_or_else(|| "no local judge wired".to_string())?;
        let commitment = local.complete(&commit_prompt(&task.user_input)).await?;
        let scored = local
            .complete(&score_prompt(&commitment, deliverable))
            .await?;
        parse_local_judgement(&scored)
    }

    /// Judge the draft, escalating to the frontier only when needed.
    ///
    /// With no calibrated τ this is exactly the pre-T170 frontier call. With
    /// one, a local verdict at or above τ is final and the frontier is never
    /// consulted.
    pub async fn judge(&self, task: &Task, deliverable: &str) -> JudgedVerdict {
        let Some(threshold) = self.escalate_below else {
            return JudgedVerdict {
                verdict: self.frontier_verdict(task, deliverable).await,
                trace: JudgeTrace {
                    escalated: true,
                    final_rung: "L2",
                    local_confidence: None,
                    local_approved: None,
                },
            };
        };
        match self.local_judgement(task, deliverable).await {
            Ok(local) if local.confidence >= threshold => JudgedVerdict {
                verdict: SemanticVerdict {
                    approved: local.approved,
                    note: format!(
                        "de-anchored local judge (confidence {:.2} >= {threshold:.2}): {}",
                        local.confidence, local.note
                    ),
                    confidence: Some(local.confidence),
                },
                trace: JudgeTrace {
                    escalated: false,
                    final_rung: "L1",
                    local_confidence: Some(local.confidence),
                    local_approved: Some(local.approved),
                },
            },
            Ok(local) => {
                debug!(
                    confidence = local.confidence,
                    threshold, "local judge below the calibrated threshold; escalating"
                );
                JudgedVerdict {
                    verdict: self.frontier_verdict(task, deliverable).await,
                    trace: JudgeTrace {
                        escalated: true,
                        final_rung: "L2",
                        local_confidence: Some(local.confidence),
                        local_approved: Some(local.approved),
                    },
                }
            }
            Err(error) => {
                warn!(%error, "local judge unavailable; escalating to the frontier judge");
                JudgedVerdict {
                    verdict: self.frontier_verdict(task, deliverable).await,
                    trace: JudgeTrace {
                        escalated: true,
                        final_rung: "L2",
                        local_confidence: None,
                        local_approved: None,
                    },
                }
            }
        }
    }

    /// The pre-T170 single-call frontier review, unchanged.
    async fn frontier_verdict(&self, task: &Task, deliverable: &str) -> SemanticVerdict {
        match self
            .frontier
            .complete(&frontier_prompt(&task.user_input, deliverable))
            .await
        {
            Ok(reply) => parse_frontier_verdict(&reply),
            Err(error) => {
                warn!(%error, "semantic reviewer unavailable; failing open");
                SemanticVerdict::approve("reviewer unavailable; heuristic stages passed")
            }
        }
    }
}

/// Record one `loop.decision` line for a review verdict.
///
/// Two consumers depend on this line and neither could exist without it:
/// the `review` calibration sample (kind `"review"`, rung `"L1"`, carrying the
/// local judge's confidence so `review_escalate` can be calibrated), and the
/// V13-A.3 frontier-call baseline, which reads the `escalated` field. The
/// `decision_audit` extractor reads `decision_kind`/`rung`/`confidence`/
/// `choice`, so those names are a cross-crate contract.
pub fn record_decision(paths: &ZenPaths, judged: &JudgedVerdict) {
    let trace = &judged.trace;
    let entry = serde_json::json!({
        "kind": "loop.decision",
        "decision": "review",
        "decision_kind": "review",
        // The local rung is the calibratable one: its confidence is what τ
        // gates on. Emitting the *final* rung would leave escalated cases
        // invisible to calibration, and those are the informative ones.
        "rung": if trace.local_confidence.is_some() { "L1" } else { "L2" },
        "final_rung": trace.final_rung,
        "escalated": trace.escalated,
        "confidence": trace.local_confidence.map(crate::decision::audit_score),
        "choice": match trace.local_approved {
            Some(true) => "approved",
            Some(false) => "vetoed",
            None => match judged.verdict.approved {
                true => "approved",
                false => "vetoed",
            },
        },
        "approved": judged.verdict.approved,
    });
    crate::decision::append_decision_audit(paths, &entry);
}

/// The judge's own answer to the task. **Contains no part of the draft** —
/// that exclusion is what makes the subsequent scoring de-anchored.
pub fn commit_prompt(task: &str) -> String {
    format!(
        "Answer the following task yourself, concisely and completely. \
         Give your best answer; do not evaluate anything and do not ask questions.\n\
         Task: {task}\n\
         Your answer:"
    )
}

/// Score a draft against the judge's own committed answer.
pub fn score_prompt(commitment: &str, deliverable: &str) -> String {
    let draft: String = deliverable.chars().take(DRAFT_CHARS).collect();
    format!(
        "You are a delivery reviewer. Below is YOUR OWN answer to a task, then a DRAFT answer.\n\
         Decide whether the draft is complete, correct, and safe to deliver — judged against your own answer.\n\
         Your own answer:\n{commitment}\n\
         Draft answer (truncated):\n{draft}\n\n\
         Respond with exactly two lines:\n\
         Line 1: \"APPROVED\" or \"REJECTED: <reason>\"\n\
         Line 2: \"CONFIDENCE: <0.0-1.0>\""
    )
}

/// The pre-T170 frontier prompt, preserved byte-for-byte so escalation cost
/// and behaviour are unchanged.
pub fn frontier_prompt(task: &str, deliverable: &str) -> String {
    let draft: String = deliverable.chars().take(DRAFT_CHARS).collect();
    format!(
        "You are a delivery reviewer. Decide whether the draft answer is complete, correct, and safe to deliver.\nTask: {task}\nDraft answer (truncated):\n{draft}\n\nRespond with exactly one line starting with \"APPROVED\" or \"REJECTED: <reason>\"."
    )
}

/// Parse the pre-T170 frontier reply format — behaviour preserved exactly.
pub fn parse_frontier_verdict(reply: &str) -> SemanticVerdict {
    let trimmed = reply.trim();
    if trimmed.to_uppercase().starts_with("APPROVED") {
        SemanticVerdict::approve(trimmed.to_string())
    } else {
        let note = trimmed
            .strip_prefix("REJECTED:")
            .unwrap_or(trimmed)
            .trim()
            .to_string();
        SemanticVerdict::reject(note)
    }
}

/// Parse the local judge's two-line reply. A reply whose confidence cannot be
/// read is an error, not a default — an unreadable score must escalate, never
/// silently pass as confident.
pub fn parse_local_judgement(reply: &str) -> Result<LocalJudgement, String> {
    let confidence = parse_confidence(reply)
        .ok_or_else(|| format!("local judgement missing a parsable CONFIDENCE: {reply}"))?;
    let verdict_line = reply
        .lines()
        .map(str::trim)
        .find(|line| {
            let upper = line.to_uppercase();
            upper.starts_with("APPROVED") || upper.starts_with("REJECTED")
        })
        .ok_or_else(|| format!("local judgement missing a verdict line: {reply}"))?;
    let approved = verdict_line.to_uppercase().starts_with("APPROVED");
    let note = if approved {
        "draft matches the judge's own committed answer".to_string()
    } else {
        strip_rejected_prefix(verdict_line)
    };
    Ok(LocalJudgement {
        approved,
        note,
        confidence,
    })
}

fn strip_rejected_prefix(line: &str) -> String {
    let lower = line.to_lowercase();
    if lower.starts_with("rejected:") {
        line["rejected:".len()..].trim().to_string()
    } else {
        line.trim().to_string()
    }
}

fn parse_confidence(reply: &str) -> Option<f32> {
    for line in reply.lines() {
        let lower = line.to_lowercase();
        if let Some(index) = lower.find("confidence:") {
            let raw = line[index + "confidence:".len()..].trim();
            let token = raw
                .split(|c: char| c.is_whitespace() || c == ',' || c == '%')
                .next()?;
            if let Ok(value) = token.trim().parse::<f32>() {
                return Some(value.clamp(0.0, 1.0));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Records every prompt it is asked to complete and replies from a script.
    struct ScriptedModel {
        replies: Mutex<Vec<String>>,
        prompts: Mutex<Vec<String>>,
    }

    impl ScriptedModel {
        fn new(replies: &[&str]) -> Arc<Self> {
            Arc::new(Self {
                replies: Mutex::new(replies.iter().map(|r| (*r).to_string()).collect()),
                prompts: Mutex::new(Vec::new()),
            })
        }

        fn prompts(&self) -> Vec<String> {
            self.prompts.lock().expect("lock").clone()
        }
    }

    #[async_trait]
    impl JudgeModel for ScriptedModel {
        async fn complete(&self, prompt: &str) -> Result<String, String> {
            self.prompts.lock().expect("lock").push(prompt.to_string());
            let mut replies = self.replies.lock().expect("lock");
            if replies.is_empty() {
                return Err("script exhausted".to_string());
            }
            Ok(replies.remove(0))
        }
    }

    struct FailingModel;

    #[async_trait]
    impl JudgeModel for FailingModel {
        async fn complete(&self, _prompt: &str) -> Result<String, String> {
            Err("local model offline".to_string())
        }
    }

    fn task() -> Task {
        Task::new(
            "explain tokio cancellation",
            0.5,
            zen_core::types::TaskType::Text,
        )
    }

    #[tokio::test]
    async fn commit_prompt_never_includes_the_draft() {
        // This IS the de-anchoring invariant: if the draft leaks into the
        // commitment, the judge is anchored and the technique is defeated.
        let draft = "UNIQUE_DRAFT_MARKER_12345";
        let local = ScriptedModel::new(&["my own answer", "APPROVED\nCONFIDENCE: 0.95"]);
        let frontier = ScriptedModel::new(&["APPROVED"]);
        let judge = DeAnchoredJudge::new(frontier, Some(local.clone()), Some(0.8));

        judge.judge(&task(), draft).await;

        let prompts = local.prompts();
        assert_eq!(prompts.len(), 2, "commit then score");
        assert!(
            !prompts[0].contains(draft),
            "the commit prompt must not contain the draft:\n{}",
            prompts[0]
        );
        assert!(prompts[0].contains("explain tokio cancellation"));
        assert!(
            prompts[1].contains(draft),
            "the score prompt must contain the draft"
        );
        assert!(
            prompts[1].contains("my own answer"),
            "the score prompt must contain the judge's own commitment"
        );
    }

    #[tokio::test]
    async fn confident_local_verdict_short_circuits_the_frontier() {
        let local = ScriptedModel::new(&["mine", "REJECTED: unsupported claim\nCONFIDENCE: 0.95"]);
        let frontier = ScriptedModel::new(&["APPROVED"]);
        let judge = DeAnchoredJudge::new(frontier.clone(), Some(local), Some(0.8));

        let verdict = judge.judge(&task(), "a draft").await;

        assert!(!verdict.verdict.approved, "the local verdict decides");
        assert_eq!(verdict.verdict.confidence, Some(0.95));
        assert!(verdict.verdict.note.contains("unsupported claim"));
        assert!(!verdict.trace.escalated);
        assert_eq!(verdict.trace.final_rung, "L1");
        assert_eq!(verdict.trace.local_confidence, Some(0.95));
        assert!(
            frontier.prompts().is_empty(),
            "a confident local verdict must not pay for the frontier call"
        );
    }

    #[tokio::test]
    async fn low_confidence_local_verdict_escalates_to_the_frontier() {
        let local = ScriptedModel::new(&["mine", "APPROVED\nCONFIDENCE: 0.4"]);
        let frontier = ScriptedModel::new(&["REJECTED: incomplete"]);
        let judge = DeAnchoredJudge::new(frontier.clone(), Some(local), Some(0.8));

        let verdict = judge.judge(&task(), "a draft").await;

        assert!(!verdict.verdict.approved, "the frontier verdict decides");
        assert_eq!(verdict.verdict.confidence, None);
        assert!(verdict.trace.escalated);
        assert_eq!(verdict.trace.final_rung, "L2");
        assert_eq!(
            verdict.trace.local_confidence,
            Some(0.4),
            "the local verdict is recorded even when it escalated — these are the \
             samples the escalation threshold is calibrated from"
        );
        assert_eq!(frontier.prompts().len(), 1);
    }

    #[tokio::test]
    async fn absent_threshold_skips_the_local_phase_entirely() {
        // The pre-T170 behaviour: one frontier call, no local cost.
        let local = ScriptedModel::new(&["mine", "APPROVED\nCONFIDENCE: 0.99"]);
        let frontier = ScriptedModel::new(&["APPROVED"]);
        let judge = DeAnchoredJudge::new(frontier.clone(), Some(local.clone()), None);

        let verdict = judge.judge(&task(), "a draft").await;

        assert!(verdict.verdict.approved);
        assert_eq!(verdict.verdict.confidence, None);
        assert!(verdict.trace.escalated);
        assert_eq!(
            verdict.trace.local_confidence, None,
            "no τ ⇒ the local phase did not run at all"
        );
        assert_eq!(frontier.prompts().len(), 1, "frontier still runs once");
        assert!(
            local.prompts().is_empty(),
            "no calibrated τ ⇒ the local phase must not run at all"
        );
    }

    #[tokio::test]
    async fn local_failure_escalates_instead_of_vetoing() {
        let frontier = ScriptedModel::new(&["APPROVED"]);
        let judge = DeAnchoredJudge::new(frontier.clone(), Some(Arc::new(FailingModel)), Some(0.8));

        let verdict = judge.judge(&task(), "a draft").await;

        assert!(
            verdict.verdict.approved,
            "a broken local judge must not block delivery"
        );
        assert!(verdict.trace.escalated);
        assert_eq!(frontier.prompts().len(), 1);
    }

    #[tokio::test]
    async fn unparsable_local_confidence_escalates() {
        let local = ScriptedModel::new(&["mine", "APPROVED"]);
        let frontier = ScriptedModel::new(&["APPROVED"]);
        let judge = DeAnchoredJudge::new(frontier.clone(), Some(local), Some(0.8));

        let verdict = judge.judge(&task(), "a draft").await;

        assert!(verdict.verdict.approved);
        assert!(verdict.trace.escalated);
        assert_eq!(
            frontier.prompts().len(),
            1,
            "an unreadable score must escalate, never pass as confident"
        );
    }

    fn test_paths() -> ZenPaths {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        std::mem::forget(dir);
        ZenPaths::for_testing(root)
    }

    #[tokio::test]
    async fn review_decision_line_carries_the_calibration_sample() {
        // Two consumers depend on this line: `review_escalate` calibration
        // (kind=review, rung=L1, confidence) and the frontier-call baseline
        // (escalated). If these field names drift, both silently go dark.
        let paths = test_paths();
        let local = ScriptedModel::new(&["mine", "APPROVED\nCONFIDENCE: 0.6"]);
        let frontier = ScriptedModel::new(&["APPROVED"]);
        let judge = DeAnchoredJudge::new(frontier, Some(local), Some(0.9));

        let judged = judge.judge(&task(), "a draft").await;
        record_decision(&paths, &judged);

        let audit = std::fs::read_to_string(paths.logs().join("audit.jsonl")).expect("audit file");
        let entry: serde_json::Value =
            serde_json::from_str(audit.lines().next().expect("one line")).expect("json");

        assert_eq!(entry["kind"], "loop.decision");
        assert_eq!(entry["decision_kind"], "review");
        assert_eq!(
            entry["rung"], "L1",
            "the local rung is emitted so escalated cases stay calibratable"
        );
        assert_eq!(entry["escalated"], true, "0.6 < 0.9, so it escalated");
        assert_eq!(entry["confidence"], 0.6);
        assert_eq!(entry["choice"], "approved");
    }

    #[test]
    fn parse_local_judgement_reads_both_line_styles() {
        let approved = parse_local_judgement("APPROVED\nCONFIDENCE: 0.91").expect("parsed");
        assert!(approved.approved);
        assert_eq!(approved.confidence, 0.91);

        let rejected = parse_local_judgement("REJECTED: missing error handling\nconfidence: 0.77 ")
            .expect("parsed");
        assert!(!rejected.approved);
        assert_eq!(rejected.confidence, 0.77);
        assert_eq!(rejected.note, "missing error handling");

        let prose = parse_local_judgement(
            "Here is my verdict:\nREJECTED: unsupported\nCONFIDENCE: 0.8 — fairly sure",
        )
        .expect("parsed");
        assert!(!prose.approved);
        assert_eq!(prose.confidence, 0.8);

        assert!(parse_local_judgement("no verdict, no score").is_err());
        assert!(parse_local_judgement("APPROVED").is_err(), "score required");
    }

    #[test]
    fn frontier_parsing_is_unchanged_from_pre_t170() {
        assert!(parse_frontier_verdict("APPROVED").approved);
        assert!(parse_frontier_verdict("approved — looks fine").approved);

        let rejected = parse_frontier_verdict("REJECTED: not enough detail");
        assert!(!rejected.approved);
        assert_eq!(rejected.note, "not enough detail");

        let unexpected = parse_frontier_verdict("I am not sure");
        assert!(!unexpected.approved, "anything not APPROVED is a veto");
    }

    #[test]
    fn frontier_prompt_matches_the_pre_t170_shape() {
        let prompt = frontier_prompt("do the thing", "the draft");
        assert!(prompt.starts_with("You are a delivery reviewer."));
        assert!(prompt.contains("Task: do the thing"));
        assert!(prompt.contains("Draft answer (truncated):\nthe draft"));
        assert!(prompt.ends_with("\"REJECTED: <reason>\"."));
    }
}
