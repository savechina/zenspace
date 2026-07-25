//! Rule-based anti-pattern checker for decisions.
//!
//! Evaluates 10 decision anti-patterns and produces an `AntiPatternReport`.
//! Three checks require LLM semantic analysis and are deferred to `WisdomSynthesizer`.

use crate::decision::{AntiPatternReport, AntiPatternViolation, Decision, Severity};

const LEGAL_KEYWORDS: &[&str] = &[
    "版权",
    "税务",
    "隐私",
    "合同",
    "lawsuit",
    "patent",
    "compliance",
    "legal",
    "法律",
    "风险评估",
];
const MARKET_KEYWORDS: &[&str] = &["获客", "流量", "市场", "policy", "政策", "growth", "增长"];
const PROCEED_KEYWORDS: &[&str] = &["proceed", "继续", "推进", "go ahead", "all in", "坚持"];

/// Urgency keywords indicating time pressure (bilingual EN/ZH)
const URGENCY_KEYWORDS: &[&str] = &[
    "now",
    "immediately",
    "right now",
    "asap",
    "urgent",
    "act now",
    "final notice",
    "last chance",
    "don't wait",
    "limited time",
    "must decide",
    "no time to waste",
    "立即",
    "马上",
    "赶紧",
    "赶快",
    "紧急",
    "最后机会",
    "限时",
];

/// Emotional intensity markers — absolutist/extreme language
const EMOTION_INTENSITY: &[&str] = &[
    "absolutely",
    "never",
    "always",
    "everyone",
    "nobody",
    "completely",
    "totally",
    "insane",
    "crazy",
    "amazing",
    "terrible",
    "worst",
    "best",
    "love",
    "hate",
    "绝对",
    "从不",
    "总是",
    "完全",
    "彻底",
    "疯狂",
    "太棒了",
    "太糟了",
];

/// Cooling-off language — presence indicates deliberation (reduces impulse score)
const COOLING_LANGUAGE: &[&str] = &[
    "sleep on",
    "think about",
    "tomorrow",
    "next week",
    "revisit",
    "consider",
    "maybe later",
    "wait",
    "明天再说",
    "考虑一下",
    "再想想",
    "不急",
    "等等",
];

/// Alternative-consideration language — presence indicates deliberation
const ALTERNATIVE_MARKERS: &[&str] = &[
    "alternatives",
    "options",
    "instead",
    "could also",
    "pros and cons",
    "trade-offs",
    "on the other hand",
    "alternative",
    "comparison",
    "备选",
    "选项",
    "权衡",
    "利弊",
    "另一方面",
];

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

fn slugify_pattern_id(pattern_id: &str) -> String {
    pattern_id.replace('_', "-")
}

pub fn persist_anti_pattern_wiki_page(
    wiki_dir: &std::path::Path,
    violation: &AntiPatternViolation,
    decision_id: &str,
) -> Result<bool, std::io::Error> {
    let slug = slugify_pattern_id(&violation.pattern_id);
    let target_dir = wiki_dir
        .join("wisdom")
        .join("anti-patterns")
        .join("decisions");
    let target_path = target_dir.join(format!("{slug}.md"));

    if target_path.exists() {
        return Ok(false);
    }

    std::fs::create_dir_all(&target_dir)?;

    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let severity_str = match violation.severity {
        Severity::Crit => "crit",
        Severity::High => "high",
        Severity::Med => "medium",
    };

    let content = format!(
        "---\n\
         id: {slug}\n\
         type: anti-pattern\n\
         subtype: decision\n\
         category: decision\n\
         severity: {severity_str}\n\
         source_decision: {decision_id}\n\
         detected_at: {now}\n\
         ---\n\n\
         # {slug}\n\n\
         Detected in decision `{decision_id}`.\n\n\
         ## Trigger\n\n\
         {message}\n\n\
         ## Avoidance\n\n\
         _(To be expanded — review the decision context and document the avoidance strategy.)_\n",
        slug = slug,
        severity_str = severity_str,
        decision_id = decision_id,
        now = now,
        message = violation.message,
    );

    std::fs::write(&target_path, content)?;
    Ok(true)
}

/// Check if decision focus diverges from stated core pursuit.
///
/// Rule-based heuristic: tokenize core_pursuit into keywords, check if any
/// keyword appears in goal/choice/facts. If no overlap → misplaced priority.
fn check_misplaced_priority(d: &Decision) -> Option<AntiPatternViolation> {
    let core = d.core_pursuit.trim();
    if core.is_empty() {
        return None; // Can't check without a stated core pursuit
    }

    let core_keywords: Vec<&str> = core
        .split_whitespace()
        .filter(|w| w.len() > 2) // Skip short stopwords
        .collect();

    if core_keywords.is_empty() {
        return None;
    }

    let decision_text = format!("{} {} {}", d.goal, d.choice, d.facts.join(" ")).to_lowercase();

    let has_overlap = core_keywords.iter().any(|kw| {
        let kw_lower = kw.to_lowercase();
        decision_text.contains(&kw_lower)
    });

    if has_overlap {
        None
    } else {
        Some(AntiPatternViolation {
            pattern_id: "misplaced_priority".into(),
            severity: Severity::High,
            message: format!(
                "Decision focus appears disconnected from stated core pursuit: '{core}'"
            ),
        })
    }
}

/// Check if legal/compliance risk is present without risk assessment.
fn check_legal_risk_blind(d: &Decision) -> Option<AntiPatternViolation> {
    let text = format!("{} {} {}", d.goal, d.choice, d.facts.join(" "));
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
    let has_risk_assessment =
        plan_lower.contains("risk") || plan.contains("风险评估") || plan_lower.contains("legal");

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

/// Check if decision relies solely on past experience without current environment analysis.
///
/// Rule-based: flags when facts are non-empty but information_sources is empty,
/// or when facts contain "past success" markers without external validation.
fn check_inertia_thinking(d: &Decision) -> Option<AntiPatternViolation> {
    if d.facts.is_empty() {
        return None; // No facts to analyze
    }

    let has_sources = !d.information_sources.is_empty();
    if has_sources {
        return None; // External sources present — not pure inertia
    }

    let inertia_markers = [
        "past success",
        "previously worked",
        "last time",
        "always worked",
        "used to",
        "before it was",
        "上次",
        "之前成功",
        "以前都",
        "历来",
    ];

    let facts_text = d.facts.join(" ").to_lowercase();
    let has_inertia_marker = inertia_markers.iter().any(|m| facts_text.contains(m));

    if has_inertia_marker || !has_sources {
        Some(AntiPatternViolation {
            pattern_id: "inertia_thinking".into(),
            severity: Severity::High,
            message:
                "Decision based solely on past experience without current environment analysis"
                    .into(),
        })
    } else {
        None
    }
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

/// Compute an emotional impulse score (0.0 = calm, 1.0 = highly impulsive)
/// based on VADER sentiment analysis rules and behavioral economics research.
fn emotional_impulse_score(text: &str) -> f64 {
    let lower = text.to_lowercase();
    let mut score = 0.0f64;

    // ALL-CAPS ratio > 30% → boost (VADER: ALL-CAPS boosts polarity)
    let alpha_count = text.chars().filter(|c| c.is_alphabetic()).count();
    if alpha_count > 0 {
        let upper_count = text.chars().filter(|c| c.is_uppercase()).count();
        let caps_ratio = upper_count as f64 / alpha_count as f64;
        if caps_ratio > 0.3 {
            score += 0.2;
        }
    }

    // Exclamation marks (>2) → +0.05 each, max +0.20
    let excl_count = text.matches('!').count().min(4);
    score += excl_count as f64 * 0.05;

    // Urgency keyword hits → +0.10 each, max +0.30
    let urgency_hits = URGENCY_KEYWORDS
        .iter()
        .filter(|kw| lower.contains(*kw))
        .count();
    score += (urgency_hits as f64 * 0.1).min(0.3);

    // No cooling-off language → +0.15
    let has_cooling = COOLING_LANGUAGE.iter().any(|kw| lower.contains(*kw));
    if !has_cooling {
        score += 0.15;
    }

    // No alternative markers → +0.15
    let has_alternatives = ALTERNATIVE_MARKERS.iter().any(|kw| lower.contains(*kw));
    if !has_alternatives {
        score += 0.15;
    }

    // Emotional intensity markers → +0.08 each, max +0.20
    let intensity_hits = EMOTION_INTENSITY
        .iter()
        .filter(|kw| lower.contains(*kw))
        .count();
    score += (intensity_hits as f64 * 0.08).min(0.2);

    score.min(1.0)
}

fn check_emotional_impulse(d: &Decision) -> Option<AntiPatternViolation> {
    let mut text = String::new();
    text.push_str(&d.goal);
    text.push(' ');
    text.push_str(&d.choice);
    text.push(' ');
    text.push_str(&d.core_pursuit);
    for fact in &d.facts {
        text.push(' ');
        text.push_str(fact);
    }
    if let Some(plan) = &d.execution_plan {
        text.push(' ');
        text.push_str(plan);
    }

    let score = emotional_impulse_score(&text);

    if score > 0.6 {
        Some(AntiPatternViolation {
            pattern_id: "emotional-impulse".to_string(),
            severity: Severity::High,
            message: format!(
                "Emotional decision detected (impulse score {score:.2}): high urgency/intensity markers without cooling-off or alternatives"
            ),
        })
    } else {
        None
    }
}

/// Check for proceeding despite known legal/compliance risk.
fn check_fluke_mindset(d: &Decision) -> Option<AntiPatternViolation> {
    let text = format!("{} {} {}", d.goal, d.choice, d.facts.join(" "));
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
    let is_overconfident = d.confidence.as_ref().is_some_and(|c| *c > 0.9);
    let insufficient_facts = d.facts.len() < 3;

    if is_overconfident && insufficient_facts {
        Some(AntiPatternViolation {
            pattern_id: "self_cognition_block".into(),
            severity: Severity::High,
            message: "High confidence with insufficient evidence (potential Dunning-Kruger)".into(),
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
            violations
                .iter()
                .any(|v| v.pattern_id == "legal_risk_blind"),
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
            !report
                .violations
                .iter()
                .any(|v| v.pattern_id == "legal_risk_blind"),
            "should not trigger with risk assessment"
        );
    }

    #[test]
    fn test_legal_risk_blind_passes_no_keywords() {
        let d = make_decision();
        let report = check_all(&d);
        assert!(
            !report
                .violations
                .iter()
                .any(|v| v.pattern_id == "legal_risk_blind"),
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
            report
                .violations
                .iter()
                .any(|v| v.pattern_id == "loss_aversion"),
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
            !report
                .violations
                .iter()
                .any(|v| v.pattern_id == "loss_aversion"),
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
            report
                .violations
                .iter()
                .any(|v| v.pattern_id == "fluke_mindset"),
            "expected fluke_mindset violation"
        );
    }

    #[test]
    fn test_authority_blindness_single_source() {
        let mut d = make_decision();
        d.information_sources = vec!["Single source".into()];

        let report = check_all(&d);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.pattern_id == "authority_blindness"),
            "expected authority_blindness violation"
        );
    }

    #[test]
    fn test_authority_blindness_passes_multiple_sources() {
        let mut d = make_decision();
        d.information_sources = vec!["Source A".into(), "Source B".into()];

        let report = check_all(&d);
        assert!(
            !report
                .violations
                .iter()
                .any(|v| v.pattern_id == "authority_blindness"),
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
            report
                .violations
                .iter()
                .any(|v| v.pattern_id == "all_in_no_reserve"),
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
            report
                .violations
                .iter()
                .any(|v| v.pattern_id == "self_cognition_block"),
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
            report
                .violations
                .iter()
                .any(|v| v.pattern_id == "market_cognition_block"),
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
    fn test_two_deferred_checks_return_none() {
        let d = make_decision();
        assert!(check_misplaced_priority(&d).is_none());
        assert!(check_inertia_thinking(&d).is_none());
    }

    #[test]
    fn test_emotional_impulse_score_calm_text() {
        let text =
            "I should sleep on this. Let me consider the alternatives and trade-offs tomorrow.";
        let score = emotional_impulse_score(text);
        assert!(score < 0.3, "calm text should have low score, got {score}");
    }

    #[test]
    fn test_emotional_impulse_score_urgent_text() {
        let text = "We must decide NOW!!! This is URGENT!!! Act immediately, no time to waste!!!";
        let score = emotional_impulse_score(text);
        assert!(
            score > 0.6,
            "urgent text should have high score, got {score}"
        );
    }

    #[test]
    fn test_emotional_impulse_score_chinese_urgent() {
        let text = "立即决定！马上行动！紧急情况！赶紧！最后机会！";
        let score = emotional_impulse_score(text);
        assert!(
            score > 0.4,
            "Chinese urgent text should score high, got {score}"
        );
    }

    #[test]
    fn test_emotional_impulse_triggers() {
        let mut d = Decision::new("test-1".into(), "Urgent choice".into(), "work".into());
        d.goal = "Must decide NOW!!! URGENT!!!".into();
        d.choice = "Act immediately, no time to waste".into();
        d.facts = vec!["Everyone agrees this is crazy".into()];
        let result = check_emotional_impulse(&d);
        assert!(result.is_some(), "should trigger for emotional text");
        let v = result.unwrap();
        assert_eq!(v.pattern_id, "emotional-impulse");
        assert_eq!(v.severity, Severity::High);
    }

    #[test]
    fn test_emotional_impulse_passes_calm() {
        let mut d = Decision::new("test-2".into(), "Calm choice".into(), "work".into());
        d.goal = "Consider this carefully".into();
        d.choice = "Let's think about the alternatives and trade-offs".into();
        d.facts = vec!["We can sleep on it and revisit tomorrow".into()];
        let result = check_emotional_impulse(&d);
        assert!(result.is_none(), "should not trigger for calm text");
    }

    #[test]
    fn test_emotional_impulse_score_caps_at_1() {
        let text =
            "NOW!!! URGENT!!! ACT NOW!!! absolutely never always completely totally insane!!!";
        let score = emotional_impulse_score(text);
        assert!(score <= 1.0, "score should not exceed 1.0, got {score}");
    }

    #[test]
    fn test_persist_anti_pattern_creates_file_for_new_pattern() {
        use std::path::PathBuf;
        let tmp = tempfile::tempdir().unwrap();
        let wiki_dir: PathBuf = tmp.path().to_path_buf();
        let violation = AntiPatternViolation {
            pattern_id: "sunk_cost_fallacy".into(),
            severity: Severity::High,
            message: "Justifying continuation with past investment".into(),
        };

        let created = persist_anti_pattern_wiki_page(&wiki_dir, &violation, "dec-123").unwrap();
        assert!(created, "file should have been created");

        let target = wiki_dir.join("wisdom/anti-patterns/decisions/sunk-cost-fallacy.md");
        assert!(target.exists(), "target file should exist");
        let body = std::fs::read_to_string(&target).unwrap();
        assert!(body.contains("id: sunk-cost-fallacy"));
        assert!(body.contains("source_decision: dec-123"));
        assert!(body.contains("severity: high"));
        assert!(body.contains("Justifying continuation with past investment"));
    }

    #[test]
    fn test_persist_anti_pattern_skips_existing_file() {
        use std::path::PathBuf;
        let tmp = tempfile::tempdir().unwrap();
        let wiki_dir: PathBuf = tmp.path().to_path_buf();
        let violation = AntiPatternViolation {
            pattern_id: "inertia_thinking".into(),
            severity: Severity::Med,
            message: "Preferring status quo".into(),
        };

        let first = persist_anti_pattern_wiki_page(&wiki_dir, &violation, "dec-1").unwrap();
        assert!(first);
        let second = persist_anti_pattern_wiki_page(&wiki_dir, &violation, "dec-2").unwrap();
        assert!(
            !second,
            "second call must be a no-op when file already exists"
        );

        let body = std::fs::read_to_string(
            wiki_dir.join("wisdom/anti-patterns/decisions/inertia-thinking.md"),
        )
        .unwrap();
        assert!(
            body.contains("source_decision: dec-1"),
            "existing file must not be overwritten by second call"
        );
    }

    #[test]
    fn test_persist_anti_pattern_creates_parent_dirs() {
        use std::path::PathBuf;
        let tmp = tempfile::tempdir().unwrap();
        let wiki_dir: PathBuf = tmp.path().to_path_buf().join("nested").join("wiki");
        assert!(!wiki_dir.exists());

        let violation = AntiPatternViolation {
            pattern_id: "authority_blindness".into(),
            severity: Severity::Med,
            message: "Trusting authority without verification".into(),
        };

        let created = persist_anti_pattern_wiki_page(&wiki_dir, &violation, "dec-9").unwrap();
        assert!(created);
        assert!(
            wiki_dir
                .join("wisdom/anti-patterns/decisions/authority-blindness.md")
                .exists()
        );
    }
}
