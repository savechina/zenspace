use std::io::BufRead;

use clap::Subcommand;
use serde_json::json;

use zen_core::errors::ZenError;
use zen_service::cleanup_service;

#[derive(Subcommand)]
pub enum CleanupCommands {
    All {
        #[arg(long, default_value = "false")]
        json: bool,
        /// Skip the destructive-action confirmation prompt (for scripts)
        #[arg(long, short = 'y', default_value = "false")]
        yes: bool,
    },
    Trash {
        #[arg(long, default_value = "false")]
        json: bool,
        /// Skip the destructive-action confirmation prompt (for scripts)
        #[arg(long, short = 'y', default_value = "false")]
        yes: bool,
    },
    Cache {
        #[arg(long, default_value = "false")]
        json: bool,
    },
}

/// Gate for destructive cleanup arms.
///
/// Functionality: blocks `zen clean all`/`zen clean trash` behind an explicit
/// y/N prompt unless `--yes` is passed.
/// User impact: these arms destroy data OUTSIDE zen's own directory (the macOS
/// system Trash via Finder, other applications' logs, IDE caches) — an
/// accidental invocation must not empty the user's Trash.
/// Default: prompt; EOF/non-interactive input aborts (scripts pass --yes).
/// Interaction: `--yes`/`-y` skips the prompt entirely.
fn confirm_destructive(yes: bool, actions: &str) -> bool {
    if yes {
        return true;
    }
    println!("⚠  zen clean is about to destroy data outside ~/.zen:");
    for line in actions.lines() {
        println!("  {line}");
    }
    print!("Proceed? [y/N] ");
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    let stdin = std::io::stdin();
    let _ = stdin.lock().read_line(&mut answer);
    answer.trim().eq_ignore_ascii_case("y")
}

pub fn execute_command(operation: &CleanupCommands) -> Result<(), ZenError> {
    match operation {
        CleanupCommands::All { json, yes } => {
            let ok = confirm_destructive(
                *yes,
                "• empty the macOS SYSTEM Trash (Finder \"empty trash\")\n• delete other applications' logs (~/Library/Logs/…, /var/log/*.log*)\n• clean IDE caches",
            );
            if !ok {
                println!("aborted — pass --yes to skip this prompt");
                return Ok(());
            }
            cleanup_service::clean_all()?;
            if *json {
                println!(
                    "{}",
                    json!({
                        "status": "success",
                        "command": "clean_all"
                    })
                );
            }
            Ok(())
        }
        CleanupCommands::Trash { json, yes } => {
            let ok = confirm_destructive(
                *yes,
                "• empty the macOS SYSTEM Trash (Finder \"empty trash\")",
            );
            if !ok {
                println!("aborted — pass --yes to skip this prompt");
                return Ok(());
            }
            cleanup_service::clean_trash()?;
            if *json {
                println!(
                    "{}",
                    json!({
                        "status": "success",
                        "command": "clean_trash"
                    })
                );
            }
            Ok(())
        }
        CleanupCommands::Cache { json } => {
            cleanup_service::clean_cache()?;
            if *json {
                println!(
                    "{}",
                    json!({
                        "status": "success",
                        "command": "clean_cache"
                    })
                );
            }
            Ok(())
        }
    }
}
