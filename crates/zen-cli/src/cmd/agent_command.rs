use clap::Subcommand;
use colored::Colorize;
use serde::{Deserialize, Serialize};
use tracing::debug;

use zen_agents::{AgentRegistry, DefaultAgentRegistry};
use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;

#[derive(Serialize, Deserialize, Default)]
struct SessionSelection {
    selected_agent: Option<String>,
}

fn persist_selection(agent_name: &str) -> Result<(), ZenError> {
    let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
    let session_path = paths.global_root().join("session.json");
    let mut session = if session_path.exists() {
        std::fs::read_to_string(&session_path)
            .ok()
            .and_then(|s| serde_json::from_str::<SessionSelection>(&s).ok())
            .unwrap_or_default()
    } else {
        SessionSelection::default()
    };
    session.selected_agent = Some(agent_name.to_string());
    let json = serde_json::to_string_pretty(&session)
        .map_err(|e| ZenError::Message(format!("serialize session: {e}")))?;
    std::fs::write(&session_path, json)
        .map_err(|e| ZenError::Message(format!("write session.json: {e}")))
}

#[derive(Serialize, Deserialize)]
struct AgentConfigEntry {
    name: String,
    key: String,
    value: String,
    updated_at: String,
}

fn persist_config(agent_name: &str, key: &str, value: &str) -> Result<(), ZenError> {
    let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
    let agents_dir = paths.global_root().join("agents");
    std::fs::create_dir_all(&agents_dir)
        .map_err(|e| ZenError::Message(format!("mkdir {}: {e}", agents_dir.display())))?;

    let entry = AgentConfigEntry {
        name: agent_name.to_string(),
        key: key.to_string(),
        value: value.to_string(),
        updated_at: chrono::Utc::now().to_rfc3339(),
    };

    let toml_str = toml::to_string(&entry)
        .map_err(|e| ZenError::Message(format!("serialize agent config: {e}")))?;
    let path = agents_dir.join(format!("{agent_name}.toml"));
    std::fs::write(&path, toml_str)
        .map_err(|e| ZenError::Message(format!("write {}: {e}", path.display())))
}

// ---------------------------------------------------------------------------
// Agent subcommands (FR-001a, FR-028)
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum AgentCommands {
    /// List available agents
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
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
        AgentCommands::List { json } => {
            debug!("listing available agents (json={})", json);
            let agents = registry.list_all();
            if *json {
                let json_arr: Vec<serde_json::Value> = agents
                    .iter()
                    .map(|a| {
                        serde_json::json!({
                            "name": a.name,
                            "role": format!("{:?}", a.role),
                            "capabilities": a.capabilities.iter().map(|c| format!("{:?}", c).to_lowercase()).collect::<Vec<_>>(),
                            "max_sensitivity": format!("{:?}", a.max_sensitivity),
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&json_arr).unwrap_or_default());
            } else {
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
            }
            Ok(())
        }
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
                    if let Err(e) = persist_selection(name) {
                        eprintln!("  (warn: could not persist selection: {e})");
                    }
                }
                Err(e) => {
                    println!(
                        "{} Unknown agent '{}' — {}",
                        "✗".red().bold(),
                        name.bright_yellow(),
                        e
                    );
                }
            }
            Ok(())
        }
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
                            if let Err(e) = persist_config(name, k, v) {
                                eprintln!("  (warn: could not persist config: {e})");
                            }
                        }
                        Err(_) => {
                            println!(
                                "{} Agent '{}' not found in registry",
                                "✗".red().bold(),
                                name
                            );
                        }
                    }
                }
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
                    }
                    Err(_) => {
                        println!("{} Agent '{}' not found", "✗".red().bold(), name);
                    }
                },
            }
            Ok(())
        }
    }
}
