use clap::Subcommand;
use colored::Colorize;
use tracing::debug;

use zen_core::config::load_config;
use zen_core::errors::ZenError;

// ---------------------------------------------------------------------------
// Config subcommands (FR-001b, FR-002)
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum ConfigCommands {
    /// Show merged configuration (priority order)
    Show,
    /// Open config editor
    Edit {
        /// Config layer to edit: workspace | global | embedded
        #[arg(short, long, default_value = "workspace")]
        layer: String,
    },
    /// Validate configuration structure
    Validate,
}

pub fn execute_command(operation: &ConfigCommands) -> Result<(), ZenError> {
    match operation {
        ConfigCommands::Show => {
            debug!("showing merged config");
            let config = load_config()?;
            println!("{}", "📋 Merged Configuration".bold());
            println!("\n{}", "─── LLM Settings ───".bright_white());
            println!(
                "  default_provider = {}",
                config
                    .default_provider
                    .as_deref()
                    .unwrap_or("(none)")
                    .cyan()
            );
            for (name, agent) in &config.agents {
                println!(
                    "  {}.provider = {}",
                    name,
                    agent.provider.as_deref().unwrap_or("(none)").cyan()
                );
                println!(
                    "  {}.model      = {}",
                    name,
                    agent.model.as_deref().unwrap_or("(none)").cyan()
                );
            }
            if let Some(ref bot) = config.qqbot {
                println!("\n{}", "─── QQ Bot ───".bright_white());
                println!(
                    "  app_id         = {}",
                    bot.app_id.as_deref().unwrap_or("(none)").cyan()
                );
                println!(
                    "  token_env      = {}",
                    bot.token_env.as_deref().unwrap_or("(none)").cyan()
                );
            }
            println!("\n{}", "─── Cron ───".bright_white());
            println!(
                "  consolidation_time = {}",
                config
                    .cron
                    .consolidation_time
                    .as_deref()
                    .unwrap_or("(none)")
                    .cyan()
            );
            println!(
                "  timezone         = {}",
                config.cron.timezone.as_deref().unwrap_or("(none)").cyan()
            );
            println!(
                "\n{}",
                "(Full config output uses pretty-printing — showing key fields)".dimmed()
            );
            Ok(())
        }
        ConfigCommands::Edit { layer } => {
            debug!("editing config layer: {}", layer);
            println!(
                "{} Opening config editor: {}",
                "📝".bright_magenta().bold(),
                layer.cyan().bold()
            );
            println!(
                "{}",
                "  (Config editor integration deferred to zen-service)".dimmed()
            );
            Ok(())
        }
        ConfigCommands::Validate => {
            debug!("validating config");
            match load_config() {
                Ok(_) => {
                    println!("{} Configuration is valid", "✓".green().bold());
                    Ok(())
                }
                Err(e) => {
                    println!("{} Configuration error: {}", "✗".red().bold(), e);
                    Err(e)
                }
            }
        }
    }
}
