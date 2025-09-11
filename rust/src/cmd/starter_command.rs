use crate::service::starter_service;

use clap::Subcommand;
use tracing::info;

#[derive(Subcommand)]
pub(crate) enum StarterCommands {
    /// Start the server
    Init {
        /// The first number
        #[arg(default_value = "9001", short, long)]
        a: i32,
        /// The second number
        #[arg(default_value = "9001", short, long)]
        b: i32,
    },
    /// Stop the server
    Add,

    /// Develop Env and Tool initialize
    Develop,

    /// Workspace initialize
    Workspace,
}

///执行 clac command run
pub(crate) fn excute_command(operation: &StarterCommands) {
    match operation {
        StarterCommands::Init { a, b } => {
            info!("Starting Jento Server application...");
            info!("{} + {} = {}", a, b, a + b);

            starter_service::init();
        }

        StarterCommands::Add => {
            starter_service::add();
        }

        StarterCommands::Develop => {
            starter_service::develop();
        }

        StarterCommands::Workspace => {
            starter_service::workspace();
        }
    }
}
