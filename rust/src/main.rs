mod cli;
mod commands;
mod config;

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(author="ren", version, about="about zenspace utils", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Hello say Name
    Hello { name: String },
    // /// Calc Two number
    // Calc {
    //     #[command(subcommand)]
    //     operation: CalcCommands,
    // },
    /// Zen version
    Version,
}

fn main() {
    println!("Hello, world!");
    let cli = Cli::parse();

    match &cli.command {
        Commands::Hello { name } => {
            println!("hello to {}", name)
        } // Commands::Calc { operation } => {
        //     excute_calc_command(operation);
        // }
        Commands::Version => {
            // Get the package version from Cargo.toml at compile time
            let version = env!("CARGO_PKG_VERSION");
            // Print the version to the console
            println!("zen version: {}", version);
        }
    }
}
