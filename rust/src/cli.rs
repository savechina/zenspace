use clap::{Parser, Subcommand};

use tracing::{debug, info};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _};

// use dotenvy;

use crate::cmd::cleanup_command::{self, CleanupCommands};
use crate::cmd::starter_command::{self, StarterCommands};
use crate::cmd::wps_command::{self, WpsCommands};
use crate::errors::ZenError;

#[derive(Parser)]
#[command(author="JenYen", version, about="About zenspace utils", long_about = None)]
#[command(propagate_version = false)]
struct Cli {
    #[command(flatten)]
    verbose: clap_verbosity_flag::Verbosity<clap_verbosity_flag::InfoLevel>,
    #[command(subcommand)]
    command: Commands,
}

/// zen subcommand
#[derive(Subcommand)]
enum Commands {
    /// Hello say Name
    Hello { name: String },
    /// cleanup trash, cache, logs etc.
    Clean {
        #[command(subcommand)]
        operation: Option<CleanupCommands>,
        #[arg(short, long, action, default_value = "false")]
        dry_run: bool,
    },

    /// init project from template and add new feature to prjoct
    Starter {
        #[command(subcommand)]
        operation: StarterCommands,
    },
    /// Work process suite
    Wps {
        #[command(subcommand)]
        operation: WpsCommands,
    },

    /// Zen version
    Version,
}

/// Zen CLI command entry
pub(crate) fn shell() -> Result<(), ZenError> {
    // CLI parse
    let cli = Cli::parse();

    let filter = EnvFilter::builder()
        .with_default_directive(cli.verbose.tracing_level_filter().into())
        .from_env()
        .unwrap();

    let reg = tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().without_time())
        .with(filter);

    reg.init();

    match &cli.command {
        Commands::Hello { name } => {
            debug!("hello :");
            println!("hello:\n{}", name);
            Ok(())
        }

        Commands::Clean { operation, dry_run } => {
            let op = operation.as_ref().unwrap_or(&CleanupCommands::Trash);
            cleanup_command::excute_command(op)?;
            Ok(())
        }

        Commands::Starter { operation } => {
            starter_command::excute_command(operation)?;
            Ok(())
        }
        Commands::Wps { operation } => wps_command::excute_command(operation),
        Commands::Version => {
            // Get the package version from Cargo.toml at compile time
            let version = env!("CARGO_PKG_VERSION");
            // Print the version to the console
            println!("zen version: {}", version);
            Ok(())
        }
    }
}
