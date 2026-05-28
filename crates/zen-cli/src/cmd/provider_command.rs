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
        /// Task type: entity_extraction, synthesis, consolidation, dispatch
        #[arg(short, long)]
        task: String,
    },
    /// Test provider connectivity
    Test {
        /// Provider name to test
        #[arg(short, long)]
        provider: Option<String>,
    },
    /// List configured providers
    List,
}

pub fn execute_command(operation: &ProviderCommands) -> Result<(), ZenError> {
    let config = load_config()?;
    let router = DefaultRouter::from_agentic(&config);

    match operation {
        ProviderCommands::Route { task } => {
            debug!("routing task: {}", task);
            println!("{}", "🔀 Provider Route Selection".bold());
            println!("  Task: {}", task.cyan().bold());

            let providers = router.list_providers();
            if providers.is_empty() {
                println!("  {}", "No providers configured".yellow());
            } else {
                println!("  Configured providers:");
                for (name, model) in &providers {
                    println!("    {} @ {}", name.green(), model.dimmed());
                }
            }

            let local_available = router.is_local_llm_available();
            println!(
                "  Local LLM: {}",
                if local_available {
                    "Available".green()
                } else {
                    "Unavailable".red()
                }
            );
            Ok(())
        },
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
        },
        ProviderCommands::List => {
            debug!("listing providers");
            let providers = router.list_providers();
            println!("{}", "📡 Configured Providers".bold());

            if providers.is_empty() {
                println!("  {}", "No providers configured".yellow());
                println!("  Edit ~/.zen/config.toml to add providers");
            } else {
                for (name, model) in &providers {
                    println!("  {}: {}", name.dimmed(), model.cyan());
                }
            }

            let local_available = router.is_local_llm_available();
            println!(
                "\n  Local LLM status: {}",
                if local_available {
                    "Connected".green()
                } else {
                    "Disconnected".red()
                }
            );
            Ok(())
        },
    }
}
