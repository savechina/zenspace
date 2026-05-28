use clap::Subcommand;
use colored::Colorize;
use tracing::debug;

use zen_core::errors::ZenError;

// ---------------------------------------------------------------------------
// Agent subcommands (FR-001a, FR-028)
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum AgentCommands {
    /// List available agents
    List,
    /// Select an agent for the current session
    Select {
        /// Agent name
        name: String,
    },
    /// Configure an agent
    Configure {
        /// Agent name to configure
        #[arg(short, long)]
        name: String,
        /// Configuration key to set (e.g. system_prompt)
        #[arg(short, long)]
        key: Option<String>,
        /// Configuration value to set
        #[arg(short, long)]
        value: Option<String>,
    },
}

/// Default agent definitions (stub — FR-028 AgentRegistry in zen-agents)
const DEFAULT_AGENTS: &[&str] = &["hermes", "metis", "codex", "hephaestus"];

pub fn execute_command(operation: &AgentCommands) -> Result<(), ZenError> {
    match operation {
        AgentCommands::List => {
            debug!("listing available agents");
            println!("{}", "🤖 Available Agents".bold());
            for agent in DEFAULT_AGENTS {
                let role = match *agent {
                    "hermes" => "  (messaging & chat)",
                    "metis" => "    (planning & reasoning)",
                    "codex" => "    (code generation)",
                    "hephaestus" => " (build & tooling)",
                    _ => "",
                };
                println!("  {} {}", agent.cyan().bold(), role.dimmed());
            }
            println!(
                "{}",
                "  (Agent registry powered by zen-agents — full definitions deferred)".dimmed()
            );
            Ok(())
        },
        AgentCommands::Select { name } => {
            debug!("selecting agent: {}", name);
            let known = DEFAULT_AGENTS.contains(&name.as_str());
            if known {
                println!(
                    "{} Selected agent: {}",
                    "✓".green().bold(),
                    name.cyan().bold()
                );
            } else {
                println!(
                    "{} Unknown agent '{}' — using custom agent definition",
                    "⚠".yellow().bold(),
                    name.bright_yellow()
                );
            }
            println!(
                "{}",
                "  (Agent context assembly deferred to zen-agents)".dimmed()
            );
            Ok(())
        },
        AgentCommands::Configure { name, key, value } => {
            match (key, value) {
                (Some(k), Some(v)) => {
                    debug!("configuring agent {} with {}={}", name, k, v);
                    println!(
                        "{} Configured {} → {}={}",
                        "✓".green().bold(),
                        name.cyan().bold(),
                        k.bright_white(),
                        v.dimmed()
                    );
                },
                _ => {
                    println!(
                        "{} Agent config for '{}'",
                        "🔧".bright_green(),
                        name.cyan().bold()
                    );
                    println!(
                        "{}",
                        "  (Use --key and --value to set config entries)".dimmed()
                    );
                },
            }
            println!(
                "{}",
                "  (Agent config persistence deferred to zen-agents)".dimmed()
            );
            Ok(())
        },
    }
}
