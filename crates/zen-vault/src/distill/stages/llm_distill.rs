/// LLM Distill Stage (FR-003) — bounded LLM enrichment with heuristic fallback.
///
/// ## Scope Logic
///
/// **Functionality**: When `LoopConfig::merge_llm_model` is `Some(model)`, attempts
/// LLM-based notion enrichment via `zen_provider::DefaultRouter`. If the router
/// is unavailable, the model is not configured, or the budget is exceeded, falls
/// back to the deterministic `NotionExtractor` heuristic.
///
/// **User impact**: Notes with an LLM-capable model configured get richer notion
/// extraction (confidence-scored, relation-aware). Without LLM config, the system
/// behaves identically to the pre-FR-003 heuristic path — zero regression.
///
/// **Default behavior**: `model: None` → pure heuristic extraction (no LLM call).
///
/// **Interaction**: Token consumption is bounded by `LoopBudget::consume_tokens`.
/// Once over budget, the stage short-circuits to heuristic for all remaining notes.
use anyhow::Result;
use tracing::{debug, info, warn};

use super::super::notion_extraction::NotionExtractor;
use super::super::types::LoopBudget;
use crate::note::Note;
use crate::notion::Notion as NotionType;

/// Stub stage for LLM-enhanced distillation (FR-003).
///
/// Holds a `LoopBudget` for token accounting and an optional model name. When
/// the model is `None` or the LLM call fails, all work falls through to the
/// heuristic `NotionExtractor`.
pub struct LlmDistillStage {
    pub budget: LoopBudget,
    /// Target model identifier (e.g. `"openai:gpt-4o"`). `None` disables LLM.
    pub model: Option<String>,
    /// Heuristic fallback extractor — always available.
    extractor: NotionExtractor,
}

impl LlmDistillStage {
    /// Create a new stage with the given budget ceiling and optional model.
    pub fn new(budget: LoopBudget, model: Option<String>) -> Self {
        Self {
            budget,
            model,
            extractor: NotionExtractor::new(),
        }
    }

    /// Distill notes into notions, using LLM when possible, heuristic otherwise.
    ///
    /// ## Algorithm
    /// 1. If `self.model` is `None` → full heuristic pass (no budget consumption).
    /// 2. If `self.model` is `Some` → estimate token cost per note, attempt
    ///    `budget.consume_tokens(estimate)`. If budget refused → fallback to
    ///    heuristic for remaining notes.
    /// 3. LLM path is a **stub**: logs the intent and falls through to heuristic
    ///    until the zen-provider integration is wired (tracked in FR-003 follow-up).
    ///
    /// Returns `(notions, consumed_tokens)` — caller can persist the budget state.
    pub fn distill_with_fallback(&mut self, notes: &[Note]) -> Result<(Vec<NotionType>, u32)> {
        // Phase 1: If no model configured, pure heuristic — zero cost.
        let model = match &self.model {
            Some(m) => m.clone(),
            None => {
                debug!("LlmDistillStage: no model configured, using heuristic only");
                let notions = self.extractor.extract_batch(notes)?;
                return Ok((notions, 0));
            }
        };

        info!(
            model = %model,
            notes_count = notes.len(),
            "LlmDistillStage: attempting LLM enrichment"
        );

        // Phase 2: Estimate budget. Rough heuristic: ~500 tokens per note
        // for LLM context (prompt + completion).
        let estimate = (notes.len() as u32) * 500;
        if !self.budget.consume_tokens(estimate) {
            warn!(
                estimated = estimate,
                remaining = self
                    .budget
                    .max_tokens
                    .saturating_sub(self.budget.consumed_tokens),
                "LlmDistillStage: budget exceeded, falling back to heuristic"
            );
            let notions = self.extractor.extract_batch(notes)?;
            return Ok((notions, 0));
        }

        // Phase 3: LLM call stub — route through DefaultRouter when wired.
        // For now, log the intent and fall through to heuristic.
        //
        // Future integration (FR-003 follow-up):
        //   let router = zen_provider::DefaultRouter::new();
        //   for note in notes {
        //       let prompt = build_distill_prompt(note);
        //       let response = router.complete(&model, &prompt, Sensitivity::Public)?;
        //       notions.extend(parse_llm_notions(&response)?);
        //   }
        debug!(
            model = %model,
            tokens_budgeted = estimate,
            "LlmDistillStage: LLM stub — defaulting to heuristic extraction"
        );

        let notions = self.extractor.extract_batch(notes)?;
        info!(
            notions_extracted = notions.len(),
            tokens_consumed = estimate,
            "LlmDistillStage: distillation complete (heuristic path)"
        );

        Ok((notions, estimate))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::note::Note;

    fn sample_note(content: &str) -> Note {
        Note {
            id: "test-001".into(),
            content: content.into(),
            ..Default::default()
        }
    }

    #[test]
    fn no_model_uses_heuristic() {
        let mut stage = LlmDistillStage::new(LoopBudget::default(), None);
        let notes = vec![sample_note("Using Rust and PostgreSQL for the project")];
        let (notions, cost) = stage.distill_with_fallback(&notes).unwrap();
        assert_eq!(cost, 0, "no model should consume zero tokens");
        assert!(
            !notions.is_empty(),
            "heuristic should extract at least one notion"
        );
    }

    #[test]
    fn over_budget_falls_back_to_heuristic() {
        let budget = LoopBudget {
            max_tokens: 10, // very low ceiling
            ..Default::default()
        };
        let mut stage = LlmDistillStage::new(budget, Some("test:model".into()));
        let notes = vec![sample_note("Building with TypeScript and React")];
        let (notions, cost) = stage.distill_with_fallback(&notes).unwrap();
        assert_eq!(cost, 0, "over-budget should consume zero additional tokens");
        assert!(!notions.is_empty());
    }

    #[test]
    fn stub_model_uses_heuristic_path() {
        let budget = LoopBudget::default();
        let mut stage = LlmDistillStage::new(budget, Some("openai:gpt-4o".into()));
        let notes = vec![sample_note("Exploring WASM and Docker containers")];
        let (notions, cost) = stage.distill_with_fallback(&notes).unwrap();
        assert!(cost > 0, "in-budget stub should consume estimated tokens");
        assert!(!notions.is_empty());
    }
}
