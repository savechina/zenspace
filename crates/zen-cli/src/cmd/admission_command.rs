//! `zen admission` — trust-ledger review + accept-mode control surface
//! (007 T007/T013/T021, FR-009).
//!
//! The ledger lives in `state.db` (migration 016). `list`/`show` are read-only
//! views; `mode` without an argument shows the effective accept mode and
//! policy-version hash, while `mode <value>` flips the RUNTIME override row
//! (migration 017) directly in `state.db` — the daemon re-resolves it on the
//! next inbound message, no restart. `--clear` removes the row (config is a
//! 5-layer read-only surface, D22; the override is the hot path on top of it).
//! Write paths refuse when `state.db` is absent — a mode flip without a
//! running daemon has nothing to override (mirror of the D3 RPC-model rule).

use chrono::{DateTime, Utc};
use clap::Subcommand;
use colored::Colorize;

use zen_core::admission::resolve_effective_accept_mode;
use zen_core::config::{AcceptMode, admission_policy_version_for_mode, load_config};
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
    /// Show the effective accept mode, or flip it at runtime (state.db override row)
    Mode {
        /// Requested mode: off | observe | auto (omit to show the current mode)
        value: Option<String>,
        /// Channel instance (v1 supports: qqbot)
        #[arg(long, default_value = "qqbot")]
        channel: String,
        /// Remove the runtime override — the effective mode falls back to config.toml
        #[arg(long, conflicts_with = "value")]
        clear: bool,
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
            clear,
            json,
        } => mode(value.as_deref(), channel, *clear, *json).await,
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

async fn mode(value: Option<&str>, channel: &str, clear: bool, json: bool) -> Result<(), ZenError> {
    if channel != "qqbot" {
        return Err(ZenError::Message(format!(
            "unknown channel '{channel}' (v1 supports: qqbot)"
        )));
    }
    let config = load_config()?;
    let qqbot_cfg = config.channels.qqbot.as_ref();
    let config_mode = qqbot_cfg
        .map(|q| q.accept_mode_or_default())
        .unwrap_or_default();
    let config_path = ZenPaths::detect()
        .map(|p| p.config_file())
        .map_err(|e| ZenError::Message(format!("path error: {e}")))?;

    let override_row = read_mode_override().await?;
    let effective = resolve_effective_accept_mode(config_mode, override_row.as_deref());
    let policy_version = admission_policy_version_for_mode(effective, qqbot_cfg);

    if clear {
        let cleared = clear_mode_override_row().await?;
        let reverted = resolve_effective_accept_mode(config_mode, None);
        let reverted_version = admission_policy_version_for_mode(reverted, qqbot_cfg);
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "channel": channel,
                    "cleared": cleared,
                    "acceptMode": reverted.as_str(),
                    "policyVersion": reverted_version,
                    "source": "config.toml",
                }))
                .map_err(|e| ZenError::Message(format!("serialize mode: {e}")))?
            );
        } else if cleared {
            println!("{}", "Runtime override cleared".bold());
            println!(
                "  accept mode now follows config: {}",
                reverted.as_str().cyan()
            );
        } else {
            println!("{}", "No runtime override present".bold());
            println!(
                "  accept mode already follows config: {}",
                reverted.as_str().cyan()
            );
        }
        return Ok(());
    }

    let Some(requested) = value else {
        let source = if override_row.is_some() {
            "runtime override (state.db)"
        } else {
            "config.toml"
        };
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "channel": channel,
                    "acceptMode": effective.as_str(),
                    "source": source,
                    "policyVersion": policy_version,
                    "configPath": config_path.display().to_string(),
                }))
                .map_err(|e| ZenError::Message(format!("serialize mode: {e}")))?
            );
        } else {
            println!("{}", "Admission accept mode".bold());
            println!("  channel        = {channel}");
            println!("  accept_mode    = {}", effective.as_str().cyan());
            println!("  source         = {source}");
            println!("  policy_version = {}", policy_version.dimmed());
            if override_row.is_some() {
                println!(
                    "  {}",
                    "runtime override active — `zen admission mode --clear` reverts to config"
                        .dimmed()
                );
            } else {
                println!("  config         = {}", config_path.display());
            }
        }
        return Ok(());
    };

    let requested = parse_requested_mode(requested)?;
    let open_tickets = open_ticket_count().await;
    write_mode_override(requested.as_str()).await?;
    let new_version = admission_policy_version_for_mode(requested, qqbot_cfg);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "channel": channel,
                "currentMode": effective.as_str(),
                "requestedMode": requested.as_str(),
                "policyVersion": new_version,
                "effect": "future admissions only (FR-014); live daemon picks it up on the next message",
                "openTickets": open_tickets,
                "revert": "zen admission mode --clear",
                "applied": true,
            }))
            .map_err(|e| ZenError::Message(format!("serialize mode: {e}")))?
        );
        return Ok(());
    }

    println!("{}", "Accept mode set (runtime override)".bold());
    println!("  channel        = {channel}");
    println!("  current_mode   = {}", effective.as_str().cyan());
    println!("  requested_mode = {}", requested.as_str().cyan());
    println!("  policy_version = {}", new_version.dimmed());
    println!();
    println!(
        "Applied: the daemon resolves the override on the next inbound message — no restart needed."
    );
    println!("config.toml is untouched; the override wins until cleared.");
    println!();
    println!(
        "Effect: future admissions only — in-flight work continues under its granted mandate (FR-014)."
    );
    if open_tickets > 0 {
        println!(
            "Note: {open_tickets} open admission ticket(s); their work continues under its recorded mandate."
        );
    }
    println!("Revert: `zen admission mode --clear`.");
    Ok(())
}

async fn read_mode_override() -> Result<Option<String>, ZenError> {
    let Some(client) = open_ledger().await? else {
        return Ok(None);
    };
    AdmissionRepo::new(&client)
        .mode_override()
        .await
        .map_err(|e| ZenError::Message(format!("read accept-mode override: {e}")))
}

async fn write_mode_override(mode: &str) -> Result<(), ZenError> {
    let paths = ZenPaths::detect().map_err(|e| ZenError::Message(format!("path error: {e}")))?;
    let db = paths.data().join("state.db");
    write_mode_override_at(&db, mode).await
}

async fn write_mode_override_at(db: &std::path::Path, mode: &str) -> Result<(), ZenError> {
    if !db.exists() {
        return Err(ZenError::Message(format!(
            "state.db not found at {} — a runtime override has nothing to override; \
             start the daemon first (`zen serve start`) or set accept_mode in \
             config.toml [channels.qqbot]",
            db.display()
        )));
    }
    let client = SqliteClient::open(db)
        .await
        .map_err(|e| ZenError::Message(format!("open state.db: {e}")))?;
    AdmissionRepo::new(&client)
        .set_mode_override(mode)
        .await
        .map_err(|e| ZenError::Message(format!("write accept-mode override: {e}")))
}

async fn clear_mode_override_row() -> Result<bool, ZenError> {
    let Some(client) = open_ledger().await? else {
        return Ok(false);
    };
    AdmissionRepo::new(&client)
        .clear_mode_override()
        .await
        .map_err(|e| ZenError::Message(format!("clear accept-mode override: {e}")))
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
        assert_eq!(
            parse_requested_mode("observe").unwrap(),
            AcceptMode::Observe
        );
        assert!(parse_requested_mode("bogus").is_err());
    }

    /// Audit-remediation R-2 (2026-11-22): the mode write path refuses when
    /// state.db is absent — a runtime override has nothing to override, the
    /// error names the remedy, and the refusal must NOT create the db; an
    /// initialized (empty-file) db migrates and accepts the flip.
    #[tokio::test]
    async fn mode_write_refuses_when_state_db_absent_then_accepts_after_init() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");

        let err = write_mode_override_at(&db, "auto").await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("state.db not found"), "{msg}");
        assert!(msg.contains("zen serve start"), "{msg}");
        assert!(!db.exists(), "refusal must not create the db");

        SqliteClient::open(&db).await.expect("init db");

        write_mode_override_at(&db, "auto")
            .await
            .expect("initialized db accepts the flip");
    }
}
