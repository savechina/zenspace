//! QQBot platform adapter — the hermes-style main adapter (Phase 13).
//!
//! NOTE: in hermes the term "adapter" spans the whole platform module
//! (`qqbot/adapter.py`); here the QQ protocol pieces stay in
//! client.rs/api.rs/auth.rs — this file is the ADAPTER PROPER:
//! [`QqBotAdapter`] implements the generic [`Channel`] trait, bridging
//! QQ events onto the zen-gateway loopback HTTP transport server.
//!
//! # PURPOSE
//! Listen loop (via [`QqBotClient`]) normalizes dispatch frames into
//! [`MessageEvent`]s, applies the allowlist gate → dedup → command
//! surface (`/new`, `/status`) or agent turn through the private
//! gateway bridge (`POST /api/v1/chat`, `GET /health`), and replies as
//! a passive message quoting inbound `msg_id` (msg_seq 1) with an
//! active-send fallback when the passive window expired.
//!
//! # USAGE
//! `QqBotAdapter::new(QqBotAdapterOptions)`; spawn via the [`Channel`]
//! impl in `serve_with_shutdown`. Bindings persist through
//! `qq_bindings`; `/new` deletes the binding for a fresh session.
//!
//! # EXPECTED
//! Allowlisted chats converse with persistent sessions; duplicate
//! platform message ids are suppressed adapter-side (bounded
//! seen-set); non-allowlisted authors are dropped silently. Messages
//! within ONE chat process strictly FIFO; distinct chats progress
//! concurrently (D1), and WS frame reads never await business logic.
//!
//! # ERRORS
//! [`Channel::run`] returns `Err` when the bindings store cannot open
//! or the HTTP carrier is unreachable at startup (P5 liveness gate);
//! per-event failures log and continue.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use zen_core::admission::{
    AdmissionStrength, Disposition, GateInput, evaluate, resolve_effective_accept_mode,
};
use zen_core::config::AcceptMode;

use super::api::QqBotApi;
use super::auth::QqBotAuth;
use super::client::{QqBotClient, QqWsEvent, QqWsEventKind};
use super::constants::INTENT_GROUP_AND_C2C;
use crate::channel::Channel;

/// The v1 Source key recorded on every admission decision (007 T006, FR-001).
const SOURCE: &str = "qqbot";

/// Upper bound of the duplicate-message suppression window.
const SEEN_CAP: usize = 1024;
/// Total wall-clock budget for ONE inbound message across every
/// gateway attempt (D8): group passive window is 5 min — the reply
/// must still fit after retries; C2C windows are far wider and the
/// server-side turn ceiling is renewal-aware since W9.5-F3 (derived
/// `4 × watchdog + margin`, 3660s at defaults — this budget stays a
/// platform-window product choice). Expiry abandons the message with
/// an error text; no further retries (in-flight duplicate turnIds
/// mirror the original result instead of re-executing).
const GROUP_DEADLINE: Duration = Duration::from_secs(280);
/// See [`GROUP_DEADLINE`].
const C2C_DEADLINE: Duration = Duration::from_secs(960);
/// Attempts per gateway call before surfacing the failure text.
const GATEWAY_ATTEMPTS: u32 = 3;
/// Passive-reply send attempts before the active fallback.
const SEND_ATTEMPTS: u32 = 3;
/// Per-chat inbound queue depth; overflow drops the event (liveness of
/// the WS reader outranks buffering a flood from one chat).
const CHAT_QUEUE_CAP: usize = 16;
/// TOTAL budget for joining all per-chat workers after shutdown fires.
const SHUTDOWN_JOIN_GRACE: Duration = Duration::from_secs(10);
/// Platform content ceiling: replies are truncated to this many chars
/// and empty replies are refused outright.
const REPLY_MAX_CHARS: usize = 2000;

/// Immutable adapter configuration — `[channels.qqbot]` plus
/// daemon-resolved endpoints; tests inject mock URLs.
#[derive(Clone)]
pub struct QqBotAdapterOptions {
    pub app_id: String,
    pub client_secret: String,
    /// Gateway HTTP carrier base, e.g. `http://127.0.0.1:9876`
    /// (resolved by the daemon from its effective HTTP carrier).
    pub chat_base: String,
    /// QQ WebSocket gateway URL.
    pub ws_url: String,
    /// QQ OpenAPI base URL.
    pub api_base: String,
    /// QQ token endpoint URL.
    pub token_url: String,
    /// Allowlisted author openids (group member or C2C user); empty
    /// deny-by-default per contracts/05. Doubles as the admission gate's
    /// explicit identity list (007 D5 — no second allowlist).
    pub allowed_users: Vec<String>,
    /// Admission accept mode — raw `[channels.qqbot] accept_mode` (007 T006,
    /// FR-004). Absent ⇒ observe (owner Q2=A). Parsed once at adapter
    /// construction as the fallback; the effective mode resolves per
    /// message against the `admission_mode_override` row (migration 017,
    /// written by `zen admission mode <value>`). No env key (D22).
    pub accept_mode: Option<String>,
    /// SQLite file for `qq_bindings` persistence (also the admission ledger).
    pub bindings_db: std::path::PathBuf,
    /// Morning-brief outbox drain tick (T101). Mapped from
    /// `[channels.qqbot] outbox_drain_interval_secs` (default 300s,
    /// clamped 60..=3600); tests inject short values directly.
    pub outbox_drain_interval: Duration,
    /// Audit JSONL sink (D3): accept/deny/reply/chat-error events.
    /// `None` disables file audit (tests); the daemon injects the same
    /// resolved path the hosting layer uses.
    pub audit_path: Option<std::path::PathBuf>,
}

/// Normalized inbound message (hermes `MessageEvent` analog): one
/// user-visible chat utterance with reply-correlation ids attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageEvent {
    /// Group or C2C conversation.
    pub kind: QqWsEventKind,
    /// Reply-routing identity: group_openid or user openid.
    pub chat_id: String,
    /// Authorization identity checked against the allowlist.
    pub author_id: String,
    /// Platform message id (passive-reply target + dedup key).
    pub msg_id: String,
    /// Mention-stripped prompt text.
    pub text: String,
}

impl MessageEvent {
    /// Builds from a raw GROUP_AT_MESSAGE_CREATE / C2C_MESSAGE_CREATE
    /// dispatch payload; `None` when required fields are absent.
    fn parse(kind: QqWsEventKind, d: &serde_json::Value) -> Option<Self> {
        let is_group = kind == QqWsEventKind::GroupAtMessage;
        let chat_id = if is_group {
            str_field(d, "group_openid")?
        } else {
            nested_str_field(d.get("author"), "id")?
        };
        let author_id = if is_group {
            nested_str_field(d.get("author"), "member_openid")
                .or_else(|| nested_str_field(d.get("author"), "id"))?
        } else {
            nested_str_field(d.get("author"), "id")?
        };
        Some(Self {
            kind,
            chat_id,
            author_id,
            msg_id: str_field(d, "id").unwrap_or_default(),
            text: strip_at_mention(str_field(d, "content").unwrap_or_default().as_str()),
        })
    }
}

fn str_field(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(serde_json::Value::as_str)
        .map(String::from)
}

fn nested_str_field(v: Option<&serde_json::Value>, key: &str) -> Option<String> {
    v.and_then(|a| str_field(a, key))
}

/// GROUP_AT_MESSAGE_CREATE content arrives as `<@!BOT_ID> text` —
/// strip ONLY that exact mention shape (a bare `<@id>` or any other
/// `<...>` prefix is user content and must survive intact).
fn strip_at_mention(content: &str) -> String {
    let trimmed = content.trim();
    if let Some(rest) = trimmed.strip_prefix("<@!")
        && let Some(end) = rest.find('>')
    {
        return rest[end + 1..].trim().to_string();
    }
    trimmed.to_string()
}

/// Deterministic idempotency key for one platform message (D2+D8):
/// uuid v5 over `chat_id:msg_id` so redeliveries of the same QQ event
/// replay the hosted turn instead of re-executing it. `None` when the
/// platform omitted `msg_id` (nothing to key on).
fn build_turn_id(chat_id: &str, msg_id: &str) -> Option<String> {
    if msg_id.is_empty() {
        return None;
    }
    let ns = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, b"zen-gateway:qqbot");
    Some(uuid::Uuid::new_v5(&ns, format!("{chat_id}:{msg_id}").as_bytes()).to_string())
}

/// Built-in channel plumbing commands that bypass the admission gate
/// unchanged (007 T006).
fn is_builtin_command(text: &str) -> bool {
    matches!(text, "/new" | "/status")
}

/// Stable trigger content hash — sha256 of `sender + "\n" + content`
/// (007 T006, FR-001/D8 dedup fallback).
fn content_hash(sender: &str, content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(sender.as_bytes());
    hasher.update(b"\n");
    hasher.update(content.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn strength_str(strength: AdmissionStrength) -> &'static str {
    match strength {
        AdmissionStrength::Explicit => "explicit",
        AdmissionStrength::Inferred => "inferred",
    }
}

/// Policy-version hash over the effective admission section, reusing the
/// zen-core definition so the CLI and the seam can never disagree (007 D6).
/// Takes the RESOLVED mode (migration 017): a runtime override row changes
/// the effective policy, and the ledger must stamp what was actually in
/// effect — a config-derived mode would freeze the boot-time version.
fn policy_version_for(mode: AcceptMode, options: &QqBotAdapterOptions) -> String {
    let config = zen_core::config::QqBotChannelConfig {
        app_id: options.app_id.clone(),
        client_secret: options.client_secret.clone(),
        allowed_users: options.allowed_users.clone(),
        home_channel: None,
        accept_mode: options.accept_mode.clone(),
        outbox_drain_interval_secs: None,
    };
    zen_core::config::admission_policy_version_for_mode(mode, Some(&config))
}

fn epoch_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Char-boundary-safe truncation to `max` chars.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

/// Private bridge onto the zen-gateway loopback HTTP transport server
/// (openclaw "channelRuntime" analog): agent turns + liveness probes,
/// the SAME surface external HTTP clients use (FR-019 coexistence by
/// construction).
struct GatewayBridge {
    /// No request-level timeout: one wall-clock [`Duration`] supplied
    /// per message spans ALL retry attempts, so a slow turn is bounded
    /// by the message budget, never re-armed per attempt (D8).
    chat_http: reqwest::Client,
    /// Independent short-timeout client for `/health` probes — a
    /// status render must fail fast even while chat turns are slow.
    health_http: reqwest::Client,
    chat_base: String,
    /// Bearer secret for the carrier auth layer (CSO audit #1);
    /// `None` = no header (loopback-only posture). `/health` is exempt
    /// from the gate, so only the chat POST carries it.
    bearer: Option<String>,
}

/// One completed chat round-trip.
struct ChatOutcome {
    reply: String,
    /// Hosted session id echoed by the carrier response; persisted to
    /// the binding table for continuity.
    session_id: Option<String>,
}

impl GatewayBridge {
    fn new(chat_base: String, bearer: Option<String>) -> Result<Self> {
        Ok(Self {
            chat_http: reqwest::Client::builder()
                .no_proxy()
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            health_http: reqwest::Client::builder()
                .no_proxy()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(10))
                .build()?,
            chat_base,
            bearer,
        })
    }

    async fn chat(
        &self,
        message: &str,
        session_id: Option<&str>,
        turn_id: Option<String>,
        deadline: Duration,
    ) -> Result<ChatOutcome> {
        let url = format!("{}/api/v1/chat", self.chat_base);
        let body = serde_json::json!({
            "message": message,
            "session_id": session_id,
            "turn_id": turn_id,
        });
        // One deadline across every attempt: expiry abandons the
        // message outright — a duplicate turnId would only wait on the
        // original turn, so retrying past the budget cannot help (D8).
        tokio::time::timeout(deadline, async {
            let mut last_err = None;
            for attempt in 1..=GATEWAY_ATTEMPTS {
                let mut request = self.chat_http.post(&url).json(&body);
                if let Some(secret) = &self.bearer {
                    request = request.bearer_auth(secret);
                }
                match request.send().await {
                    Ok(resp) if resp.status().is_success() => {
                        let body: serde_json::Value = resp.json().await.unwrap_or_default();
                        return Ok(ChatOutcome {
                            reply: body
                                .get("reply")
                                .and_then(|v| v.as_str())
                                .unwrap_or("(empty reply)")
                                .to_string(),
                            session_id: body
                                .get("session_id")
                                .and_then(|v| v.as_str())
                                .map(str::to_string),
                        });
                    }
                    Ok(resp) => {
                        let status = resp.status();
                        let body = resp.text().await.unwrap_or_default();
                        if status.is_client_error() {
                            return Err(anyhow!("gateway {status}: {body}"));
                        }
                        last_err = Some(anyhow!("gateway {status}: {body}"));
                    }
                    Err(e) => last_err = Some(e.into()),
                }
                tokio::time::sleep(Duration::from_secs(u64::from(attempt))).await;
            }
            Err(last_err.unwrap_or_else(|| anyhow!("gateway unreachable")))
        })
        .await
        .map_err(|_| anyhow!("chat deadline {}s exceeded", deadline.as_secs()))?
    }

    async fn health(&self) -> Result<serde_json::Value> {
        let resp = self
            .health_http
            .get(format!("{}/health", self.chat_base))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(anyhow!("gateway health {}", resp.status()));
        }
        Ok(resp.json().await?)
    }
}

/// Chat-relevant dispatch frames normalize into a [`MessageEvent`];
/// everything else (READY receipts, other platforms' events) is None.
fn normalize_event(event: QqWsEvent) -> Option<MessageEvent> {
    match event.kind {
        QqWsEventKind::GroupAtMessage | QqWsEventKind::C2cMessage => {
            MessageEvent::parse(event.kind, &event.raw)
        }
        _ => None,
    }
}

/// Clonable so each per-chat worker task can hold its own handle to
/// the shared auth/api/bridge stack (D1 concurrency model).
#[derive(Clone)]
pub struct QqBotAdapter {
    options: QqBotAdapterOptions,
    /// Configured accept mode — the fallback when no runtime override row
    /// exists (`admission_mode_override`, migration 017); the effective mode
    /// is resolved per inbound message.
    mode: AcceptMode,
    /// Single shared token authority: REST send path and WS identify
    /// flow MUST refresh against one cache or they race double-fetch.
    auth: Arc<QqBotAuth>,
    api: Arc<QqBotApi>,
    bridge: Arc<GatewayBridge>,
}

impl QqBotAdapter {
    /// Appends one structured line to `audit.jsonl` (D3) — mirrors the
    /// hosting-layer sink exactly: fire-and-forget, failures warn and
    /// never fail the operation. Epoch-millis `ts` for parity with
    /// existing records.
    fn audit(&self, mut record: serde_json::Value) {
        let Some(path) = self.options.audit_path.clone() else {
            return;
        };
        if let Ok(ts) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            record["ts"] = serde_json::Value::String(ts.as_millis().to_string());
        }
        record["channel"] = serde_json::Value::String("qqbot".into());
        tokio::task::spawn_blocking(move || {
            use std::io::Write;
            let line = format!("{record}\n");
            let write = || -> std::io::Result<()> {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)?;
                f.write_all(line.as_bytes())
            };
            if let Err(e) = write() {
                tracing::warn!(path = %path.display(), "qqbot audit write failed: {e}");
            }
        });
    }
    pub fn new(options: QqBotAdapterOptions) -> Result<Self> {
        let auth = Arc::new(QqBotAuth::new(
            options.app_id.clone(),
            options.client_secret.clone(),
            options.token_url.clone(),
        ));
        let bridge = GatewayBridge::new(options.chat_base.clone(), None)?;
        let mode = AcceptMode::parse(options.accept_mode.as_deref());
        Ok(Self {
            api: Arc::new(QqBotApi::new(Arc::clone(&auth), options.api_base.clone())),
            auth,
            bridge: Arc::new(bridge),
            mode,
            options,
        })
    }

    /// Attaches the gateway carrier's bearer secret (CSO audit #1) so
    /// bridge POSTs clear the HTTP auth layer. A builder step after
    /// [`Self::new`] rather than an options field because the CLI
    /// constructs `QqBotAdapterOptions` with a field-exact literal —
    /// the secret rides the daemon wiring, not the options struct.
    /// Empty secrets normalize to `None` (no header).
    ///
    /// # Errors
    /// Bridge rebuild failure (reqwest client construction).
    pub fn with_gateway_bearer(mut self, secret: Option<String>) -> Result<Self> {
        if let Some(secret) = secret.filter(|s| !s.is_empty()) {
            self.bridge = Arc::new(GatewayBridge::new(
                self.options.chat_base.clone(),
                Some(secret),
            )?);
        }
        Ok(self)
    }

    /// Fast structural gates — idempotency-key presence, emptiness, and the
    /// adapter-side replay guard for built-in channel commands. Allowlist and
    /// duplicate decisions belong to the admission gate: non-allowlisted work
    /// requests are queued so their rejection can be recorded (007 T006,
    /// FR-005/FR-011), and replayed work events reach the repository's dedup
    /// index instead of being swallowed here (SC-005). Pure in-memory checks
    /// so the WS dispatch loop NEVER awaits business logic (D1/oracle P3).
    /// Returns `true` when the event should enter its chat's queue.
    fn admitted(
        &self,
        event: &MessageEvent,
        seen: &mut HashSet<String>,
        seen_order: &mut VecDeque<String>,
    ) -> bool {
        // Events without a platform msg_id have no idempotency key and
        // would collide in the dedup set — drop before tracking (they
        // also cannot be replied to passively).
        if event.msg_id.is_empty() {
            warn!(chat_id = %event.chat_id, "qqbot event missing msg_id; dropping");
            return false;
        }
        if event.chat_id.is_empty() || event.text.is_empty() {
            return false;
        }
        // Built-in channel commands are plumbing, not work requests: they
        // bypass admission unchanged and keep the adapter's replay guard.
        if is_builtin_command(&event.text) {
            if seen.contains(&event.msg_id) {
                debug!(msg_id = %event.msg_id, "qqbot duplicate channel command suppressed");
                return false;
            }
            seen.insert(event.msg_id.clone());
            seen_order.push_back(event.msg_id.clone());
            if seen_order.len() > SEEN_CAP
                && let Some(evicted) = seen_order.pop_front()
            {
                seen.remove(&evicted);
            }
        }
        if self.options.allowed_users.contains(&event.author_id) {
            self.audit(serde_json::json!({
                "kind": "qqbot.accepted",
                "eventKind": if event.kind == QqWsEventKind::GroupAtMessage { "group" } else { "c2c" },
                "chatId": event.chat_id,
                "authorId": event.author_id,
                "msgId": event.msg_id,
            }));
        }
        true
    }

    /// Slow path for one queued message: built-in commands keep their
    /// existing behavior; every work request passes the host-enforced
    /// admission gate before any session/turn dispatch (007 T006, FR-002).
    /// Runs on the chat's private worker task.
    async fn process_message(
        &self,
        event: &MessageEvent,
        client: &zen_repo::SqliteClient,
    ) -> Result<()> {
        let is_group = event.kind == QqWsEventKind::GroupAtMessage;
        let repo = zen_repo::QqBindingRepo::new(client);
        match event.text.as_str() {
            "/new" => {
                repo.delete(&event.chat_id).await?;
                self.reply(event, is_group, "已开启新会话 (new session)")
                    .await?;
            }
            "/status" => match self.bridge.health().await {
                Ok(body) => {
                    self.reply(event, is_group, &format!("gateway: {body}"))
                        .await?;
                }
                Err(e) => {
                    self.reply(event, is_group, &format!("gateway offline: {e}"))
                        .await?;
                }
            },
            prompt => {
                self.process_work_request(event, client, &repo, prompt)
                    .await?;
            }
        }
        Ok(())
    }

    /// Resolves the per-message effective admission policy (migration 017):
    /// runtime override row > configured mode. A repository failure fails
    /// OPEN to the configured mode (warn, never a dispatch blocker) — the
    /// override is an owner convenience, not a gate input.
    async fn effective_policy(&self, client: &zen_repo::SqliteClient) -> (AcceptMode, String) {
        let repo = zen_repo::AdmissionRepo::new(client);
        let override_row = match repo.mode_override().await {
            Ok(row) => row,
            Err(e) => {
                warn!(error = %e, "admission override read failed (fail-open to config)");
                None
            }
        };
        let mode = resolve_effective_accept_mode(self.mode, override_row.as_deref());
        (mode, policy_version_for(mode, &self.options))
    }

    /// Admission seam (007 T006/T009, FR-002/FR-004/FR-012/FR-014).
    ///
    /// Records one decision for every work request, then dispatches only
    /// `Admitted` verdicts through the EXISTING hosted-turn path. `Observe`
    /// and `Rejected` write the ledger and return with zero side effects (no
    /// turn, no reply, no binding write). Any repository failure fails closed:
    /// an unresolved row when possible, never a dispatch.
    async fn process_work_request(
        &self,
        event: &MessageEvent,
        client: &zen_repo::SqliteClient,
        repo: &zen_repo::QqBindingRepo<'_>,
        prompt: &str,
    ) -> Result<()> {
        let admission = zen_repo::AdmissionRepo::new(client);
        let thread_key = event.chat_id.as_str();
        let is_thread_continuation = match admission.open_ticket_for_thread(thread_key).await {
            Ok(ticket) => ticket.is_some(),
            Err(e) => {
                warn!(error = %e, thread = %thread_key, "admission continuation lookup failed");
                self.record_unresolved(event, client, &format!("continuation lookup failed: {e}"))
                    .await;
                return Ok(());
            }
        };
        let (mode, policy_version) = self.effective_policy(client).await;
        let decision = evaluate(&GateInput {
            mode,
            allowlist: &self.options.allowed_users,
            sender: &event.author_id,
            is_thread_continuation,
            matched_inference_rule: None,
        });

        let decision_id = uuid::Uuid::now_v7().to_string();
        let hash = content_hash(&event.author_id, prompt);
        let event_id = (!event.msg_id.is_empty()).then_some(event.msg_id.as_str());
        let created_at = epoch_secs();
        let (disposition, matched_rule, scope, strength, reason) = match &decision.disposition {
            Disposition::Rejected { reason } => {
                ("rejected", None, None, None, Some(reason.as_str()))
            }
            Disposition::WouldAccept {
                scope,
                strength,
                rule,
            } => (
                "would_accept",
                Some(rule.as_str()),
                Some(scope.as_str()),
                Some(strength_str(*strength)),
                None,
            ),
            Disposition::Admitted {
                scope,
                strength,
                rule,
            } => (
                "admitted",
                Some(rule.as_str()),
                Some(scope.as_str()),
                Some(strength_str(*strength)),
                None,
            ),
        };

        let outcome = admission
            .record_decision(zen_repo::DecisionRecord {
                id: &decision_id,
                source: SOURCE,
                event_id,
                content_hash: &hash,
                sender: &event.author_id,
                thread_key,
                content: prompt,
                disposition,
                matched_rule,
                scope,
                accept_mode: mode.as_str(),
                strength,
                policy_version: &policy_version,
                reason,
                created_at,
            })
            .await;
        match outcome {
            Ok(zen_repo::DecisionOutcome::New) => {}
            Ok(zen_repo::DecisionOutcome::Duplicate { id }) => {
                self.audit_decision(
                    &id,
                    "duplicate",
                    event,
                    AuditContext {
                        mode,
                        policy_version: &policy_version,
                        matched_rule: None,
                        scope: None,
                        reason: None,
                    },
                );
                return Ok(());
            }
            Err(e) => {
                warn!(error = %e, "admission decision record failed (fail-closed)");
                self.audit_decision(
                    &decision_id,
                    "unresolved",
                    event,
                    AuditContext {
                        mode,
                        policy_version: &policy_version,
                        matched_rule: None,
                        scope: None,
                        reason: Some(&e.to_string()),
                    },
                );
                return Ok(());
            }
        }
        self.audit_decision(
            &decision_id,
            disposition,
            event,
            AuditContext {
                mode,
                policy_version: &policy_version,
                matched_rule,
                scope,
                reason,
            },
        );
        if let Some(reason) = reason
            && reason == "sender not allowlisted"
        {
            self.audit(serde_json::json!({
                "kind": "qqbot.rejected",
                "reason": "not-allowlisted",
                "chatId": event.chat_id,
                "authorId": event.author_id,
                "msgId": event.msg_id,
            }));
        }

        if !decision.disposition.is_admitted() {
            return Ok(());
        }

        let mandate_id = match admission
            .grant_mandate(zen_repo::MandateGrant {
                decision_id: &decision_id,
                matched_rule,
                scope: scope.unwrap_or("sender"),
                strength: strength.unwrap_or("explicit"),
                accept_mode: mode.as_str(),
                policy_version: &policy_version,
                granted_at: created_at,
            })
            .await
        {
            Ok(id) => id,
            Err(e) => {
                warn!(error = %e, "admission mandate grant failed (fail-closed)");
                self.record_unresolved(event, client, &format!("mandate grant failed: {e}"))
                    .await;
                return Ok(());
            }
        };
        let turn_id = build_turn_id(&event.chat_id, &event.msg_id);
        let ticket_id = match admission
            .open_ticket(zen_repo::TicketOpen {
                mandate_id: &mandate_id,
                reply_address: &event.chat_id,
                turn_ref: turn_id.as_deref(),
                created_at,
            })
            .await
        {
            Ok(id) => id,
            Err(e) => {
                warn!(error = %e, "admission ticket open failed (fail-closed)");
                self.record_unresolved(event, client, &format!("ticket open failed: {e}"))
                    .await;
                return Ok(());
            }
        };

        let result = self.run_prompt(event, repo, prompt).await;
        let report_state = if result.is_ok() { "ready" } else { "failed" };
        if let Err(e) = admission.close_ticket(&ticket_id, report_state).await {
            warn!(error = %e, ticket = %ticket_id, "admission ticket close failed");
        }
        result
    }

    /// The EXISTING hosted-turn dispatch (chat-binding session → gateway
    /// `/api/v1/chat` → reply). Factored out unchanged so the admitted path
    /// reuses it byte-for-byte (007 D4).
    async fn run_prompt(
        &self,
        event: &MessageEvent,
        repo: &zen_repo::QqBindingRepo<'_>,
        prompt: &str,
    ) -> Result<()> {
        let is_group = event.kind == QqWsEventKind::GroupAtMessage;
        let bound_session = repo.get(&event.chat_id).await?.map(|row| row.session_id);
        let turn_id = build_turn_id(&event.chat_id, &event.msg_id);
        let deadline = if is_group {
            GROUP_DEADLINE
        } else {
            C2C_DEADLINE
        };
        match self
            .bridge
            .chat(prompt, bound_session.as_deref(), turn_id.clone(), deadline)
            .await
        {
            Ok(outcome) => {
                if let Some(session_id) = outcome.session_id
                    && bound_session.as_deref() != Some(session_id.as_str())
                {
                    repo.upsert(&event.chat_id, &session_id).await?;
                }
                self.reply(event, is_group, &outcome.reply).await?;
                Ok(())
            }
            Err(e) => {
                warn!(error = %e, "qqbot chat call failed");
                self.audit(serde_json::json!({
                    "kind": "qqbot.chat_error",
                    "chatId": event.chat_id,
                    "msgId": event.msg_id,
                    "turnId": turn_id,
                    "error": e.to_string(),
                }));
                self.reply(event, is_group, "agent 暂时不可用，请稍后再试")
                    .await?;
                Err(e)
            }
        }
    }

    /// Fail-closed ledger write (007 FR-012): an unresolved row with the
    /// precise reason, best-effort (a dead store cannot record its own
    /// failure). A synthetic non-NULL event id keeps the row outside both
    /// dedup indexes so a later replay of the same event can still land its
    /// real decision.
    async fn record_unresolved(
        &self,
        event: &MessageEvent,
        client: &zen_repo::SqliteClient,
        detail: &str,
    ) {
        let admission = zen_repo::AdmissionRepo::new(client);
        let (mode, policy_version) = self.effective_policy(client).await;
        let id = uuid::Uuid::now_v7().to_string();
        let synthetic_event_id = format!("unresolved:{id}");
        let hash = content_hash(&event.author_id, &event.text);
        let created_at = epoch_secs();
        if let Err(e) = admission
            .record_decision(zen_repo::DecisionRecord {
                id: &id,
                source: SOURCE,
                event_id: Some(&synthetic_event_id),
                content_hash: &hash,
                sender: &event.author_id,
                thread_key: &event.chat_id,
                content: &event.text,
                disposition: "unresolved",
                matched_rule: None,
                scope: None,
                accept_mode: mode.as_str(),
                strength: None,
                policy_version: &policy_version,
                reason: Some(detail),
                created_at,
            })
            .await
        {
            warn!(error = %e, "unresolved admission record failed (fail-closed)");
        }
        self.audit_decision(
            &id,
            "unresolved",
            event,
            AuditContext {
                mode,
                policy_version: &policy_version,
                matched_rule: None,
                scope: None,
                reason: Some(detail),
            },
        );
    }
}

/// Per-disposition audit context: the effective policy under which the
/// decision was made (migration 017 — stamped, never boot-cached), plus
/// the recorded rule/scope/reason details.
struct AuditContext<'a> {
    mode: AcceptMode,
    policy_version: &'a str,
    matched_rule: Option<&'a str>,
    scope: Option<&'a str>,
    reason: Option<&'a str>,
}

impl QqBotAdapter {
    /// One `admission.decision` audit line per disposition (007 T006,
    /// FR-006/FR-003 observability).
    fn audit_decision(
        &self,
        decision_id: &str,
        disposition: &str,
        event: &MessageEvent,
        ctx: AuditContext<'_>,
    ) {
        self.audit(serde_json::json!({
            "kind": "admission.decision",
            "decisionId": decision_id,
            "disposition": disposition,
            "source": SOURCE,
            "sender": event.author_id,
            "threadKey": event.chat_id,
            "matchedRule": ctx.matched_rule,
            "scope": ctx.scope,
            "mode": ctx.mode.as_str(),
            "policyVersion": ctx.policy_version,
            "reason": ctx.reason,
        }));
    }

    /// Audit wrapper around [`Self::try_reply`]: every reply outcome
    /// (success or final failure) lands in `audit.jsonl` — D3 closes
    /// the "swallowed reply errors" audit gap.
    async fn reply(&self, event: &MessageEvent, is_group: bool, content: &str) -> Result<()> {
        let result = self.try_reply(event, is_group, content).await;
        match &result {
            Ok(()) => self.audit(serde_json::json!({
                "kind": "qqbot.replied",
                "outcome": "success",
                "chatId": event.chat_id,
                "msgId": event.msg_id,
            })),
            Err(e) => self.audit(serde_json::json!({
                "kind": "qqbot.replied",
                "outcome": "failed",
                "chatId": event.chat_id,
                "msgId": event.msg_id,
                "error": e.to_string(),
            })),
        }
        result
    }

    /// Passive reply first (quoting msg_id); each physical send uses
    /// the next `msg_seq` — QQ rejects repeated `msg_id+msg_seq` pairs
    /// inside one passive window. On exhaustion falls back to an active
    /// send (msg_id dropped, seq continues). Empty content is refused
    /// and over-long replies truncated to [`REPLY_MAX_CHARS`].
    ///
    /// # Errors
    /// Every send attempt failed (passive + active) — callers audit.
    async fn try_reply(&self, event: &MessageEvent, is_group: bool, content: &str) -> Result<()> {
        let trimmed = content.trim();
        anyhow::ensure!(
            !trimmed.is_empty(),
            "refusing to send empty qqbot reply for msg {}",
            event.msg_id
        );
        let content = truncate_chars(trimmed, REPLY_MAX_CHARS);
        let mut seq: u32 = 1;
        for attempt in 1..=SEND_ATTEMPTS {
            let result = if is_group {
                self.api
                    .send_group_message(&event.chat_id, &content, Some(&event.msg_id), seq)
                    .await
            } else {
                self.api
                    .send_c2c_message(&event.chat_id, &content, Some(&event.msg_id), seq)
                    .await
            };
            if result.is_ok() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(u64::from(attempt))).await;
            seq += 1;
        }
        warn!("passive reply exhausted; falling back to active send");
        if is_group {
            self.api
                .send_group_message(&event.chat_id, &content, None, seq)
                .await
                .map(|_| ())
        } else {
            self.api
                .send_c2c_message(&event.chat_id, &content, None, seq)
                .await
                .map(|_| ())
        }
    }
}

/// Startup liveness probes against the HTTP carrier before dialing QQ
/// (P5): a channel bridging onto a dead carrier would otherwise accept
/// events it can never answer. Probe failure is FATAL for the channel.
/// Five attempts over ~5s tolerate slow sibling startup under load.
const STARTUP_PROBE_ATTEMPTS: u32 = 5;

#[async_trait::async_trait]
impl Channel for QqBotAdapter {
    fn name(&self) -> &'static str {
        "qqbot"
    }

    async fn run(&self, mut shutdown: watch::Receiver<bool>) -> Result<()> {
        // Carrier liveness gate: escalate "bridge target dead at
        // startup" from warn-and-spin to fatal so the daemon log shows
        // one loud error instead of endless silent event drops.
        for attempt in 1..=STARTUP_PROBE_ATTEMPTS {
            match self.bridge.health().await {
                Ok(_) => break,
                Err(e) if attempt == STARTUP_PROBE_ATTEMPTS => {
                    return Err(anyhow!(
                        "gateway HTTP carrier at {} unreachable after {STARTUP_PROBE_ATTEMPTS} probes: {e}",
                        self.options.chat_base
                    ));
                }
                Err(e) => {
                    warn!(attempt, error = %e, "qqbot startup carrier probe failed; retrying");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }

        let bindings = Arc::new(
            zen_repo::SqliteClient::open(&self.options.bindings_db)
                .await
                .map_err(|e| {
                    anyhow!(
                        "open qq bindings store {}: {e}",
                        self.options.bindings_db.display()
                    )
                })?,
        );

        let gateway = QqBotClient::new(
            Arc::clone(&self.auth),
            self.options.ws_url.clone(),
            INTENT_GROUP_AND_C2C,
        );

        let (events_tx, mut events_rx) = mpsc::channel::<QqWsEvent>(64);
        let gateway_shutdown = shutdown.clone();
        let gateway_task =
            tokio::spawn(async move { gateway.run(events_tx, gateway_shutdown).await });

        // Morning-brief outbox drainer (FR-038 push face, T101): staged
        // `morning-brief-<date>.json` files deliver to bound chats on a
        // tick. Outbox dir = the resolved logs dir the daemon injects via
        // `audit_path`; `None` (tests) disables the drainer. Fail-closed:
        // only `Public` briefs deliver (`consumer_allowlisted=false` —
        // personal push is a future opt-in knob).
        if let Some(outbox_dir) = self
            .options
            .audit_path
            .clone()
            .and_then(|p| p.parent().map(|d| d.join("outbox")))
        {
            let drainer = Arc::new(super::outbox_drainer::OutboxDrainer::new(
                Arc::clone(&self.api),
                outbox_dir,
                false,
            ));
            let drain_bindings = Arc::clone(&bindings);
            let mut drain_shutdown = shutdown.clone();
            let drain_interval = self.options.outbox_drain_interval;
            tokio::spawn(async move {
                let list_chats = || async {
                    zen_repo::QqBindingRepo::new(drain_bindings.as_ref())
                        .list_chat_ids()
                        .await
                        .unwrap_or_default()
                };
                drainer.drain_once(&list_chats().await).await;
                loop {
                    tokio::select! {
                        _ = drain_shutdown.changed() => {
                            if *drain_shutdown.borrow_and_update() {
                                break;
                            }
                        }
                        _ = tokio::time::sleep(drain_interval) => {
                            drainer.drain_once(&list_chats().await).await;
                        }
                    }
                }
            });
        }

        let this = Arc::new(self.clone());
        let mut seen: HashSet<String> = HashSet::new();
        let mut seen_order: VecDeque<String> = VecDeque::new();
        // chat_id → queue sender + worker handle; one FIFO per chat,
        // chats progress concurrently (D1).
        let mut chats: HashMap<String, (mpsc::Sender<MessageEvent>, tokio::task::JoinHandle<()>)> =
            HashMap::new();

        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    if *shutdown.borrow_and_update() {
                        break;
                    }
                }
                event = events_rx.recv() => {
                    let Some(event) = event else { break };
                    let Some(message) = normalize_event(event) else {
                        continue;
                    };
                    if !this.admitted(&message, &mut seen, &mut seen_order) {
                        continue;
                    }
                    let chat_id = message.chat_id.clone();
                    // Reap finished workers so idle chat ids cannot
                    // accumulate entries forever.
                    if chats
                        .get(&message.chat_id)
                        .is_some_and(|(tx, _)| tx.is_closed())
                    {
                        chats.remove(&message.chat_id);
                    }
                    let sender = match chats.entry(message.chat_id.clone()) {
                        std::collections::hash_map::Entry::Occupied(e) => e.get().0.clone(),
                        std::collections::hash_map::Entry::Vacant(e) => {
                            let (tx, rx) = mpsc::channel::<MessageEvent>(CHAT_QUEUE_CAP);
                            let worker = Arc::clone(&this);
                            let worker_bindings = Arc::clone(&bindings);
                            let handle = tokio::spawn(async move {
                                let mut rx = rx;
                                while let Some(event) = rx.recv().await {
                                    if let Err(err) = worker
                                        .process_message(&event, worker_bindings.as_ref())
                                        .await
                                    {
                                        warn!(error = %err, chat_id = %event.chat_id, msg_id = %event.msg_id, "qqbot message processing failed");
                                    }
                                }
                            });
                            let (tx, _) = e.insert((tx, handle));
                            tx.clone()
                        }
                    };
                    if sender.try_send(message).is_err() {
                        warn!(chat_id = %chat_id, "qqbot chat queue saturated; dropping event");
                    }
                }
            }
        }

        // Shutdown: close every queue (workers drain remaining events
        // then exit on a closed channel), then join within one shared
        // grace budget — in-flight replies are NOT silently lost.
        let join_deadline = tokio::time::Instant::now() + SHUTDOWN_JOIN_GRACE;
        for (chat_id, (tx, handle)) in chats.drain() {
            drop(tx);
            let remaining = join_deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(remaining, handle).await {
                Ok(Ok(())) => {}
                Ok(Err(join_err)) => {
                    warn!(chat_id = %chat_id, error = %join_err, "qqbot chat worker panicked")
                }
                Err(_) => {
                    warn!(chat_id = %chat_id, "qqbot chat worker did not drain in time")
                }
            }
        }
        gateway_task.abort();
        info!("qqbot channel stopped");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::State, routing::post};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn strips_leading_bot_mention_exact_shape_only() {
        assert_eq!(strip_at_mention("<@!123456> hello world"), "hello world");
        // Bare @mention without the ! is user content — preserved.
        assert_eq!(strip_at_mention("<@123456> no bang"), "<@123456> no bang");
        assert_eq!(strip_at_mention("plain text"), "plain text");
        assert_eq!(strip_at_mention("<unclosed"), "<unclosed");
    }

    #[test]
    fn derives_stable_turn_id_per_chat_and_msg() {
        let a = build_turn_id("gOK", "M1").unwrap();
        let b = build_turn_id("gOK", "M1").unwrap();
        assert_eq!(a, b);
        assert_ne!(a, build_turn_id("gOK", "M2").unwrap());
        assert_ne!(a, build_turn_id("other", "M1").unwrap());
        assert!(build_turn_id("gOK", "").is_none());
    }

    #[test]
    fn parses_group_event_into_message_event() {
        let raw = json!({
            "id": "M1",
            "group_openid": "gOK",
            "content": "<@!BOTID> 你好",
            "author": {"id": "uX", "member_openid": "memberOK"}
        });
        let ev = MessageEvent::parse(QqWsEventKind::GroupAtMessage, &raw).unwrap();
        assert_eq!(ev.chat_id, "gOK");
        assert_eq!(ev.author_id, "memberOK");
        assert_eq!(ev.msg_id, "M1");
        assert_eq!(ev.text, "你好");
    }

    #[test]
    fn parses_c2c_event_with_author_identity() {
        let raw = json!({
            "id": "M2",
            "content": "hi",
            "author": {"id": "userOpenid"}
        });
        let ev = MessageEvent::parse(QqWsEventKind::C2cMessage, &raw).unwrap();
        assert_eq!(ev.chat_id, "userOpenid");
        assert_eq!(ev.author_id, "userOpenid");
        assert_eq!(ev.text, "hi");
    }

    #[test]
    fn rejects_event_missing_required_fields() {
        assert!(MessageEvent::parse(QqWsEventKind::GroupAtMessage, &json!({"id": "x"})).is_none());
    }

    fn test_adapter() -> QqBotAdapter {
        QqBotAdapter::new(QqBotAdapterOptions {
            app_id: "A".into(),
            client_secret: "S".into(),
            chat_base: "http://127.0.0.1:1".into(),
            ws_url: "ws://127.0.0.1:1/ws".into(),
            api_base: "http://127.0.0.1:1".into(),
            token_url: "http://127.0.0.1:1/token".into(),
            allowed_users: vec!["a".into()],
            accept_mode: None,
            bindings_db: std::env::temp_dir().join("qqbot-adapter-tests.db"),
            outbox_drain_interval: Duration::from_secs(60),
            audit_path: None,
        })
        .unwrap()
    }

    fn event_with(msg_id: &str) -> MessageEvent {
        MessageEvent {
            kind: QqWsEventKind::GroupAtMessage,
            chat_id: "g".into(),
            author_id: "a".into(),
            msg_id: msg_id.into(),
            text: "/status".into(),
        }
    }

    /// T072/D5 (scoped by 007 T006 to channel commands — work-request replay
    /// is the admission repository's concern): the command dedup window is
    /// bounded — inserting past SEEN_CAP evicts the oldest id.
    #[test]
    fn seen_set_evicts_oldest_beyond_cap() {
        let adapter = test_adapter();
        let mut seen = HashSet::new();
        let mut order = VecDeque::new();
        for i in 0..SEEN_CAP {
            assert!(adapter.admitted(&event_with(&format!("m{i}")), &mut seen, &mut order));
        }
        assert_eq!(seen.len(), SEEN_CAP);
        // m0 still tracked; one more admission pushes it out.
        assert!(adapter.admitted(&event_with("extra"), &mut seen, &mut order));
        assert!(!seen.contains("m0"), "oldest id must be evicted");
        assert_eq!(seen.len(), SEEN_CAP);
        // Evicted id is admissible again (window forgot it).
        assert!(
            adapter.admitted(&event_with("m0"), &mut seen, &mut order),
            "evicted id must re-enter the window"
        );
    }

    #[derive(Clone)]
    struct HitCounter(Arc<AtomicUsize>);

    async fn serve_chat_route(
        responder: impl Fn(usize) -> (axum::http::StatusCode, serde_json::Value) + Send + Sync + 'static,
    ) -> (String, Arc<AtomicUsize>) {
        // axum handlers must be Clone — wrap the behavior fn so each
        // clone shares one Arc.
        let responder = std::sync::Arc::new(responder);
        let counter = HitCounter(Arc::new(AtomicUsize::new(0)));
        let hits = Arc::clone(&counter.0);
        let app = Router::new()
            .route(
                "/api/v1/chat",
                post(move |State(st): State<HitCounter>| {
                    let responder = std::sync::Arc::clone(&responder);
                    async move {
                        let n = st.0.fetch_add(1, Ordering::SeqCst);
                        let (status, body) = responder(n);
                        (status, Json(body))
                    }
                }),
            )
            .with_state(counter);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app.into_make_service()).await;
        });
        (format!("http://{addr}"), hits)
    }

    /// T072/D5 (oracle retry matrix): a 5xx blip is retried and the
    /// second attempt succeeds.
    #[tokio::test]
    async fn bridge_recovers_after_server_error() {
        let (base, hits) = serve_chat_route(|n| match n {
            0 => (axum::http::StatusCode::SERVICE_UNAVAILABLE, json!("boom")),
            _ => (axum::http::StatusCode::OK, json!({"reply": "ok"})),
        })
        .await;
        let bridge = GatewayBridge::new(base, None).unwrap();
        let outcome = bridge
            .chat("hi", None, None, Duration::from_secs(30))
            .await
            .unwrap();
        assert_eq!(outcome.reply, "ok");
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    /// T072/D5: a 4xx is a permanent rejection — no retry attempts.
    #[tokio::test]
    async fn bridge_fails_fast_on_client_error() {
        let (base, hits) = serve_chat_route(|_| {
            (
                axum::http::StatusCode::BAD_REQUEST,
                json!({"error": "nope"}),
            )
        })
        .await;
        let bridge = GatewayBridge::new(base, None).unwrap();
        let result = bridge.chat("hi", None, None, Duration::from_secs(30)).await;
        assert!(result.is_err(), "4xx must surface immediately");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    /// CSO #1: a tiny carrier fixture capturing the Authorization
    /// header the bridge actually sends.
    async fn serve_chat_capture_auth() -> (String, Arc<std::sync::Mutex<Option<String>>>) {
        let captured: Arc<std::sync::Mutex<Option<String>>> = Arc::default();
        let cap = Arc::clone(&captured);
        let app = Router::new().route(
            "/api/v1/chat",
            post(move |headers: axum::http::HeaderMap| {
                let cap = Arc::clone(&cap);
                async move {
                    *cap.lock().unwrap() = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    (axum::http::StatusCode::OK, Json(json!({"reply": "ok"})))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app.into_make_service()).await;
        });
        (format!("http://{addr}"), captured)
    }

    #[tokio::test]
    async fn bridge_attaches_bearer_header_when_secret_present() {
        let (base, captured) = serve_chat_capture_auth().await;
        let bridge = GatewayBridge::new(base, Some("s3cret".into())).unwrap();
        let outcome = bridge
            .chat("hi", None, None, Duration::from_secs(30))
            .await
            .unwrap();
        assert_eq!(outcome.reply, "ok");
        assert_eq!(captured.lock().unwrap().as_deref(), Some("Bearer s3cret"));
    }

    #[tokio::test]
    async fn bridge_omits_bearer_header_without_secret() {
        let (base, captured) = serve_chat_capture_auth().await;
        let bridge = GatewayBridge::new(base, None).unwrap();
        let outcome = bridge
            .chat("hi", None, None, Duration::from_secs(30))
            .await
            .unwrap();
        assert_eq!(outcome.reply, "ok");
        assert!(captured.lock().unwrap().is_none());
    }

    #[test]
    fn with_gateway_bearer_wires_secret_into_bridge() {
        let adapter = test_adapter()
            .with_gateway_bearer(Some("k".into()))
            .unwrap();
        assert_eq!(adapter.bridge.bearer.as_deref(), Some("k"));
        assert!(test_adapter().bridge.bearer.is_none());
    }
}
