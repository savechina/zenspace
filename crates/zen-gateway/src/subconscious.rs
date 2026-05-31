use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use chrono::NaiveDate;
use tracing::{debug, info};

use zen_core::config::AgenticConfig;
use zen_core::paths::ZenPaths;
use zen_memory::daily_log::DailyLog;
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
    config: AgenticConfig,
}

impl SubconsciousTick {
    pub fn new(config: AgenticConfig) -> Result<Self> {
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
    _config: &AgenticConfig,
    date: NaiveDate,
) -> Result<Vec<MicroAction>> {
    let mut actions = Vec::new();

    let today_entries = DailyLog::read_entries(zen_paths, date)?;

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

    actions.push(MicroAction::Log(format!(
        "Tick complete at {date}: {} log entries, {pending_notes} inbox notes",
        today_entries.len()
    )));

    Ok(actions)
}

fn load_identity(zen_paths: &ZenPaths) -> Result<IdentityContext> {
    zen_memory::memory_service::load_all(zen_paths)
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
}
