use clap::Parser;
use colored::Colorize;
use std::io::{self, Write};
use tracing::debug;

use zen_agents::AgentOrchestrator;
use zen_core::config::load_config;
use zen_core::constants::MEMVID_STORE_FILE;
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
            let mem_dir = paths.memory();
            std::fs::create_dir_all(&mem_dir).ok();
            let store_path = mem_dir.join(MEMVID_STORE_FILE);
            match AgentOrchestrator::new(router.clone()).with_memory(store_path) {
                Ok(o) => o,
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to init memory store for CLI chat, continuing without memory");
                    AgentOrchestrator::new(DefaultRouter::from_agentic(config))
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to detect Zen paths for CLI chat");
            AgentOrchestrator::new(router.clone())
        }
    };
    let mut session = SessionContext::new("default".to_string(), String::new());

    if let Some(name) = agent {
        session.agent_name = name.clone();
    }

    if let Ok(paths) = zen_core::paths::ZenPaths::detect() {
        use zen_vault::search::{SearchService, TierSelector};
        let service = SearchService::new(router.clone());
        let tier = TierSelector::select_tier(message);
        let mut seen = std::collections::HashSet::new();

        let db_path = paths.db().join("state.db");
        let client = match zen_repo::SqliteClient::open(&db_path).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to create database client for chat search");
                return Err(ZenError::Message(format!("Database error: {}", e)));
            }
        };

        for dir in [paths.inbox(), paths.wiki()] {
            if let Ok(results) = service.search(message, &dir, &client, Some(tier)).await {
                for r in results {
                    if seen.insert(r.file.clone()) {
                        session.knowledge.push(zen_core::types::RetrievedNote {
                            path: r.file.display().to_string(),
                            content: r.content,
                            sensitivity: zen_core::types::Sensitivity::Public,
                            relevance: 1.0,
                        });
                    }
                }
            }
        }

        if !session.knowledge.is_empty() {
            tracing::info!(
                count = session.knowledge.len(),
                "Knowledge context injected for CLI chat"
            );
        }
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

            if let Ok(paths) = zen_core::paths::ZenPaths::detect() {
                let summary = format!(
                    "Chat with {} agent — {} tokens.",
                    session.agent_name,
                    response.len() / 4
                );
                tracing::debug!(agent = %session.agent_name, "writing daily log entry for CLI chat");
                let _ = zen_memory::journal::Journal::create_entry(&paths, &summary);
            }

            Ok(())
        }
        Err(e) => {
            eprintln!("{} {}", "Error:".red().bold(), e);
            Err(ZenError::Message(format!("Chat error: {}", e)))
        }
    }
}
