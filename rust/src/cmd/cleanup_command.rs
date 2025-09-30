use clap::{Args, Subcommand};
use include_dir::Dir;
use tracing_subscriber::filter::targets;

use crate::service::cleanup_service;

#[derive(Subcommand)]
pub(crate) enum CleanupCommands {
    /// Clean All,include trash, cache ,logs ,ide file etc.
    All,
    /// Clean System Trash
    Trash,
    /// Clean Cache data
    Cache,
}

///执行 clac command run
pub(crate) fn excute_command(operation: &CleanupCommands) {
    match operation {
        CleanupCommands::All => {
            cleanup_service::clean_all();
        }
        CleanupCommands::Trash => {
            cleanup_service::clean_trash();
        }

        CleanupCommands::Cache => {
            cleanup_service::clean_cache();
        } // None => {
          //     // This is the default behavior.
          //     // You can print a help message, run another subcommand, etc.
          //     println!("No subcommand provided. Running the default behavior.");
          //     // Example: run the "list" command by default
          //     run_default_list_command();
          // }
    }
}
