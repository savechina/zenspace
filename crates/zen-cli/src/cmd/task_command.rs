use clap::Subcommand;
use tracing::debug;

use zen_core::errors::ZenError;

#[derive(Subcommand)]
pub enum TaskCommands {
    /// List dispatched tasks
    List,
    /// Show details of a task
    Show {
        /// Task ID
        id: String,
    },
    /// Cancel a dispatched task
    Cancel {
        /// Task ID
        id: String,
    },
}

pub fn execute_command(cmd: &TaskCommands) -> Result<(), ZenError> {
    match cmd {
        TaskCommands::List => {
            debug!("task list");
            println!("No dispatched tasks");
            Ok(())
        },
        TaskCommands::Show { id } => {
            debug!("task show: id={}", id);
            println!("Task {}: status=Pending (stub)", id);
            Ok(())
        },
        TaskCommands::Cancel { id } => {
            debug!("task cancel: id={}", id);
            println!("Task {}: cancelled (stub)", id);
            Ok(())
        },
    }
}
