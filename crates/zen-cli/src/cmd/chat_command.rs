use clap::Parser;
use colored::Colorize;
use tracing::debug;

use zen_core::errors::ZenError;
use zen_gateway::client::SurfaceClient;

#[derive(Parser)]
pub struct ChatArgs {
    /// Message to send to the agent
    message: String,
    /// Agent name (default: auto-routed)
    #[arg(long)]
    agent: Option<String>,
}

/// US3 gateway path (T022): the turn executes on the sole-owner daemon
/// via `session/turn`; knowledge context arrives via `knowledge/search`.
/// `ZEN_ASK_FOR_APPROVAL` has no client-side effect here — hosted turns
/// run under the daemon-side approval policy until Q3 routing lands
/// with US4 (T030). Journal persistence stays local (fire-and-forget).
pub async fn execute_command(args: &ChatArgs) -> Result<(), ZenError> {
    let ChatArgs { message, agent } = args;
    debug!("chat: {} (agent: {:?})", message, agent);

    if std::env::var("ZEN_ASK_FOR_APPROVAL").is_ok_and(|p| !p.is_empty()) {
        tracing::info!(
            "ZEN_ASK_FOR_APPROVAL set: hosted turns use the daemon-side approval policy \
             (Q3 routing lands with US4)"
        );
    }

    let surface = SurfaceClient::open_default("zen-chat", env!("CARGO_PKG_VERSION"))
        .await
        .map_err(|e| ZenError::Message(format!("gateway: {}", e)))?;

    let session_id = surface
        .ensure_session(None, agent.as_deref())
        .await
        .map_err(|e| ZenError::Message(format!("gateway: {}", e)))?;

    let knowledge = surface
        .search_knowledge(message, None, 5)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "knowledge/search failed — continuing without context");
            Vec::new()
        });
    if !knowledge.is_empty() {
        tracing::info!(
            count = knowledge.len(),
            "Knowledge context injected for CLI chat (gateway)"
        );
    }

    let agent_label = agent.clone().unwrap_or_else(|| "auto".to_string());
    println!("{} {}", "[Agent]".cyan().bold(), agent_label);
    println!("{}", surface.link_state().banner().dimmed());

    let result = surface
        .turn_with_recovery(&session_id, message, knowledge)
        .await;

    match result {
        Ok(response) => {
            println!("{response}");
            println!(
                "\n{} {} tokens",
                "\u{2713}".green().bold(),
                response.len() / 4
            );

            if let Ok(paths) = zen_core::paths::ZenPaths::detect() {
                let summary = format!(
                    "Chat with {agent_label} agent — {} tokens.",
                    response.len() / 4
                );
                tracing::debug!(agent = %agent_label, "writing daily log entry for CLI chat");
                if let Err(e) = zen_memory::journal::Journal::create_entry(&paths, &summary) {
                    tracing::warn!(error = %e, "failed to write daily journal entry for CLI chat");
                }
            }

            Ok(())
        }
        Err(e) => {
            eprintln!("{} {}", "Error:".red().bold(), e);
            Err(ZenError::Message(format!("Chat error: {}", e)))
        }
    }
}
