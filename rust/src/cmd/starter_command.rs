use std::env;

use crate::{model::starter_model::Project, service::starter_service};

use clap::{Args, Subcommand};
use include_dir::Dir;

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
        ///The feature Name
        feature_name: String,
        /// The table name
        table_name: String,
        /// The project name
        #[arg(short = 'n', long, env)]
        project_name: String,
        /// The group name
        #[arg(short, long, env)]
        group_name: String,
        /// The package name
        #[arg(short, long, env)]
        package_name: String,
        /// the arch type for template. it support: ddd, mvc .
        #[arg(short = 't', long, env, default_value = "ddd")]
        arch_type: String,
    },

    ///Initialize develop ENV to install develop tools.
    #[command(visible_aliases = ["tool","dev"] )]
    Develop,

    /// Workspace initialize.
    #[command(visible_aliases = ["space","s"] )]
    Workspace,
}

///执行 clac command run
pub(crate) fn excute_command(operation: &StarterCommands) {
    match operation {
        StarterCommands::Init {
            project: project_name,
            group: group_name,
            package: package_name,
            arch_type,
        } => {
            println!(
                "Project: {} + {} + {} + {} ",
                project_name, group_name, package_name, arch_type
            );

            let project = Project {
                project_name: project_name.clone(),
                group_name: group_name.clone(),
                package_name: package_name.clone(),
                arch_type: arch_type.clone(),
            };

            //pwd current directory
            let current_dir = env::current_dir().unwrap();

            let output_root = current_dir;
            println!("Output: {}", output_root.display());

            starter_service::init_project(project, output_root);

            println!("Init project done");
        }

        StarterCommands::Add {
            feature_name,
            table_name,
            project_name,
            group_name,
            package_name,
            arch_type,
        } => {
            println!("{} ", "Develop add feature:");
            println!("Package: {} \t Table: {} ", feature_name, table_name);

            let project = Project::builder()
                .project_name(project_name.clone())
                .group_name(group_name.clone())
                .package_name(package_name.clone())
                .arch_type(arch_type.clone())
                .build();

            starter_service::add_feature(feature_name.clone(), table_name.clone(), project);
        }

        StarterCommands::Develop => {
            starter_service::develop_tool();
        }

        StarterCommands::Workspace => {
            starter_service::workspace();
        }
    }
}
