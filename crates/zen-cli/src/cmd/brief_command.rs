use clap::Subcommand;
use colored::Colorize;
use tracing::debug;

use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;
use zen_knowledge::brief::{generate_briefing, render_briefing_md, save_briefing};

#[derive(Subcommand)]
pub enum BriefCommands {
    /// Generate today's daily briefing
    Run,
}

pub fn execute_command(cmd: &BriefCommands) -> Result<(), ZenError> {
    match cmd {
        BriefCommands::Run => {
            debug!("generating daily briefing");

            let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;

            let briefing =
                generate_briefing(&paths).map_err(|e| ZenError::Message(e.to_string()))?;

            let summary = format_summary(&briefing);
            println!("{summary}");

            let md = render_briefing_md(&briefing);
            let saved_path =
                save_briefing(&paths, &md).map_err(|e| ZenError::Message(e.to_string()))?;

            println!(
                "\n{} Briefing saved to {}",
                "✓".green().bold(),
                saved_path.display().to_string().cyan()
            );

            Ok(())
        },
    }
}

fn format_summary(briefing: &zen_knowledge::brief::Briefing) -> String {
    let summary = format!(
        "Briefing — {}\n  Inbox: {} notes pending\n  Today: {} log entries\n  Habits: {}\n  Goals: {}\n  Contradictions: {}",
        briefing.date.format("%Y-%m-%d"),
        briefing.pending_notes.to_string().bold(),
        briefing.today_log_entries.to_string().bold(),
        format!("{} pending", briefing.pending_habits.len()).bold(),
        format!("{} active", briefing.active_goals.len()).bold(),
        briefing.contradictions.to_string().bold()
    );

    if !briefing.recommended_actions.is_empty() {
        let actions: String = briefing
            .recommended_actions
            .iter()
            .enumerate()
            .map(|(i, a)| format!("  {}. {}", i + 1, a))
            .collect::<Vec<_>>()
            .join("\n");
        format!("{summary}\n\nActions:\n{actions}")
    } else {
        summary
    }
}
