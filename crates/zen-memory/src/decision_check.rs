//! Rule-based anti-pattern checker for decisions.
//!
//! Evaluates 10 decision anti-patterns and produces an `AntiPatternReport`.
//! Three checks require LLM semantic analysis and are deferred to `WisdomSynthesizer`.

use crate::decision::{AntiPatternReport, AntiPatternViolation, Decision, Severity};

const LEGAL_KEYWORDS: &[&str] = &[
    "版权", "税务", "隐私", "合同", "lawsuit", "patent", "compliance", "legal", "法律",
    "风险评估",
];
const MARKET_KEYWORDS: &[&str] = &[
    "获客", "流量", "市场", "policy", "政策", "growth", "增长",
];
const PROCEED_KEYWORDS: &[&str] = &["proceed", "继续", "推进", "go ahead", "all in", "坚持"];

/// Run all 10 anti-pattern checks and return aggregated report.
pub fn check_all(d: &Decision) -> AntiPatternReport {
    let mut violations = Vec::new();

    let checks: Vec<Option<AntiPatternViolation>> = vec![
        check_misplaced_priority(d),
        check_legal_risk_blind(d),
        check_inertia_thinking(d),
        check_loss_aversion(d),
        check_emotional_impulse(d),
        check_fluke_mindset(d),
        check_authority_blindness(d),
        check_all_in_no_reserve(d),
        check_self_cognition_block(d),
        check_market_cognition_block(d),
    ];

    for v in checks.into_iter().flatten() {
        violations.push(v);
    }

    let has_crit = violations.iter().any(|v| v.severity == Severity::Crit);

    AntiPatternReport {
        violations,
        has_crit,
    }
}

/// DEFERRED: requires LLM semantic analysis to compare decision vs core_pursuit.
/// WisdomSynthesizer handles this.
fn check_misplaced_priority(_d: &Decision) -> Option<AntiPatternViolation> {
    None
}

/// Check if legal/compliance risk is present without risk assessment.
fn check_legal_risk_blind(d: &Decision) -> Option<AntiPatternViolation> {
    let text = format!(
        "{} {} {}",
        d.goal,
        d.choice,
        d.facts.join(" ")
    );
    let text_lower = text.to_lowercase();

    let has_legal = LEGAL_KEYWORDS.iter().any(|kw| {
        let kw_lower = kw.to_lowercase();
        text_lower.contains(&kw_lower) || text.contains(kw)
    });

    if !has_legal {
        return None;
    }

    let plan = d.execution_plan.as_deref().unwrap_or("");
    let plan_lower = plan.to_lowercase();
    let has_risk_assessment = plan_lower.contains("risk")
        || plan.contains("风险评估")
        || plan_lower.contains("legal");

    if has_risk_assessment {
        None
    } else {
        Some(AntiPatternViolation {
            pattern_id: "legal_risk_blind".into(),
            severity: Severity::Crit,
            message: "Legal/compliance risk detected without risk assessment".into(),
        })
    }
}

/// DEFERRED: requires LLM to analyze reasoning basis.
/// WisdomSynthesizer handles this.
fn check_inertia_thinking(_d: &Decision) -> Option<AntiPatternViolation> {
    None
}

/// Check for sunk cost trap with unaffordable loss.
fn check_loss_aversion(d: &Decision) -> Option<AntiPatternViolation> {
    if d.cost_analysis.sunk > 0.0
        && !d.cost_analysis.is_recoverable
        && d.expected_value
            .as_ref()
            .is_some_and(|ev| !ev.loss_affordable)
    {
        Some(AntiPatternViolation {
            pattern_id: "loss_aversion".into(),
            severity: Severity::Crit,
            message: "Sunk costs exceeding stop-loss threshold without recoverability".into(),
        })
    } else {
        None
    }
}

/// DEFERRED: requires first_considered_at timestamp to check <1h decision time.
fn check_emotional_impulse(_d: &Decision) -> Option<AntiPatternViolation> {
    None
}

/// Check for proceeding despite known legal/compliance risk.
fn check_fluke_mindset(d: &Decision) -> Option<AntiPatternViolation> {
    let text = format!(
        "{} {} {}",
        d.goal,
        d.choice,
        d.facts.join(" ")
    );
    let text_lower = text.to_lowercase();

    let has_legal = LEGAL_KEYWORDS.iter().any(|kw| {
        let kw_lower = kw.to_lowercase();
        text_lower.contains(&kw_lower) || text.contains(kw)
    });

    if !has_legal {
        return None;
    }

    let choice_lower = d.choice.to_lowercase();
    let is_proceeding = PROCEED_KEYWORDS.iter().any(|kw| {
        let kw_lower = kw.to_lowercase();
        choice_lower.contains(&kw_lower) || d.choice.contains(kw)
    });

    if is_proceeding {
        Some(AntiPatternViolation {
            pattern_id: "fluke_mindset".into(),
            severity: Severity::Crit,
            message: "Proceeding despite known legal/compliance risk".into(),
        })
    } else {
        None
    }
}

/// Check for single information source without cross-verification.
fn check_authority_blindness(d: &Decision) -> Option<AntiPatternViolation> {
    if d.information_sources.len() <= 1 {
        Some(AntiPatternViolation {
            pattern_id: "authority_blindness".into(),
            severity: Severity::Med,
            message: "Single information source — no cross-verification".into(),
        })
    } else {
        None
    }
}

/// Check for all-in commitment with no fallback and unaffordable loss.
fn check_all_in_no_reserve(d: &Decision) -> Option<AntiPatternViolation> {
    if d.alternatives.is_empty()
        && d.expected_value
            .as_ref()
            .is_some_and(|ev| !ev.loss_affordable)
    {
        Some(AntiPatternViolation {
            pattern_id: "all_in_no_reserve".into(),
            severity: Severity::Crit,
            message: "All-in commitment with no fallback and unaffordable loss".into(),
        })
    } else {
        None
    }
}

/// Check for overconfidence with insufficient evidence.
fn check_self_cognition_block(d: &Decision) -> Option<AntiPatternViolation> {
    let is_overconfident = d
        .confidence
        .as_ref()
        .is_some_and(|c| *c > 0.9);
    let insufficient_facts = d.facts.len() < 3;

    if is_overconfident && insufficient_facts {
        Some(AntiPatternViolation {
            pattern_id: "self_cognition_block".into(),
            severity: Severity::High,
            message: "High confidence with insufficient evidence (potential Dunning-Kruger)"
                .into(),
        })
    } else {
        None
    }
}

/// Check for market/growth decision without market data sources.
fn check_market_cognition_block(d: &Decision) -> Option<AntiPatternViolation> {
    let text = format!("{} {} {}", d.domain, d.goal, d.choice);
    let text_lower = text.to_lowercase();

    let has_market = MARKET_KEYWORDS.iter().any(|kw| {
        let kw_lower = kw.to_lowercase();
        text_lower.contains(&kw_lower) || text.contains(kw)
    });

    if has_market && d.information_sources.is_empty() {
        Some(AntiPatternViolation {
            pattern_id: "market_cognition_block".into(),
            severity: Severity::High,
            message: "Market/growth decision without market data sources".into(),
        })
    } else {
        None
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::{CostBreakdown, ExpectedValue};

    fn make_decision() -> Decision {
        Decision::new("test".into(), "Test Decision".into(), "tech".into())
    }

    #[test]
    fn test_legal_risk_blind_triggers() {
        let mut d = make_decision();
        d.goal = "Handle legal compliance".into();
        d.facts = vec!["Need to address patent issues".into()];
        d.execution_plan = Some("Step 1, Step 2".into());

        let report = check_all(&d);
        let violations = &report.violations;
        assert!(
            violations.iter().any(|v| v.pattern_id == "legal_risk_blind"),
            "expected legal_risk_blind violation"
        );
    }

    #[test]
    fn test_legal_risk_blind_passes_with_assessment() {
        let mut d = make_decision();
        d.goal = "Handle legal compliance".into();
        d.execution_plan = Some("Do risk assessment first".into());

        let report = check_all(&d);
        assert!(
            !report.violations.iter().any(|v| v.pattern_id == "legal_risk_blind"),
            "should not trigger with risk assessment"
        );
    }

    #[test]
    fn test_legal_risk_blind_passes_no_keywords() {
        let d = make_decision();
        let report = check_all(&d);
        assert!(
            !report.violations.iter().any(|v| v.pattern_id == "legal_risk_blind"),
            "should not trigger without legal keywords"
        );
    }

    #[test]
    fn test_loss_aversion_triggers() {
        let mut d = make_decision();
        d.cost_analysis = CostBreakdown {
            sunk: 5000.0,
            is_recoverable: false,
            ..CostBreakdown::default()
        };
        d.expected_value = Some(ExpectedValue {
            success_probability: 0.3,
            payoff_if_success: 10000.0,
            loss_if_failure: 20000.0,
            is_positive_ev: false,
            loss_affordable: false,
        });

        let report = check_all(&d);
        assert!(
            report.violations.iter().any(|v| v.pattern_id == "loss_aversion"),
            "expected loss_aversion violation"
        );
    }

    #[test]
    fn test_loss_aversion_passes_recoverable() {
        let mut d = make_decision();
        d.cost_analysis = CostBreakdown {
            sunk: 5000.0,
            is_recoverable: true,
            ..CostBreakdown::default()
        };
        d.expected_value = Some(ExpectedValue {
            success_probability: 0.3,
            payoff_if_success: 10000.0,
            loss_if_failure: 20000.0,
            is_positive_ev: false,
            loss_affordable: false,
        });

        let report = check_all(&d);
        assert!(
            !report.violations.iter().any(|v| v.pattern_id == "loss_aversion"),
            "should not trigger when recoverable"
        );
    }

    #[test]
    fn test_fluke_mindset_triggers() {
        let mut d = make_decision();
        d.goal = "Handle copyright issues".into();
        d.choice = "Proceed with the plan".into();
        d.facts = vec!["Legal review needed".into()];

        let report = check_all(&d);
        assert!(
            report.violations.iter().any(|v| v.pattern_id == "fluke_mindset"),
            "expected fluke_mindset violation"
        );
    }

    #[test]
    fn test_authority_blindness_single_source() {
        let mut d = make_decision();
        d.information_sources = vec!["Single source".into()];

        let report = check_all(&d);
        assert!(
            report.violations.iter().any(|v| v.pattern_id == "authority_blindness"),
            "expected authority_blindness violation"
        );
    }

    #[test]
    fn test_authority_blindness_passes_multiple_sources() {
        let mut d = make_decision();
        d.information_sources = vec!["Source A".into(), "Source B".into()];

        let report = check_all(&d);
        assert!(
            !report.violations.iter().any(|v| v.pattern_id == "authority_blindness"),
            "should not trigger with multiple sources"
        );
    }

    #[test]
    fn test_all_in_no_reserve_triggers() {
        let mut d = make_decision();
        d.alternatives = Vec::new();
        d.expected_value = Some(ExpectedValue {
            success_probability: 0.4,
            payoff_if_success: 10000.0,
            loss_if_failure: 50000.0,
            is_positive_ev: false,
            loss_affordable: false,
        });

        let report = check_all(&d);
        assert!(
            report.violations.iter().any(|v| v.pattern_id == "all_in_no_reserve"),
            "expected all_in_no_reserve violation"
        );
    }

    #[test]
    fn test_self_cognition_block_overconfidence() {
        let mut d = make_decision();
        d.confidence = Some(0.95);
        d.facts = vec!["Fact 1".into()];

        let report = check_all(&d);
        assert!(
            report.violations.iter().any(|v| v.pattern_id == "self_cognition_block"),
            "expected self_cognition_block violation"
        );
    }

    #[test]
    fn test_market_cognition_block_triggers() {
        let mut d = make_decision();
        d.domain = "growth".into();
        d.goal = "Increase market share".into();
        d.information_sources = Vec::new();

        let report = check_all(&d);
        assert!(
            report.violations.iter().any(|v| v.pattern_id == "market_cognition_block"),
            "expected market_cognition_block violation"
        );
    }

    #[test]
    fn test_check_all_aggregates_violations() {
        let mut d = make_decision();
        d.information_sources = vec!["Source A".into()];
        d.confidence = Some(0.95);
        d.facts = vec!["Fact 1".into()];

        let report = check_all(&d);
        assert!(report.violations.len() >= 2);
    }

    #[test]
    fn test_check_all_has_crit_true() {
        let mut d = make_decision();
        d.goal = "Legal compliance matter".into();
        d.execution_plan = Some("Just do it".into());

        let report = check_all(&d);
        assert!(report.has_crit);
    }

    #[test]
    fn test_check_all_has_crit_false() {
        let mut d = make_decision();
        d.information_sources = vec!["A".into(), "B".into()];
        d.facts = vec!["Fact 1".into(), "Fact 2".into(), "Fact 3".into()];
        d.confidence = Some(0.7);

        let report = check_all(&d);
        assert!(!report.has_crit);
    }

    #[test]
    fn test_three_deferred_checks_return_none() {
        let d = make_decision();
        assert!(check_misplaced_priority(&d).is_none());
        assert!(check_inertia_thinking(&d).is_none());
        assert!(check_emotional_impulse(&d).is_none());
    }
}
