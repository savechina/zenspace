//! `zen admission` — trust-ledger review + accept-mode request surface
//! (007 T007/T013, FR-009).
//!
//! The ledger lives in `state.db` (migration 016). `list`/`show` are read-only
//! views; `mode` without an argument shows the effective accept mode and
//! policy-version hash, while `mode <value>` is a CONFIG-GUIDED request: it
//! validates the value and prints the exact `config.toml` edit. Config is a
//! 5-layer read-only surface (D22), so v1 deliberately does not write runtime
//! state from this process; the daemon reads the edit after `zen serve restart`.

use chrono::{DateTime, Utc};
use clap::Subcommand;
use colored::Colorize;

use zen_core::config::{AcceptMode, QqBotChannelConfig, load_config};
use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;
use zen_repo::{AdmissionRepo, DecisionRow, DispositionFilter, SqliteClient};

#[derive(Subcommand)]
pub enum AdmissionCommands {
    /// List trust-ledger decisions (would-accepts, rejections, unresolved, admitted, duplicates)
    List {
        /// Filter by disposition (would_accept | rejected | unresolved | admitted | duplicate)
        #[arg(long)]
        mode: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Maximum rows to show (newest first)
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
    /// Show one decision record in full why-when-what detail
    Show {
        /// Decision id
        id: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show the effective accept mode, or print the exact config.toml edit for a flip
    Mode {
        /// Requested mode: off | observe | auto (omit to show the current mode)
        value: Option<String>,
        /// Channel instance (v1 supports: qqbot)
        #[arg(long, default_value = "qqbot")]
        channel: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

pub async fn execute_command(operation: &AdmissionCommands) -> Result<(), ZenError> {
    match operation {
        AdmissionCommands::List { mode, json, limit } => list(mode.as_deref(), *json, *limit).await,
        AdmissionCommands::Show { id, json } => show(id, *json).await,
        AdmissionCommands::Mode {
            value,
            channel,
            json,
        } => mode(value.as_deref(), channel, *json).await,
    }
}

async fn open_ledger() -> Result<Option<SqliteClient>, ZenError> {
    let paths = ZenPaths::detect().map_err(|e| ZenError::Message(format!("path error: {e}")))?;
    let db = paths.data().join("state.db");
    if !db.exists() {
        return Ok(None);
    }
    SqliteClient::open(&db)
        .await
        .map(Some)
        .map_err(|e| ZenError::Message(format!("open state.db: {e}")))
}

async fn list(mode: Option<&str>, json: bool, limit: i64) -> Result<(), ZenError> {
    let Some(client) = open_ledger().await? else {
        if json {
            println!("[]");
        } else {
            println!("{}", "No admission decisions recorded yet.".yellow());
            println!(
                "  {}",
                "Tip: configure [channels.qqbot] — observe mode starts recording would-accepts."
                    .dimmed()
            );
        }
        return Ok(());
    };
    let repo = AdmissionRepo::new(&client);
    let rows = repo
        .list_decisions(
            DispositionFilter {
                disposition: mode.map(str::to_string),
                since: None,
            },
            limit.clamp(1, 1000),
        )
        .await
        .map_err(|e| ZenError::Message(format!("read admission decisions: {e}")))?;

    if json {
        let value = serde_json::Value::Array(rows.iter().map(row_json).collect());
        println!(
            "{}",
            serde_json::to_string_pretty(&value)
                .map_err(|e| ZenError::Message(format!("serialize decisions: {e}")))?
        );
        return Ok(());
    }
    if rows.is_empty() {
        println!("{}", "No matching admission decisions.".yellow());
        return Ok(());
    }
    println!("{}", "Admission decisions".bold());
    for row in &rows {
        let stamp = format_ts(row.created_at);
        let disposition = paint_disposition(&row.disposition);
        let rule = row.matched_rule.as_deref().unwrap_or("-");
        let scope = row.scope.as_deref().unwrap_or("-");
        println!(
            "  {}  {}  sender={}  thread={}  rule={}  scope={}  id={}",
            stamp.dimmed(),
            disposition,
            row.sender.cyan(),
            row.thread_key,
            rule,
            scope,
            row.id
        );
        if let Some(reason) = row.reason.as_deref() {
            println!("      reason: {reason}");
        }
    }
    Ok(())
}

async fn show(id: &str, json: bool) -> Result<(), ZenError> {
    let Some(client) = open_ledger().await? else {
        return Err(ZenError::Message(
            "no admission ledger yet — configure [channels.qqbot] and let a request arrive".into(),
        ));
    };
    let repo = AdmissionRepo::new(&client);
    let row = repo
        .get_decision(id)
        .await
        .map_err(|e| ZenError::Message(format!("read admission decision: {e}")))?
        .ok_or_else(|| ZenError::Message(format!("admission decision not found: {id}")))?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&row_json(&row))
                .map_err(|e| ZenError::Message(format!("serialize decision: {e}")))?
        );
        return Ok(());
    }
    for line in decision_detail_lines(&row) {
        println!("{line}");
    }
    Ok(())
}

async fn mode(value: Option<&str>, channel: &str, json: bool) -> Result<(), ZenError> {
    if channel != "qqbot" {
        return Err(ZenError::Message(format!(
            "unknown channel '{channel}' (v1 supports: qqbot)"
        )));
    }
    let config = load_config()?;
    let current = config
        .channels
        .qqbot
        .as_ref()
        .map(QqBotChannelConfig::accept_mode_or_default)
        .unwrap_or_default();
    let policy_version = config.admission_policy_version();
    let config_path = ZenPaths::detect()
        .map(|p| p.config_file())
        .map_err(|e| ZenError::Message(format!("path error: {e}")))?;

    let Some(requested) = value else {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "channel": channel,
                    "acceptMode": current.as_str(),
                    "policyVersion": policy_version,
                    "configPath": config_path.display().to_string(),
                    "source": "config.toml (5-layer read-only surface)",
                }))
                .map_err(|e| ZenError::Message(format!("serialize mode: {e}")))?
            );
        } else {
            println!("{}", "Admission accept mode".bold());
            println!("  channel        = {channel}");
            println!("  accept_mode    = {}", current.as_str().cyan());
            println!("  policy_version = {}", policy_version.dimmed());
            println!("  config         = {}", config_path.display());
        }
        return Ok(());
    };

    let requested = parse_requested_mode(requested)?;
    let open_tickets = open_ticket_count().await;
    let snippet = format!("[channels.qqbot]\naccept_mode = \"{}\"", requested.as_str());
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "channel": channel,
                "currentMode": current.as_str(),
                "requestedMode": requested.as_str(),
                "policyVersion": policy_version,
                "configPath": config_path.display().to_string(),
                "snippet": snippet,
                "effect": "future admissions only (FR-014)",
                "openTickets": open_tickets,
                "applied": false,
            }))
            .map_err(|e| ZenError::Message(format!("serialize mode: {e}")))?
        );
        return Ok(());
    }

    println!("{}", "Accept-mode change requested".bold());
    println!("  channel        = {channel}");
    println!("  current_mode   = {}", current.as_str().cyan());
    println!("  requested_mode = {}", requested.as_str().cyan());
    println!("  policy_version = {}", policy_version.dimmed());
    println!();
    println!("Runtime mode follows config.toml (5-layer read-only surface). Apply:");
    println!("  file: {}", config_path.display());
    println!("  {snippet}");
    println!();
    println!(
        "Effect: future admissions only — in-flight work continues under its granted mandate (FR-014)."
    );
    if open_tickets > 0 {
        println!(
            "Note: {open_tickets} open admission ticket(s); their work continues under its recorded mandate."
        );
    }
    println!("Then reload the channel: `zen serve restart`.");
    Ok(())
}

fn parse_requested_mode(raw: &str) -> Result<AcceptMode, ZenError> {
    match raw.trim().to_lowercase().as_str() {
        "off" => Ok(AcceptMode::Off),
        "observe" => Ok(AcceptMode::Observe),
        "auto" => Ok(AcceptMode::Auto),
        other => Err(ZenError::Message(format!(
            "invalid accept mode '{other}' (expected off | observe | auto)"
        ))),
    }
}

async fn open_ticket_count() -> i64 {
    let Ok(Some(client)) = open_ledger().await else {
        return 0;
    };
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM admission_tickets WHERE status = 'open'")
        .fetch_one(client.pool())
        .await
        .unwrap_or(0)
}

fn decision_detail_lines(row: &DecisionRow) -> Vec<String> {
    vec![
        format!("{}", "Admission decision".bold()),
        format!("  id             = {}", row.id),
        format!("  disposition    = {}", paint_disposition(&row.disposition)),
        format!("  created_at     = {}", format_ts(row.created_at)),
        format!("  source         = {}", row.source),
        format!(
            "  event_id       = {}",
            row.event_id.as_deref().unwrap_or("-")
        ),
        format!("  sender         = {}", row.sender),
        format!("  thread_key     = {}", row.thread_key),
        format!("  accept_mode    = {}", row.accept_mode),
        format!(
            "  strength       = {}",
            row.strength.as_deref().unwrap_or("-")
        ),
        format!(
            "  matched_rule   = {}",
            row.matched_rule.as_deref().unwrap_or("-")
        ),
        format!("  scope          = {}", row.scope.as_deref().unwrap_or("-")),
        format!("  policy_version = {}", row.policy_version),
        format!(
            "  reason         = {}",
            row.reason.as_deref().unwrap_or("-")
        ),
        format!("  content_hash   = {}", row.content_hash),
        format!("  content        = {}", row.content),
    ]
}

fn row_json(row: &DecisionRow) -> serde_json::Value {
    serde_json::json!({
        "id": row.id,
        "source": row.source,
        "eventId": row.event_id,
        "contentHash": row.content_hash,
        "sender": row.sender,
        "threadKey": row.thread_key,
        "content": row.content,
        "disposition": row.disposition,
        "matchedRule": row.matched_rule,
        "scope": row.scope,
        "acceptMode": row.accept_mode,
        "strength": row.strength,
        "policyVersion": row.policy_version,
        "reason": row.reason,
        "createdAt": row.created_at,
    })
}

fn paint_disposition(disposition: &str) -> String {
    let text = disposition.to_string();
    match disposition {
        "admitted" => text.green().to_string(),
        "would_accept" => text.yellow().to_string(),
        "rejected" => text.red().to_string(),
        "unresolved" => text.bright_red().to_string(),
        "duplicate" => text.dimmed().to_string(),
        _ => text,
    }
}

fn format_ts(secs: i64) -> String {
    DateTime::<Utc>::from_timestamp(secs, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| secs.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_row() -> DecisionRow {
        DecisionRow {
            id: "decision-1".into(),
            source: "qqbot".into(),
            event_id: Some("msg-1".into()),
            content_hash: "abc123".into(),
            sender: "owner-openid".into(),
            thread_key: "group_A".into(),
            content: "please summarize my inbox".into(),
            disposition: "admitted".into(),
            matched_rule: Some("allowlist".into()),
            scope: Some("sender".into()),
            accept_mode: "auto".into(),
            strength: Some("explicit".into()),
            policy_version: "pv-xyz".into(),
            reason: None,
            created_at: 1_700_000_000,
        }
    }

    /// T013/SC-002: `show` renders the complete why-when-what record from the
    /// row alone — sender, rule, scope, mode, policy version, timestamp, and
    /// the trigger content all appear.
    #[test]
    fn admission_show_renders_complete_why_when_what_record() {
        let rendered = decision_detail_lines(&sample_row()).join("\n");
        for needle in [
            "decision-1",
            "disposition    = admitted",
            "source         = qqbot",
            "event_id       = msg-1",
            "sender         = owner-openid",
            "thread_key     = group_A",
            "accept_mode    = auto",
            "strength       = explicit",
            "matched_rule   = allowlist",
            "scope          = sender",
            "policy_version = pv-xyz",
            "content        = please summarize my inbox",
            "2023-11-14",
        ] {
            assert!(
                rendered.contains(needle),
                "missing {needle:?} in:\n{rendered}"
            );
        }
    }

    #[test]
    fn admission_mode_validation_rejects_unknown_values() {
        assert_eq!(parse_requested_mode("AUTO").unwrap(), AcceptMode::Auto);
        assert_eq!(parse_requested_mode("off").unwrap(), AcceptMode::Off);
        assert!(parse_requested_mode("everything").is_err());
    }
}
