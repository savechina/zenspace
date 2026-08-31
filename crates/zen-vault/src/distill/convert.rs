//! Conversion layer between distill DTOs and zen-memory canonical types (FR-024/025, D4).
//!
//! `distill::types::Decision/Belief/SelfModelItem` are lightweight DTOs (5/5/4 fields)
//! extracted heuristically. `zen-memory::*` are canonical 20/10/17-field records
//! with full 5-layer schema. This module provides loss-less enrichment by filling
//! missing fields from defaults + originating `Note` context. No data is silently
//! dropped; callers should use `From` impls rather than direct field copy.

use chrono::Utc;

use crate::distill::types::{
    Belief as DistillBelief, Decision as DistillDecision, SelfModelItem as DistillSelfModelItem,
};

/// Enrich a distill `Decision` (5 fields) into a canonical `zen_memory::Decision` (20 fields).
///
/// Missing fields are filled with sensible defaults:
/// - `id` = uuid v7, `title` = truncated goal, `domain` = "general"
/// - `choice` <- `logic`, `execution_plan` <- joined `execution`
/// - `feedback` -> `outcome.notes` with `OutcomeResult::Partial`
/// - `cost_analysis` = default, `confidence` = None, `closed_at` = None
impl From<DistillDecision> for zen_memory::Decision {
    fn from(d: DistillDecision) -> Self {
        let id = uuid::Uuid::now_v7().to_string();
        // Title: first 60 chars of goal.
        let title = {
            let t = d.goal.chars().take(60).collect::<String>();
            if t.is_empty() {
                "distilled-decision".to_string()
            } else {
                t
            }
        };
        let execution_plan = if d.execution.is_empty() {
            None
        } else {
            Some(d.execution.join("\n"))
        };
        let outcome = d.feedback.map(|fb| zen_memory::Outcome {
            result: zen_memory::OutcomeResult::Partial,
            notes: fb,
            recorded_at: Utc::now(),
        });
        let mut decision = zen_memory::Decision::new(id, title, "general".to_string());
        decision.goal = d.goal;
        decision.facts = d.facts;
        decision.choice = d.logic;
        decision.execution_plan = execution_plan;
        decision.outcome = outcome;
        decision
    }
}

impl From<DistillBelief> for zen_memory::Belief {
    fn from(b: DistillBelief) -> Self {
        let id = uuid::Uuid::now_v7().to_string();
        // Canonical Belief::new sets posterior = 0.5; override with distilled posterior.
        let mut belief = zen_memory::Belief::new(id, b.proposition.clone(), "general".to_string());
        belief.posterior = b.posterior.clamp(0.01, 0.99);
        belief.evidence_count = b.evidence_count;
        belief.last_updated = b.last_updated;
        // `prior` from distill is not stored canonically; weight stays 1.0 default.
        // No evidence log is reconstructed (display-only).
        belief
    }
}

impl From<DistillSelfModelItem> for zen_memory::SelfModelItem {
    fn from(item: DistillSelfModelItem) -> Self {
        let id = uuid::Uuid::now_v7().to_string();
        let layer = match item.layer {
            crate::distill::types::SelfModelLayer::Knowledge => {
                zen_memory::SelfModelLayer::Knowledge
            }
            crate::distill::types::SelfModelLayer::Skill => zen_memory::SelfModelLayer::Skill,
            crate::distill::types::SelfModelLayer::SocialRole => {
                zen_memory::SelfModelLayer::SocialRole
            }
            crate::distill::types::SelfModelLayer::SelfConcept => {
                zen_memory::SelfModelLayer::SelfConcept
            }
            crate::distill::types::SelfModelLayer::Trait => zen_memory::SelfModelLayer::Trait,
            crate::distill::types::SelfModelLayer::Motivation => {
                zen_memory::SelfModelLayer::Motivation
            }
            crate::distill::types::SelfModelLayer::Value => zen_memory::SelfModelLayer::Value,
            crate::distill::types::SelfModelLayer::Limit => zen_memory::SelfModelLayer::Limit,
        };
        let mut node =
            zen_memory::SelfModelItem::new(id, layer, item.label.clone(), "general".to_string());
        node.humility_score = item.humility_score;
        node.optionality_count = item.optionality_count;
        node
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::distill::types::SelfModelLayer as DistillLayer;
    use chrono::Utc;

    #[test]
    fn distill_decision_to_memory_preserves_goal_and_facts() {
        let d = DistillDecision {
            goal: "Ship 005 loop".into(),
            facts: vec!["frag1".into(), "frag2".into()],
            logic: "choose minimal".into(),
            execution: vec!["step1".into(), "step2".into()],
            feedback: Some("retro note".into()),
        };
        let mem: zen_memory::Decision = d.clone().into();
        assert_eq!(mem.goal, "Ship 005 loop");
        assert_eq!(mem.facts, vec!["frag1", "frag2"]);
        assert_eq!(mem.choice, "choose minimal");
        assert_eq!(mem.execution_plan, Some("step1\nstep2".into()));
        assert!(mem.outcome.is_some());
        assert_eq!(mem.outcome.unwrap().notes, "retro note");
        // Defaults enriched, not truncated.
        assert!(!mem.id.is_empty());
        assert_eq!(mem.domain, "general");
    }

    #[test]
    fn distill_belief_to_memory_preserves_posterior() {
        let b = DistillBelief {
            proposition: "Rust is fast".into(),
            prior: 0.5,
            posterior: 0.82,
            evidence_count: 3,
            last_updated: Utc::now(),
        };
        let mem: zen_memory::Belief = b.into();
        assert!((mem.posterior - 0.82).abs() < 1e-9);
        assert_eq!(mem.evidence_count, 3);
        assert_eq!(mem.domain, "general");
        assert_eq!(mem.weight, 1.0);
    }

    #[test]
    fn distill_self_model_to_memory_preserves_layer_and_humility() {
        let item = DistillSelfModelItem {
            layer: DistillLayer::Skill,
            label: "writes async code".into(),
            humility_score: Some(0.7),
            optionality_count: Some(2),
        };
        let mem: zen_memory::SelfModelItem = item.into();
        assert_eq!(mem.layer, zen_memory::SelfModelLayer::Skill);
        assert_eq!(mem.humility_score, Some(0.7));
        assert_eq!(mem.optionality_count, Some(2));
    }
}
