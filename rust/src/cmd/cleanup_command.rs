use clap::{Args, Subcommand};

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

///执行 Clean command run
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
        }
    }
}
