use clap::Parser;
use colored::Colorize;
use std::io::{self, Write};
use tracing::debug;

use zen_agents::AgentOrchestrator;
use zen_core::config::load_config;
use zen_core::errors::ZenError;
use zen_core::types::SessionContext;
use zen_provider::DefaultRouter;

#[derive(Parser)]
pub struct ChatArgs {
    /// Message to send to the agent
    message: String,
    /// Agent name (default: auto-routed)
    #[arg(long)]
    agent: Option<String>,
}

pub async fn execute_command(args: &ChatArgs) -> Result<(), ZenError> {
    let ChatArgs { message, agent } = args;
    debug!("chat: {} (agent: {:?})", message, agent);

    let config = load_config().map_err(|e| ZenError::Message(format!("Config error: {}", e)))?;

    let router = DefaultRouter::from_agentic(config);
    let orchestrator = match zen_core::paths::ZenPaths::detect() {
        Ok(paths) => {
            let memvid_path = paths.memvid_dir();
            std::fs::create_dir_all(&memvid_path).ok();
            match AgentOrchestrator::new(router).with_memory(memvid_path) {
                Ok(o) => o,
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to init memvid for CLI chat, continuing without memory");
                    AgentOrchestrator::new(DefaultRouter::from_agentic(config))
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to detect Zen paths for CLI chat");
            AgentOrchestrator::new(router)
        }
    };
    let mut session = SessionContext::new("default".to_string(), String::new());

    if let Some(name) = agent {
        session.agent_name = name.clone();
    }

    let agent_label = session.agent_name.clone();
    println!("{} {}", "[Agent]".cyan().bold(), agent_label);
    print!("\x1b[?25l");
    io::stdout().flush().ok();

    let result = orchestrator
        .execute_stream(&mut session, message, |token| {
            print!("{}", token);
            io::stdout().flush().ok();
        })
        .await;

    print!("\x1b[?25h");
    println!();

    match result {
        Ok(response) => {
            println!(
                "\n{} {} tokens",
                "\u{2713}".green().bold(),
                response.len() / 4
            );
            Ok(())
        }
        Err(e) => {
            eprintln!("{} {}", "Error:".red().bold(), e);
            Err(ZenError::Message(format!("Chat error: {}", e)))
        }
    }
}
