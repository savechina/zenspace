use clap::{Args, Parser, Subcommand};
use dotenvy;

use crate::cmd::starter_command::{self, StarterCommands};

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

    /// Zen version
    Version,
}

pub(crate) fn shell() {
    println!("Hello, world!");
    let cli = Cli::parse();

    match &cli.command {
        Commands::Hello { name } => {
            println!("hello to {}", name)
        }

        Commands::Starter { operation } => {
            starter_command::excute_command(operation);
        }

        Commands::Version => {
            // Get the package version from Cargo.toml at compile time
            let version = env!("CARGO_PKG_VERSION");
            // Print the version to the console
            println!("zen version: {}", version);
        }
    }
}
