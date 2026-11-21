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

use crate::cmd::admission_command::{self, AdmissionCommands};
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
#[command(author = "JenYen", version, about = "Your personal AI agentic workspace — notes in, wiki out", long_about = None)]
#[command(propagate_version = false)]
struct Cli {
    #[command(flatten)]
    verbose: clap_verbosity_flag::Verbosity<clap_verbosity_flag::InfoLevel>,
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(long, hide = true)]
    internal_sandbox_launcher: bool,

    /// Sandbox mode for agent tool calls in this invocation
    /// (read-only, workspace-write, ask, danger-full-access)
    #[arg(long, value_enum, default_value_t = SandboxModeArg::WorkspaceWrite)]
    sandbox: SandboxModeArg,

    /// Approval policy for agent tool calls (e.g. "ask"); exported as
    /// ZEN_ASK_FOR_APPROVAL for this invocation
    #[arg(long)]
    ask_for_approval: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Cleanup: cache is ungated; all/trash prompt y/N (they destroy data
    /// outside ~/.zen)
    Clean {
        #[command(subcommand)]
        operation: Option<CleanupCommands>,
        #[arg(short, long, action, default_value = "false")]
        dry_run: bool,
    },
    /// Interactive LLM chat (non-TUI)
    Chat {
        #[command(flatten)]
        args: ChatArgs,
    },
    /// Generate project scaffolds from templates
    Starter {
        #[command(subcommand)]
        operation: StarterCommands,
    },
    /// Work-process utilities
    Wps {
        #[command(subcommand)]
        operation: WpsCommands,
    },
    /// Show version information
    Version,
    /// Agentic session lifecycle: start, status, list, archive
    Session {
        #[command(subcommand)]
        operation: SessionCommands,
    },
    /// Gateway daemon control: start, stop, restart, status, launchd install
    Serve {
        #[command(subcommand)]
        operation: ServeCommands,
    },
    /// List registered agents and their tiers
    Agent {
        #[command(subcommand)]
        operation: AgentCommands,
    },
    /// Inspect the inbound trust ledger and request channel accept-mode changes
    Admission {
        #[command(subcommand)]
        operation: AdmissionCommands,
    },
    /// Initialize or inspect the ~/.zen workspace structure
    Workspace {
        #[command(subcommand)]
        operation: WorkspaceCommands,
        #[arg(short, long, action, default_value = "false")]
        dry_run: bool,
    },
    /// Show the merged effective configuration (5-layer priority)
    Config {
        #[command(subcommand)]
        operation: ConfigCommands,
    },
    /// LLM provider management: list, route, connectivity test
    Provider {
        #[command(subcommand)]
        operation: ProviderCommands,
    },
    /// Audit log operations: per-session log, export, integrity verify
    Audit {
        #[command(subcommand)]
        operation: AuditCommands,
    },
    /// View structured logs: tail, filter by level, search
    Logs {
        /// Number of lines to display (default: 50)
        #[arg(short = 'n', long, default_value = "50")]
        lines: usize,
        /// Filter by sensitivity level (public, private, confidential)
        #[arg(short = 'l', long)]
        level: Option<String>,
        /// Show the freshest tail snapshot of the newest rotation (not a live tail)
        #[arg(short = 'f', long)]
        follow: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Optional subcommand: agent, session, search
        #[command(subcommand)]
        operation: Option<LogCommands>,
    },
    /// Wiki operations: list, show, reindex, lint, distill, loop, rollback
    Wiki {
        #[command(subcommand)]
        operation: WikiCommands,
    },
    /// List available models across configured providers
    Model {
        #[command(subcommand)]
        operation: ModelCommands,
    },
    /// Plugin management: list, install, enable, disable, rehash, tools
    Plugin {
        #[command(subcommand)]
        operation: PluginCommands,
    },
    /// Auth and keychain operations: list, login, logout, status
    Auth {
        #[command(subcommand)]
        operation: AuthCommands,
    },
    /// Habit tracking: check-in, list, stats, add, remove
    Habit {
        #[command(subcommand)]
        operation: HabitCommands,
    },
    /// Goal management: set, progress, list, complete
    Goal {
        #[command(subcommand)]
        operation: GoalCommands,
    },
    /// Skill learning surface: list, run, progress, show, precipitate, confirm
    Skill {
        #[command(subcommand)]
        operation: SkillCommands,
    },
    /// Self-learning gates: run, stage, queue, confirm, report, arena
    Discover {
        #[command(subcommand)]
        operation: DiscoverCommands,
    },
    /// Run a command under OS-level sandbox isolation; status, policy, test
    Sandbox {
        #[command(flatten)]
        args: SandboxArgs,
    },
    /// Run system health checks (11 liveness probes)
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
        init_tracing(filter, logs_to_file(&cmd))?;
        dispatch_command(cmd).await
    } else {
        unreachable!("clap parse: command is neither None nor Some")
    }
}

/// Whether this invocation should open the file log appender.
///
/// The appender enforces `max_log_files(3)` at construction, which makes
/// opening it a mutating operation. Diagnosis surfaces (`zen logs`,
/// `zen doctor`, `zen serve status`) must stay read-only and therefore log to
/// stderr; all other commands keep file logging.
fn logs_to_file(command: &Commands) -> bool {
    !matches!(
        command,
        Commands::Logs { .. }
            | Commands::Doctor { .. }
            | Commands::Serve {
                operation: ServeCommands::Status { .. },
            }
    )
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
        let appender = tracing_appender::rolling::RollingFileAppender::builder()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix("zen.log")
            .max_log_files(3)
            .build(&log_dir)
            .map_err(|e| ZenError::Message(format!("Failed to build log appender: {e}")))?;
        tracing_subscriber::registry()
            .with(layer.with_writer(appender))
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
        Commands::Admission { ref operation } => {
            admission_command::execute_command(operation).await
        }
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

#[cfg(test)]
mod dispatch_tracing_tests {
    use super::*;
    use clap::Parser;

    fn logs(args: bool) -> Commands {
        Commands::Logs {
            lines: 50,
            level: None,
            follow: args,
            json: false,
            operation: None,
        }
    }

    #[test]
    fn t249a_readers_never_open_the_pruning_file_appender() {
        assert!(!logs_to_file(&logs(false)), "zen logs must log to stderr");
        assert!(
            !logs_to_file(&Commands::Doctor { json: false }),
            "zen doctor must log to stderr"
        );
        assert!(
            !logs_to_file(&Commands::Serve {
                operation: ServeCommands::Status { json: false },
            }),
            "zen serve status must log to stderr"
        );
    }

    #[test]
    fn t249a_writers_keep_file_logging() {
        assert!(logs_to_file(&Commands::Version));
        assert!(logs_to_file(&Commands::Clean {
            operation: None,
            dry_run: false,
        }));
        assert!(logs_to_file(&Commands::Chat {
            args: ChatArgs::parse_from(["chat", "hello"]),
        }));
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

// ---------------------------------------------------------------------------
// Help-about sentinel (2026-11-21 plan-eng-review, F1)
//
// PURPOSE: Fail the test gate when a Commands variant ships without a clap
//          about line — `zen --help` described only 3/24 commands because
//          nothing pinned help-text quality (docs_consistency pins the docs
//          table against the registry, not the live about strings).
// USAGE: Runs under `bin/test` with the rest of the suite; behavioral via
//        clap introspection, no source parsing.
// EXPECTED: PASS while every subcommand renders a non-empty, non-placeholder
//           about and the top-level tagline is real.
// ERRORS: A red test names the exact command that went blank — add the `///`
//         one-liner (docs/src/cli-commands.md is the copy source).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod help_about_sentinel {
    use super::*;
    use clap::CommandFactory;

    /// Pre-backfill placeholder strings that must never return as abouts
    /// (receipts: cli.rs:55 tagline, SandboxArgs doc leak).
    const PLACEHOLDER_ABOUTS: [&str; 2] = [
        "About zenspace utils",
        "Command-line arguments for the `zen sandbox` subcommand",
    ];

    #[test]
    fn every_subcommand_carries_a_real_about() {
        let blank: Vec<String> = Cli::command()
            .get_subcommands()
            .filter(|s| s.get_name() != "help")
            .filter(|s| match s.get_about().map(|a| a.to_string()) {
                Some(about) => {
                    let t = about.trim();
                    t.is_empty() || PLACEHOLDER_ABOUTS.contains(&t)
                }
                None => true,
            })
            .map(|s| s.get_name().to_string())
            .collect();
        assert!(
            blank.is_empty(),
            "commands render blank or placeholder rows in `zen --help`: {} — add the \
             `///` one-liner (docs/src/cli-commands.md is the copy source)",
            blank.join(", ")
        );
    }

    #[test]
    fn top_level_about_is_not_the_placeholder() {
        let about = Cli::command()
            .get_about()
            .map(|a| a.to_string())
            .unwrap_or_default();
        let t = about.trim();
        assert!(
            !t.is_empty() && !PLACEHOLDER_ABOUTS.contains(&t),
            "top-level about is blank or the '{t}' placeholder — `zen --help` opens with it"
        );
    }
}

#[cfg(test)]
mod json_flags_parse {
    use super::*;

    // 2026-11-21 plan-eng-review T2: list/status/report-shaped subcommands
    // honor `--json` (human output stays the default). Parse-level contract
    // pin; the output-shape e2e pins live in crates/zen/tests/.
    #[test]
    fn readout_subcommands_parse_the_json_flag() {
        match Cli::try_parse_from(["zen", "session", "list", "--json"])
            .unwrap()
            .command
        {
            Some(Commands::Session {
                operation: SessionCommands::List { json },
            }) => assert!(json, "session list --json must parse to json=true"),
            _ => panic!("expected `zen session list --json` to parse"),
        }
        match Cli::try_parse_from(["zen", "session", "status", "--json"])
            .unwrap()
            .command
        {
            Some(Commands::Session {
                operation: SessionCommands::Status { json },
            }) => assert!(json, "session status --json must parse to json=true"),
            _ => panic!("expected `zen session status --json` to parse"),
        }
        match Cli::try_parse_from(["zen", "model", "list", "--json"])
            .unwrap()
            .command
        {
            Some(Commands::Model {
                operation: ModelCommands::List { json },
            }) => assert!(json, "model list --json must parse to json=true"),
            _ => panic!("expected `zen model list --json` to parse"),
        }
        match Cli::try_parse_from(["zen", "provider", "list", "--json"])
            .unwrap()
            .command
        {
            Some(Commands::Provider {
                operation: ProviderCommands::List { json },
            }) => assert!(json, "provider list --json must parse to json=true"),
            _ => panic!("expected `zen provider list --json` to parse"),
        }
        match Cli::try_parse_from(["zen", "provider", "route", "--task", "synthesis", "--json"])
            .unwrap()
            .command
        {
            Some(Commands::Provider {
                operation: ProviderCommands::Route { task: _, json },
            }) => assert!(json, "provider route --json must parse to json=true"),
            _ => panic!("expected `zen provider route --json` to parse"),
        }
        match Cli::try_parse_from(["zen", "config", "show", "--json"])
            .unwrap()
            .command
        {
            Some(Commands::Config {
                operation: ConfigCommands::Show { json },
            }) => assert!(json, "config show --json must parse to json=true"),
            _ => panic!("expected `zen config show --json` to parse"),
        }
        match Cli::try_parse_from(["zen", "habit", "list", "--json"])
            .unwrap()
            .command
        {
            Some(Commands::Habit {
                operation: HabitCommands::List { json },
            }) => assert!(json, "habit list --json must parse to json=true"),
            _ => panic!("expected `zen habit list --json` to parse"),
        }
        match Cli::try_parse_from(["zen", "habit", "stats", "reading", "--json"])
            .unwrap()
            .command
        {
            Some(Commands::Habit {
                operation:
                    HabitCommands::Stats {
                        name: _,
                        days: _,
                        json,
                    },
            }) => assert!(json, "habit stats --json must parse to json=true"),
            _ => panic!("expected `zen habit stats <name> --json` to parse"),
        }
        match Cli::try_parse_from(["zen", "goal", "list", "--json"])
            .unwrap()
            .command
        {
            Some(Commands::Goal {
                operation: GoalCommands::List { json },
            }) => assert!(json, "goal list --json must parse to json=true"),
            _ => panic!("expected `zen goal list --json` to parse"),
        }
        match Cli::try_parse_from(["zen", "auth", "list", "--json"])
            .unwrap()
            .command
        {
            Some(Commands::Auth {
                operation: AuthCommands::List { json },
            }) => assert!(json, "auth list --json must parse to json=true"),
            _ => panic!("expected `zen auth list --json` to parse"),
        }
        match Cli::try_parse_from(["zen", "auth", "status", "--json"])
            .unwrap()
            .command
        {
            Some(Commands::Auth {
                operation: AuthCommands::Status { json },
            }) => assert!(json, "auth status --json must parse to json=true"),
            _ => panic!("expected `zen auth status --json` to parse"),
        }
        match Cli::try_parse_from(["zen", "audit", "log", "--session", "s1", "--json"])
            .unwrap()
            .command
        {
            Some(Commands::Audit {
                operation: AuditCommands::Log { session: _, json },
            }) => assert!(json, "audit log --json must parse to json=true"),
            _ => panic!("expected `zen audit log --json` to parse"),
        }
        match Cli::try_parse_from(["zen", "plugin", "list", "--json"])
            .unwrap()
            .command
        {
            Some(Commands::Plugin {
                operation:
                    PluginCommands::List {
                        kind: _,
                        enabled: _,
                        json,
                    },
            }) => assert!(json, "plugin list --json must parse to json=true"),
            _ => panic!("expected `zen plugin list --json` to parse"),
        }
        match Cli::try_parse_from(["zen", "plugin", "tools", "--json"])
            .unwrap()
            .command
        {
            Some(Commands::Plugin {
                operation: PluginCommands::Tools { json },
            }) => assert!(json, "plugin tools --json must parse to json=true"),
            _ => panic!("expected `zen plugin tools --json` to parse"),
        }
        match Cli::try_parse_from(["zen", "discover", "queue", "--json"])
            .unwrap()
            .command
        {
            Some(Commands::Discover {
                operation: DiscoverCommands::Queue { json },
            }) => assert!(json, "discover queue --json must parse to json=true"),
            _ => panic!("expected `zen discover queue --json` to parse"),
        }
        match Cli::try_parse_from(["zen", "wiki", "list", "--json"])
            .unwrap()
            .command
        {
            Some(Commands::Wiki {
                operation: WikiCommands::List { json },
            }) => assert!(json, "wiki list --json must parse to json=true"),
            _ => panic!("expected `zen wiki list --json` to parse"),
        }
    }

    #[test]
    fn json_flag_absent_keeps_the_human_default() {
        match Cli::try_parse_from(["zen", "session", "list"])
            .unwrap()
            .command
        {
            Some(Commands::Session {
                operation: SessionCommands::List { json },
            }) => assert!(!json, "human output stays the default"),
            _ => panic!("expected `zen session list` to parse"),
        }
        match Cli::try_parse_from(["zen", "provider", "list"])
            .unwrap()
            .command
        {
            Some(Commands::Provider {
                operation: ProviderCommands::List { json },
            }) => assert!(!json, "human output stays the default"),
            _ => panic!("expected `zen provider list` to parse"),
        }
    }
}

#[cfg(test)]
mod discover_workflow_parse {
    use super::*;

    fn discover_operation(args: &[&str]) -> DiscoverCommands {
        let cli = Cli::try_parse_from(args).unwrap();
        match cli.command {
            Some(Commands::Discover { operation }) => operation,
            _ => panic!("expected a `discover` subcommand"),
        }
    }

    #[test]
    fn confirm_workflow_subcommand_parses_draft_name() {
        match discover_operation(&["zen", "discover", "confirm-workflow", "research-synthesis"]) {
            DiscoverCommands::ConfirmWorkflow { draft_name } => {
                assert_eq!(draft_name, "research-synthesis");
            }
            _ => panic!("expected DiscoverCommands::ConfirmWorkflow"),
        }
    }

    #[test]
    fn reject_workflow_subcommand_parses_draft_name() {
        match discover_operation(&["zen", "discover", "reject-workflow", "shape-deadbeef"]) {
            DiscoverCommands::RejectWorkflow { draft_name } => {
                assert_eq!(draft_name, "shape-deadbeef");
            }
            _ => panic!("expected DiscoverCommands::RejectWorkflow"),
        }
    }
}
