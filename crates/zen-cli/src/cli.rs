use clap::{Parser, Subcommand};

use tracing::debug;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _};

use zen_core::errors::ZenError;

use crate::cmd::agent_command::{self, AgentCommands};
use crate::cmd::audit_command::{self, AuditCommands};
use crate::cmd::auth_command::{self, AuthCommands};
use crate::cmd::brief_command::{self, BriefCommands};
use crate::cmd::chat_command::{self, ChatArgs};
use crate::cmd::cleanup_command::{self, CleanupCommands};
use crate::cmd::config_command::{self, ConfigCommands};
use crate::cmd::consolidate_command::{self, ConsolidateCommands};
use crate::cmd::graph_command::{self, GraphCommands};
use crate::cmd::ingest_command::{self, IngestCommands};
use crate::cmd::lint_command::{self, LintCommands};
use crate::cmd::note_command::{self, NoteCommands};
use crate::cmd::plugin_command::{self, PluginCommands};
use crate::cmd::provider_command::{self, ProviderCommands};
use crate::cmd::reindex_command::{self, ReindexCommands};
use crate::cmd::research_command::{self, ResearchCommands};
use crate::cmd::routine_command::{self, RoutineCommands};
use crate::cmd::search_command::{self, SearchCommands};
use crate::cmd::serve_command::{self, ServeCommands};
use crate::cmd::session_command::{self, SessionCommands};
use crate::cmd::similar_command::{self, SimilarCommands};
use crate::cmd::starter_command::{self, StarterCommands};
use crate::cmd::task_command::{self, TaskCommands};
use crate::cmd::workspace_command::{self, WorkspaceCommands};
use crate::cmd::wps_command::{self, WpsCommands};

#[derive(Parser)]
#[command(author = "JenYen", version, about = "About zenspace utils", long_about = None)]
#[command(propagate_version = false)]
struct Cli {
    #[command(flatten)]
    verbose: clap_verbosity_flag::Verbosity<clap_verbosity_flag::InfoLevel>,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    Hello {
        name: String,
    },
    Clean {
        #[command(subcommand)]
        operation: Option<CleanupCommands>,
        #[arg(short, long, action, default_value = "false")]
        dry_run: bool,
    },
    Chat {
        #[command(flatten)]
        args: ChatArgs,
    },
    Starter {
        #[command(subcommand)]
        operation: StarterCommands,
    },
    Wps {
        #[command(subcommand)]
        operation: WpsCommands,
    },
    Version,
    Session {
        #[command(subcommand)]
        operation: SessionCommands,
    },
    Serve {
        #[command(subcommand)]
        operation: ServeCommands,
    },
    Agent {
        #[command(subcommand)]
        operation: AgentCommands,
    },
    Workspace {
        #[command(subcommand)]
        operation: WorkspaceCommands,
        #[arg(short, long, action, default_value = "false")]
        dry_run: bool,
    },
    Config {
        #[command(subcommand)]
        operation: ConfigCommands,
    },
    Provider {
        #[command(subcommand)]
        operation: ProviderCommands,
    },
    Audit {
        #[command(subcommand)]
        operation: AuditCommands,
    },
    Note {
        #[command(subcommand)]
        operation: NoteCommands,
    },
    Search {
        #[command(subcommand)]
        operation: SearchCommands,
    },
    Similar {
        #[command(subcommand)]
        operation: SimilarCommands,
    },
    Graph {
        #[command(subcommand)]
        operation: GraphCommands,
    },
    Reindex {
        #[command(subcommand)]
        operation: ReindexCommands,
    },
    Research {
        #[command(subcommand)]
        operation: ResearchCommands,
    },
    Consolidate {
        #[command(subcommand)]
        operation: ConsolidateCommands,
    },
    Lint {
        #[command(subcommand)]
        operation: LintCommands,
    },
    Ingest {
        #[command(subcommand)]
        operation: IngestCommands,
    },
    Routine {
        #[command(subcommand)]
        operation: RoutineCommands,
    },
    Task {
        #[command(subcommand)]
        operation: TaskCommands,
    },
    Brief {
        #[command(subcommand)]
        operation: BriefCommands,
    },
    Plugin {
        #[command(subcommand)]
        operation: PluginCommands,
    },
    Auth {
        #[command(subcommand)]
        operation: AuthCommands,
    },
}

pub async fn shell() -> Result<(), ZenError> {
    let cli = Cli::parse();

    let is_tui = cli.command.is_none();

    let filter = EnvFilter::builder()
        .with_default_directive(cli.verbose.tracing_level_filter().into())
        .from_env()
        .unwrap();

    if is_tui {
        let log_dir = zen_core::paths::ZenPaths::detect()
            .map(|p| p.logs())
            .unwrap_or_else(|_| std::env::temp_dir().join("zen-logs"));
        std::fs::create_dir_all(&log_dir).ok();
        let log_path = log_dir.join("zen.log");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| ZenError::Message(format!("Failed to open log file: {}", e)))?;
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_timer(tracing_subscriber::fmt::time::LocalTime::new(
                        time::format_description::parse(
                            "[year]-[month]-[day] [hour]:[minute]:[second]",
                        )
                        .unwrap(),
                    ))
                    .with_ansi(false)
                    .with_writer(std::sync::Mutex::new(file)),
            )
            .with(filter)
            .init();
    } else {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_timer(tracing_subscriber::fmt::time::LocalTime::new(
                        time::format_description::parse(
                            "[year]-[month]-[day] [hour]:[minute]:[second]",
                        )
                        .unwrap(),
                    ))
                    .with_writer(std::io::stderr),
            )
            .with(filter)
            .init();
    }

    match &cli.command {
        None => {
            // TUI runs on main thread (ratatui requires it)
            crate::tui::run().map_err(|e| ZenError::Message(format!("TUI error: {}", e)))
        }

        Some(Commands::Hello { name }) => {
            debug!("hello :");
            println!("hello:\n{}", name);
            Ok(())
        }

        Some(Commands::Clean { operation, dry_run }) => {
            debug!("clean dry_run:{}", dry_run);
            let op = operation
                .as_ref()
                .unwrap_or(&CleanupCommands::Trash { json: false });
            cleanup_command::execute_command(op)?;
            Ok(())
        }
        Some(Commands::Chat { args }) => chat_command::execute_command(args).await,

        Some(Commands::Starter { operation }) => {
            starter_command::execute_command(operation)?;
            Ok(())
        }
        Some(Commands::Wps { operation }) => wps_command::execute_command(operation),
        Some(Commands::Version) => {
            let version = env!("CARGO_PKG_VERSION");
            println!("zen version: {}", version);
            Ok(())
        }
        Some(Commands::Session { operation }) => session_command::execute_command(operation),
        Some(Commands::Serve { operation }) => serve_command::execute_command(operation).await,
        Some(Commands::Agent { operation }) => agent_command::execute_command(operation),
        Some(Commands::Workspace { operation, dry_run }) => {
            debug!("workspace dry_run:{}", dry_run);
            workspace_command::execute_command(operation)?;
            Ok(())
        }
        Some(Commands::Config { operation }) => config_command::execute_command(operation),
        Some(Commands::Provider { operation }) => provider_command::execute_command(operation),
        Some(Commands::Audit { operation }) => audit_command::execute_command(operation),
        Some(Commands::Note { operation }) => note_command::execute_command(operation),
        Some(Commands::Search { operation }) => search_command::execute_command(operation),
        Some(Commands::Similar { operation }) => similar_command::execute_command(operation),
        Some(Commands::Graph { operation }) => graph_command::execute_command(operation),
        Some(Commands::Reindex { operation }) => reindex_command::execute_command(operation),
        Some(Commands::Research { operation }) => research_command::execute_command(operation),
        Some(Commands::Consolidate { operation }) => {
            consolidate_command::execute_command(operation)
        }
        Some(Commands::Lint { operation }) => lint_command::execute_command(operation),
        Some(Commands::Ingest { operation }) => ingest_command::execute_command(operation),
        Some(Commands::Task { operation }) => task_command::execute_command(operation),
        Some(Commands::Routine { operation }) => routine_command::execute_command(operation),
        Some(Commands::Brief { operation }) => brief_command::execute_command(operation),
        Some(Commands::Plugin { operation }) => plugin_command::execute_command(operation),
        Some(Commands::Auth { operation }) => auth_command::execute_command(operation),
    }
}
