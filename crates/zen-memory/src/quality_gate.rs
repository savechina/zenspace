use serde::{Deserialize, Serialize};

use crate::decision::Decision;

// §9.1 — Extraction guardrails

pub const EXTRACTION_GUARDRAILS: &str = r#"
Pre-extraction self-checks (any hit → discard or downgrade):
  [诱导式] Am I leading the user toward a preset answer?
  [忽略核心] Is this bonus feature, not core pursuit?
  [迎合解读] Am I cherry-picking positive signals only?
  [迷信数据] Does the data mask user struggle? (溯源: find raw conversation)
  [防御心理] Is the user offering face-saving excuse?
  [背景不一致] Am I filling in context the user didn't provide?
  [轻佻表达] Is this a claim without quantification? (flag for review)

Pre-promotion 6 questions (M2→M3 gate):
  1. What is this content?
  2. Complete?
  3. Source credible?
  4. Evidence verified how?
  5. Alternative explanations?
  6. Worth long-term memory?
"#;

pub const DECISION_PRINCIPLES: &str = r#"
7 Decision Principles (apply when extracting decision signals):
  1. 第一性原理 (First Principles): Stripped to fundamentals — is this truly necessary?
  2. 能力圈 (Competence Circle): Is this inside my competence?
  3. 逆向思维 (Inversion): What would make this fail? Avoid that first.
  4. 二阶思维 (Second-Order): And then what? Second-order consequences?
  5. 永远有筹码 (Keep Chips): Worst case — how many chips remain? Never all-in.
  6. 成本观 (Cost Awareness): Economic/time/credit/sunk — all 4 ledgers checked?
  7. 奥卡姆剃刀 (Occam's Razor): What's the simpler explanation?
"#;

pub const PRE_PROMOTION_QUESTIONS: &str = r#"
Pre-promotion 6 questions (M2→M3 gate):
  1. What is this content?
  2. Complete?
  3. Source credible?
  4. Evidence verified how?
  5. Alternative explanations?
  6. Worth long-term memory?
"#;

// §9.2 — Information Quality Gate

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Bias {
    SurvivorshipBias,
    SelectionBias,
    GeographicBias, // 北上深 ≠ 全国
    TemporalBias,
    SelfReportingBias,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InformationQualityGate {
    pub source_credibility: f64,
    pub definition_clarity: bool,
    pub sampling_bias: Option<Bias>,
    pub cross_verified: bool,
    pub fact_opinion_separated: bool,
    pub frivolous_expression: bool,
}

impl Default for InformationQualityGate {
    fn default() -> Self {
        Self {
            source_credibility: 0.5,
            definition_clarity: false,
            sampling_bias: None,
            cross_verified: false,
            fact_opinion_separated: false,
            frivolous_expression: false,
        }
    }
}

impl InformationQualityGate {
    pub fn can_promote_to_m3(&self) -> bool {
        self.source_credibility > 0.5
            && self.definition_clarity
            && !self.frivolous_expression
            && (self.cross_verified || self.sampling_bias.is_none())
    }

    /// Evidence grade per `docs/specs/005-agentic-loop/contracts/memory-grade.json`:
    /// `Unverified | Corroborated | Actionable`.
    ///
    /// - `Actionable`: passes [`Self::can_promote_to_m3`] (credible, clear, verified).
    /// - `Corroborated`: credible and well-formed but missing the verification leg
    ///   (`cross_verified == false` with a sampling bias present).
    /// - `Unverified`: fails even the basic clarity/credibility bar.
    pub fn grade(&self) -> MemoryGrade {
        if self.can_promote_to_m3() {
            MemoryGrade::Actionable
        } else if self.source_credibility > 0.5
            && self.definition_clarity
            && !self.frivolous_expression
        {
            MemoryGrade::Corroborated
        } else {
            MemoryGrade::Unverified
        }
    }

    pub fn fail_reasons(&self) -> Vec<&'static str> {
        let mut reasons = Vec::new();
        if self.source_credibility <= 0.5 {
            reasons.push("source_credibility <= 0.5");
        }
        if !self.definition_clarity {
            reasons.push("definition_clarity is false");
        }
        if self.frivolous_expression {
            reasons.push("frivolous_expression is true");
        }
        if !self.cross_verified && self.sampling_bias.is_some() {
            reasons.push("sampling_bias present without cross-verification");
        }
        reasons
    }
}

/// Evidence grade for a signal, per `contracts/memory-grade.json` output.
///
/// Serialization is externally-tagged PascalCase (`"Unverified"`, `"Corroborated"`,
/// `"Actionable"`), matching the contract strings and the existing [`Bias`] enum style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MemoryGrade {
    Unverified,
    Corroborated,
    Actionable,
}

/// Minimum whitespace-separated tokens for a journal signal to count as
/// "definition_clarity" — shorter fragments are treated as vague.
pub const MIN_SIGNAL_TOKENS: usize = 5;

/// Grade a raw journal signal string (SessionJournaler M2 pre-filter +
/// ZenDream M2→M3 promotion gate — single shared heuristic, T073).
///
/// Gate inputs are derived from what is knowable for a bare signal line:
/// - `source_credibility`: LLM extraction passed the
///   [`EXTRACTION_GUARDRAILS`] self-check (0.7); keyword-only pattern match
///   is weaker evidence (0.6). Both sit above the 0.5 promotion floor so the
///   gate discriminates on *content quality*, not extraction path.
/// - `definition_clarity`: at least [`MIN_SIGNAL_TOKENS`] tokens (specific
///   enough to be durable after 6 months).
/// - `frivolous_expression`: the 轻佻表达 guardrail — an unquantified claim
///   (`tokens < MIN_SIGNAL_TOKENS` with no digit).
/// - `sampling_bias`: `None` — journal signals are first-party self-report by
///   construction; bias screening happens at extraction time via
///   [`EXTRACTION_GUARDRAILS`] (迷信数据/迎合解读), not post-hoc.
/// - `cross_verified`: true only for LLM extraction (guardrail
///   cross-examination counts as within-cycle verification).
///
/// Returns `(grade, gate)` so callers can surface `fail_reasons` in logs.
pub fn grade_session_signal(
    text: &str,
    llm_extracted: bool,
) -> (MemoryGrade, InformationQualityGate) {
    let tokens = text.split_whitespace().count();
    let has_quantification = text.chars().any(|c| c.is_ascii_digit());
    let gate = InformationQualityGate {
        source_credibility: if llm_extracted { 0.7 } else { 0.6 },
        definition_clarity: tokens >= MIN_SIGNAL_TOKENS,
        sampling_bias: None,
        cross_verified: llm_extracted,
        fact_opinion_separated: llm_extracted,
        frivolous_expression: tokens < MIN_SIGNAL_TOKENS && !has_quantification,
    };
    (gate.grade(), gate)
}

// §9.3 — Decision Promotion Gate

#[derive(Debug, Clone)]
pub struct DecisionPromotionReport {
    pub anti_patterns_passed: bool,
    pub cost_analysis_present: bool,
    pub ev_calculated: bool,
    pub goal_path_resolved: bool,
    pub can_promote: bool,
    pub fail_reasons: Vec<String>,
}

pub fn check_decision_promotion(
    has_cost: bool,
    has_ev: bool,
    is_path_not_goal: bool,
    has_goal_link: bool,
    anti_pattern_has_crit: bool,
) -> DecisionPromotionReport {
    let anti_patterns_passed = !anti_pattern_has_crit;
    let cost_analysis_present = has_cost;
    let ev_calculated = has_ev;
    let goal_path_resolved = !is_path_not_goal || has_goal_link;

    let can_promote =
        anti_patterns_passed && cost_analysis_present && ev_calculated && goal_path_resolved;

    let mut fail_reasons = Vec::new();
    if !anti_patterns_passed {
        fail_reasons.push("anti-pattern CRIT violation blocks promotion".into());
    }
    if !cost_analysis_present {
        fail_reasons.push("cost analysis missing".into());
    }
    if !ev_calculated {
        fail_reasons.push("expected value not calculated".into());
    }
    if !goal_path_resolved {
        fail_reasons.push("path decision without goal link".into());
    }

    DecisionPromotionReport {
        anti_patterns_passed,
        cost_analysis_present,
        ev_calculated,
        goal_path_resolved,
        can_promote,
        fail_reasons,
    }
}

// §9.4 — Decision Principle Enforcement (7 principles from DESIGN.md §7.2)

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionPrincipleReport {
    pub first_principles: bool,
    pub competence_circle: bool,
    pub inversion: bool,
    pub second_order: bool,
    pub keep_chips: bool,
    pub cost_awareness: bool,
    pub occams_razor: bool,
    pub all_passed: bool,
    pub failed_count: usize,
}

pub fn check_decision_principles(d: &Decision) -> DecisionPrincipleReport {
    let first_principles = !d.facts.is_empty();
    let competence_circle = !d.domain.is_empty();
    let inversion = !d.alternatives.is_empty();
    let second_order = d.execution_plan.is_some();
    let keep_chips = d.cost_analysis.is_recoverable || d.alternatives.len() >= 2;
    let cost_awareness = d.cost_analysis.economic > 0.0 || d.cost_analysis.time_hours > 0.0;
    let occams_razor = d.alternatives.len() <= 3;

    let checks = [
        first_principles,
        competence_circle,
        inversion,
        second_order,
        keep_chips,
        cost_awareness,
        occams_razor,
    ];
    let failed_count = checks.iter().filter(|&&c| !c).count();
    let all_passed = failed_count == 0;

    DecisionPrincipleReport {
        first_principles,
        competence_circle,
        inversion,
        second_order,
        keep_chips,
        cost_awareness,
        occams_razor,
        all_passed,
        failed_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::CostBreakdown;

    #[test]
    fn guardrails_const_not_empty() {
        assert!(!EXTRACTION_GUARDRAILS.is_empty());
        assert!(EXTRACTION_GUARDRAILS.contains("诱导式"));
        assert!(EXTRACTION_GUARDRAILS.contains("Pre-promotion"));
    }

    #[test]
    fn decision_principles_const_not_empty() {
        assert!(!DECISION_PRINCIPLES.is_empty());
        assert!(DECISION_PRINCIPLES.contains("第一性原理"));
        assert!(DECISION_PRINCIPLES.contains("奥卡姆剃刀"));
    }

    #[test]
    fn iqg_default_fails() {
        let gate = InformationQualityGate::default();
        assert!(!gate.can_promote_to_m3());
        assert_eq!(gate.source_credibility, 0.5);
        assert!(!gate.definition_clarity);
    }

    #[test]
    fn iqg_all_pass() {
        let gate = InformationQualityGate {
            source_credibility: 0.8,
            definition_clarity: true,
            sampling_bias: None,
            cross_verified: true,
            fact_opinion_separated: true,
            frivolous_expression: false,
        };
        assert!(gate.can_promote_to_m3());
        assert!(gate.fail_reasons().is_empty());
    }

    #[test]
    fn iqg_frivolous_fails() {
        let gate = InformationQualityGate {
            source_credibility: 0.9,
            definition_clarity: true,
            sampling_bias: None,
            cross_verified: true,
            fact_opinion_separated: true,
            frivolous_expression: true,
        };
        assert!(!gate.can_promote_to_m3());
        let reasons = gate.fail_reasons();
        assert!(reasons.contains(&"frivolous_expression is true"));
    }

    #[test]
    fn iqg_low_credibility_fails() {
        let gate = InformationQualityGate {
            source_credibility: 0.3,
            definition_clarity: true,
            sampling_bias: None,
            cross_verified: true,
            fact_opinion_separated: true,
            frivolous_expression: false,
        };
        assert!(!gate.can_promote_to_m3());
        let reasons = gate.fail_reasons();
        assert!(reasons.contains(&"source_credibility <= 0.5"));
    }

    #[test]
    fn iqg_no_cross_verify_with_bias_fails() {
        let gate = InformationQualityGate {
            source_credibility: 0.8,
            definition_clarity: true,
            sampling_bias: Some(Bias::SelectionBias),
            cross_verified: false,
            fact_opinion_separated: true,
            frivolous_expression: false,
        };
        assert!(!gate.can_promote_to_m3());
        let reasons = gate.fail_reasons();
        assert!(reasons.contains(&"sampling_bias present without cross-verification"));
    }

    #[test]
    fn iqg_no_cross_verify_no_bias_passes() {
        let gate = InformationQualityGate {
            source_credibility: 0.8,
            definition_clarity: true,
            sampling_bias: None,
            cross_verified: false,
            fact_opinion_separated: true,
            frivolous_expression: false,
        };
        assert!(gate.can_promote_to_m3());
        assert!(gate.fail_reasons().is_empty());
    }

    #[test]
    fn iqg_fail_reasons_lists_all() {
        let gate = InformationQualityGate::default();
        let reasons = gate.fail_reasons();
        assert_eq!(reasons.len(), 2);
        assert!(reasons.contains(&"source_credibility <= 0.5"));
        assert!(reasons.contains(&"definition_clarity is false"));
    }

    #[test]
    fn iqg_bias_enum_serialization() {
        let bias = Bias::GeographicBias;
        let json = serde_json::to_string(&bias).unwrap();
        assert!(json.contains("GeographicBias"));
        let deserialized: Bias = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, Bias::GeographicBias);
    }

    // ── T074: full grade matrix (Unverified | Corroborated | Actionable) ──

    fn gate(
        credibility: f64,
        clarity: bool,
        bias: Option<Bias>,
        cross: bool,
        frivolous: bool,
    ) -> InformationQualityGate {
        InformationQualityGate {
            source_credibility: credibility,
            definition_clarity: clarity,
            sampling_bias: bias,
            cross_verified: cross,
            fact_opinion_separated: true,
            frivolous_expression: frivolous,
        }
    }

    #[test]
    fn grade_matrix_actionable_when_promotable() {
        let cases = [
            gate(0.8, true, None, true, false),
            gate(0.8, true, None, false, false),
            gate(0.8, true, Some(Bias::SelectionBias), true, false),
            gate(1.0, true, None, true, false),
        ];
        for g in &cases {
            assert_eq!(g.grade(), MemoryGrade::Actionable, "gate: {g:?}");
            assert!(g.can_promote_to_m3());
            assert!(g.fail_reasons().is_empty());
        }
    }

    #[test]
    fn grade_matrix_corroborated_missing_verification_only() {
        let g = gate(0.8, true, Some(Bias::SelfReportingBias), false, false);
        assert_eq!(g.grade(), MemoryGrade::Corroborated);
        assert!(!g.can_promote_to_m3());
        assert_eq!(
            g.fail_reasons(),
            vec!["sampling_bias present without cross-verification"]
        );
    }

    #[test]
    fn grade_matrix_unverified_low_credibility() {
        let g = gate(0.5, true, None, true, false);
        assert_eq!(g.grade(), MemoryGrade::Unverified, "0.5 is not > 0.5");
        assert!(!g.can_promote_to_m3());
        assert!(g.fail_reasons().contains(&"source_credibility <= 0.5"));
    }

    #[test]
    fn grade_matrix_unverified_unclear() {
        let g = gate(0.9, false, None, true, false);
        assert_eq!(g.grade(), MemoryGrade::Unverified);
        assert!(g.fail_reasons().contains(&"definition_clarity is false"));
    }

    #[test]
    fn grade_matrix_unverified_frivolous() {
        let g = gate(0.9, true, None, true, true);
        assert_eq!(g.grade(), MemoryGrade::Unverified);
        assert!(g.fail_reasons().contains(&"frivolous_expression is true"));
    }

    #[test]
    fn grade_matrix_default_is_unverified() {
        let g = InformationQualityGate::default();
        assert_eq!(g.grade(), MemoryGrade::Unverified);
    }

    #[test]
    fn grade_serializes_per_contract() {
        for (g, expected) in [
            (gate(0.8, true, None, true, false), "Actionable"),
            (
                gate(0.8, true, Some(Bias::SelfReportingBias), false, false),
                "Corroborated",
            ),
            (InformationQualityGate::default(), "Unverified"),
        ] {
            let json = serde_json::to_string(&g.grade()).unwrap();
            assert_eq!(json, format!("\"{expected}\""));
            let round: MemoryGrade = serde_json::from_str(&json).unwrap();
            assert_eq!(round, g.grade());
        }
    }

    #[test]
    fn grade_session_signal_vague_is_unverified_and_blocked() {
        let (grade, g) = grade_session_signal("ok", false);
        assert_eq!(grade, MemoryGrade::Unverified);
        assert!(!g.can_promote_to_m3());
        assert!(!g.definition_clarity);
        assert!(g.frivolous_expression);
        assert!(!g.fail_reasons().is_empty());
    }

    #[test]
    fn grade_session_signal_specific_llm_is_actionable() {
        let (grade, g) = grade_session_signal(
            "migrated the session store from json blobs to sqlite wal",
            true,
        );
        assert_eq!(grade, MemoryGrade::Actionable);
        assert!(g.can_promote_to_m3());
        assert_eq!(g.source_credibility, 0.7);
        assert!(g.cross_verified);
    }

    #[test]
    fn grade_session_signal_specific_keyword_actionable() {
        let (grade, g) = grade_session_signal("fixed the login redirect loop on safari", false);
        assert_eq!(grade, MemoryGrade::Actionable);
        assert_eq!(g.source_credibility, 0.6);
        assert!(!g.cross_verified);
        assert!(g.sampling_bias.is_none(), "bias leg passes via is_none");
    }

    #[test]
    fn grade_session_signal_unquantified_short_is_frivolous() {
        let (grade, g) = grade_session_signal("refactor stuff maybe", false);
        assert_eq!(grade, MemoryGrade::Unverified);
        assert!(g.frivolous_expression);
    }

    #[test]
    fn dpg_all_pass() {
        let report = check_decision_promotion(true, true, false, false, false);
        assert!(report.can_promote);
        assert!(report.fail_reasons.is_empty());
    }

    #[test]
    fn dpg_crit_blocks() {
        let report = check_decision_promotion(true, true, false, false, true);
        assert!(!report.can_promote);
        assert!(!report.anti_patterns_passed);
        assert!(
            report
                .fail_reasons
                .iter()
                .any(|r| r.contains("anti-pattern"))
        );
    }

    #[test]
    fn dpg_missing_cost() {
        let report = check_decision_promotion(false, true, false, false, false);
        assert!(!report.can_promote);
        assert!(!report.cost_analysis_present);
    }

    #[test]
    fn dpg_path_without_goal() {
        let report = check_decision_promotion(true, true, true, false, false);
        assert!(!report.can_promote);
        assert!(!report.goal_path_resolved);
        assert!(
            report
                .fail_reasons
                .iter()
                .any(|r| r.contains("path decision"))
        );
    }

    #[test]
    fn dpg_path_with_goal_passes() {
        let report = check_decision_promotion(true, true, true, true, false);
        assert!(report.can_promote);
    }

    #[test]
    fn dpg_fail_reasons() {
        let report = check_decision_promotion(false, false, true, false, true);
        assert!(!report.can_promote);
        assert_eq!(report.fail_reasons.len(), 4);
    }

    fn minimal_decision() -> Decision {
        Decision {
            id: "test".into(),
            title: "test".into(),
            domain: String::new(),
            goal: String::new(),
            is_path_not_goal: false,
            core_pursuit: String::new(),
            facts: vec![],
            information_sources: vec![],
            choice: String::new(),
            alternatives: vec![],
            controllability: None,
            expected_value: None,
            confidence: None,
            cost_analysis: CostBreakdown::default(),
            execution_plan: None,
            low_cost_validation: None,
            outcome: None,
            retrospective: None,
            decided_at: chrono::Utc::now(),
            closed_at: None,
        }
    }

    #[test]
    fn dpr_minimal_decision_fails_most() {
        let d = minimal_decision();
        let report = check_decision_principles(&d);
        assert!(!report.all_passed);
        assert!(report.failed_count >= 4);
        assert!(!report.first_principles);
        assert!(!report.competence_circle);
        assert!(!report.inversion);
        assert!(!report.second_order);
        assert!(!report.cost_awareness);
    }

    #[test]
    fn dpr_full_decision_passes_all() {
        let mut d = minimal_decision();
        d.domain = "career".into();
        d.facts = vec!["fact1".into()];
        d.alternatives = vec!["alt1".into(), "alt2".into()];
        d.execution_plan = Some("step1".into());
        d.cost_analysis = CostBreakdown {
            economic: 100.0,
            time_hours: 10.0,
            credit: 0.0,
            sunk: 0.0,
            is_recoverable: true,
        };
        let report = check_decision_principles(&d);
        assert!(report.all_passed);
        assert_eq!(report.failed_count, 0);
        assert!(report.first_principles);
        assert!(report.competence_circle);
        assert!(report.inversion);
        assert!(report.second_order);
        assert!(report.keep_chips);
        assert!(report.cost_awareness);
        assert!(report.occams_razor);
    }

    #[test]
    fn dpr_too_many_alternatives_fails_occams() {
        let mut d = minimal_decision();
        d.alternatives = vec!["a".into(), "b".into(), "c".into(), "d".into()];
        let report = check_decision_principles(&d);
        assert!(!report.occams_razor);
    }
}
