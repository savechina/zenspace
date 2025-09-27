use clap::{Args, Subcommand};
use include_dir::Dir;
use tracing_subscriber::filter::targets;

#[derive(Subcommand)]
pub(crate) enum StarterCommands {
    Trash,
    All,
    Cache,
}
