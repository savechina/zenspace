use serde::{Deserialize, Serialize};

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
    let goal_path_resolved = !is_path_not_goal || (is_path_not_goal && has_goal_link);

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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(report
            .fail_reasons
            .iter()
            .any(|r| r.contains("anti-pattern")));
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
        assert!(report
            .fail_reasons
            .iter()
            .any(|r| r.contains("path decision")));
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
}
