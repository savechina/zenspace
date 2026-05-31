use clap::Subcommand;
use tracing::info;

use zen_core::errors::ZenError;

#[derive(Subcommand)]
pub enum RoutineCommands {
    /// List all routines
    List,
    /// Trigger a routine by name
    Trigger {
        /// Routine name
        name: String,
    },
    /// Enable a routine
    Enable {
        /// Routine name
        name: String,
    },
    /// Disable a routine
    Disable {
        /// Routine name
        name: String,
    },
}

pub fn execute_command(cmd: &RoutineCommands) -> Result<(), ZenError> {
    match cmd {
        RoutineCommands::List => {
            info!("routine list stub");
            println!("routine list (stub) - no routines registered yet");
            Ok(())
        }
        RoutineCommands::Trigger { name } => {
            info!(routine_trigger = name.as_str(), "routine trigger stub");
            println!("routine trigger stub: name=\"{}\"", name);
            Ok(())
        }
        RoutineCommands::Enable { name } => {
            info!(routine_enable = name.as_str(), "routine enable stub");
            println!("routine enable stub: name=\"{}\"", name);
            Ok(())
        }
        RoutineCommands::Disable { name } => {
            info!(routine_disable = name.as_str(), "routine disable stub");
            println!("routine disable stub: name=\"{}\"", name);
            Ok(())
        }
    }
}
