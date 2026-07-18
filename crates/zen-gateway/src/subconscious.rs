use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use chrono::{Duration, NaiveDate};
use tracing::{debug, info};

use zen_core::config::ZenConfig;
use zen_core::paths::ZenPaths;
use zen_memory::journal::Journal;
use zen_memory::memory_service::IdentityContext;

/// Micro-action type decided by the subconscious tick.
#[derive(Debug, Clone)]
pub enum MicroAction {
    Remind(String),
    Suggest(String),
    Log(String),
    Organize(String),
}

/// Subconscious tick handler — runs during idle periods to evaluate
/// workspace state and decide micro-actions.
pub struct SubconsciousTick {
    zen_paths: ZenPaths,
    config: ZenConfig,
}

impl SubconsciousTick {
    pub fn new(config: ZenConfig) -> Result<Self> {
        let zen_paths = ZenPaths::detect()?;
        Ok(Self { zen_paths, config })
    }

    /// Execute a single tick: read state, decide action, log result.
    pub fn tick(&self) -> Result<Vec<MicroAction>> {
        let actions = evaluate_tick(
            &self.zen_paths,
            &self.config,
            chrono::Utc::now().naive_utc().date(),
        )?;

        if !actions.is_empty() {
            append_subconscious_log(&self.zen_paths, &actions)?;
            info!("subconscious tick: {} actions decided", actions.len());
        }

        Ok(actions)
    }

    /// Returns the configured interval in seconds based on cron config.
    pub fn interval_secs(&self) -> u64 {
        self.config
            .cron
            .subconscious_interval_minutes
            .unwrap_or(5)
            .saturating_mul(60) as u64
    }
}

fn evaluate_tick(
    zen_paths: &ZenPaths,
    _config: &ZenConfig,
    date: NaiveDate,
) -> Result<Vec<MicroAction>> {
    let mut actions = Vec::new();

    let today_entries = Journal::read_entries(zen_paths, date)?;

    if today_entries.is_empty() {
        actions.push(MicroAction::Remind(
            "No log entries today. Consider recording your activities.".to_string(),
        ));
    }

    match load_identity(zen_paths) {
        Ok(ctx) if ctx.has_content() => {
            actions.push(MicroAction::Log(format!(
                "Identity loaded: {} files",
                ctx.file_count()
            )));
        }
        Ok(_) => {
            actions.push(MicroAction::Suggest(
                "No identity context found. Consider creating SOUL.md / MEMORY.md.".to_string(),
            ));
        }
        Err(e) => {
            actions.push(MicroAction::Log(format!("Identity load note: {e}")));
        }
    }

    let pending_notes = count_inbox_notes(zen_paths);
    if pending_notes > 0 {
        actions.push(MicroAction::Organize(format!(
            "{pending_notes} inbox notes are pending consolidation"
        )));
    }

    if let Some(reminder) = evaluate_soul_goals(zen_paths, date) {
        actions.push(reminder);
    }

    actions.push(MicroAction::Log(format!(
        "Tick complete at {date}: {} log entries, {pending_notes} inbox notes",
        today_entries.len()
    )));

    Ok(actions)
}

fn load_identity(zen_paths: &ZenPaths) -> Result<IdentityContext> {
    zen_memory::memory_service::load_all(zen_paths)
}

fn evaluate_soul_goals(zen_paths: &ZenPaths, date: NaiveDate) -> Option<MicroAction> {
    let soul_path = zen_paths.identity().join("SOUL.md");
    if !soul_path.exists() {
        return None;
    }

    let soul_content = std::fs::read_to_string(&soul_path).ok()?;
    let goals = parse_soul_goals(&soul_content);
    if goals.is_empty() {
        return None;
    }

    let recent_text = load_recent_journal_text(zen_paths, date, 7);
    if recent_text.is_empty() {
        return Some(MicroAction::Remind(format!(
            "Goals defined in SOUL.md but no journal entries in the last 7 days: {}",
            goals.join(", ")
        )));
    }

    let recent_lower = recent_text.to_lowercase();
    for goal in &goals {
        let goal_lower = goal.to_lowercase();
        let keywords: Vec<&str> = goal_lower
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|w| w.len() > 3)
            .collect();

        let match_count = keywords.iter().filter(|kw| recent_lower.contains(*kw)).count();
        if match_count == 0 {
            return Some(MicroAction::Remind(format!(
                "Goal misalignment: '{}' has no matching journal entries in the last 7 days",
                goal
            )));
        }
    }

    None
}

fn parse_soul_goals(content: &str) -> Vec<String> {
    let mut goals = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(goal) = trimmed
            .strip_prefix("- Goal:")
            .or_else(|| trimmed.strip_prefix("- Intention:"))
        {
            let goal = goal.trim().to_string();
            if !goal.is_empty() {
                goals.push(goal);
            }
        } else if trimmed.starts_with("# ") && !trimmed.starts_with("## ") {
            let heading = trimmed.trim_start_matches("# ").trim().to_string();
            if !heading.is_empty() && heading.len() > 3 {
                goals.push(heading);
            }
        }
    }
    goals
}

fn load_recent_journal_text(zen_paths: &ZenPaths, date: NaiveDate, days: u32) -> String {
    let mut text = String::new();
    for i in 0..days {
        let check_date = date - Duration::days(i as i64);
        if let Ok(entries) = Journal::read_entries(zen_paths, check_date) {
            for entry in &entries {
                text.push_str(&entry.content);
                text.push('\n');
            }
        }
    }
    text
}

fn count_inbox_notes(zen_paths: &ZenPaths) -> usize {
    let inbox = zen_paths.inbox();
    if !inbox.is_dir() {
        return 0;
    }
    std::fs::read_dir(&inbox)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.path()
                        .extension()
                        .is_some_and(|ext| ext == "md" || ext == "txt")
                })
                .count()
        })
        .unwrap_or(0)
}

fn subconscious_log_path(zen_paths: &ZenPaths) -> PathBuf {
    zen_paths.logs().join("subconscious.md")
}

fn append_subconscious_log(zen_paths: &ZenPaths, actions: &[MicroAction]) -> Result<()> {
    let path = subconscious_log_path(zen_paths);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let now = chrono::Utc::now().naive_utc();
    let header = format!(
        "\n## {} — Subconscious Tick\n\n",
        now.format("%Y-%m-%d %H:%M:%S")
    );

    let body: String = actions
        .iter()
        .map(|a| match a {
            MicroAction::Remind(msg) => format!("- **Remind**: {msg}"),
            MicroAction::Suggest(msg) => format!("- **Suggest**: {msg}"),
            MicroAction::Log(msg) => format!("- **Log**: {msg}"),
            MicroAction::Organize(msg) => format!("- **Organize**: {msg}"),
        })
        .collect::<Vec<_>>()
        .join("\n");

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(file, "{header}{body}")?;

    debug!("subconscious log appended to {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_micro_action_display() {
        let action = MicroAction::Remind("test".to_string());
        match action {
            MicroAction::Remind(msg) => assert_eq!(msg, "test"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_parse_soul_goals_goal_prefix() {
        let content = "# My Goals\n\n- Goal: Ship the feature by Friday\n- Intention: Learn Rust async";
        let goals = parse_soul_goals(content);
        assert_eq!(goals.len(), 3);
        assert!(goals.contains(&"My Goals".to_string()));
        assert!(goals.contains(&"Ship the feature by Friday".to_string()));
        assert!(goals.contains(&"Learn Rust async".to_string()));
    }

    #[test]
    fn test_parse_soul_goals_heading() {
        let content = "# Health and Fitness\n\nSome content here.";
        let goals = parse_soul_goals(content);
        assert_eq!(goals.len(), 1);
        assert!(goals.contains(&"Health and Fitness".to_string()));
    }

    #[test]
    fn test_parse_soul_goals_mixed() {
        let content = "# Overall Direction\n\n- Goal: Improve code quality\n- Intention: Write more tests";
        let goals = parse_soul_goals(content);
        assert_eq!(goals.len(), 3);
    }

    #[test]
    fn test_parse_soul_goals_empty() {
        let content = "Just some plain text without goals.";
        let goals = parse_soul_goals(content);
        assert!(goals.is_empty());
    }

    #[test]
    fn test_parse_soul_goals_skips_subheadings() {
        let content = "# Main Goal\n\n## Sub Heading\n\n- Goal: Specific task";
        let goals = parse_soul_goals(content);
        assert_eq!(goals.len(), 2);
        assert!(goals.contains(&"Main Goal".to_string()));
        assert!(goals.contains(&"Specific task".to_string()));
    }
}
