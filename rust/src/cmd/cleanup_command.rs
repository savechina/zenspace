use clap::Subcommand;

use crate::{errors::ZenError, service::cleanup_service};

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
pub(crate) fn excute_command(operation: &CleanupCommands) -> Result<(), ZenError> {
    match operation {
        CleanupCommands::All => {
            cleanup_service::clean_all();
            Ok(())
        }
        CleanupCommands::Trash => {
            cleanup_service::clean_trash();
            Ok(())
        }

        CleanupCommands::Cache => {
            cleanup_service::clean_cache();
            Ok(())
        }
    }
}
