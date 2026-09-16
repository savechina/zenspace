//! L3 surface facade (tasks T022–T025) — one client runtime per
//! user-facing surface (`zen chat`, TUI) with turn-level link recovery.
//!
//! PURPOSE: Collapses the wire choreography every surface repeats into
//! three calls — [`SurfaceClient::ensure_session`],
//! [`SurfaceClient::search_knowledge`], and
//! [`SurfaceClient::turn_with_recovery`] — while tracking the link state
//! contracts/04 binds to UI banners: `connecting…` → `ok (v…)` →
//! `offline — memory & agent features degraded (retrying)`. Recovery is
//! reconnect-between-turns by design (design §8 open-item 1, P2 scope):
//! a dead link fails the in-flight turn visibly (FR-011/012 "never
//! silent") and the NEXT call redials via connect-or-spawn.
//!
//! USAGE: `SurfaceClient::open_default(None, "zen-chat", version).await`
//! then `turn_with_recovery(&session_id, prompt, knowledge).await`.
//! Each surface owns ONE instance (single-consumer notification demux
//! on the wrapped [`GatewayClient`]); clones share nothing — pass
//! `Arc<SurfaceClient>` when multiple tasks need the same surface.
//!
//! EXPECTED: RPC-level failures return [`SurfaceError::Rpc`] with the
//! server's catalog error while the link stays up; transport-class
//! failures (send/closed/timeout with a dead reader) mark the surface
//! [`GatewayLinkState::OfflineDegraded`] and return
//! [`SurfaceError::Offline`].
//!
//! ERRORS: `open`/`ensure_session`/`turn_with_recovery` fail with
//! [`SurfaceError::Offline`] when dial/spawn/handshake cannot establish
//! a link; knowledge and turn RPCs surface catalog codes via
//! [`SurfaceError::Rpc`]. Q3 approvals are answered by the background
//! notification pump via [`SurfaceClient::set_approval_policy`]
//! (default: deny — fail-safe).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use zen_core::types::RetrievedNote;

use crate::client::GatewayClient;
use crate::protocol::{Capabilities, RpcErrorBody};
use crate::transport::uds;

/// Interactive approval policy consulted for every Q3 request routed to
/// this surface. Returns `true` to approve. Default policy denies
/// everything (fail-safe) until a surface installs one.
pub type ApprovalPolicy = Arc<dyn Fn(&str, &serde_json::Value) -> bool + Send + Sync>;

/// One `session/event` notification, demultiplexed for UI consumption.
#[derive(Debug, Clone)]
pub struct TurnNotification {
    pub turn_id: String,
    pub seq: u64,
    pub kind: String,
    pub payload: serde_json::Value,
}

/// Client-side ceiling for one hosted turn (LLM + tool rounds). Must
/// exceed the server watchdog (`ZEN_TURN_WATCHDOG_SECS`, default 900s)
/// so the server's own guards resolve the turn before the client
/// abandons it.
pub const TURN_TIMEOUT: Duration = Duration::from_secs(960);

/// Effective turn ceiling: [`TURN_TIMEOUT`] unless
/// `ZEN_TURN_TIMEOUT_SECS` overrides it (parsed once per process).
fn turn_timeout() -> Duration {
    static OVERRIDE: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *OVERRIDE.get_or_init(|| {
        std::env::var("ZEN_TURN_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .map(Duration::from_secs)
            .unwrap_or(TURN_TIMEOUT)
    })
}

/// Link states surfaced to UI banners (contracts/04 §Degraded-mode UX,
/// FR-011/012 binding).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayLinkState {
    /// Socket dial + handshake in flight.
    Connecting,
    /// Live and handshaked; carries the negotiated server version.
    Ok(String),
    /// Transport dead; gateway-backed features degraded until the next
    /// call retries the link.
    OfflineDegraded,
}

impl GatewayLinkState {
    /// The exact banner text contracts/04 prescribes for this state.
    #[must_use]
    pub fn banner(&self) -> String {
        match self {
            Self::Connecting => "gateway: connecting…".to_string(),
            Self::Ok(version) => format!("gateway: ok (v{version})"),
            Self::OfflineDegraded => {
                "gateway: offline — memory & agent features degraded (retrying)".to_string()
            }
        }
    }
}

/// Failures surfaced by [`SurfaceClient`] operations.
#[derive(Debug)]
pub enum SurfaceError {
    /// Server returned a catalog error (method-level failure; link up).
    Rpc(RpcErrorBody),
    /// Link-level failure (dial/spawn/handshake/transport dead). The
    /// surface should show the degraded banner; the next call retries.
    Offline(String),
    /// The turn was cancelled (e.g. user pressed Esc). Not a hard
    /// failure — the surface should render a cancellation notice.
    Cancelled,
}

impl std::fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rpc(e) => write!(f, "gateway rpc error {}: {}", e.code, e.message),
            Self::Offline(msg) => write!(f, "gateway offline: {msg}"),
            Self::Cancelled => write!(f, "gateway turn cancelled"),
        }
    }
}

impl std::error::Error for SurfaceError {}

impl From<RpcErrorBody> for SurfaceError {
    fn from(e: RpcErrorBody) -> Self {
        Self::Rpc(e)
    }
}

/// One gateway link bound to a single surface, wrapping the low-level
/// [`GatewayClient`] with session/knowledge/turn choreography and
/// reconnect-between-turns recovery.
pub struct SurfaceClient {
    socket_path: PathBuf,
    embedded: std::sync::RwLock<Option<crate::client::EmbeddedGatewayGuard>>,
    client_name: String,
    client_version: String,
    capabilities: Capabilities,
    inner: tokio::sync::Mutex<Option<GatewayClient>>,
    link: std::sync::RwLock<GatewayLinkState>,
    approval_policy: std::sync::RwLock<Option<ApprovalPolicy>>,
    /// Turn ids currently in flight on THIS surface. The notification
    /// pump fail-safe denies any approval routed here for a turn this
    /// surface did not submit (SC-007: origin-only approvals).
    active_turns: std::sync::Arc<std::sync::RwLock<std::collections::HashSet<String>>>,
    events_tx: tokio::sync::broadcast::Sender<TurnNotification>,
    /// Notification pump for the CURRENT link (T056). Every redial
    /// aborts the prior pump before installing its replacement, so
    /// reconnect cycles never leak blocked pump tasks.
    pump: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl SurfaceClient {
    /// Dials the default daemon socket — attaching to a live server or
    /// embedding the gateway in this process when none answers.
    ///
    /// # Errors
    /// [`SurfaceError::Offline`] when embed/dial or the handshake fails
    /// within the readiness window.
    pub async fn open_default(
        client_name: &str,
        client_version: &str,
    ) -> Result<Self, SurfaceError> {
        Self::open(uds::default_socket_path(), client_name, client_version).await
    }

    /// Opens a link on `socket_path` (attach-or-embed), performing the
    /// initialize→initialized handshake immediately so the first turn
    /// pays no setup cost.
    ///
    /// # Errors
    /// [`SurfaceError::Offline`] when embed/dial or the handshake fails
    /// within the readiness window.
    pub async fn open(
        socket_path: PathBuf,
        client_name: &str,
        client_version: &str,
    ) -> Result<Self, SurfaceError> {
        let (events_tx, _) = tokio::sync::broadcast::channel(256);
        let surface = Self {
            socket_path,
            embedded: std::sync::RwLock::new(None),
            client_name: client_name.to_string(),
            client_version: client_version.to_string(),
            capabilities: Capabilities::default(),
            inner: tokio::sync::Mutex::new(None),
            link: std::sync::RwLock::new(GatewayLinkState::Connecting),
            approval_policy: std::sync::RwLock::new(None),
            active_turns: std::sync::Arc::new(std::sync::RwLock::new(
                std::collections::HashSet::new(),
            )),
            events_tx,
            pump: std::sync::Mutex::new(None),
        };
        surface.dial().await?;
        Ok(surface)
    }

    /// Installs the Q3 approval policy consulted by the notification
    /// pump. Without one, every approval is denied (fail-safe).
    pub fn set_approval_policy(&self, policy: ApprovalPolicy) {
        *self.approval_policy.write().expect("policy lock") = Some(policy);
    }

    /// Subscribes to demultiplexed `session/event` notifications.
    /// Lagging subscribers drop oldest frames (broadcast semantics);
    /// the merged final text always arrives in `turn_completed`.
    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<TurnNotification> {
        self.events_tx.subscribe()
    }

    /// Current link state for banner rendering.
    #[must_use]
    pub fn link_state(&self) -> GatewayLinkState {
        self.link.read().expect("link state lock poisoned").clone()
    }

    fn set_link(&self, state: GatewayLinkState) {
        *self.link.write().expect("link state lock poisoned") = state;
    }

    /// Attach-or-spawn + handshake; installs the fresh client and flips
    /// the link to `Ok(serverVersion)`.
    ///
    /// Spawns a detached daemon (setsid) instead of embedding the server
    /// in this process: a one-shot CLI that hosts the gateway dies when
    /// the command exits, EOF-ing every surface attached to it
    /// ("uds transport closed by peer"). A detached daemon outlives all
    /// clients by design (codex app-server parity: exactly one server
    /// process, surfaces come and go).
    async fn dial(&self) -> Result<GatewayClient, SurfaceError> {
        self.set_link(GatewayLinkState::Connecting);
        // T055: probe-enabled dial — handshake plus serverVersion check;
        // an incompatible daemon is restarted once before this returns.
        let client = GatewayClient::connect_or_spawn_with_probe(
            &self.socket_path,
            None,
            &self.client_name,
            &self.client_version,
            self.capabilities,
        )
        .await
        .map_err(|e| {
            self.set_link(GatewayLinkState::OfflineDegraded);
            SurfaceError::Offline(e.to_string())
        })?;
        // Invariant: never take embedded ownership here — see dial() doc.
        *self.embedded.write().expect("embedded lock poisoned") = None;
        let version = crate::protocol::SERVER_PROTOCOL_VERSION.to_string();
        self.set_link(GatewayLinkState::Ok(version));
        *self.inner.lock().await = Some(client.clone());
        self.spawn_notification_pump(client.clone());
        Ok(client)
    }

    /// Background demux loop for one live link: answers Q3 approval
    /// requests through the installed policy (default deny) and fans
    /// `session/event` notifications out to subscribers. Exits when the
    /// reader dies; a later redial ABORTS this pump before installing
    /// its replacement (T056 — orphaned pumps block forever on their
    /// still-open notification channel). Returns the new pump's
    /// [`tokio::task::AbortHandle`] for liveness observation.
    fn spawn_notification_pump(&self, client: GatewayClient) -> tokio::task::AbortHandle {
        {
            let mut slot = self.pump.lock().expect("pump slot poisoned");
            if let Some(prior) = slot.take() {
                prior.abort();
            }
        }
        let policy_slot = {
            let policy = self.approval_policy.read().expect("policy lock");
            policy.clone()
        };
        let active_turns = Arc::clone(&self.active_turns);
        let events_tx = self.events_tx.clone();
        let handle = tokio::spawn(async move {
            let mut client = client;
            loop {
                let frame = match client.next_notification().await {
                    Ok(f) => f,
                    Err(_) => break,
                };
                match frame {
                    crate::protocol::Frame::ServerRequest {
                        id, method, params, ..
                    } if method == "approval/request" => {
                        let foreign_turn = params["turnId"].as_str().is_some_and(|t| {
                            !active_turns.read().expect("active turns lock").contains(t)
                        });
                        let name = params
                            .get("invocation")
                            .and_then(|i| i.get("name"))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown")
                            .to_string();
                        let args = params
                            .get("invocation")
                            .and_then(|i| i.get("args"))
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        let approve = !foreign_turn
                            && policy_slot
                                .as_ref()
                                .is_some_and(|policy| policy(&name, &args));
                        if foreign_turn {
                            tracing::warn!(
                                %id,
                                tool = %name,
                                "denied approval routed for a turn not owned by this surface"
                            );
                        }
                        tracing::info!(%id, tool = %name, approve, "approval decided");
                        let decision = if approve { "approve" } else { "deny" };
                        let _ = client
                            .respond(&id, serde_json::json!({ "decision": decision }))
                            .await;
                    }
                    crate::protocol::Frame::Notification { method, params, .. }
                        if method == "session/event" =>
                    {
                        let event = TurnNotification {
                            turn_id: params["turnId"].as_str().unwrap_or_default().into(),
                            seq: params["seq"].as_u64().unwrap_or(0),
                            kind: params["kind"].as_str().unwrap_or_default().into(),
                            payload: params,
                        };
                        let _ = events_tx.send(event);
                    }
                    _ => {}
                }
            }
        });
        let abort = handle.abort_handle();
        *self.pump.lock().expect("pump slot poisoned") = Some(handle);
        abort
    }

    /// Returns a live client, redialing when the cached one is dead
    /// (reconnect-between-turns entry point).
    async fn ensure_link(&self) -> Result<GatewayClient, SurfaceError> {
        let mut guard = self.inner.lock().await;
        if let Some(client) = guard.as_ref()
            && client.is_connected()
        {
            return Ok(client.clone());
        }
        *guard = None;
        drop(guard);
        let client = self.dial().await?;
        Ok(client)
    }

    /// Drops the cached client after a transport-class failure and
    /// flags the surface degraded.
    async fn mark_offline(&self, reason: &str) -> SurfaceError {
        *self.inner.lock().await = None;
        self.set_link(GatewayLinkState::OfflineDegraded);
        SurfaceError::Offline(reason.to_string())
    }

    /// Classifies an RPC failure: with the reader still alive it is a
    /// server-side catalog error; otherwise the link is dead and the
    /// surface goes degraded.
    async fn classify(&self, client: &GatewayClient, err: RpcErrorBody) -> SurfaceError {
        if client.is_connected() {
            SurfaceError::Rpc(err)
        } else {
            self.mark_offline(&err.message).await
        }
    }

    /// session/start — registers (or adopts) `session_id` daemon-side,
    /// returning the authoritative id.
    ///
    /// # Errors
    /// [`SurfaceError::Rpc`] on catalog errors;
    /// [`SurfaceError::Offline`] when the link cannot be established.
    pub async fn ensure_session(
        &self,
        session_id: Option<&str>,
        agent: Option<&str>,
    ) -> Result<String, SurfaceError> {
        let client = self.ensure_link().await?;
        let mut params = serde_json::Map::new();
        if let Some(id) = session_id {
            params.insert("sessionId".into(), serde_json::json!(id));
        }
        if let Some(agent) = agent {
            params.insert("agent".into(), serde_json::json!(agent));
        }
        let result = match client
            .request("session/start", serde_json::Value::Object(params))
            .await
        {
            Ok(result) => result,
            Err(e) => return Err(self.classify(&client, e).await),
        };
        result
            .get("sessionId")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                SurfaceError::Rpc(RpcErrorBody {
                    code: -32603,
                    name: "internal".into(),
                    message: "session/start result missing sessionId".into(),
                    data: None,
                })
            })
    }

    /// knowledge/search — server-side 5-tier retrieval replacing the
    /// legacy client-side SearchService block (contracts/02 §Knowledge).
    /// `tiers = None` lets the daemon auto-select.
    ///
    /// # Errors
    /// [`SurfaceError::Rpc`] on catalog errors;
    /// [`SurfaceError::Offline`] when the link cannot be established.
    pub async fn search_knowledge(
        &self,
        query: &str,
        tiers: Option<&[&str]>,
        limit: u64,
    ) -> Result<Vec<RetrievedNote>, SurfaceError> {
        let client = self.ensure_link().await?;
        let mut params = serde_json::Map::new();
        params.insert("query".into(), serde_json::json!(query));
        params.insert("limit".into(), serde_json::json!(limit));
        if let Some(tiers) = tiers {
            params.insert("tiers".into(), serde_json::json!(tiers));
        }
        let result = match client
            .request("knowledge/search", serde_json::Value::Object(params))
            .await
        {
            Ok(result) => result,
            Err(e) => return Err(self.classify(&client, e).await),
        };
        let notes = result
            .get("notes")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut parsed = Vec::with_capacity(notes.len());
        for note in notes {
            let path = note
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let content = note
                .get("content")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let sensitivity = note
                .get("sensitivity")
                .and_then(serde_json::Value::as_str)
                .and_then(crate::server::hosting::parse_sensitivity)
                .unwrap_or(zen_core::types::Sensitivity::Public);
            let relevance = note.get("relevance").and_then(serde_json::Value::as_f64);
            parsed.push(RetrievedNote {
                path,
                content,
                sensitivity,
                relevance: relevance.unwrap_or(1.0),
            });
        }
        Ok(parsed)
    }

    /// session/turn with a client-minted turnId, the full turn budget,
    /// and reconnect-between-turns semantics: a dead link fails this
    /// turn as [`SurfaceError::Offline`] (banner-visible, FR-011/012)
    /// and the next call redials.
    ///
    /// # Errors
    /// [`SurfaceError::Rpc`] on catalog errors (including turn
    /// failures with the link up);
    /// [`SurfaceError::Offline`] when the link is dead or dies
    /// mid-turn.
    pub async fn turn_with_recovery(
        &self,
        session_id: &str,
        prompt: &str,
        knowledge: Vec<RetrievedNote>,
    ) -> Result<String, SurfaceError> {
        // One logical turn = at most two attempts on ONE server-side id:
        // if the link died under us (embedded owner drained, daemon
        // restarted), the fresh-link retry stays idempotent.
        let turn_id = format!("turn-{}", uuid::Uuid::now_v7());
        self.active_turns
            .write()
            .expect("active turns lock")
            .insert(turn_id.clone());
        let outcome = {
            let run = || self.hosted_turn_once(session_id, &turn_id, prompt, &knowledge);
            match run().await {
                Ok(response) => Ok(response),
                Err(SurfaceError::Offline(reason)) => {
                    tracing::warn!(
                        turn_id = %turn_id,
                        %reason,
                        "turn link died mid-flight; redialing once"
                    );
                    self.ensure_link().await?;
                    run().await
                }
                // Session lived on an owner that died between registration
                // and this request: re-register on the live link, retry.
                Err(SurfaceError::Rpc(e)) if e.code == -32003 => {
                    tracing::warn!(
                        turn_id = %turn_id,
                        "session vanished with its owner; re-registering once"
                    );
                    run().await
                }
                // Server returned "turn cancelled" (-32603) — a clean,
                // well-defined cancellation signal (Esc-to-interrupt).
                Err(SurfaceError::Rpc(e))
                    if e.code == -32603 && e.message.contains("cancelled") =>
                {
                    Err(SurfaceError::Cancelled)
                }
                Err(other) => Err(other),
            }
        };
        self.active_turns
            .write()
            .expect("active turns lock")
            .remove(&turn_id);
        outcome
    }

    /// Sends `session/cancel` for every turn currently tracked as
    /// in-flight on this surface. Advisory: cancelling a terminal or
    /// unknown turn is a no-op server-side.
    ///
    /// Returns the number of cancel requests that were successfully
    /// dispatched (individual RPC failures are logged and skipped).
    ///
    /// Safe to call concurrently with an in-flight
    /// [`turn_with_recovery`] — the active-turn snapshot is taken
    /// under a short read lock before any RPC is sent.
    pub async fn cancel_active_turns(&self) -> usize {
        // Snapshot active turn IDs under a brief read lock — never
        // hold across await.
        let turn_ids: Vec<String> = {
            let active = self.active_turns.read().expect("active turns lock");
            active.iter().cloned().collect()
        };
        if turn_ids.is_empty() {
            return 0;
        }
        let client = match self.ensure_link().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "cancel_active_turns: link unavailable");
                return 0;
            }
        };
        let mut count = 0;
        for turn_id in &turn_ids {
            let params = serde_json::json!({ "turnId": turn_id });
            match client.request("session/cancel", params).await {
                Ok(_) => {
                    count += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        turn_id = %turn_id,
                        error = %e,
                        "cancel_active_turns: RPC failed for turn"
                    );
                }
            }
        }
        count
    }

    /// One dial+send of `session/turn`; recovery layers on top of this.
    async fn hosted_turn_once(
        &self,
        session_id: &str,
        turn_id: &str,
        prompt: &str,
        knowledge: &[RetrievedNote],
    ) -> Result<String, SurfaceError> {
        let authoritative_id = self.ensure_session(Some(session_id), None).await?;
        let client = self.ensure_link().await?;
        let params = serde_json::json!({
            "turnId": turn_id,
            "sessionId": authoritative_id,
            "prompt": prompt,
            "knowledge": knowledge,
        });
        let result = match client
            .request_timeout("session/turn", params, turn_timeout())
            .await
        {
            Ok(result) => result,
            Err(e) => {
                return Err(self.classify(&client, e).await);
            }
        };
        result
            .get("response")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                SurfaceError::Rpc(RpcErrorBody {
                    code: -32603,
                    name: "internal".into(),
                    message: "session/turn result missing response".into(),
                    data: None,
                })
            })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::server::stub_server;
    use crate::transport::uds;

    /// Serves one canned dispatcher connection that answers
    /// session/start by echoing the requested id and session/turn by
    /// echoing the prompt prefixed with the injected knowledge count.
    async fn serve_canned(socket: &Path) {
        let listener = uds::bind_socket(socket).await.unwrap();
        tokio::spawn(async move {
            loop {
                let Ok(side) = uds::accept_transport(&listener).await else {
                    return;
                };
                let server = stub_server(side)
                    .unwrap()
                    .handle("session/start", |params| async move {
                        let id = params
                            .get("sessionId")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("minted")
                            .to_string();
                        Ok(serde_json::json!({"sessionId": id, "agent": "auto"}))
                    })
                    .unwrap()
                    .handle("session/turn", |params| async move {
                        let prompt = params
                            .get("prompt")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let knowledge = params
                            .get("knowledge")
                            .and_then(serde_json::Value::as_array)
                            .map(Vec::len)
                            .unwrap_or(0);
                        Ok(serde_json::json!({
                            "turnId": "t",
                            "response": format!("{prompt} (knowledge={knowledge})"),
                        }))
                    })
                    .unwrap();
                let _ = server.run().await;
            }
        });
    }

    async fn open_test_surface(socket: &Path) -> SurfaceClient {
        SurfaceClient::open(socket.to_path_buf(), "surface-test", "0.0")
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn ensure_session_adopts_requested_id() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("a.sock");
        serve_canned(&sock).await;
        let surface = open_test_surface(&sock).await;
        assert_eq!(
            surface.link_state().banner(),
            format!(
                "gateway: ok (v{})",
                crate::protocol::SERVER_PROTOCOL_VERSION
            )
        );
        let id = surface.ensure_session(Some("tui-1"), None).await.unwrap();
        assert_eq!(id, "tui-1");
    }

    #[tokio::test]
    async fn turn_round_trips_knowledge_and_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("b.sock");
        serve_canned(&sock).await;
        let surface = open_test_surface(&sock).await;

        let notes = vec![RetrievedNote {
            path: "inbox/x.md".into(),
            content: "payload".into(),
            sensitivity: zen_core::types::Sensitivity::Public,
            relevance: 1.0,
        }];
        let response = surface
            .turn_with_recovery("s1", "hello", notes)
            .await
            .unwrap();
        assert_eq!(response, "hello (knowledge=1)");
    }

    #[tokio::test]
    async fn midflight_offline_retries_once_with_same_turn_id() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("d.sock");
        let listener = std::sync::Arc::new(uds::bind_socket(&sock).await.unwrap());
        let seen: std::sync::Arc<std::sync::Mutex<Vec<String>>> = std::sync::Arc::default();

        // Connection 1: handshake + session/start, then the turn handler
        // records the turnId and stalls forever — the test kills the
        // socket's write side (owner dying mid-turn). A try_clone'd FD
        // shares the open-file-description, so shutdown() on it closes
        // the connection the server holds.
        let seen1 = std::sync::Arc::clone(&seen);
        let listener1 = std::sync::Arc::clone(&listener);
        let (killer_tx, killer_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (stream, _) = listener1.accept().await.unwrap();
            let side = std::sync::Arc::new(uds::UdsTransport::from_stream(stream).unwrap());
            let _ = killer_tx.send(std::sync::Arc::clone(&side));
            let server = crate::server::dispatch::DispatchServer::from_arc(side)
                .handle("session/start", |params| async move {
                    let id = params
                        .get("sessionId")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("minted")
                        .to_string();
                    Ok(serde_json::json!({"sessionId": id, "agent": "auto"}))
                })
                .unwrap()
                .handle("session/turn", move |params| {
                    let seen1 = std::sync::Arc::clone(&seen1);
                    async move {
                        seen1.lock().unwrap().push(
                            params
                                .get("turnId")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        );
                        loop {
                            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                        }
                    }
                })
                .unwrap();
            let _ = server.run().await;
        });

        let surface = std::sync::Arc::new(open_test_surface(&sock).await);
        let turn_surface = std::sync::Arc::clone(&surface);
        let turn = tokio::spawn(async move {
            turn_surface
                .turn_with_recovery("s1", "hello", Vec::new())
                .await
        });

        for _ in 0..500 {
            if !seen.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        assert_eq!(seen.lock().unwrap().len(), 1, "first attempt observed");
        killer_rx
            .await
            .unwrap()
            .shutdown_write()
            .await
            .expect("kill conn1 write side");

        // The retry redials the still-live listener; connection 2
        // answers normally and records the same turnId again.
        let seen2 = std::sync::Arc::clone(&seen);
        let side = uds::accept_transport(&listener).await.unwrap();
        tokio::spawn(async move {
            let server = stub_server(side)
                .unwrap()
                .handle("session/start", |params| async move {
                    let id = params
                        .get("sessionId")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("minted")
                        .to_string();
                    Ok(serde_json::json!({"sessionId": id, "agent": "auto"}))
                })
                .unwrap()
                .handle("session/turn", move |params| {
                    let seen2 = std::sync::Arc::clone(&seen2);
                    async move {
                        seen2.lock().unwrap().push(
                            params
                                .get("turnId")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        );
                        Ok(serde_json::json!({"turnId": "t", "response": "recovered"}))
                    }
                })
                .unwrap();
            let _ = server.run().await;
        });

        let response = turn.await.unwrap().unwrap();
        assert_eq!(response, "recovered");
        let ids = seen.lock().unwrap().clone();
        assert_eq!(ids.len(), 2, "exactly one retry, both attempts observed");
        assert_eq!(ids[0], ids[1], "retry must reuse the same turnId");
        assert!(matches!(surface.link_state(), GatewayLinkState::Ok(_)));
    }

    #[tokio::test]
    async fn dead_peer_flags_offline_then_recovers_between_turns() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("c.sock");

        // A peer that accepts then dies: open must fail fast with an
        // Offline-class error (never a hang, never silent).
        let listener = uds::bind_socket(&sock).await.unwrap();
        tokio::spawn(async move {
            if let Ok(side) = uds::accept_transport(&listener).await {
                drop(side);
            }
        });
        let err = SurfaceClient::open(sock.clone(), "recovery-test", "0.0")
            .await
            .err()
            .expect("dead peer must fail open");
        assert!(matches!(err, SurfaceError::Offline(_)), "got: {err}");

        // Simulate a surface that went offline mid-session (cached link
        // dropped, banner degraded): the next turn redials and succeeds
        // against a fresh server on the same path.
        std::fs::remove_file(&sock).unwrap();
        serve_canned(&sock).await;
        let surface = SurfaceClient {
            socket_path: sock,
            embedded: std::sync::RwLock::new(None),
            client_name: "recovery-test".to_string(),
            client_version: "0.0".to_string(),
            capabilities: Capabilities::default(),
            inner: tokio::sync::Mutex::new(None),
            link: std::sync::RwLock::new(GatewayLinkState::OfflineDegraded),
            approval_policy: std::sync::RwLock::new(None),
            active_turns: std::sync::Arc::new(std::sync::RwLock::new(
                std::collections::HashSet::new(),
            )),
            events_tx: tokio::sync::broadcast::channel(16).0,
            pump: std::sync::Mutex::new(None),
        };
        assert_eq!(
            surface.link_state(),
            GatewayLinkState::OfflineDegraded,
            "banner must reflect degraded state"
        );
        let response = surface
            .turn_with_recovery("s", "again", Vec::new())
            .await
            .unwrap();
        assert_eq!(response, "again (knowledge=0)");
        assert!(matches!(surface.link_state(), GatewayLinkState::Ok(_)));
    }

    /// T056: repeated redials must not leak notification pumps. A pump
    /// whose link was replaced stays blocked on its (still-open)
    /// notification channel forever unless the replacement ABORTS it —
    /// proven here by requiring the first pump to terminate (its link
    /// stays alive, so termination can only come from the abort).
    #[tokio::test]
    async fn redial_aborts_prior_notification_pump() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("pump.sock");
        // Concurrent accept loop (serve_canned serves connections
        // sequentially; this test needs TWO links open at once).
        let listener = uds::bind_socket(&sock).await.unwrap();
        tokio::spawn(async move {
            loop {
                if let Ok(side) = uds::accept_transport(&listener).await
                    && let Ok(server) = stub_server(side)
                {
                    tokio::spawn(async move {
                        let _ = server.run().await;
                    });
                }
            }
        });
        let surface = open_test_surface(&sock).await;

        async fn dial_test_link(sock: &Path, name: &str) -> GatewayClient {
            let client = GatewayClient::connect(sock).await.unwrap();
            client
                .handshake(name, "0.0", Capabilities::default())
                .await
                .unwrap();
            client
        }
        let first = dial_test_link(&sock, "pump-one").await;
        let first_pump = surface.spawn_notification_pump(first);
        assert!(
            !first_pump.is_finished(),
            "pump #1 must run while its link is alive"
        );

        // Redial (what ensure_link does after a dead link): the second
        // pump replaces the first through the same path dial() uses.
        let second = dial_test_link(&sock, "pump-two").await;
        let second_pump = surface.spawn_notification_pump(second);
        assert!(!second_pump.is_finished(), "pump #2 must run");

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !first_pump.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "first pump leaked: still running after replacement"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            !surface
                .pump
                .lock()
                .expect("pump slot poisoned")
                .as_ref()
                .is_some_and(tokio::task::JoinHandle::is_finished),
            "replacement pump must stay live for the next redial"
        );
    }

    /// cancel_active_turns returns 0 when no turns are in flight.
    #[tokio::test]
    async fn cancel_no_active_turns_returns_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("cancel_zero.sock");
        serve_canned(&sock).await;
        let surface = open_test_surface(&sock).await;
        let count = surface.cancel_active_turns().await;
        assert_eq!(count, 0);
    }

    /// cancel_active_turns sends session/cancel for each tracked turn
    /// and returns the count of successful cancels.
    #[tokio::test]
    async fn cancel_active_turn_sends_rpc_and_returns_count() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("cancel_one.sock");
        let listener = uds::bind_socket(&sock).await.unwrap();

        // Shared state: the turn handler waits on this notify;
        // the cancel handler signals it.
        let cancel_notify = std::sync::Arc::new(tokio::sync::Notify::new());
        let cancel_seen = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let notify_clone = cancel_notify.clone();
        let seen_clone = cancel_seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok(side) = uds::accept_transport(&listener).await else {
                    return;
                };
                let n1 = notify_clone.clone();
                let n2 = notify_clone.clone();
                let s1 = seen_clone.clone();
                let server = crate::server::dispatch::DispatchServer::new(side)
                    .handle("session/start", |params| async move {
                        let id = params
                            .get("sessionId")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("minted")
                            .to_string();
                        Ok(serde_json::json!({"sessionId": id, "agent": "auto"}))
                    })
                    .unwrap()
                    .handle("session/turn", move |_params| {
                        let n = n1.clone();
                        async move {
                            tokio::time::timeout(std::time::Duration::from_secs(10), n.notified())
                                .await
                                .map_err(|_| {
                                    crate::protocol::RpcError::internal(
                                        "timeout waiting for cancel",
                                    )
                                })?;
                            Err(crate::protocol::RpcError::internal("turn cancelled"))
                        }
                    })
                    .unwrap()
                    .handle("session/cancel", move |_params| {
                        let n = n2.clone();
                        let s = s1.clone();
                        async move {
                            s.store(true, std::sync::atomic::Ordering::SeqCst);
                            n.notify_one();
                            Ok(serde_json::json!({"outcome": "cancelled"}))
                        }
                    })
                    .unwrap();
                let _ = server.run().await;
            }
        });

        let surface = std::sync::Arc::new(open_test_surface(&sock).await);

        // Start a turn that will stall.
        let turn_surface = std::sync::Arc::clone(&surface);
        let turn_handle = tokio::spawn(async move {
            turn_surface
                .turn_with_recovery("s1", "hello", Vec::new())
                .await
        });

        // Wait until the turn is registered in active_turns.
        for _ in 0..500 {
            if !surface.active_turns.read().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            !surface.active_turns.read().unwrap().is_empty(),
            "turn must be registered before cancel"
        );

        // Cancel should send session/cancel and return 1.
        let count = surface.cancel_active_turns().await;
        assert_eq!(count, 1, "must report one successful cancel");

        // The server must have received the cancel.
        for _ in 0..100 {
            if cancel_seen.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            cancel_seen.load(std::sync::atomic::Ordering::SeqCst),
            "server must have received session/cancel"
        );

        // The turn must complete (not hang).
        let result = tokio::time::timeout(Duration::from_secs(5), turn_handle)
            .await
            .expect("turn must not hang after cancel")
            .expect("turn task must not panic");

        // After cancel, turn_with_recovery returns SurfaceError::Cancelled.
        assert!(
            matches!(result, Err(SurfaceError::Cancelled)),
            "cancelled turn must return SurfaceError::Cancelled, got: {result:?}"
        );
    }

    /// turn_with_recovery returns Cancelled (not a hard Rpc error) when
    /// the server cancels the turn — this is the shape the TUI agent
    /// needs to render "⚠ cancelled".
    #[tokio::test]
    async fn cancelled_turn_returns_surface_cancelled_not_rpc_error() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("cancel_err.sock");
        let listener = uds::bind_socket(&sock).await.unwrap();

        let cancel_notify = std::sync::Arc::new(tokio::sync::Notify::new());
        let notify_clone = cancel_notify.clone();
        tokio::spawn(async move {
            loop {
                let Ok(side) = uds::accept_transport(&listener).await else {
                    return;
                };
                let n1 = notify_clone.clone();
                let n2 = notify_clone.clone();
                let server = crate::server::dispatch::DispatchServer::new(side)
                    .handle("session/start", |params| async move {
                        let id = params
                            .get("sessionId")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("minted")
                            .to_string();
                        Ok(serde_json::json!({"sessionId": id, "agent": "auto"}))
                    })
                    .unwrap()
                    .handle("session/turn", move |_params| {
                        let n = n1.clone();
                        async move {
                            tokio::time::timeout(std::time::Duration::from_secs(10), n.notified())
                                .await
                                .map_err(|_| crate::protocol::RpcError::internal("timeout"))?;
                            Err(crate::protocol::RpcError::internal("turn cancelled"))
                        }
                    })
                    .unwrap()
                    .handle("session/cancel", move |_params| {
                        let n = n2.clone();
                        async move {
                            n.notify_one();
                            Ok(serde_json::json!({"outcome": "cancelled"}))
                        }
                    })
                    .unwrap();
                let _ = server.run().await;
            }
        });

        let surface = std::sync::Arc::new(open_test_surface(&sock).await);
        let turn_surface = std::sync::Arc::clone(&surface);
        let turn_handle = tokio::spawn(async move {
            turn_surface
                .turn_with_recovery("s1", "prompt", Vec::new())
                .await
        });

        // Wait for turn registration.
        for _ in 0..500 {
            if !surface.active_turns.read().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        // Cancel.
        surface.cancel_active_turns().await;

        // Verify the error type is Cancelled, not Rpc.
        let result = tokio::time::timeout(Duration::from_secs(5), turn_handle)
            .await
            .expect("turn must not hang")
            .expect("task must not panic");

        match &result {
            Err(SurfaceError::Cancelled) => {} // expected
            other => panic!("expected SurfaceError::Cancelled, got: {other:?}"),
        }
    }

    /// Advisory double-cancel is safe: calling cancel_active_turns twice
    /// does not panic or return an error.
    #[tokio::test]
    async fn advisory_double_cancel_is_safe() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("cancel_double.sock");
        serve_canned(&sock).await;
        let surface = open_test_surface(&sock).await;

        // No active turns — both calls return 0 without RPC errors.
        let c1 = surface.cancel_active_turns().await;
        let c2 = surface.cancel_active_turns().await;
        assert_eq!(c1, 0);
        assert_eq!(c2, 0);
    }
}
