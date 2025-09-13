use crate::service::starter_service;

use clap::Subcommand;
use tracing::info;

#[derive(Subcommand)]
pub(crate) enum StarterCommands {
    /// Init project from template by project
    Init {
        /// The project name
        project: String,
        /// The group name
        group: String,
        /// The package name
        package: String,
    },

    /// Add project feature
    Add {
        /// The package name
        package: String,
        /// The table name
        table: String,
    },

    ///Initialize develop ENV to install develop tools.
    Develop,

    /// Workspace initialize
    Workspace,
}

///执行 clac command run
pub(crate) fn excute_command(operation: &StarterCommands) {
    match operation {
        StarterCommands::Init {
            project,
            group,
            package,
        } => {
            println!("{} + {} + {}", project, group, package);
            starter_service::init();
        }

        StarterCommands::Add { package, table } => {
            println!("Package: {} \t Table: {} ", package, table);
            starter_service::add();
        }

        StarterCommands::Develop => {
            starter_service::develop_tool();
        }

        StarterCommands::Workspace => {
            starter_service::workspace();
        }
    }
}
