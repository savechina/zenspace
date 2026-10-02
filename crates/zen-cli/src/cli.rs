use clap::{Parser, Subcommand};

use tracing::debug;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _};

use clap::ValueEnum;
use zen_core::errors::ZenError;
use zen_core::sandbox::SandboxMode;

#[derive(ValueEnum, Clone, Copy, PartialEq, Eq)]
pub enum SandboxModeArg {
    ReadOnly,
    WorkspaceWrite,
    Ask,
    DangerFullAccess,
}

impl From<SandboxModeArg> for SandboxMode {
    fn from(arg: SandboxModeArg) -> Self {
        match arg {
            SandboxModeArg::ReadOnly => SandboxMode::ReadOnly,
            SandboxModeArg::WorkspaceWrite => SandboxMode::WorkspaceWrite,
            SandboxModeArg::Ask => SandboxMode::Ask,
            SandboxModeArg::DangerFullAccess => SandboxMode::DangerFullAccess,
        }
    }
}

use crate::cmd::agent_command::{self, AgentCommands};
use crate::cmd::audit_command::{self, AuditCommands};
use crate::cmd::auth_command::{self, AuthCommands};
use crate::cmd::chat_command::{self, ChatArgs};
use crate::cmd::cleanup_command::{self, CleanupCommands};
use crate::cmd::config_command::{self, ConfigCommands};
use crate::cmd::discover_command::{self, DiscoverCommands};
use crate::cmd::doctor_command;
use crate::cmd::goal_command::{self, GoalCommands};
use crate::cmd::habit_command::{self, HabitCommands};
use crate::cmd::logs_command::{self, LogCommands};
use crate::cmd::model_command::{self, ModelCommands};
use crate::cmd::plugin_command::{self, PluginCommands};
use crate::cmd::provider_command::{self, ProviderCommands};
use crate::cmd::sandbox_command::{self, SandboxArgs};
use crate::cmd::serve_command::{self, ServeCommands};
use crate::cmd::session_command::{self, SessionCommands};
use crate::cmd::skill_command::{self, SkillCommands};
use crate::cmd::starter_command::{self, StarterCommands};
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

    #[arg(long, hide = true)]
    internal_sandbox_launcher: bool,

    #[arg(long, value_enum, default_value_t = SandboxModeArg::WorkspaceWrite)]
    sandbox: SandboxModeArg,

    #[arg(long)]
    ask_for_approval: Option<String>,
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
    Wiki {
        #[command(subcommand)]
        operation: WikiCommands,
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
    Habit {
        #[command(subcommand)]
        operation: HabitCommands,
    },
    Goal {
        #[command(subcommand)]
        operation: GoalCommands,
    },
    Skill {
        #[command(subcommand)]
        operation: SkillCommands,
    },
    Discover {
        #[command(subcommand)]
        operation: DiscoverCommands,
    },
    Sandbox {
        #[command(flatten)]
        args: SandboxArgs,
    },
    /// Run system health checks (8 liveness probes)
    Doctor {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

pub async fn shell() -> Result<(), ZenError> {
    let cli = Cli::parse();
    // Suppress tantivy file-watcher polling noise (warns every ~500ms when
    // a temp index directory lacks meta.json — harmless but noisy).
    // tantivy uses the `log` crate, not `tracing`, so we must also set
    // `log` via `env_logger`-style filter AND the `tracing` EnvFilter.
    let rust_log = std::env::var("RUST_LOG").unwrap_or_default();
    let suppress = "tantivy=off";
    let rust_log = if rust_log.is_empty() {
        format!("info,{suppress}")
    } else {
        format!("{rust_log},{suppress}")
    };
    // SAFETY: set_var is called early in main before any threads are spawned.
    unsafe { std::env::set_var("RUST_LOG", &rust_log) };

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,tantivy=off"));

    if cli.internal_sandbox_launcher {
        return zen_plugin::sandbox_launcher::run_sandbox_launcher().await;
    }

    if let Some(ref policy) = cli.ask_for_approval {
        // SAFETY: set_var is called early in main before any threads are spawned.
        unsafe { std::env::set_var("ZEN_ASK_FOR_APPROVAL", policy) };
    }

    if cli.command.is_none() {
        init_tracing(filter, true)?;
        let config = zen_core::config::load_config()
            .map_err(|e| ZenError::Message(format!("Config error: {}", e)))?;
        // Inline is the default; the alternate-screen full TUI stays
        // selectable for scenarios that need mouse capture / selection mode.
        // Truthy parsing: only non-empty values other than 0/false enable it.
        let fullscreen = std::env::var("ZEN_TUI_FULLSCREEN")
            .map(|v| {
                let v = v.trim();
                !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
            })
            .unwrap_or(false);
        let is_inline = !fullscreen;
        if is_inline {
            crate::tui::run_inline(config)
                .map_err(|e| ZenError::Message(format!("TUI error: {}", e)))
        } else {
            crate::tui::run(config).map_err(|e| ZenError::Message(format!("TUI error: {}", e)))
        }
    } else if let Some(cmd) = cli.command {
        init_tracing(filter, true)?;
        dispatch_command(cmd).await
    } else {
        unreachable!("clap parse: command is neither None nor Some")
    }
}

fn init_tracing(filter: EnvFilter, use_file: bool) -> Result<(), ZenError> {
    #[allow(deprecated)]
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
            let op = operation.as_ref().unwrap_or(&CleanupCommands::Trash {
                json: false,
                yes: false,
            });
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
        Commands::Logs {
            lines,
            level,
            follow,
            json,
            ref operation,
        } => match operation {
            Some(cmd) => logs_command::execute_command(cmd),
            None => logs_command::execute_show(lines, level.as_deref(), follow, json),
        },
        Commands::Wiki { ref operation } => wiki_command::execute_command(operation).await,
        Commands::Model { ref operation } => model_command::execute_command(operation),
        Commands::Plugin { ref operation } => plugin_command::execute_command(operation),
        Commands::Auth { ref operation } => auth_command::execute_command(operation),
        Commands::Habit { ref operation } => habit_command::execute_command(operation),
        Commands::Goal { ref operation } => goal_command::execute_command(operation),
        Commands::Skill { ref operation } => skill_command::execute_command(operation).await,
        Commands::Discover { ref operation } => discover_command::execute_command(operation).await,
        Commands::Sandbox { ref args } => sandbox_command::execute_command(args),
        Commands::Doctor { json } => doctor_command::execute_command(json),
    }
}

// ---------------------------------------------------------------------------
// Docs-consistency gate (2026-09-30 plan-eng-review, Open-A)
//
// PURPOSE: Fail the test gate when docs/src/cli-commands.md drifts from the
//          live clap surface — the guide rotted three times silently (wrong
//          command count, removed commands still taught, phantom keys).
// USAGE: Runs under `bin/test`/rust.yml with the rest of the suite; no CI
//        wiring changes (docs.yml only triggers on docs/**, so it can never
//        see the crate changes that cause drift — the rust gate can).
// EXPECTED: PASS while every subcommand is documented as `zen <name>` code
//           form and the header count claim matches the enum.
// ERRORS: A red test names the exact missing command or the drifted count —
//         fix the DOC (or the enum if the command was deliberately added).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod docs_consistency {
    use super::*;
    use clap::CommandFactory;

    const GUIDE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/src/cli-commands.md"
    );
    const AGENTS_MD: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../AGENTS.md");

    /// Marks where AGENTS.md stops describing the CURRENT surface and starts
    /// the dated historical record. Current-surface sections restate the live
    /// command count in three places (crate table, STRUCTURE tree, the COMMANDS
    /// heading); the history below the marker may legitimately quote retired
    /// counts (the T193 entry cites "29 commands"), so the count gate is scoped
    /// to the text above it.
    const AGENTS_HISTORY_MARKER: &str = "## Recent Changes";

    /// AGENTS.md section holding its authoritative command table. Coverage is
    /// scoped here rather than to the whole file: the historical notes discuss
    /// commands in backticked prose, so a whole-file scan is satisfied by a
    /// passing mention and deleting a table row would go unnoticed.
    const AGENTS_COMMAND_TABLE_SECTION: &str = "## COMMANDS (CLI)";

    fn read(path: &str) -> String {
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
    }

    fn agents_md_command_table(text: &str) -> &str {
        let start = text.find(AGENTS_COMMAND_TABLE_SECTION).unwrap_or_else(|| {
            panic!(
                "AGENTS.md no longer contains the '{AGENTS_COMMAND_TABLE_SECTION}' \
                     section this gate reads — move the marker or re-scope the gate"
            )
        }) + AGENTS_COMMAND_TABLE_SECTION.len();
        let body = &text[start..];
        let end = body.find("\n## ").unwrap_or(body.len());
        &body[..end]
    }

    fn subcommand_names() -> Vec<String> {
        // `help` is clap's auto-added builtin, not a Commands variant.
        Cli::command()
            .get_subcommands()
            .map(|s| s.get_name().to_string())
            .filter(|n| n != "help")
            .collect()
    }

    /// Shared coverage predicate: every live subcommand must appear in `text`
    /// as the code-form PREFIX `` `zen <name> `` (opening backtick anchored), so
    /// standalone rows (`zen chat`) and nested rows (`zen sandbox test` for
    /// `sandbox`) both match while prose without code ticks cannot
    /// false-positive.
    fn assert_documents_every_subcommand(label: &str, text: &str) {
        let missing: Vec<String> = subcommand_names()
            .into_iter()
            .filter(|name| !text.contains(&format!("`zen {name}")))
            .collect();
        assert!(
            missing.is_empty(),
            "{label} does not document {} as `zen <name>` — update the file (or \
             the Commands enum if the addition is intentional)",
            missing.join(", ")
        );
    }

    /// Numeric claims of the form "<N> commands" found in `text`: the
    /// whitespace-delimited token immediately preceding each lowercase
    /// "commands", with surrounding punctuation stripped so a parenthesised
    /// claim ("(23 commands") counts too. A token must be all digits once
    /// trimmed, which is what keeps prose out ("Nine manual commands") and what
    /// excludes the capitalised heading "Agentic Commands" and "N subcommands"
    /// (both leave a non-numeric token behind).
    fn numeric_count_claims(text: &str) -> Vec<u32> {
        text.split("commands")
            .filter_map(|chunk| chunk.split_whitespace().next_back())
            .filter_map(|token| {
                let digits = token.trim_matches(|c: char| !c.is_ascii_digit());
                if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                digits.parse::<u32>().ok()
            })
            .collect()
    }

    #[test]
    fn user_guide_documents_every_subcommand() {
        assert_documents_every_subcommand("docs/src/cli-commands.md", &read(GUIDE));
    }

    /// The guide's header count claim ("N subcommands") must equal the live
    /// variant count — the first historical rot class was exactly this drift.
    #[test]
    fn user_guide_subcommand_count_claim_matches() {
        let doc = read(GUIDE);
        let n = subcommand_names().len();
        assert!(
            doc.contains(&format!("{n} subcommands")),
            "docs/src/cli-commands.md does not claim '{n} subcommands' — the \
             count claim drifted (live Commands enum has {n} variants)"
        );
    }

    /// AGENTS.md carries its own command table and count claims, and was
    /// outside every rot gate: it kept claiming "20 commands" for three
    /// commands while the live enum grew to 23, and its table silently lost the
    /// `zen sandbox` row. The `c6067d9` gate reads the user guide only, so the
    /// same drift class recurred here unchecked. Coverage + count are both
    /// pinned below.
    #[test]
    fn agents_md_documents_every_subcommand() {
        let text = read(AGENTS_MD);
        assert_documents_every_subcommand(
            "AGENTS.md's command table (## COMMANDS (CLI))",
            agents_md_command_table(&text),
        );
    }

    #[test]
    fn agents_md_command_count_claims_match() {
        let text = read(AGENTS_MD);
        let end = text.find(AGENTS_HISTORY_MARKER).unwrap_or_else(|| {
            panic!(
                "AGENTS.md no longer contains the '{AGENTS_HISTORY_MARKER}' section \
                 this gate scopes the current-surface count claims to — move the marker \
                 or re-scope the gate"
            )
        });
        let (current, _history) = text.split_at(end);
        let n = subcommand_names().len() as u32;
        let stale: Vec<u32> = numeric_count_claims(current)
            .into_iter()
            .filter(|claimed| *claimed != n)
            .collect();
        assert!(
            stale.is_empty(),
            "AGENTS.md current-surface sections claim {stale:?} commands but the live \
             Commands enum has {n} — update every '<N> commands' claim above the \
             '{AGENTS_HISTORY_MARKER}' marker"
        );
    }
}
