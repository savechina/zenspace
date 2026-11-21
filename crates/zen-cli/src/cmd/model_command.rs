use clap::Subcommand;
use colored::Colorize;

use zen_core::config::load_config;
use zen_core::errors::ZenError;

#[derive(Subcommand)]
pub enum ModelCommands {
    /// List available models from all configured providers
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

pub fn execute_command(operation: &ModelCommands) -> Result<(), ZenError> {
    match operation {
        ModelCommands::List { json } => {
            let config = load_config()?;

            if *json {
                let payload: Vec<_> = config
                    .providers
                    .iter()
                    .map(|(provider_name, provider)| {
                        serde_json::json!({
                            "provider": provider_name,
                            "default_model": provider.default_model,
                            "models": provider
                                .models
                                .iter()
                                .map(|(model_id, entry)| {
                                    serde_json::json!({
                                        "id": model_id,
                                        "default": Some(model_id.as_str())
                                            == provider.default_model.as_deref(),
                                        "temperature": entry
                                            .options
                                            .as_ref()
                                            .and_then(|o| o.temperature),
                                        "max_tokens": entry
                                            .options
                                            .as_ref()
                                            .and_then(|o| o.max_tokens),
                                        "variants": entry.variants.keys().collect::<Vec<_>>(),
                                    })
                                })
                                .collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&payload)
                        .map_err(|e| ZenError::Message(format!("json: {e}")))?
                );
                return Ok(());
            }

            println!("{}", "Models".bold());

            for (provider_name, provider) in &config.providers {
                let default = provider.default_model.as_deref().unwrap_or("(none)");
                println!("  {} (default: {})", provider_name.cyan(), default.yellow());

                for (model_id, entry) in &provider.models {
                    let temp = entry
                        .options
                        .as_ref()
                        .and_then(|o| o.temperature)
                        .map(|t| format!("temp={t}"))
                        .unwrap_or_default();
                    let tokens = entry
                        .options
                        .as_ref()
                        .and_then(|o| o.max_tokens)
                        .map(|m| format!("max_tokens={m}"))
                        .unwrap_or_default();
                    let variant_count = entry.variants.len();
                    let variants = if variant_count > 0 {
                        let names: Vec<_> = entry.variants.keys().collect();
                        format!(
                            " [{} variants: {}]",
                            variant_count,
                            names
                                .iter()
                                .map(|s| s.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    } else {
                        String::new()
                    };

                    let marker = if Some(model_id.as_str()) == provider.default_model.as_deref() {
                        " *"
                    } else {
                        ""
                    };

                    println!(
                        "    {}{}  {}{}  {}{}",
                        model_id.bright_white(),
                        marker,
                        temp.dimmed(),
                        if temp.is_empty() && tokens.is_empty() {
                            ""
                        } else {
                            " "
                        },
                        tokens.dimmed(),
                        variants.dimmed(),
                    );
                }

                if provider.models.is_empty() {
                    println!("    {} (no model catalog, using default)", default.dimmed());
                }
            }

            Ok(())
        }
    }
}
