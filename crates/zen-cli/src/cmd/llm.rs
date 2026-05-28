use clap::Subcommand;
use colored::Colorize;
use tracing::debug;

use zen_core::config::load_config;
use zen_core::errors::ZenError;

// ---------------------------------------------------------------------------
// LLM subcommands (FR-003, FR-077)
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum LlmCommands {
    /// Show provider routing for a task type
    Route {
        /// Task type: entity_extraction, synthesis, consolidation, dispatch
        #[arg(short, long)]
        task: String,
    },
    /// Test LLM connectivity
    Test {
        /// Provider name to test
        #[arg(short, long)]
        provider: Option<String>,
    },
    /// List configured LLM providers
    Providers,
}

pub fn execute_command(operation: &LlmCommands) -> Result<(), ZenError> {
    match operation {
        LlmCommands::Route { task } => {
            debug!("routing task: {}", task);
            let config = load_config()?;

            let task_config = match task.as_str() {
                "entity_extraction" => config.agents.get("entity_extraction"),
                "synthesis" => config.agents.get("synthesis"),
                "contradiction_detection" => config.agents.get("contradiction_detection"),
                "dispatch" => config.agents.get("dispatch"),
                _ => None,
            };

            println!("{}", "🔀 LLM Route Selection".bold());
            println!("  Task: {}", task.cyan().bold());

            match task_config {
                Some(t) => {
                    let provider_name = t.provider.as_deref().unwrap_or("(none)");
                    let provider = config.providers.get(provider_name);
                    println!(
                        "  Provider: {}",
                        provider_name.green()
                    );
                    println!("  Model: {}", t.model.as_deref().unwrap_or("(none)").cyan());
                    if let Some(p) = provider {
                        println!(
                            "  Base URL: {}",
                            p.base_url.as_deref().unwrap_or("(none)").dimmed()
                        );
                    }
                },
                None => {
                    println!(
                        "  Using default: {}",
                        config
                            .default_provider
                            .as_deref()
                            .unwrap_or("(none)")
                            .yellow()
                    );
                },
            }
            println!(
                "{}",
                "  (Full sensitivity-aware routing deferred to zen-llm)".dimmed()
            );
            Ok(())
        },
        LlmCommands::Test { provider } => {
            let p = provider.as_deref().unwrap_or("ollama");
            debug!("testing LLM provider: {}", p);
            println!(
                "{} Testing connection to: {}",
                "🔌".bright_blue().bold(),
                p.cyan().bold()
            );
            println!(
                "  {}",
                "Service-layer LLM connectivity check deferred".dimmed()
            );
            Ok(())
        },
        LlmCommands::Providers => {
            debug!("listing LLM providers");
            let config = load_config()?;
            println!("{}", "📡 Configured LLM Providers".bold());

            let default = config.default_provider.as_deref().unwrap_or("ollama");
            println!("  Default: {}", default.green().bold());

            println!("\n  Providers:");
            for (name, provider) in &config.providers {
                println!(
                    "    {} (type: {}, model: {})",
                    name.cyan(),
                    provider.r#type.as_deref().unwrap_or("unknown"),
                    provider.default_model.as_deref().unwrap_or("default")
                );
            }

            println!("\n  Agent Tasks:");
            for (name, agent) in &config.agents {
                println!(
                    "    {} → {} ({})",
                    name.cyan(),
                    agent.provider.as_deref().unwrap_or("default"),
                    agent.model.as_deref().unwrap_or("default")
                );
            }
            println!(
                "{}",
                "  (Provider list from config — live discovery deferred)".dimmed()
            );
            Ok(())
        },
    }
}
