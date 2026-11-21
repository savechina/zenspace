use clap::Subcommand;
use colored::Colorize;
use tracing::debug;

use zen_core::config::load_config;
use zen_core::errors::ZenError;
use zen_provider::{DefaultRouter, LlmRouter};

#[derive(Subcommand)]
pub enum ProviderCommands {
    /// Show provider routing for a task type
    Route {
        /// Task type: notion_extraction, synthesis, consolidation, dispatch
        #[arg(short, long)]
        task: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Test provider connectivity
    Test {
        /// Provider name to test
        #[arg(short, long)]
        provider: Option<String>,
    },
    /// List configured providers
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

pub fn execute_command(operation: &ProviderCommands) -> Result<(), ZenError> {
    let config = load_config()?;
    let router = DefaultRouter::from_agentic(config);

    match operation {
        ProviderCommands::Route { task, json } => {
            debug!("routing task: {}", task);

            let providers = router.list_providers();
            let local_available = router.is_local_llm_available();

            if *json {
                let payload = serde_json::json!({
                    "task": task,
                    "providers": providers
                        .iter()
                        .map(|(name, model)| serde_json::json!({ "name": name, "model": model }))
                        .collect::<Vec<_>>(),
                    "local_llm_available": local_available,
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&payload)
                        .map_err(|e| ZenError::Message(format!("json: {e}")))?
                );
                return Ok(());
            }

            println!("{}", "🔀 Provider Route Selection".bold());
            println!("  Task: {}", task.cyan().bold());

            if providers.is_empty() {
                println!("  {}", "No providers configured".yellow());
            } else {
                println!("  Configured providers:");
                for (name, model) in &providers {
                    println!("    {} @ {}", name.green(), model.dimmed());
                }
            }

            println!(
                "  Local LLM: {}",
                if local_available {
                    "Available".green()
                } else {
                    "Unavailable".red()
                }
            );
            Ok(())
        }
        ProviderCommands::Test { provider } => {
            let p = provider.as_deref().unwrap_or("ollama");
            debug!("testing provider: {}", p);
            println!(
                "{} Testing connection to: {}",
                "🔌".bright_blue().bold(),
                p.cyan().bold()
            );

            if router.is_local_llm_available() {
                println!("  {}", "Local LLM is reachable".green());
            } else {
                println!("  {}", "Local LLM is not reachable".red());
                println!("  Ensure Ollama is running at http://localhost:11434");
            }
            Ok(())
        }
        ProviderCommands::List { json } => {
            debug!("listing providers");
            let providers = router.list_providers();
            let local_available = router.is_local_llm_available();

            if *json {
                let payload = serde_json::json!({
                    "providers": providers
                        .iter()
                        .map(|(name, model)| serde_json::json!({ "name": name, "model": model }))
                        .collect::<Vec<_>>(),
                    "local_llm_available": local_available,
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&payload)
                        .map_err(|e| ZenError::Message(format!("json: {e}")))?
                );
                return Ok(());
            }

            println!("{}", "📡 Configured Providers".bold());

            if providers.is_empty() {
                println!("  {}", "No providers configured".yellow());
                println!("  Edit ~/.zen/config.toml to add providers");
            } else {
                for (name, model) in &providers {
                    println!("  {}: {}", name.dimmed(), model.cyan());
                }
            }

            println!(
                "\n  Local LLM status: {}",
                if local_available {
                    "Connected".green()
                } else {
                    "Disconnected".red()
                }
            );
            Ok(())
        }
    }
}
