use clap::Subcommand;
use serde_json::json;

use zen_core::errors::ZenError;
use zen_service::cleanup_service;

#[derive(Subcommand)]
pub enum CleanupCommands {
    All {
        #[arg(long, default_value = "false")]
        json: bool,
    },
    Trash {
        #[arg(long, default_value = "false")]
        json: bool,
    },
    Cache {
        #[arg(long, default_value = "false")]
        json: bool,
    },
}

pub fn execute_command(operation: &CleanupCommands) -> Result<(), ZenError> {
    match operation {
        CleanupCommands::All { json } => {
            cleanup_service::clean_all()?;
            if *json {
                println!("{}", json!({
                    "status": "success",
                    "command": "clean_all"
                }));
            }
            Ok(())
        },
        CleanupCommands::Trash { json } => {
            cleanup_service::clean_trash()?;
            if *json {
                println!("{}", json!({
                    "status": "success",
                    "command": "clean_trash"
                }));
            }
            Ok(())
        },
        CleanupCommands::Cache { json } => {
            cleanup_service::clean_cache()?;
            if *json {
                println!("{}", json!({
                    "status": "success",
                    "command": "clean_cache"
                }));
            }
            Ok(())
        },
    }
}
