use clap::Subcommand;
use serde_json::json;
use tracing::debug;

use zen_core::errors::ZenError;
use zen_service::wps_service;

#[derive(Subcommand)]
pub enum WpsCommands {
    Archive {
        from_dir: Option<String>,
        #[arg(long, default_value = "false")]
        json: bool,
    },
    #[command(visible_aliases = ["dot"])]
    Dotfiles {
        #[arg(short, long, default_value = "false")]
        restore: bool,
        #[arg(long, default_value = "false")]
        json: bool,
    },
    #[command(visible_aliases = ["time", "t", "timestamp"])]
    Unixtime {
        timestamp: Option<i64>,
        #[arg(short = 't', long, default_value = "s")]
        timeunit: String,
        #[arg(long, default_value = "false")]
        json: bool,
    },
}

pub fn execute_command(operation: &WpsCommands) -> Result<(), ZenError> {
    match operation {
        WpsCommands::Archive { from_dir, json } => {
            wps_service::archive(from_dir.clone(), None)?;
            if *json {
                println!(
                    "{}",
                    json!({
                        "status": "success",
                        "command": "archive",
                        "from_dir": from_dir
                    })
                );
            } else {
                println!(
                    "zstd compress directory: {:}  ",
                    from_dir.clone().unwrap_or(String::new())
                );
            }
            Ok(())
        }
        WpsCommands::Dotfiles { restore, json } => {
            wps_service::dotfiles(*restore)?;
            if *json {
                println!(
                    "{}",
                    json!({
                        "status": "success",
                        "command": "dotfiles",
                        "restore": restore
                    })
                );
            } else {
                println!("dotfiles restore: {}", restore);
            }
            Ok(())
        }
        WpsCommands::Unixtime {
            timestamp,
            timeunit,
            json,
        } => {
            debug!("{} - {}", (*timestamp).unwrap_or(-1), timeunit);
            wps_service::unixtime(*timestamp, timeunit.clone())?;
            if *json {
                println!(
                    "{}",
                    json!({
                        "status": "success",
                        "command": "unixtime",
                        "timestamp": timestamp,
                        "timeunit": timeunit
                    })
                );
            }
            Ok(())
        }
    }
}
