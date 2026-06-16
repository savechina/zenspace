use clap::Subcommand;
use colored::Colorize;
use tracing::debug;

use zen_core::errors::ZenError;
use zen_core::types::SessionStatus;

use crate::session::SessionOrchestrator;

// ---------------------------------------------------------------------------
// Session subcommands (FR-076, FR-078)
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum SessionCommands {
    /// Start a new session with an agent
    Start {
        /// Agent name to use
        #[arg(short, long, default_value = "default")]
        agent: String,
        /// Workspace path for the session
        #[arg(short, long)]
        workspace: Option<String>,
    },
    /// Show current session status
    Status,
    /// List all sessions
    List,
    /// Archive the current session
    Archive {
        /// Session ID (defaults to current session)
        #[arg(short, long)]
        session_id: Option<String>,
    },
}

pub fn execute_command(operation: &SessionCommands) -> Result<(), ZenError> {
    match operation {
        SessionCommands::Start { agent, workspace } => {
            let workspace_path = workspace.as_deref().unwrap_or(".");
            debug!(
                "starting session with agent: {} workspace: {}",
                agent, workspace_path
            );

            let orchestrator = SessionOrchestrator::new();
            let session = orchestrator
                .start_session_with_agent(workspace_path, agent)
                .map_err(|e| ZenError::Message(format!("failed to start session: {e}")))?;

            println!(
                "{} Session started: {}",
                "▶".green().bold(),
                session.id.cyan().bold()
            );
            println!("  Agent: {}", session.agent_name.dimmed());
            println!(
                "  Sensitivity: {}",
                format!("{:?}", session.sensitivity_policy).dimmed()
            );
            println!("  Workspace: {}", session.workspace.dimmed());

            Ok(())
        }
        SessionCommands::Status => {
            debug!("showing session status");
            println!("{}", "📋 Session Status".bold());
            println!("  Status: {}", "No active session".yellow());
            println!("  Sessions dir: {}", "not yet configured".dimmed());
            Ok(())
        }
        SessionCommands::List => {
            debug!("listing sessions");
            let orchestrator = SessionOrchestrator::new();
            let sessions = orchestrator
                .list_sessions()
                .map_err(|e| ZenError::Message(format!("failed to list sessions: {e}")))?;

            if sessions.is_empty() {
                println!("{}", "No sessions found".yellow());
            } else {
                println!("{} Sessions ({}):", "📋".bold(), sessions.len());
                for s in &sessions {
                    let status_color = match s.status {
                        SessionStatus::Active => "Active".green(),
                        SessionStatus::Compacted => "Compacted".yellow(),
                        SessionStatus::Completed => "Completed".blue(),
                        SessionStatus::Failed => "Failed".red(),
                        SessionStatus::Archived => "Archived".bright_black(),
                    };
                    println!(
                        "  {} — {} — {}",
                        s.id.dimmed(),
                        status_color.bold(),
                        s.workspace.dimmed()
                    );
                }
            }

            Ok(())
        }
        SessionCommands::Archive { session_id } => {
            let sid = session_id.as_deref().unwrap_or("current");
            debug!("archiving session: {}", sid);
            println!(
                "{} Archiving session: {}",
                "📦".bright_yellow(),
                sid.cyan().bold()
            );
            println!(
                "{}",
                "  (Service-layer session archival deferred to zen-service)".dimmed()
            );
            Ok(())
        }
    }
}
