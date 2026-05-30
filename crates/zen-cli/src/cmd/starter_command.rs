use serde_json::json;
use zen_core::errors::ZenError;
use zen_service::starter_service;

use clap::Subcommand;

#[derive(Subcommand)]
pub enum StarterCommands {
    #[command(visible_aliases = ["tool", "dev"])]
    Develop {
        #[arg(long, default_value = "false")]
        json: bool,
    },

    #[command(visible_aliases = ["space", "s"])]
    Workspace {
        #[arg(long, default_value = "false")]
        json: bool,
    },
}

pub fn execute_command(operation: &StarterCommands) -> Result<(), ZenError> {
    match operation {
        StarterCommands::Develop { json } => {
            starter_service::develop_tool();
            if *json {
                println!(
                    "{}",
                    json!({
                        "status": "success",
                        "command": "develop"
                    })
                );
            }
            Ok(())
        },
        StarterCommands::Workspace { json } => {
            starter_service::workspace();
            if *json {
                println!(
                    "{}",
                    json!({
                        "status": "success",
                        "command": "workspace"
                    })
                );
            }
            Ok(())
        },
    }
}
