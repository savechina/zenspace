use std::env;

use crate::{model::starter::Project, service::starter_service};

use clap::{Args, Subcommand};
use include_dir::Dir;
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
        /// the arch type for template. it support: ddd, mvc .
        #[arg(short = 't', long, env, default_value = "ddd")]
        arch_type: String,
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
            arch_type,
        } => {
            println!(
                "Project: {} + {} + {} + {} ",
                project, group, package, arch_type
            );

            let project = Project {
                project_name: project.clone(),
                group_name: group.clone(),
                package_name: package.clone(),
                arch_type: arch_type.clone(),
            };

            //pwd current directory
            let current_dir = env::current_dir().unwrap();

            let output_root = current_dir.join("target");
            println!("Output: {}", output_root.display());

            starter_service::init(project, output_root);

            println!("Init project done");
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
