//! Decision tracking with 5-layer schema and anti-pattern detection.
//!
//! Provides structured decision records with Goal/Facts/Logic/Execution/Feedback layers,
//! markdown persistence (YAML frontmatter + body sections), and rule-based anti-pattern
//! checking.
//!
//! Storage: `wiki/wisdom/decisions/{id}.md`

use std::fs;
use std::path::Path;

use anyhow::{Context as _, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::frontmatter::{extract_frontmatter, parse_field};

// ─── Data types ────────────────────────────────────────────────────────

/// A structured decision record with 5-layer schema.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Decision {
    /// Unique identifier (e.g., "decision-2026-06-26-career-switch").
    pub id: String,
    /// Short title of the decision.
    pub title: String,
    /// Domain categorization (e.g., "career", "architecture", "finance").
    pub domain: String,

    // Layer 1: Goal
    /// What this decision aims to achieve.
    pub goal: String,
    /// Whether the goal is a path, not the end goal itself.
    pub is_path_not_goal: bool,
    /// The core pursuit driving this decision.
    pub core_pursuit: String,

    // Layer 2: Facts
    /// Known facts relevant to the decision.
    pub facts: Vec<String>,
    /// Sources of information used.
    pub information_sources: Vec<String>,

    // Layer 3: Logic
    /// The chosen option.
    pub choice: String,
    /// Alternative options considered.
    pub alternatives: Vec<String>,
    /// Controllability factor (0.0-1.0).
    pub controllability: Option<f64>,
    /// Expected value analysis.
    pub expected_value: Option<ExpectedValue>,
    /// Confidence in the decision (0.0-1.0).
    pub confidence: Option<f64>,

    // Layer 4: Execution
    /// Cost breakdown for this decision.
    pub cost_analysis: CostBreakdown,
    /// Execution plan description.
    pub execution_plan: Option<String>,
    /// Low-cost validation approach.
    pub low_cost_validation: Option<String>,

    // Layer 5: Feedback
    /// Outcome after execution (if available).
    pub outcome: Option<Outcome>,
    /// Retrospective notes.
    pub retrospective: Option<String>,

    // Metadata
    /// When the decision was made.
    pub decided_at: DateTime<Utc>,
    /// When the decision was closed (completed/abandoned).
    pub closed_at: Option<DateTime<Utc>>,
}

/// Cost breakdown for a decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CostBreakdown {
    /// Economic cost (monetary).
    pub economic: f64,
    /// Time cost in hours.
    pub time_hours: f64,
    /// Credit/reputation cost.
    pub credit: f64,
    /// Sunk cost (already spent, non-recoverable).
    pub sunk: f64,
    /// Whether the cost is recoverable.
    pub is_recoverable: bool,
}

impl Default for CostBreakdown {
    fn default() -> Self {
        Self {
            economic: 0.0,
            time_hours: 0.0,
            credit: 0.0,
            sunk: 0.0,
            is_recoverable: true,
        }
    }
}

/// Expected value analysis for a decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExpectedValue {
    /// Probability of success (0.0-1.0).
    pub success_probability: f64,
    /// Payoff if successful.
    pub payoff_if_success: f64,
    /// Loss if failure occurs.
    pub loss_if_failure: f64,
    /// Whether the expected value is positive.
    pub is_positive_ev: bool,
    /// Whether the loss is affordable given economic capacity.
    pub loss_affordable: bool,
}

impl ExpectedValue {
    /// Calculate expected value: p * payoff - (1-p) * loss.
    pub fn ev(&self) -> f64 {
        self.success_probability * self.payoff_if_success
            - (1.0 - self.success_probability) * self.loss_if_failure
    }

    /// Recompute `is_positive_ev` and `loss_affordable` flags from current field values.
    pub fn compute_flags(&mut self) {
        self.is_positive_ev = self.ev() > 0.0;
        // Loss is affordable if zero or less than the payoff
        self.loss_affordable = self.loss_if_failure >= 0.0
            && (self.loss_if_failure == 0.0 || self.loss_if_failure < self.payoff_if_success);
    }
}

/// Outcome result after decision execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum OutcomeResult {
    Success,
    Failure,
    Partial,
}

/// Outcome record for a closed decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Outcome {
    /// The result of the decision.
    pub result: OutcomeResult,
    /// Notes about the outcome.
    pub notes: String,
    /// When the outcome was recorded.
    pub recorded_at: DateTime<Utc>,
}

/// Anti-pattern check report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AntiPatternReport {
    /// List of violations detected.
    pub violations: Vec<AntiPatternViolation>,
    /// Whether any CRIT-level violations exist.
    pub has_crit: bool,
}

/// A single anti-pattern violation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AntiPatternViolation {
    /// Identifier of the anti-pattern detected.
    pub pattern_id: String,
    /// Severity level.
    pub severity: Severity,
    /// Human-readable message.
    pub message: String,
}

/// Severity levels for anti-pattern violations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Critical — blocks promotion.
    Crit,
    /// High severity.
    High,
    /// Medium severity.
    Med,
}

// ─── Decision methods ──────────────────────────────────────────────────

impl Decision {
    /// Create a new decision with sensible defaults.
    pub fn new(id: String, title: String, domain: String) -> Self {
        Self {
            id,
            title,
            domain,
            goal: String::new(),
            is_path_not_goal: false,
            core_pursuit: String::new(),
            facts: Vec::new(),
            information_sources: Vec::new(),
            choice: String::new(),
            alternatives: Vec::new(),
            controllability: None,
            expected_value: None,
            confidence: None,
            cost_analysis: CostBreakdown::default(),
            execution_plan: None,
            low_cost_validation: None,
            outcome: None,
            retrospective: None,
            decided_at: Utc::now(),
            closed_at: None,
        }
    }

    /// Check if the decision is closed (has outcome or closed_at timestamp).
    pub fn is_closed(&self) -> bool {
        self.closed_at.is_some() || self.outcome.is_some()
    }

    /// Calculate age in days since the decision was made.
    pub fn age_days(&self, now: DateTime<Utc>) -> i64 {
        (now - self.decided_at).num_days()
    }

    /// Slugify a title into a decision ID.
    pub fn slugify_title(title: &str) -> String {
        title
            .to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect::<String>()
            .trim_matches('-')
            .to_string()
            .chars()
            .take(60)
            .collect()
    }
}

// ─── File persistence ──────────────────────────────────────────────────

impl Decision {
    /// Serialize decision to markdown format with YAML frontmatter + body sections.
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();
        md.push_str("---\n");
        md.push_str(&format!("id: {}\n", self.id));
        md.push_str(&format!("title: \"{}\"\n", self.title.replace('"', "\\\"")));
        md.push_str(&format!("domain: {}\n", self.domain));
        md.push_str(&format!("decided_at: {}\n", self.decided_at.to_rfc3339()));
        md.push_str(&format!(
            "closed_at: {}\n",
            self.closed_at
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_else(|| "null".to_string())
        ));
        if let Some(conf) = self.confidence {
            md.push_str(&format!("confidence: {conf}\n"));
        }
        md.push_str(&format!("is_path_not_goal: {}\n", self.is_path_not_goal));
        md.push_str("---\n\n");

        md.push_str(&format!("# Decision: {}\n\n", self.title));

        // Layer 1: Goal
        md.push_str("## Goal\n\n");
        md.push_str(&format!("goal: {}\n", self.goal));
        md.push_str(&format!("core_pursuit: {}\n", self.core_pursuit));
        md.push('\n');

        // Layer 2: Facts
        md.push_str("## Facts\n\n");
        if self.facts.is_empty() {
            md.push_str("_(no facts recorded)_\n");
        } else {
            for fact in &self.facts {
                md.push_str(&format!("- {fact}\n"));
            }
        }
        md.push('\n');

        // Sources
        md.push_str("## Sources\n\n");
        if self.information_sources.is_empty() {
            md.push_str("_(no sources recorded)_\n");
        } else {
            for source in &self.information_sources {
                md.push_str(&format!("- {source}\n"));
            }
        }
        md.push('\n');

        // Layer 3: Logic
        md.push_str("## Logic\n\n");
        md.push_str(&format!("choice: {}\n", self.choice));
        if !self.alternatives.is_empty() {
            md.push_str("alternatives:\n");
            for alt in &self.alternatives {
                md.push_str(&format!("- {alt}\n"));
            }
        }
        if let Some(ctrl) = self.controllability {
            md.push_str(&format!("controllability: {ctrl}\n"));
        }
        if let Some(ref ev) = self.expected_value {
            md.push_str(&format!(
                "success_probability: {}\n",
                ev.success_probability
            ));
            md.push_str(&format!("payoff: {}\n", ev.payoff_if_success));
            md.push_str(&format!("loss: {}\n", ev.loss_if_failure));
        }
        md.push('\n');

        // Layer 4: Execution
        md.push_str("## Execution\n\n");
        if let Some(ref plan) = self.execution_plan {
            md.push_str(&format!("plan: {plan}\n"));
        }
        if let Some(ref val) = self.low_cost_validation {
            md.push_str(&format!("low_cost_validation: {val}\n"));
        }
        md.push_str(&format!("cost_economic: {}\n", self.cost_analysis.economic));
        md.push_str(&format!("cost_time: {}\n", self.cost_analysis.time_hours));
        md.push_str(&format!("cost_credit: {}\n", self.cost_analysis.credit));
        md.push_str(&format!("cost_sunk: {}\n", self.cost_analysis.sunk));
        md.push_str(&format!(
            "is_recoverable: {}\n",
            self.cost_analysis.is_recoverable
        ));
        md.push('\n');

        // Layer 5: Feedback
        md.push_str("## Feedback\n\n");
        match &self.outcome {
            Some(outcome) => {
                let result_str = match outcome.result {
                    OutcomeResult::Success => "success",
                    OutcomeResult::Failure => "failure",
                    OutcomeResult::Partial => "partial",
                };
                md.push_str(&format!("outcome: {result_str}\n"));
                md.push_str(&format!("outcome_notes: {}\n", outcome.notes));
            }
            None => {
                md.push_str("outcome: (pending)\n");
            }
        }
        if let Some(ref retro) = self.retrospective {
            md.push_str(&format!("retrospective: {retro}\n"));
        }
        md
    }

    /// Save decision to `dir/{id}.md`.
    pub fn save(&self, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir)
            .with_context(|| format!("failed to create decisions dir: {}", dir.display()))?;
        let path = dir.join(format!("{}.md", self.id));
        let content = self.to_markdown();
        fs::write(&path, content)
            .with_context(|| format!("failed to write decision file: {}", path.display()))?;
        Ok(())
    }

    /// Load a specific decision from a directory by ID.
    pub fn load(dir: &Path, id: &str) -> Result<Decision> {
        let path = dir.join(format!("{id}.md"));
        Self::from_file(&path)
    }

    /// Load all decisions from a directory of `.md` files.
    pub fn load_all(dir: &Path) -> Result<Vec<Decision>> {
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut decisions = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "md") {
                match Self::from_file(&path) {
                    Ok(d) => decisions.push(d),
                    Err(e) => {
                        warn!(
                            path = %path.display(),
                            error = %e,
                            "failed to parse decision file, skipping"
                        );
                    }
                }
            }
        }
        Ok(decisions)
    }

    /// Parse a decision from a markdown file.
    pub fn from_file(path: &Path) -> Result<Decision> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read decision file: {}", path.display()))?;
        Self::from_markdown(&content)
    }

    /// Parse decision from markdown string (frontmatter + body).
    pub fn from_markdown(content: &str) -> Result<Decision> {
        let fm =
            extract_frontmatter(content).ok_or_else(|| anyhow::anyhow!("missing frontmatter"))?;
        let id = parse_field(&fm, "id").ok_or_else(|| anyhow::anyhow!("missing id field"))?;
        let title = parse_field(&fm, "title")
            .map(|s| s.trim_matches('"').to_string())
            .unwrap_or_default();
        let domain = parse_field(&fm, "domain").unwrap_or_else(|| "uncategorized".to_string());
        let decided_at = parse_field(&fm, "decided_at")
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);
        let closed_at = parse_field(&fm, "closed_at")
            .as_deref()
            .and_then(|s| {
                if s == "null" {
                    None
                } else {
                    DateTime::parse_from_rfc3339(s).ok()
                }
            })
            .map(|dt| dt.with_timezone(&Utc));
        let confidence = parse_field(&fm, "confidence").and_then(|s| s.parse().ok());
        let is_path_not_goal = parse_field(&fm, "is_path_not_goal")
            .map(|s| s == "true")
            .unwrap_or(false);

        // Parse body sections
        let body = extract_body(content)?;
        let goal = parse_body_key(&body, "goal").unwrap_or_default();
        let core_pursuit = parse_body_key(&body, "core_pursuit").unwrap_or_default();
        let facts = parse_body_list(&body, "## Facts");
        let information_sources = parse_body_list(&body, "## Sources");
        let choice = parse_body_key(&body, "choice").unwrap_or_default();
        let alternatives = parse_body_list_in_section(&body, "## Logic", "alternatives");
        let controllability = parse_body_key(&body, "controllability").and_then(|s| s.parse().ok());
        let execution_plan = parse_body_key(&body, "plan");
        let low_cost_validation = parse_body_key(&body, "low_cost_validation");

        // Parse expected value from body
        let expected_value = parse_expected_value(&body);

        // Parse cost breakdown from body
        let cost_analysis = parse_cost_breakdown(&body);

        // Parse outcome from body
        let outcome = parse_outcome(&body);
        let retrospective = parse_body_key(&body, "retrospective");

        Ok(Decision {
            id,
            title,
            domain,
            goal,
            is_path_not_goal,
            core_pursuit,
            facts,
            information_sources,
            choice,
            alternatives,
            controllability,
            expected_value,
            confidence,
            cost_analysis,
            execution_plan,
            low_cost_validation,
            outcome,
            retrospective,
            decided_at,
            closed_at,
        })
    }

    /// Run anti-pattern checks and return a report.
    pub fn run_anti_pattern_check(&self) -> AntiPatternReport {
        crate::decision_check::check_all(self)
    }
}

// ─── Body parsing helpers ──────────────────────────────────────────────

/// Extract everything after the frontmatter closing `---`.
fn extract_body(content: &str) -> Result<String> {
    let mut lines = content.lines();
    // Skip opening ---
    lines.next();
    let mut past_frontmatter = false;
    let mut body = String::new();
    for line in lines {
        if !past_frontmatter {
            if line.trim() == "---" {
                past_frontmatter = true;
            }
            continue;
        }
        body.push_str(line);
        body.push('\n');
    }
    Ok(body)
}

fn parse_body_key(body: &str, key: &str) -> Option<String> {
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(&format!("{key}:")) {
            let val = rest.trim().to_string();
            if !val.is_empty() && val != "(pending)" {
                return Some(val);
            }
        }
    }
    None
}

/// Parse a list of items from a section (e.g., `- item` lines under `## Facts`).
fn parse_body_list(body: &str, section_header: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut in_section = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed == section_header {
            in_section = true;
            continue;
        }
        if in_section {
            if trimmed.starts_with("## ") {
                break;
            }
            if let Some(item) = trimmed.strip_prefix("- ") {
                let item = item.trim().to_string();
                if !item.is_empty() {
                    items.push(item);
                }
            }
        }
    }
    items
}

/// Parse a list from a sub-section within a section (e.g., `alternatives:` under `## Logic`).
fn parse_body_list_in_section(body: &str, section_header: &str, list_key: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut in_section = false;
    let mut in_list = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed == section_header {
            in_section = true;
            continue;
        }
        if in_section {
            if trimmed.starts_with("## ") {
                break;
            }
            if trimmed == format!("{list_key}:") {
                in_list = true;
                continue;
            }
            if in_list {
                if let Some(item) = trimmed.strip_prefix("- ") {
                    let item = item.trim().to_string();
                    if !item.is_empty() {
                        items.push(item);
                    }
                } else if !trimmed.is_empty() {
                    in_list = false;
                }
            }
        }
    }
    items
}

/// Parse expected value fields from body.
fn parse_expected_value(body: &str) -> Option<ExpectedValue> {
    let sp = parse_body_key(body, "success_probability").and_then(|s| s.parse().ok())?;
    let payoff = parse_body_key(body, "payoff").and_then(|s| s.parse().ok())?;
    let loss = parse_body_key(body, "loss").and_then(|s| s.parse().ok())?;

    let ev = sp * payoff - (1.0 - sp) * loss;
    Some(ExpectedValue {
        success_probability: sp,
        payoff_if_success: payoff,
        loss_if_failure: loss,
        is_positive_ev: ev > 0.0,
        loss_affordable: false,
    })
}

/// Parse cost breakdown fields from body.
fn parse_cost_breakdown(body: &str) -> CostBreakdown {
    CostBreakdown {
        economic: parse_body_key(body, "cost_economic")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0),
        time_hours: parse_body_key(body, "cost_time")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0),
        credit: parse_body_key(body, "cost_credit")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0),
        sunk: parse_body_key(body, "cost_sunk")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0),
        is_recoverable: parse_body_key(body, "is_recoverable")
            .map(|s| s == "true")
            .unwrap_or(true),
    }
}

/// Parse outcome from body.
fn parse_outcome(body: &str) -> Option<Outcome> {
    let outcome_str = parse_body_key(body, "outcome")?;
    match outcome_str.as_str() {
        "success" | "failure" | "partial" => {
            let result = match outcome_str.as_str() {
                "success" => OutcomeResult::Success,
                "failure" => OutcomeResult::Failure,
                "partial" => OutcomeResult::Partial,
                _ => unreachable!(),
            };
            let notes = parse_body_key(body, "outcome_notes").unwrap_or_default();
            Some(Outcome {
                result,
                notes,
                recorded_at: Utc::now(),
            })
        }
        _ => None,
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_new_defaults() {
        let d = Decision::new("test-id".into(), "Test Title".into(), "tech".into());
        assert_eq!(d.id, "test-id");
        assert_eq!(d.title, "Test Title");
        assert_eq!(d.domain, "tech");
        assert!(d.goal.is_empty());
        assert!(!d.is_path_not_goal);
        assert!(d.core_pursuit.is_empty());
        assert!(d.facts.is_empty());
        assert!(d.information_sources.is_empty());
        assert!(d.choice.is_empty());
        assert!(d.alternatives.is_empty());
        assert_eq!(d.controllability, None);
        assert_eq!(d.expected_value, None);
        assert_eq!(d.confidence, None);
        assert_eq!(d.cost_analysis, CostBreakdown::default());
        assert_eq!(d.execution_plan, None);
        assert_eq!(d.low_cost_validation, None);
        assert_eq!(d.outcome, None);
        assert_eq!(d.retrospective, None);
        assert_eq!(d.closed_at, None);
    }

    #[test]
    fn test_is_closed_with_outcome() {
        let mut d = Decision::new("test".into(), "Test".into(), "domain".into());
        assert!(!d.is_closed());
        d.outcome = Some(Outcome {
            result: OutcomeResult::Success,
            notes: "done".into(),
            recorded_at: Utc::now(),
        });
        assert!(d.is_closed());
    }

    #[test]
    fn test_is_closed_with_closed_at() {
        let mut d = Decision::new("test".into(), "Test".into(), "domain".into());
        assert!(!d.is_closed());
        d.closed_at = Some(Utc::now());
        assert!(d.is_closed());
    }

    #[test]
    fn test_is_closed_neither() {
        let d = Decision::new("test".into(), "Test".into(), "domain".into());
        assert!(!d.is_closed());
    }

    #[test]
    fn test_age_days() {
        let d = Decision::new("test".into(), "Test".into(), "domain".into());
        let now = Utc::now();
        assert_eq!(d.age_days(now), 0);
        let future = now + chrono::Duration::days(5);
        assert_eq!(d.age_days(future), 5);
    }

    #[test]
    fn test_slugify_title() {
        assert_eq!(
            Decision::slugify_title("Switch Career Path"),
            "switch-career-path"
        );
        assert_eq!(Decision::slugify_title("  spaces  "), "spaces");
        let long = Decision::slugify_title(
            "A very long title that exceeds the sixty character limit for slugs",
        );
        assert_eq!(long.len(), 60);
    }

    #[test]
    fn test_to_markdown_roundtrip() {
        let mut d = Decision::new("rt-test".into(), "Round Trip".into(), "career".into());
        d.goal = "Transition".into();
        d.core_pursuit = "Fulfillment".into();
        d.facts = vec!["Fact A".into(), "Fact B".into()];
        d.information_sources = vec!["Source X".into()];
        d.choice = "Option 1".into();
        d.alternatives = vec!["Option 2".into(), "Option 3".into()];
        d.controllability = Some(0.7);
        d.confidence = Some(0.8);
        d.execution_plan = Some("Step 1".into());

        let md = d.to_markdown();
        let parsed = Decision::from_markdown(&md).unwrap();
        assert_eq!(parsed.id, "rt-test");
        assert_eq!(parsed.title, "Round Trip");
        assert_eq!(parsed.domain, "career");
        assert_eq!(parsed.goal, "Transition");
        assert_eq!(parsed.core_pursuit, "Fulfillment");
        assert_eq!(parsed.facts, vec!["Fact A", "Fact B"]);
        assert_eq!(parsed.information_sources, vec!["Source X"]);
        assert_eq!(parsed.choice, "Option 1");
        assert_eq!(parsed.alternatives, vec!["Option 2", "Option 3"]);
        assert_eq!(parsed.controllability, Some(0.7));
        assert_eq!(parsed.confidence, Some(0.8));
        assert_eq!(parsed.execution_plan, Some("Step 1".into()));
    }

    #[test]
    fn test_to_markdown_includes_all_layers() {
        let mut d = Decision::new("test".into(), "Test".into(), "domain".into());
        d.goal = "Goal text".into();
        d.facts = vec!["Fact 1".into()];
        d.choice = "Chosen".into();
        d.execution_plan = Some("Plan".into());

        let md = d.to_markdown();
        assert!(md.contains("## Goal"));
        assert!(md.contains("## Facts"));
        assert!(md.contains("## Sources"));
        assert!(md.contains("## Logic"));
        assert!(md.contains("## Execution"));
        assert!(md.contains("## Feedback"));
    }

    #[test]
    fn test_from_markdown_parses_frontmatter() {
        let md = r#"---
id: test-id
title: "Test Decision"
domain: tech
decided_at: 2026-06-26T09:00:00Z
closed_at: null
confidence: 0.75
is_path_not_goal: false
---

# Decision: Test Decision

## Goal
goal: Do something
core_pursuit: Value

## Facts
- Fact A

## Sources
- Source B

## Logic
choice: Option 1

## Execution

## Feedback
outcome: (pending)
"#;
        let d = Decision::from_markdown(md).unwrap();
        assert_eq!(d.id, "test-id");
        assert_eq!(d.title, "Test Decision");
        assert_eq!(d.domain, "tech");
        assert_eq!(d.confidence, Some(0.75));
        assert!(!d.is_path_not_goal);
        assert!(d.closed_at.is_none());
    }

    #[test]
    fn test_from_markdown_parses_body_lists() {
        let md = r#"---
id: test
title: "Test"
domain: d
decided_at: 2026-06-26T09:00:00Z
closed_at: null
is_path_not_goal: false
---

# Decision: Test

## Goal
goal: Goal
core_pursuit: Pursuit

## Facts
- First fact
- Second fact
- Third fact

## Sources
- Source A
- Source B

## Logic
choice: Choice A
alternatives:
- Alt B
- Alt C

## Execution

## Feedback
outcome: (pending)
"#;
        let d = Decision::from_markdown(md).unwrap();
        assert_eq!(d.facts.len(), 3);
        assert_eq!(d.information_sources.len(), 2);
        assert_eq!(d.alternatives, vec!["Alt B", "Alt C"]);
    }

    #[test]
    fn test_save_creates_file() {
        let tmp = tempdir().unwrap();
        let d = Decision::new("save-test".into(), "Save Test".into(), "domain".into());
        d.save(tmp.path()).unwrap();
        let path = tmp.path().join("save-test.md");
        assert!(path.exists());
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("id: save-test"));
    }

    #[test]
    fn test_load_all_empty_dir() {
        let tmp = tempdir().unwrap();
        let decisions = Decision::load_all(tmp.path()).unwrap();
        assert!(decisions.is_empty());
    }

    #[test]
    fn test_load_all_multiple_files() {
        let tmp = tempdir().unwrap();
        let d1 = Decision::new("d1".into(), "Decision 1".into(), "a".into());
        let d2 = Decision::new("d2".into(), "Decision 2".into(), "b".into());
        d1.save(tmp.path()).unwrap();
        d2.save(tmp.path()).unwrap();

        let loaded = Decision::load_all(tmp.path()).unwrap();
        assert_eq!(loaded.len(), 2);
        let ids: Vec<&str> = loaded.iter().map(|d| d.id.as_str()).collect();
        assert!(ids.contains(&"d1"));
        assert!(ids.contains(&"d2"));
    }

    #[test]
    fn test_expected_value_ev_calculation() {
        let ev = ExpectedValue {
            success_probability: 0.6,
            payoff_if_success: 50000.0,
            loss_if_failure: 5000.0,
            is_positive_ev: true,
            loss_affordable: true,
        };
        // EV = 0.6 * 50000 - 0.4 * 5000 = 30000 - 2000 = 28000
        assert!((ev.ev() - 28000.0).abs() < 0.01);
    }

    #[test]
    fn test_expected_value_compute_flags_positive() {
        let ev = ExpectedValue {
            success_probability: 0.7,
            payoff_if_success: 10000.0,
            loss_if_failure: 2000.0,
            is_positive_ev: true,
            loss_affordable: false,
        };
        let computed = ExpectedValue {
            success_probability: ev.success_probability,
            payoff_if_success: ev.payoff_if_success,
            loss_if_failure: ev.loss_if_failure,
            is_positive_ev: ev.ev() > 0.0,
            loss_affordable: ev.loss_if_failure < 5000.0,
        };
        assert!(computed.is_positive_ev);
        assert!(computed.loss_affordable);
    }

    #[test]
    fn test_expected_value_compute_flags_negative() {
        let ev = ExpectedValue {
            success_probability: 0.3,
            payoff_if_success: 5000.0,
            loss_if_failure: 10000.0,
            is_positive_ev: false,
            loss_affordable: false,
        };
        // EV = 0.3 * 5000 - 0.7 * 10000 = 1500 - 7000 = -5500
        assert!(ev.ev() < 0.0);
        assert!(!ev.is_positive_ev);
    }

    #[test]
    fn test_cost_breakdown_default() {
        let cb = CostBreakdown::default();
        assert_eq!(cb.economic, 0.0);
        assert_eq!(cb.time_hours, 0.0);
        assert_eq!(cb.credit, 0.0);
        assert_eq!(cb.sunk, 0.0);
        assert!(cb.is_recoverable);
    }
}
