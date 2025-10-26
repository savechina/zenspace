use clap::{Parser, Subcommand};
// use dotenvy;

use crate::cmd::cleanup_command::{self, CleanupCommands};
use crate::cmd::starter_command::{self, StarterCommands};
use crate::cmd::wps_command::{self, WpsCommands};

#[derive(Parser)]
#[command(author="JenYen", version, about="About zenspace utils", long_about = None)]
#[command(propagate_version = false)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

/// zen subcommand
#[derive(Subcommand)]
enum Commands {
    /// Hello say Name
    Hello { name: String },
    /// cleanup trash, cache, logs etc.
    Clean {
        #[command(subcommand)]
        operation: Option<CleanupCommands>,
        #[arg(short, long, action, default_value = "false")]
        dry_run: bool,
    },

    /// init project from template and add new feature to prjoct
    Starter {
        #[command(subcommand)]
        operation: StarterCommands,
    },
    /// Work process suite
    Wps {
        #[command(subcommand)]
        operation: WpsCommands,
    },

    /// Zen version
    Version,
}

/// Zen CLI command entry
pub(crate) fn shell() {
    // CLI parse
    let cli = Cli::parse();

    match &cli.command {
        Commands::Hello { name } => {
            println!("hello:\n{}", name)
        }

        Commands::Clean { operation, dry_run } => {
            let op = operation.as_ref().unwrap_or(&CleanupCommands::Trash);
            cleanup_command::excute_command(op);
        }

        Commands::Starter { operation } => {
            starter_command::excute_command(operation);
        }

        Commands::Wps { operation } => {
            wps_command::excute_command(operation);
        }
        Commands::Version => {
            // Get the package version from Cargo.toml at compile time
            let version = env!("CARGO_PKG_VERSION");
            // Print the version to the console
            println!("zen version: {}", version);
        }
    }
}
