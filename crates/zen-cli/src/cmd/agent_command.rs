use clap::Subcommand;
use colored::Colorize;
use tracing::debug;

use zen_agents::{AgentRegistry, DefaultAgentRegistry};
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

pub fn execute_command(operation: &AgentCommands) -> Result<(), ZenError> {
    let registry = DefaultAgentRegistry::new();

    match operation {
        AgentCommands::List => {
            debug!("listing available agents");
            let agents = registry.list_all();
            println!("{}", "🤖 Available Agents".bold());
            for agent in &agents {
                let role = format!("{:?}", agent.role).to_lowercase();
                let caps = agent
                    .capabilities
                    .iter()
                    .map(|c| format!("{:?}", c).to_lowercase())
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("  {} ({})", agent.name.cyan().bold(), role.dimmed());
                println!("    Capabilities: {}", caps.dimmed());
            }
            println!(
                "\n  Total: {} agents registered",
                agents.len().to_string().green().bold()
            );
            Ok(())
        },
        AgentCommands::Select { name } => {
            debug!("selecting agent: {}", name);
            match registry.find_by_name(name) {
                Ok(agent) => {
                    println!(
                        "{} Selected agent: {}",
                        "✓".green().bold(),
                        agent.name.cyan().bold()
                    );
                    println!("  Role: {:?}", agent.role);
                    println!("  Sensitivity: {:?}", agent.max_sensitivity);
                    // TODO: Persist selection to session config
                },
                Err(e) => {
                    println!(
                        "{} Unknown agent '{}' — {}",
                        "✗".red().bold(),
                        name.bright_yellow(),
                        e
                    );
                },
            }
            Ok(())
        },
        AgentCommands::Configure { name, key, value } => {
            match (key, value) {
                (Some(k), Some(v)) => {
                    debug!("configuring agent {} with {}={}", name, k, v);
                    match registry.find_by_name(name) {
                        Ok(_) => {
                            println!(
                                "{} Configured {} → {}={}",
                                "✓".green().bold(),
                                name.cyan().bold(),
                                k.bright_white(),
                                v.dimmed()
                            );
                            // TODO: Persist config to ~/.zen/agents/<name>.toml
                        },
                        Err(_) => {
                            println!(
                                "{} Agent '{}' not found in registry",
                                "✗".red().bold(),
                                name
                            );
                        },
                    }
                },
                _ => match registry.find_by_name(name) {
                    Ok(agent) => {
                        println!(
                            "{} Agent config for '{}'",
                            "🔧".bright_green(),
                            name.cyan().bold()
                        );
                        println!("  Role: {:?}", agent.role);
                        println!("  Capabilities: {:?}", agent.capabilities);
                        println!("  LLM Preferences: {:?}", agent.llm_preferences);
                    },
                    Err(_) => {
                        println!("{} Agent '{}' not found", "✗".red().bold(), name);
                    },
                },
            }
            Ok(())
        },
    }
}
