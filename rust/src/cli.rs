use clap::{Args, Parser, Subcommand};
// use dotenvy;

use crate::cmd::starter_command::{self, StarterCommands};
use crate::cmd::wps_command::{self, WpsCommands};

#[derive(Parser)]
#[command(author="JenYen", version, about="About zenspace utils", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Hello say Name
    Hello { name: String },

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

pub(crate) fn shell() {
    // CLI parse
    let cli = Cli::parse();

    match &cli.command {
        Commands::Hello { name } => {
            println!("hello to {}", name)
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
