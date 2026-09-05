//! Pi-style trigger-hit auto-routing (FR-037 / D03, contract `skill-hit.json`).
//!
//! Matches a user query against skill trigger phrases **before**
//! [`crate::orchestrator::AgentOrchestrator::route()`]; on a hit the skill
//! prompt is injected into the M1 context (top-5, Cowan-4 capped — see
//! `orchestrator::M1_TOP_K`).
//!
//! # Similarity implementation (loud statement, per task T075)
//!
//! The contract documents the score as **trigram-Jaccard 0-1** — no
//! embedding-cosine helper is injectable here without pulling the ONNX
//! embedding runtime (ort / fastembed) into the per-turn routing path, a
//! heavyweight dependency the task forbids. The score is **trigram-Jaccard**,
//! reused from `zen_vault::distill::trigram_jaccard` (same 0-1 scale,
//! already a workspace dependency, no new crates). Two properties keep it
//! contract compatible:
//! 1. the score stays in `0.0..=1.0`, compared against the same 0.72
//!    threshold constant;
//! 2. containment (normalized query contains the normalized trigger) scores
//!    a full 1.0, so literal trigger hits are never lost to bag-of-grams
//!    dilution on long queries.
//!
//! Swapping in a real embedding cosine later only requires replacing
//! [`score_trigger`] — the threshold, max_hits and gating stay identical.

use std::collections::HashSet;

use zen_repo::normalize_alias;
use zen_vault::distill::trigram_jaccard;

use crate::skill_loader::SkillDefinition;

/// Minimum score for a trigger to count as a hit (contract skill-hit.json).
pub const SKILL_HIT_THRESHOLD: f32 = 0.72;

/// Maximum number of skills injected per query (contract skill-hit.json).
pub const SKILL_HIT_MAX_HITS: usize = 1;

/// Rendered skill prompt cap injected into M1 (chars, ~1k tokens).
pub const SKILL_PROMPT_MAX_CHARS: usize = 4000;

/// One matched skill (contract `skill-hit.json` output shape).
#[derive(Debug, Clone, PartialEq)]
pub struct SkillHit {
    /// `SKILL.md` name.
    pub skill: String,
    /// Similarity score in `0.0..=1.0` (trigram-Jaccard; see module docs).
    pub score: f32,
    /// Trigger phrases that individually reached the threshold.
    pub triggers_matched: Vec<String>,
}

/// Routes user queries to skills by normalized trigger similarity.
///
/// Scope logic (Constitution XV):
/// - Functionality: pure query→skills matcher used by the orchestrator ahead
///   of `route()`; no IO, no config access (callers gate on the global
///   `[skills.auto_route]` switch and per-skill `auto_route` frontmatter).
/// - User impact: determines whether a skill prompt auto-injects into the
///   M1 context for a query.
/// - Default: threshold 0.72, at most 1 hit.
/// - Interaction: skills with empty `triggers` can never hit; disabled
///   skills must be filtered by the caller (`is_auto_route_enabled`).
#[derive(Debug, Clone)]
pub struct SkillHitRouter {
    threshold: f32,
    max_hits: usize,
}

impl Default for SkillHitRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl SkillHitRouter {
    /// Router with the contract defaults (threshold 0.72, max 1 hit).
    pub fn new() -> Self {
        Self {
            threshold: SKILL_HIT_THRESHOLD,
            max_hits: SKILL_HIT_MAX_HITS,
        }
    }

    pub fn threshold(&self) -> f32 {
        self.threshold
    }

    pub fn max_hits(&self) -> usize {
        self.max_hits
    }

    /// Match `query` against every skill's triggers.
    ///
    /// Parameters:
    /// - `query`: raw user input; normalized once via `normalize_alias`
    ///   (the contract's named normalizer).
    /// - `skills`: already eligibility-filtered definitions; skills with
    ///   empty `triggers` never hit.
    ///
    /// Returns at most `max_hits` hits, best score first; empty when no
    /// trigger reaches the threshold (contract error `threshold_not_met`).
    pub fn route(&self, query: &str, skills: &[SkillDefinition]) -> Vec<SkillHit> {
        let query_norm = normalize_alias(query);
        if query_norm.is_empty() {
            return Vec::new();
        }

        let mut hits: Vec<SkillHit> = Vec::new();
        for skill in skills {
            let mut best = 0.0f32;
            let mut matched = Vec::new();
            let mut seen = HashSet::new();
            for trigger in &skill.triggers {
                let trigger_norm = normalize_alias(trigger);
                if trigger_norm.is_empty() || !seen.insert(trigger_norm.clone()) {
                    continue;
                }
                let score = score_trigger(&query_norm, &trigger_norm);
                if score >= self.threshold {
                    matched.push(trigger.clone());
                    best = best.max(score);
                }
            }
            if !matched.is_empty() {
                hits.push(SkillHit {
                    skill: skill.name.clone(),
                    score: best,
                    triggers_matched: matched,
                });
            }
        }

        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.skill.cmp(&b.skill))
        });
        hits.truncate(self.max_hits);
        hits
    }
}

/// Score one normalized trigger against the normalized query.
///
/// Containment → 1.0 (literal trigger present); otherwise trigram-Jaccard.
fn score_trigger(query_norm: &str, trigger_norm: &str) -> f32 {
    if query_norm.contains(trigger_norm) {
        return 1.0;
    }
    trigram_jaccard(query_norm, trigger_norm) as f32
}

/// Render the M1 injection prompt for a hit skill: description, prompt and
/// body, capped at [`SKILL_PROMPT_MAX_CHARS`].
pub fn render_skill_prompt(def: &SkillDefinition) -> String {
    let mut prompt = String::new();
    prompt.push_str("## Matched skill: ");
    prompt.push_str(&def.name);
    prompt.push('\n');
    if !def.description.is_empty() {
        prompt.push_str(&def.description);
        prompt.push_str("\n\n");
    }
    if !def.prompt.is_empty() {
        prompt.push_str(&def.prompt);
        prompt.push_str("\n\n");
    }
    prompt.push_str(&def.body);

    if prompt.chars().count() > SKILL_PROMPT_MAX_CHARS {
        let truncated: String = prompt.chars().take(SKILL_PROMPT_MAX_CHARS).collect();
        prompt = truncated + "\n\n[skill body truncated]";
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(name: &str, triggers: &[&str]) -> SkillDefinition {
        SkillDefinition {
            name: name.to_string(),
            description: format!("{name} description"),
            triggers: triggers.iter().map(|t| t.to_string()).collect(),
            auto_route: None,
            tools: Vec::new(),
            context_files: Vec::new(),
            prompt: String::new(),
            body: String::new(),
        }
    }

    #[test]
    fn containment_scores_full() {
        let router = SkillHitRouter::new();
        let hits = router.route(
            "please do a weekly review for me",
            &[def("weekly-review", &["weekly review"])],
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].skill, "weekly-review");
        assert_eq!(hits[0].score, 1.0);
        assert_eq!(hits[0].triggers_matched, vec!["weekly review"]);
    }

    #[test]
    fn identical_strings_hit_via_jaccard() {
        let router = SkillHitRouter::new();
        let hits = router.route("cargo build fix", &[def("build-fix", &["cargo build fix"])]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].score, 1.0);
    }

    #[test]
    fn below_threshold_never_hits() {
        let router = SkillHitRouter::new();
        let hits = router.route(
            "zebra qq qux unrelated words",
            &[def("build-fix", &["cargo build fix"])],
        );
        assert!(hits.is_empty(), "unrelated query must not hit: {hits:?}");
    }

    #[test]
    fn max_hits_one_picks_best_score() {
        let router = SkillHitRouter::new();
        let skills = vec![
            def("weak", &["cargo build fix partly similar words"]),
            def("exact", &["cargo build fix"]),
        ];
        let hits = router.route("run cargo build fix now", &skills);
        assert_eq!(hits.len(), 1, "max_hits=1 must truncate");
        assert_eq!(hits[0].skill, "exact");
    }

    #[test]
    fn empty_triggers_never_hit() {
        let router = SkillHitRouter::new();
        let hits = router.route("anything at all", &[def("mute", &[])]);
        assert!(hits.is_empty());
    }

    #[test]
    fn disabled_skill_is_caller_filtered() {
        let mut skill = def("opted-out", &["weekly review"]);
        assert!(skill.is_auto_route_enabled(), "absent → enabled");
        skill.auto_route = Some(false);
        assert!(!skill.is_auto_route_enabled(), "explicit opt-out");

        // The router itself is flag-free; the orchestrator filters first.
        let router = SkillHitRouter::new();
        let eligible: Vec<SkillDefinition> = Vec::new();
        assert!(router.route("weekly review", &eligible).is_empty());
    }

    #[test]
    fn threshold_matches_contract_constant() {
        assert_eq!(SKILL_HIT_THRESHOLD, 0.72);
        assert_eq!(SKILL_HIT_MAX_HITS, 1);
        assert_eq!(SkillHitRouter::new().threshold(), 0.72);
        assert_eq!(SkillHitRouter::new().max_hits(), 1);
    }

    #[test]
    fn empty_query_never_hits() {
        let router = SkillHitRouter::new();
        assert!(router.route("   ", &[def("x", &["x"])]).is_empty());
    }

    #[test]
    fn render_skill_prompt_includes_parts_and_caps() {
        let mut skill = def("big", &["t"]);
        skill.description = "Short desc".to_string();
        skill.prompt = "Do the thing.".to_string();
        skill.body = "body".to_string();
        let rendered = render_skill_prompt(&skill);
        assert!(rendered.contains("## Matched skill: big"));
        assert!(rendered.contains("Short desc"));
        assert!(rendered.contains("Do the thing."));
        assert!(rendered.contains("body"));

        skill.body = "x".repeat(SKILL_PROMPT_MAX_CHARS + 500);
        let capped = render_skill_prompt(&skill);
        assert!(capped.chars().count() < SKILL_PROMPT_MAX_CHARS + 100);
        assert!(capped.contains("[skill body truncated]"));
    }
}
