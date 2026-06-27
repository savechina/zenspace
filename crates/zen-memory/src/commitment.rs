use std::fmt;
use std::fs;
use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use tracing::warn;

// ─── GTD lifecycle states ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CommitmentState {
    Drafted,
    Validated,
    Executing,
    Reviewing,
    Completed,
    Abandoned,
    Pivoted,
}

impl fmt::Display for CommitmentState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Drafted => write!(f, "drafted"),
            Self::Validated => write!(f, "validated"),
            Self::Executing => write!(f, "executing"),
            Self::Reviewing => write!(f, "reviewing"),
            Self::Completed => write!(f, "completed"),
            Self::Abandoned => write!(f, "abandoned"),
            Self::Pivoted => write!(f, "pivoted"),
        }
    }
}

impl CommitmentState {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "drafted" => Some(Self::Drafted),
            "validated" => Some(Self::Validated),
            "executing" => Some(Self::Executing),
            "reviewing" => Some(Self::Reviewing),
            "completed" => Some(Self::Completed),
            "abandoned" => Some(Self::Abandoned),
            "pivoted" => Some(Self::Pivoted),
            _ => None,
        }
    }
}

// ─── Transition errors ─────────────────────────────────────────────────

#[derive(Debug, Clone, thiserror::Error)]
pub enum TransitionError {
    #[error("missing low-cost validation for drafted→validated")]
    MissingValidation,
    #[error("missing stop-loss for drafted→validated")]
    MissingStopLoss,
    #[error("insufficient milestones ({0}/3 required) for validated→executing")]
    InsufficientMilestones(u32),
    #[error("missing milestone data feedback for executing→reviewing")]
    MissingMilestoneDataFeedback,
    #[error("missing retrospective for reviewing→completed")]
    MissingRetrospective,
    #[error("missing stop-loss extraction for any→abandoned")]
    MissingStopLossExtraction,
    #[error("missing sustained value plan for reviewing→pivoted")]
    MissingSustainedValue,
    #[error("invalid transition from {from} to {to}")]
    InvalidTransition { from: String, to: String },
}

// ─── Commitment errors ─────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum CommitmentError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse error: {0}")]
    Parse(String),
}

// ─── Nested types ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Milestone {
    pub description: String,
    pub target_date: Option<NaiveDate>,
    pub completed: bool,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StopLossLine {
    pub economic: Option<f64>,
    pub time_hours: Option<f64>,
    pub trigger_action: String,
    pub triggered: bool,
    pub triggered_at: Option<DateTime<Utc>>,
}

impl Default for StopLossLine {
    fn default() -> Self {
        Self {
            economic: None,
            time_hours: None,
            trigger_action: "abandon + extract sustained value".to_string(),
            triggered: false,
            triggered_at: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionChecklist {
    pub low_cost_validation: Option<String>,
    pub detail_risks_identified: bool,
    pub avoid_perfect_decision: bool,
    pub milestone_feedback_planned: bool,
    pub retrospective_planned: bool,
    pub self_discipline_assessed: bool,
    pub stop_loss_committed: bool,
    pub sustained_value_plan: Option<String>,
}

impl ExecutionChecklist {
    pub fn completed_count(&self) -> u32 {
        let mut count = 0u32;
        if self.low_cost_validation.is_some() {
            count += 1;
        }
        if self.detail_risks_identified {
            count += 1;
        }
        if self.avoid_perfect_decision {
            count += 1;
        }
        if self.milestone_feedback_planned {
            count += 1;
        }
        if self.retrospective_planned {
            count += 1;
        }
        if self.self_discipline_assessed {
            count += 1;
        }
        if self.stop_loss_committed {
            count += 1;
        }
        if self.sustained_value_plan.is_some() {
            count += 1;
        }
        count
    }

    pub fn total_count(&self) -> u32 {
        8
    }

    pub fn is_complete(&self) -> bool {
        self.completed_count() == self.total_count()
    }
}

// ─── Main Commitment struct ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commitment {
    pub id: String,
    pub what: String,
    pub state: CommitmentState,
    pub by_when: Option<NaiveDate>,
    pub review_at: Option<NaiveDate>,
    pub next_action: String,
    pub two_minute_rule: bool,
    pub milestones: Vec<Milestone>,
    pub stop_loss: StopLossLine,
    pub discipline_streak: u32,
    pub sustained_value_plan: Option<String>,
    pub retrospective: Option<String>,
    pub execution_checklist: ExecutionChecklist,
    pub source_journal: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
}

impl Commitment {
    pub fn new(what: &str) -> Self {
        let now = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            what: what.to_string(),
            state: CommitmentState::Drafted,
            by_when: None,
            review_at: None,
            next_action: String::new(),
            two_minute_rule: false,
            milestones: Vec::new(),
            stop_loss: StopLossLine::default(),
            discipline_streak: 0,
            sustained_value_plan: None,
            retrospective: None,
            execution_checklist: ExecutionChecklist::default(),
            source_journal: None,
            created_at: now,
            updated_at: now,
            closed_at: None,
        }
    }

    pub fn from_raw(text: &str) -> Self {
        let mut c = Self::new(text);
        c.next_action = text.to_string();
        c
    }

    pub fn slug(&self) -> String {
        slugify_text(&self.what)
    }

    // ─── State machine ────────────────────────────────────────────────

    pub fn can_transition_to(&self, target: &CommitmentState) -> Result<(), TransitionError> {
        use CommitmentState::*;
        match (&self.state, target) {
            (Drafted, Validated) => {
                if self.execution_checklist.low_cost_validation.is_none() {
                    return Err(TransitionError::MissingValidation);
                }
                if !self.stop_loss.triggered && self.stop_loss.economic.is_none()
                    && self.stop_loss.time_hours.is_none()
                {
                    return Err(TransitionError::MissingStopLoss);
                }
                Ok(())
            }
            (Validated, Executing) => {
                let count = self.milestones.len() as u32;
                if count < 3 {
                    return Err(TransitionError::InsufficientMilestones(count));
                }
                Ok(())
            }
            (Executing, Reviewing) => {
                // DESIGN.md §5.3: each milestone has quantitative data feedback
                let all_have_data = self
                    .milestones
                    .iter()
                    .all(|m| m.completed && m.completed_at.is_some());
                if !all_have_data {
                    return Err(TransitionError::MissingMilestoneDataFeedback);
                }
                Ok(())
            }
            (Reviewing, Completed) => {
                // DESIGN.md §5.3: retrospective written (自己多找问题)
                match &self.retrospective {
                    Some(r) if !r.trim().is_empty() => Ok(()),
                    _ => Err(TransitionError::MissingRetrospective),
                }
            }
            (Reviewing, Pivoted) => {
                if self.sustained_value_plan.is_none() {
                    return Err(TransitionError::MissingSustainedValue);
                }
                Ok(())
            }
            (_, to) if *to == Abandoned => {
                // DESIGN.md §5.3: stop-loss trigger OR sustained_value extraction
                if !self.stop_loss.triggered && self.sustained_value_plan.is_none() {
                    return Err(TransitionError::MissingStopLossExtraction);
                }
                Ok(())
            }
            (Completed | Abandoned | Pivoted, _) => Err(TransitionError::InvalidTransition {
                from: self.state.to_string(),
                to: target.to_string(),
            }),
            (from, to) => Err(TransitionError::InvalidTransition {
                from: from.to_string(),
                to: to.to_string(),
            }),
        }
    }

    pub fn transition(&mut self, target: CommitmentState) -> Result<(), TransitionError> {
        self.can_transition_to(&target)?;
        self.state = target;
        self.updated_at = Utc::now();
        if self.state == CommitmentState::Completed
            || self.state == CommitmentState::Abandoned
            || self.state == CommitmentState::Pivoted
        {
            self.closed_at = Some(Utc::now());
        }
        Ok(())
    }

    pub fn trigger_stop_loss(&mut self) {
        self.stop_loss.triggered = true;
        self.stop_loss.triggered_at = Some(Utc::now());
        let _ = self.transition(CommitmentState::Abandoned);
    }

    pub fn add_milestone(&mut self, description: &str, target_date: Option<NaiveDate>) {
        self.milestones.push(Milestone {
            description: description.to_string(),
            target_date,
            completed: false,
            completed_at: None,
        });
        self.updated_at = Utc::now();
    }

    pub fn complete_milestone(&mut self, index: usize) -> Result<(), String> {
        let m = self
            .milestones
            .get_mut(index)
            .ok_or_else(|| format!("milestone index {index} out of range"))?;
        if m.completed {
            return Err("milestone already completed".to_string());
        }
        m.completed = true;
        m.completed_at = Some(Utc::now());
        self.updated_at = Utc::now();
        Ok(())
    }

    pub fn milestone_count(&self) -> usize {
        self.milestones.len()
    }

    pub fn is_overdue(&self) -> bool {
        if matches!(
            self.state,
            CommitmentState::Completed | CommitmentState::Abandoned | CommitmentState::Pivoted
        ) {
            return false;
        }
        match self.review_at {
            Some(date) => date < Utc::now().date_naive(),
            None => false,
        }
    }

    // ─── File persistence ─────────────────────────────────────────────

    pub fn save(&self, dir: &Path) -> Result<std::path::PathBuf, CommitmentError> {
        fs::create_dir_all(dir).map_err(CommitmentError::Io)?;
        let filename = format!("{}.md", self.slug());
        let path = dir.join(&filename);
        let content = self.to_markdown();
        fs::write(&path, content).map_err(CommitmentError::Io)?;
        Ok(path)
    }

    pub fn load(path: &Path) -> Result<Self, CommitmentError> {
        let content = fs::read_to_string(path).map_err(CommitmentError::Io)?;
        Self::from_markdown(&content)
    }

    pub fn load_all(dir: &Path) -> Result<Vec<Self>, CommitmentError> {
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut commitments = Vec::new();
        for entry in fs::read_dir(dir).map_err(CommitmentError::Io)? {
            let entry = entry.map_err(CommitmentError::Io)?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "md") {
                match Self::load(&path) {
                    Ok(c) => commitments.push(c),
                    Err(e) => {
                        warn!(
                            path = %path.display(),
                            error = %e,
                            "failed to parse commitment file, skipping"
                        );
                    }
                }
            }
        }
        Ok(commitments)
    }

    pub fn to_markdown(&self) -> String {
        let mut md = String::new();
        md.push_str("---\n");
        md.push_str(&format!("id: {}\n", self.id));
        md.push_str(&format!(
            "what: \"{}\"\n",
            self.what.replace('"', "\\\"")
        ));
        md.push_str(&format!("state: {}\n", self.state));
        if let Some(d) = self.by_when {
            md.push_str(&format!("by_when: {}\n", d));
        }
        if let Some(d) = self.review_at {
            md.push_str(&format!("review_at: {}\n", d));
        }
        md.push_str(&format!(
            "next_action: \"{}\"\n",
            self.next_action.replace('"', "\\\"")
        ));
        md.push_str(&format!("two_minute_rule: {}\n", self.two_minute_rule));
        md.push_str(&format!("discipline_streak: {}\n", self.discipline_streak));
        md.push_str(&format!(
            "created_at: {}\n",
            self.created_at.to_rfc3339()
        ));
        md.push_str(&format!(
            "updated_at: {}\n",
            self.updated_at.to_rfc3339()
        ));
        if let Some(closed) = self.closed_at {
            md.push_str(&format!("closed_at: {}\n", closed.to_rfc3339()));
        }
        if let Some(ref journal) = self.source_journal {
            md.push_str(&format!("source_journal: {}\n", journal));
        }
        md.push_str("---\n\n");

        md.push_str(&format!("# Commitment: {}\n\n", self.what));

        md.push_str(&format!("**State**: {}\n\n", self.state));

        md.push_str("## Milestones\n\n");
        if self.milestones.is_empty() {
            md.push_str("_(no milestones)_\n");
        } else {
            for m in &self.milestones {
                let check = if m.completed { "x" } else { " " };
                md.push_str(&format!("- [{check}] {}", m.description));
                if let Some(d) = m.target_date {
                    md.push_str(&format!(" (by {d})"));
                }
                md.push('\n');
            }
        }
        md.push('\n');

        md.push_str("## Stop Loss\n\n");
        if let Some(e) = self.stop_loss.economic {
            md.push_str(&format!("economic: {e}\n"));
        }
        if let Some(t) = self.stop_loss.time_hours {
            md.push_str(&format!("time_hours: {t}\n"));
        }
        md.push_str(&format!(
            "trigger_action: {}\n",
            self.stop_loss.trigger_action
        ));
        md.push_str(&format!("triggered: {}\n", self.stop_loss.triggered));
        md.push('\n');

        if let Some(ref plan) = self.sustained_value_plan {
            md.push_str("## Sustained Value Plan\n\n");
            md.push_str(&format!("{plan}\n\n"));
        }

        md
    }

    pub fn from_markdown(content: &str) -> Result<Self, CommitmentError> {
        let fm = extract_frontmatter(content)
            .map_err(|e| CommitmentError::Parse(e.to_string()))?;

        let id = parse_yaml_field(&fm, "id")
            .ok_or_else(|| CommitmentError::Parse("missing id field".into()))?;
        let what = parse_yaml_field(&fm, "what")
            .map(|s| s.trim_matches('"').to_string())
            .unwrap_or_default();
        let state = parse_yaml_field(&fm, "state")
            .and_then(|s| CommitmentState::from_str(&s))
            .unwrap_or(CommitmentState::Drafted);
        let by_when = parse_yaml_field(&fm, "by_when")
            .and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok());
        let review_at = parse_yaml_field(&fm, "review_at")
            .and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok());
        let next_action = parse_yaml_field(&fm, "next_action")
            .map(|s| s.trim_matches('"').to_string())
            .unwrap_or_default();
        let two_minute_rule = parse_yaml_field(&fm, "two_minute_rule")
            .map(|s| s == "true")
            .unwrap_or(false);
        let discipline_streak = parse_yaml_field(&fm, "discipline_streak")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let created_at = parse_yaml_field(&fm, "created_at")
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);
        let updated_at = parse_yaml_field(&fm, "updated_at")
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);
        let closed_at = parse_yaml_field(&fm, "closed_at")
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc));
        let source_journal = parse_yaml_field(&fm, "source_journal");

        let body = extract_body(content)
            .map_err(|e| CommitmentError::Parse(e.to_string()))?;
        let milestones = parse_milestones(&body);
        let stop_loss = parse_stop_loss(&body);
        let sustained_value_plan = parse_body_section(&body, "## Sustained Value Plan");
        let retrospective = parse_body_section(&body, "## Retrospective");

        Ok(Self {
            id,
            what,
            state,
            by_when,
            review_at,
            next_action,
            two_minute_rule,
            milestones,
            stop_loss,
            discipline_streak,
            sustained_value_plan,
            retrospective,
            execution_checklist: ExecutionChecklist::default(),
            source_journal,
            created_at,
            updated_at,
            closed_at,
        })
    }
}

// ─── Slugify ───────────────────────────────────────────────────────────

pub fn slugify_text(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
        .chars()
        .take(60)
        .collect()
}

// ─── Frontmatter parsing helpers ───────────────────────────────────────

fn extract_frontmatter(content: &str) -> Result<String> {
    let mut lines = content.lines();
    let first = lines.next().unwrap_or("").trim();
    if first != "---" {
        anyhow::bail!("missing frontmatter opening ---");
    }
    let mut fm = String::new();
    for line in lines {
        if line.trim() == "---" {
            return Ok(fm);
        }
        fm.push_str(line);
        fm.push('\n');
    }
    anyhow::bail!("missing frontmatter closing ---");
}

fn parse_yaml_field(frontmatter: &str, key: &str) -> Option<String> {
    for line in frontmatter.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(&format!("{key}:")) {
            let val = rest.trim().to_string();
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

fn extract_body(content: &str) -> Result<String> {
    let mut lines = content.lines();
    lines.next(); // skip opening ---
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

fn parse_milestones(body: &str) -> Vec<Milestone> {
    let mut milestones = Vec::new();
    let mut in_section = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed == "## Milestones" {
            in_section = true;
            continue;
        }
        if in_section {
            if trimmed.starts_with("## ") {
                break;
            }
            if let Some(item) = trimmed.strip_prefix("- ") {
                let completed = item.starts_with("[x] ");
                let desc = if completed {
                    item.strip_prefix("[x] ").unwrap_or(item)
                } else {
                    item.strip_prefix("[ ] ").unwrap_or(item)
                };
                // strip trailing (by YYYY-MM-DD)
                let (desc, target_date) = if let Some(idx) = desc.rfind(" (by ") {
                    let date_str = &desc[idx + 5..].trim_end_matches(')');
                    (
                        desc[..idx].to_string(),
                        NaiveDate::parse_from_str(date_str, "%Y-%m-%d").ok(),
                    )
                } else {
                    (desc.to_string(), None)
                };
                milestones.push(Milestone {
                    description: desc,
                    target_date,
                    completed,
                    completed_at: None,
                });
            }
        }
    }
    milestones
}

fn parse_stop_loss(body: &str) -> StopLossLine {
    let mut sl = StopLossLine::default();
    let mut in_section = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed == "## Stop Loss" {
            in_section = true;
            continue;
        }
        if in_section {
            if trimmed.starts_with("## ") {
                break;
            }
            if let Some(v) = trimmed.strip_prefix("economic: ") {
                sl.economic = v.parse().ok();
            } else if let Some(v) = trimmed.strip_prefix("time_hours: ") {
                sl.time_hours = v.parse().ok();
            } else if let Some(v) = trimmed.strip_prefix("trigger_action: ") {
                sl.trigger_action = v.to_string();
            } else if let Some(v) = trimmed.strip_prefix("triggered: ") {
                sl.triggered = v == "true";
            }
        }
    }
    sl
}

fn parse_body_section(body: &str, header: &str) -> Option<String> {
    let mut lines_iter = body.lines();
    // find the header
    loop {
        let line = lines_iter.next()?.trim();
        if line == header {
            break;
        }
    }
    let mut content = String::new();
    for line in lines_iter {
        let trimmed = line.trim();
        if trimmed.starts_with("## ") {
            break;
        }
        content.push_str(line);
        content.push('\n');
    }
    let trimmed = content.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_new_defaults() {
        let c = Commitment::new("ship feature X");
        assert_eq!(c.what, "ship feature X");
        assert_eq!(c.state, CommitmentState::Drafted);
        assert!(c.by_when.is_none());
        assert!(c.review_at.is_none());
        assert!(c.next_action.is_empty());
        assert!(!c.two_minute_rule);
        assert!(c.milestones.is_empty());
        assert!(!c.stop_loss.triggered);
        assert_eq!(c.discipline_streak, 0);
        assert!(c.sustained_value_plan.is_none());
        assert!(c.closed_at.is_none());
    }

    #[test]
    fn test_from_raw_parses_text() {
        let c = Commitment::from_raw("write tests by Friday");
        assert_eq!(c.what, "write tests by Friday");
        assert_eq!(c.next_action, "write tests by Friday");
        assert_eq!(c.state, CommitmentState::Drafted);
    }

    #[test]
    fn test_slug_generation() {
        let c = Commitment::new("Ship Feature X!");
        assert_eq!(c.slug(), "ship-feature-x");
    }

    #[test]
    fn test_transition_drafted_to_validated_ok() {
        let mut c = Commitment::new("test");
        c.execution_checklist.low_cost_validation = Some("prototype".into());
        c.stop_loss.time_hours = Some(10.0);
        assert!(c.transition(CommitmentState::Validated).is_ok());
        assert_eq!(c.state, CommitmentState::Validated);
    }

    #[test]
    fn test_transition_drafted_to_validated_fails_without_validation() {
        let mut c = Commitment::new("test");
        c.stop_loss.time_hours = Some(10.0);
        let err = c.transition(CommitmentState::Validated).unwrap_err();
        assert!(matches!(err, TransitionError::MissingValidation));
    }

    #[test]
    fn test_transition_validated_to_executing_requires_3_milestones() {
        let mut c = Commitment::new("test");
        c.execution_checklist.low_cost_validation = Some("proto".into());
        c.stop_loss.time_hours = Some(5.0);
        c.transition(CommitmentState::Validated).unwrap();

        c.add_milestone("m1", None);
        c.add_milestone("m2", None);
        let err = c.transition(CommitmentState::Executing).unwrap_err();
        assert!(matches!(err, TransitionError::InsufficientMilestones(2)));

        c.add_milestone("m3", None);
        assert!(c.transition(CommitmentState::Executing).is_ok());
    }

    #[test]
    fn test_trigger_stop_loss_sets_triggered_and_state() {
        let mut c = Commitment::new("test");
        c.trigger_stop_loss();
        assert!(c.stop_loss.triggered);
        assert!(c.stop_loss.triggered_at.is_some());
        assert_eq!(c.state, CommitmentState::Abandoned);
        assert!(c.closed_at.is_some());
    }

    #[test]
    fn test_add_milestone() {
        let mut c = Commitment::new("test");
        c.add_milestone("first step", Some(NaiveDate::from_ymd_opt(2026, 7, 1).unwrap()));
        assert_eq!(c.milestone_count(), 1);
        assert_eq!(c.milestones[0].description, "first step");
        assert!(!c.milestones[0].completed);
    }

    #[test]
    fn test_complete_milestone() {
        let mut c = Commitment::new("test");
        c.add_milestone("step 1", None);
        c.complete_milestone(0).unwrap();
        assert!(c.milestones[0].completed);
        assert!(c.milestones[0].completed_at.is_some());
    }

    #[test]
    fn test_complete_milestone_out_of_range() {
        let mut c = Commitment::new("test");
        assert!(c.complete_milestone(0).is_err());
    }

    #[test]
    fn test_milestone_count() {
        let mut c = Commitment::new("test");
        assert_eq!(c.milestone_count(), 0);
        c.add_milestone("a", None);
        c.add_milestone("b", None);
        assert_eq!(c.milestone_count(), 2);
    }

    #[test]
    fn test_is_overdue() {
        let mut c = Commitment::new("test");
        c.review_at = Some(Utc::now().date_naive() - chrono::Duration::days(1));
        assert!(c.is_overdue());

        c.state = CommitmentState::Completed;
        assert!(!c.is_overdue());
    }

    #[test]
    fn test_is_not_overdue_future_date() {
        let mut c = Commitment::new("test");
        c.review_at = Some(Utc::now().date_naive() + chrono::Duration::days(7));
        assert!(!c.is_overdue());
    }

    #[test]
    fn test_save_load_roundtrip() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("commitments");

        let mut c = Commitment::new("ship feature X");
        c.by_when = Some(NaiveDate::from_ymd_opt(2026, 8, 1).unwrap());
        c.review_at = Some(NaiveDate::from_ymd_opt(2026, 7, 15).unwrap());
        c.next_action = "write spec".to_string();
        c.add_milestone("m1", None);
        c.add_milestone("m2", None);
        c.add_milestone("m3", None);
        c.sustained_value_plan = Some("keep the prototype".to_string());

        let path = c.save(&dir).unwrap();
        assert!(path.exists());

        let loaded = Commitment::load(&path).unwrap();
        assert_eq!(loaded.what, "ship feature X");
        assert_eq!(loaded.state, CommitmentState::Drafted);
        assert_eq!(loaded.by_when, c.by_when);
        assert_eq!(loaded.review_at, c.review_at);
        assert_eq!(loaded.next_action, "write spec");
        assert_eq!(loaded.milestones.len(), 3);
        assert_eq!(
            loaded.sustained_value_plan,
            Some("keep the prototype".to_string())
        );
    }

    #[test]
    fn test_load_all() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("commitments");

        let c1 = Commitment::new("task one");
        let c2 = Commitment::new("task two");
        c1.save(&dir).unwrap();
        c2.save(&dir).unwrap();

        let all = Commitment::load_all(&dir).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_load_all_empty_dir() {
        let tmp = tempdir().unwrap();
        let all = Commitment::load_all(tmp.path()).unwrap();
        assert!(all.is_empty());
    }

    #[test]
    fn test_execution_checklist_completed_count() {
        let mut ec = ExecutionChecklist::default();
        assert_eq!(ec.completed_count(), 0);
        ec.low_cost_validation = Some("test".into());
        ec.detail_risks_identified = true;
        assert_eq!(ec.completed_count(), 2);
    }

    #[test]
    fn test_execution_checklist_is_complete() {
        let mut ec = ExecutionChecklist::default();
        assert!(!ec.is_complete());
        ec.low_cost_validation = Some("v".into());
        ec.detail_risks_identified = true;
        ec.avoid_perfect_decision = true;
        ec.milestone_feedback_planned = true;
        ec.retrospective_planned = true;
        ec.self_discipline_assessed = true;
        ec.stop_loss_committed = true;
        ec.sustained_value_plan = Some("v".into());
        assert!(ec.is_complete());
    }

    #[test]
    fn test_cannot_transition_from_completed() {
        let mut c = Commitment::new("test");
        c.state = CommitmentState::Completed;
        let err = c.transition(CommitmentState::Executing).unwrap_err();
        assert!(matches!(err, TransitionError::InvalidTransition { .. }));
    }

    #[test]
    fn test_to_markdown_format() {
        let c = Commitment::new("test commitment");
        let md = c.to_markdown();
        assert!(md.starts_with("---\n"));
        assert!(md.contains("state: drafted"));
        assert!(md.contains("# Commitment: test commitment"));
        assert!(md.contains("_(no milestones)_"));
    }

    #[test]
    fn test_display_state() {
        assert_eq!(CommitmentState::Drafted.to_string(), "drafted");
        assert_eq!(CommitmentState::Abandoned.to_string(), "abandoned");
    }

    #[test]
    fn test_from_markdown_roundtrip() {
        let mut c = Commitment::new("roundtrip test");
        c.execution_checklist.low_cost_validation = Some("proto".into());
        c.stop_loss.time_hours = Some(5.0);
        c.add_milestone("step 1", None);
        c.add_milestone("step 2", Some(NaiveDate::from_ymd_opt(2026, 8, 1).unwrap()));
        c.add_milestone("step 3", None);

        let md = c.to_markdown();
        let parsed = Commitment::from_markdown(&md).unwrap();
        assert_eq!(parsed.what, "roundtrip test");
        assert_eq!(parsed.milestones.len(), 3);
        assert_eq!(parsed.milestones[0].description, "step 1");
        assert!(!parsed.milestones[0].completed);
        assert_eq!(
            parsed.milestones[1].target_date,
            Some(NaiveDate::from_ymd_opt(2026, 8, 1).unwrap())
        );
    }

    #[test]
    fn test_transition_to_pivoted_requires_sustained_value() {
        let mut c = Commitment::new("test");
        c.execution_checklist.low_cost_validation = Some("proto".into());
        c.stop_loss.time_hours = Some(5.0);
        c.transition(CommitmentState::Validated).unwrap();
        for i in 0..3 {
            c.add_milestone(&format!("m{i}"), None);
        }
        c.transition(CommitmentState::Executing).unwrap();
        
        // Complete all milestones before transitioning to Reviewing
        for i in 0..3 {
            c.complete_milestone(i).unwrap();
        }
        c.transition(CommitmentState::Reviewing).unwrap();

        let err = c.transition(CommitmentState::Pivoted).unwrap_err();
        assert!(matches!(err, TransitionError::MissingSustainedValue));

        c.sustained_value_plan = Some("keep the learnings".into());
        assert!(c.transition(CommitmentState::Pivoted).is_ok());
    }

    #[test]
    fn test_transition_executing_to_reviewing_requires_completed_milestones() {
        let mut c = Commitment::new("test");
        c.execution_checklist.low_cost_validation = Some("proto".into());
        c.stop_loss.time_hours = Some(5.0);
        c.transition(CommitmentState::Validated).unwrap();
        for i in 0..3 {
            c.add_milestone(&format!("m{i}"), None);
        }
        c.transition(CommitmentState::Executing).unwrap();

        // Should fail because milestones are not completed
        let err = c.transition(CommitmentState::Reviewing).unwrap_err();
        assert!(matches!(err, TransitionError::MissingMilestoneDataFeedback));

        // Complete all milestones
        for i in 0..3 {
            c.complete_milestone(i).unwrap();
        }
        assert!(c.transition(CommitmentState::Reviewing).is_ok());
    }

    #[test]
    fn test_transition_reviewing_to_completed_requires_retrospective() {
        let mut c = Commitment::new("test");
        c.execution_checklist.low_cost_validation = Some("proto".into());
        c.stop_loss.time_hours = Some(5.0);
        c.transition(CommitmentState::Validated).unwrap();
        for i in 0..3 {
            c.add_milestone(&format!("m{i}"), None);
        }
        c.transition(CommitmentState::Executing).unwrap();
        for i in 0..3 {
            c.complete_milestone(i).unwrap();
        }
        c.transition(CommitmentState::Reviewing).unwrap();

        // Should fail because retrospective is not set
        let err = c.transition(CommitmentState::Completed).unwrap_err();
        assert!(matches!(err, TransitionError::MissingRetrospective));

        c.retrospective = Some("reflected on the project".into());
        assert!(c.transition(CommitmentState::Completed).is_ok());
    }

    #[test]
    fn test_transition_any_to_abandoned_requires_stop_loss_triggered() {
        let mut c = Commitment::new("test");
        c.execution_checklist.low_cost_validation = Some("proto".into());
        c.stop_loss.time_hours = Some(5.0);
        c.transition(CommitmentState::Validated).unwrap();
        for i in 0..3 {
            c.add_milestone(&format!("m{i}"), None);
        }
        c.transition(CommitmentState::Executing).unwrap();

        // Should fail because stop-loss is not triggered
        let err = c.transition(CommitmentState::Abandoned).unwrap_err();
        assert!(matches!(err, TransitionError::MissingStopLossExtraction));

        c.stop_loss.triggered = true;
        assert!(c.transition(CommitmentState::Abandoned).is_ok());
    }

    #[test]
    fn test_transition_executing_to_reviewing_fails_without_completed_at() {
        let mut c = Commitment::new("test");
        c.execution_checklist.low_cost_validation = Some("proto".into());
        c.stop_loss.time_hours = Some(5.0);
        c.transition(CommitmentState::Validated).unwrap();
        for i in 0..3 {
            c.add_milestone(&format!("m{i}"), None);
        }
        c.transition(CommitmentState::Executing).unwrap();

        let err = c.transition(CommitmentState::Reviewing).unwrap_err();
        assert!(matches!(err, TransitionError::MissingMilestoneDataFeedback));

        for i in 0..3 {
            c.complete_milestone(i).unwrap();
        }
        assert!(c.transition(CommitmentState::Reviewing).is_ok());
    }

    #[test]
    fn test_transition_reviewing_to_completed_fails_with_empty_retrospective() {
        let mut c = Commitment::new("test");
        c.execution_checklist.low_cost_validation = Some("proto".into());
        c.stop_loss.time_hours = Some(5.0);
        c.transition(CommitmentState::Validated).unwrap();
        for i in 0..3 {
            c.add_milestone(&format!("m{i}"), None);
        }
        c.transition(CommitmentState::Executing).unwrap();
        for i in 0..3 {
            c.complete_milestone(i).unwrap();
        }
        c.transition(CommitmentState::Reviewing).unwrap();

        c.retrospective = Some("".into());
        let err = c.transition(CommitmentState::Completed).unwrap_err();
        assert!(matches!(err, TransitionError::MissingRetrospective));

        c.retrospective = Some("   ".into());
        let err = c.transition(CommitmentState::Completed).unwrap_err();
        assert!(matches!(err, TransitionError::MissingRetrospective));

        c.retrospective = Some("what went wrong".into());
        assert!(c.transition(CommitmentState::Completed).is_ok());
    }

    #[test]
    fn test_transition_any_to_abandoned_succeeds_with_sustained_value_plan() {
        let mut c = Commitment::new("test");
        c.execution_checklist.low_cost_validation = Some("proto".into());
        c.stop_loss.time_hours = Some(5.0);
        c.transition(CommitmentState::Validated).unwrap();
        for i in 0..3 {
            c.add_milestone(&format!("m{i}"), None);
        }
        c.transition(CommitmentState::Executing).unwrap();

        c.sustained_value_plan = Some("preserve learnings as documentation".into());
        assert!(c.transition(CommitmentState::Abandoned).is_ok());
    }
}
