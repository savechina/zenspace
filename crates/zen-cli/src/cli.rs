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
use crate::cmd::logs_command::{self, LogCommands};
use crate::cmd::model_command::{self, ModelCommands};
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
use crate::cmd::wiki_command::{self, WikiCommands};
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
    Logs {
        /// Number of lines to display (default: 50)
        #[arg(short = 'n', long, default_value = "50")]
        lines: usize,
        /// Filter by sensitivity level (public, private, confidential)
        #[arg(short = 'l', long)]
        level: Option<String>,
        /// Follow log output in real time (like tail -f)
        #[arg(short = 'f', long)]
        follow: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Optional subcommand: agent, session, search
        #[command(subcommand)]
        operation: Option<LogCommands>,
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
    Wiki {
        #[command(subcommand)]
        operation: WikiCommands,
    },
    Brief {
        #[command(subcommand)]
        operation: BriefCommands,
    },
    Model {
        #[command(subcommand)]
        operation: ModelCommands,
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
    let filter = EnvFilter::builder()
        .with_default_directive(cli.verbose.tracing_level_filter().into())
        .from_env()
        .unwrap();

    if cli.command.is_none() {
        init_tracing(filter, true)?;
        let config = zen_core::config::load_config()
            .map_err(|e| ZenError::Message(format!("Config error: {}", e)))?;
        crate::tui::run(config).map_err(|e| ZenError::Message(format!("TUI error: {}", e)))
    } else if let Some(cmd) = cli.command {
        init_tracing(filter, false)?;
        dispatch_command(cmd).await
    } else {
        unreachable!("clap parse: command is neither None nor Some")
    }
}

fn init_tracing(filter: EnvFilter, use_file: bool) -> Result<(), ZenError> {
    let time_fmt =
        time::format_description::parse("[year]-[month]-[day] [hour]:[minute]:[second]").unwrap();

    let layer = tracing_subscriber::fmt::layer()
        .with_timer(tracing_subscriber::fmt::time::LocalTime::new(time_fmt))
        .with_ansi(false);

    if use_file {
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
            .with(layer.with_writer(std::sync::Mutex::new(file)))
            .with(filter)
            .init();
    } else {
        tracing_subscriber::registry()
            .with(layer.with_writer(std::io::stderr))
            .with(filter)
            .init();
    }
    Ok(())
}

async fn dispatch_command(command: Commands) -> Result<(), ZenError> {
    match command {
        Commands::Clean {
            ref operation,
            ref dry_run,
        } => {
            debug!("clean dry_run:{}", dry_run);
            let op = operation
                .as_ref()
                .unwrap_or(&CleanupCommands::Trash { json: false });
            cleanup_command::execute_command(op)?;
            Ok(())
        }
        Commands::Chat { ref args } => chat_command::execute_command(args).await,
        Commands::Starter { ref operation } => {
            starter_command::execute_command(operation)?;
            Ok(())
        }
        Commands::Wps { ref operation } => wps_command::execute_command(operation),
        Commands::Version => {
            println!("zen version: {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Commands::Session { ref operation } => session_command::execute_command(operation),
        Commands::Serve { ref operation } => serve_command::execute_command(operation).await,
        Commands::Agent { ref operation } => agent_command::execute_command(operation),
        Commands::Workspace {
            ref operation,
            ref dry_run,
        } => {
            debug!("workspace dry_run:{}", dry_run);
            workspace_command::execute_command(operation)?;
            Ok(())
        }
        Commands::Config { ref operation } => config_command::execute_command(operation),
        Commands::Provider { ref operation } => provider_command::execute_command(operation),
        Commands::Audit { ref operation } => audit_command::execute_command(operation),
        Commands::Note { ref operation } => note_command::execute_command(operation),
        Commands::Search { ref operation } => search_command::execute_command(operation),
        Commands::Similar { ref operation } => similar_command::execute_command(operation),
        Commands::Graph { ref operation } => graph_command::execute_command(operation),
        Commands::Reindex { ref operation } => reindex_command::execute_command(operation),
        Commands::Research { ref operation } => research_command::execute_command(operation),
        Commands::Consolidate { ref operation } => consolidate_command::execute_command(operation),
        Commands::Lint { ref operation } => lint_command::execute_command(operation),
        Commands::Logs { lines, level, follow, json, ref operation } => {
            match operation {
                Some(cmd) => logs_command::execute_command(cmd),
                None => logs_command::execute_show(lines, level.as_deref(), follow, json),
            }
        }
        Commands::Ingest { ref operation } => ingest_command::execute_command(operation),
        Commands::Task { ref operation } => task_command::execute_command(operation),
        Commands::Wiki { ref operation } => wiki_command::execute_command(operation),
        Commands::Routine { ref operation } => routine_command::execute_command(operation).await,
        Commands::Brief { ref operation } => brief_command::execute_command(operation),
        Commands::Model { ref operation } => model_command::execute_command(operation),
        Commands::Plugin { ref operation } => plugin_command::execute_command(operation),
        Commands::Auth { ref operation } => auth_command::execute_command(operation),
    }
}
