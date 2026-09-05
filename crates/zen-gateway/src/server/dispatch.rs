//! Registry-driven dispatch server — routes Q1 frames through the method
//! registry and a handler map, enforces the handshake gate, and bridges
//! Q3 approval requests with timeout (tasks T007/T008 support).
//!
//! PURPOSE: One dispatcher serves both the contract suite (fake handlers)
//! and the real daemon (business handlers installed per method). Protocol
//! behavior lives here; business logic never does.
//!
//! USAGE: Build via [`DispatchServer::new`] on a server-side transport,
//! install handlers with [`DispatchServer::handle`], then run
//! [`DispatchServer::run`] on a spawned task. Server-initiated requests go
//! through [`ConnectionHandle::request_approval`].
//!
//! EXPECTED: pre-initialize requests → -32000; unknown methods → -32601
//! with `supportedMethods`; unknown notifications silently ignored; the
//! `initialized` notification flips the connection to `Initialized`.
//!
//! ERRORS: Handler failures surface as Q2 error frames using catalog
//! codes; transport close ends the run loop cleanly.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::oneshot;
use tracing::{debug, warn};

use crate::protocol::{
    Capabilities, Frame, HandshakeState, MethodRegistry, RpcError, RpcErrorBody,
    SERVER_PROTOCOL_VERSION, handle_initialize,
};
use crate::server::connection::ClientIdentity;
use crate::transport::Transport;

/// Default Q3 deadline (data-model E7 ApprovalTimeout: 120s).
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(120);

/// Methods whose handlers run on a spawned task instead of inline:
/// `session/turn` must never block the recv loop (a hosted turn lasts
/// seconds-to-minutes and `session/cancel` must stay responsive);
/// slow-pool methods are bounded reads that must not head-of-line
/// block turn traffic (design §6 SlowMethodPool).
/// Handlers that outlive or stall past one recv-loop iteration — run on
/// their own task so `session/cancel` and heartbeat `health/status` on
/// the same connection are never head-of-line blocked behind them.
///
/// # EOF contract (hold-transport)
///
/// Every detached task spawned for these methods holds an
/// `Arc<dyn Transport>` clone for its response send, so the carrier fd
/// stays open for as long as any of them lives. Aborting the dispatch
/// run loop ([`DispatchServer::run`]) therefore can NOT close the
/// connection and can NOT deliver EOF to the client — it only stops
/// new inbound frames from being read. The SOLE EOF path for a client
/// waiting on a spawned method is the server-side write half-close
/// `UdsTransport::shutdown_write` (`transport/uds.rs`), which is what
/// connection owners must pair with the abort: the surface client kill
/// path (`client/surface.rs`) and the crash-recovery suite
/// (`tests/crash_recovery.rs`) both do `abort` + `shutdown_write`.
/// Contract proven mechanically by `tests/dispatch_shutdown.rs`. The
/// spawn model itself is intentional — responsiveness (an unblocked
/// recv loop) beats owned teardown (prior learning
/// dispatch-spawned-methods-hold-transport).
const SPAWNED_METHODS: &[&str] = &["session/turn", "knowledge/search", "memory/putEntry"];
const SLOW_POOL_METHODS: &[&str] = &[
    "health/status",
    "agent/list",
    "agent/status",
    "skill/list",
    "memory/stats",
    "memory/search",
];
/// Concurrency cap for slow-pool offload per connection.
const SLOW_POOL_LIMIT: usize = 4;

/// Server-side handler: takes method params, returns a result value or a
/// catalog error. Installed per method name via [`DispatchServer::handle`].
pub type HandlerFn = Arc<
    dyn Fn(
            serde_json::Value,
        ) -> futures_util::future::BoxFuture<'static, Result<serde_json::Value, RpcError>>
        + Send
        + Sync,
>;

/// Outcome of a Q3 request: the client's Q4 result body, or a catalog error.
pub type ApprovalOutcome = Result<serde_json::Value, RpcError>;

type ClientReadyHook = Box<dyn Fn(ClientIdentity, Capabilities) + Send + Sync>;

struct Shared {
    state: tokio::sync::Mutex<HandshakeState>,
    capabilities: tokio::sync::Mutex<Capabilities>,
    pending: tokio::sync::Mutex<HashMap<String, oneshot::Sender<ApprovalOutcome>>>,
    next_server_id: tokio::sync::Mutex<u64>,
    last_handshake: std::sync::Mutex<Option<(ClientIdentity, Capabilities)>>,
    on_client_ready: std::sync::Mutex<Option<ClientReadyHook>>,
}
/// Registry-driven dispatcher routing Q1 frames through the method
/// registry and a handler map. Carrier-agnostic: holds the server-side
/// endpoint as `Arc<dyn Transport>` (object-safe via `async_trait`), so
/// in-process, UDS, and future carriers share one code path without
/// generic plumbing.
pub struct DispatchServer {
    transport: Arc<dyn Transport>,
    handlers: HashMap<&'static str, HandlerFn>,
    shared: Arc<Shared>,
    /// Bounded offload pool for slow read methods (design §6).
    slow_pool: Arc<tokio::sync::Semaphore>,
}

impl DispatchServer {
    /// Creates a dispatcher over any carrier, boxing it behind the
    /// object-safe trait (generic sugar for [`DispatchServer::from_arc`]).
    pub fn new<T: Transport + 'static>(transport: T) -> Self {
        Self::from_arc(Arc::new(transport))
    }

    /// Creates a dispatcher over an already-shared carrier endpoint.
    pub fn from_arc(transport: Arc<dyn Transport>) -> Self {
        Self {
            transport,
            handlers: HashMap::new(),
            slow_pool: Arc::new(tokio::sync::Semaphore::new(SLOW_POOL_LIMIT)),
            shared: Arc::new(Shared {
                state: HandshakeState::Connecting.into(),
                capabilities: Capabilities::default().into(),
                pending: HashMap::new().into(),
                next_server_id: 0.into(),
                last_handshake: None.into(),
                on_client_ready: None.into(),
            }),
        }
    }

    /// Registers a callback fired once per connection when the
    /// `initialized` notification completes the handshake (E2
    /// Connecting→Initialized). Carries the client identity and
    /// negotiated capabilities from the preceding successful
    /// `initialize`. The daemon uses this to flip its E2
    /// [`ClientConnection`](crate::server::connection::ClientConnection)
    /// record; protocol behavior is unchanged when unset.
    pub fn on_client_ready(
        self,
        hook: impl Fn(ClientIdentity, Capabilities) + Send + Sync + 'static,
    ) -> Self {
        self.shared
            .on_client_ready
            .lock()
            .expect("on_client_ready lock poisoned")
            .replace(Box::new(hook));
        self
    }

    /// Installs (or replaces) the handler for a registered method.
    /// Handler names not present in [`MethodRegistry`] are rejected —
    /// dispatch and the registry can never drift apart.
    ///
    /// # Errors
    /// Returns an error naming the unregistered method.
    pub fn handle<F, Fut>(mut self, method: &'static str, handler: F) -> anyhow::Result<Self>
    where
        F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<serde_json::Value, RpcError>> + Send + 'static,
    {
        if MethodRegistry::lookup(method).is_none() {
            anyhow::bail!("cannot register handler for unregistered method {method:?}");
        }
        let handler = Arc::new(move |params| {
            let fut = handler(params);
            Box::pin(fut) as futures_util::future::BoxFuture<'static, _>
        });
        self.handlers.insert(method, handler);
        Ok(self)
    }

    /// Runs the receive loop until the transport closes. Every inbound
    /// frame is answered inline (single connection owner — the daemon
    /// spawns one dispatcher per accepted connection).
    ///
    /// # Errors
    /// Returns an error only on outbound send failure; a peer-side close
    /// ends the loop with `Ok(())`.
    pub async fn run(self) -> anyhow::Result<()> {
        let this = Arc::new(self);
        loop {
            let frame = match this.transport.recv().await {
                Ok(f) => f,
                Err(_) => {
                    debug!("dispatch server: transport closed");
                    return Ok(());
                }
            };
            if let Err(e) = Self::on_frame(&this, frame).await {
                warn!("dispatch server outbound error: {e:#}");
                return Err(e);
            }
        }
    }

    async fn on_frame(this: &Arc<Self>, frame: Frame) -> anyhow::Result<()> {
        match frame {
            Frame::ClientRequest {
                id, method, params, ..
            } => {
                if SPAWNED_METHODS.contains(&method.as_str()) {
                    // Hosted turns outlive the recv loop; respond async.
                    // EOF CONTRACT: this detached task clones the
                    // Arc<dyn Transport>, so it pins the carrier fd even
                    // after `run` is aborted — delivering EOF to the
                    // client requires the server-side write half-close
                    // `UdsTransport::shutdown_write`; see the
                    // SPAWNED_METHODS EOF contract.
                    let task = Arc::clone(this);
                    tokio::spawn(async move {
                        let response = Self::dispatch_request(&task, id, &method, params).await;
                        if let Err(e) = task.transport.send(response).await {
                            warn!(%id, "spawned dispatch send failed: {e}");
                        }
                    });
                    return Ok(());
                }
                let slow = SLOW_POOL_METHODS.contains(&method.as_str());
                if slow && this.slow_pool.available_permits() == 0 {
                    // Bounded pool full: shed load rather than queue
                    // unboundedly behind other slow methods.
                    return this
                        .transport
                        .send(Frame::error_response(id, RpcError::rate_limited(1000)))
                        .await;
                }
                let _permit = if slow {
                    Some(this.slow_pool.acquire().await)
                } else {
                    None
                };
                let response = Self::dispatch_request(this, id, &method, params).await;
                drop(_permit);
                this.transport.send(response).await
            }
            Frame::Notification { method, .. } => {
                if method == "initialized" {
                    *this.shared.state.lock().await = HandshakeState::Initialized;
                    let ready = this
                        .shared
                        .last_handshake
                        .lock()
                        .expect("handshake lock")
                        .take();
                    if let (Some(hook), Some((identity, caps))) = (
                        this.shared
                            .on_client_ready
                            .lock()
                            .expect("hook lock")
                            .as_ref(),
                        ready,
                    ) {
                        hook(identity, caps);
                    }
                    debug!("connection initialized");
                } else {
                    debug!(method, "notification accepted (no-op)");
                }
                Ok(())
            }
            Frame::ClientResponse {
                id, result, error, ..
            } => {
                Self::complete_pending(this, &id, result, error).await;
                Ok(())
            }
            // A Q2/Q3 frame arriving server-side is a malformed peer;
            // dropping it (logged) keeps the loop alive.
            other => {
                warn!("dispatch server dropped unexpected inbound frame: {other:?}");
                Ok(())
            }
        }
    }

    #[tracing::instrument(skip(this, params), fields(rpc_id = id, method))]
    async fn dispatch_request(
        this: &Arc<Self>,
        id: u64,
        method: &str,
        params: serde_json::Value,
    ) -> Frame {
        if method == "initialize" {
            let state = *this.shared.state.lock().await;
            if state == HandshakeState::Initialized {
                return Frame::error_response(id, RpcError::not_initialized());
            }
            return match handle_initialize(&params) {
                Ok(result) => {
                    *this.shared.capabilities.lock().await = result.capabilities;
                    let identity = ClientIdentity {
                        name: params
                            .get("clientInfo")
                            .and_then(|c| c.get("name"))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown")
                            .to_string(),
                        version: params
                            .get("clientInfo")
                            .and_then(|c| c.get("version"))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("0")
                            .to_string(),
                    };
                    *this.shared.last_handshake.lock().expect("handshake lock") =
                        Some((identity, result.capabilities));
                    Frame::response(id, serde_json::to_value(result).expect("serialize result"))
                }
                Err(e) => Frame::error_response(id, e),
            };
        }

        let state = *this.shared.state.lock().await;
        if state != HandshakeState::Initialized {
            return Frame::error_response(id, RpcError::not_initialized());
        }

        if MethodRegistry::lookup(method).is_none() {
            return Frame::error_response(
                id,
                RpcError::method_not_found(&MethodRegistry::client_request_methods()),
            );
        }

        match this.handlers.get(method) {
            Some(handler) => match handler(params).await {
                Ok(result) => Frame::response(id, result),
                Err(e) => Frame::error_response(id, e),
            },
            None => Frame::error_response(
                id,
                RpcError::internal(&format!(
                    "method {method} registered but no handler installed"
                )),
            ),
        }
    }

    /// Resolves a pending Q3 request from a Q4 frame body; unknown ids
    /// (late responses after timeout) are dropped silently.
    async fn complete_pending(
        this: &Arc<Self>,
        id: &str,
        result: Option<serde_json::Value>,
        error: Option<RpcErrorBody>,
    ) {
        let mut pending = this.shared.pending.lock().await;
        let Some(tx) = pending.remove(id) else { return };
        let resolved = match (result, error) {
            (Some(value), None) => Ok(value),
            (_, Some(body)) => Err(RpcError {
                code: body.code,
                name: "client-error",
                message: body.message,
                data: body.data,
            }),
            (None, None) => Err(RpcError::internal("client response carried no result")),
        };
        let _ = tx.send(resolved);
    }
}

impl DispatchServer {
    /// Returns a clonable handle for server-initiated (Q3) requests —
    /// used by turn hosting to solicit approvals from the originating
    /// connection.
    pub fn connection(&self) -> ConnectionHandle {
        ConnectionHandle {
            transport: Arc::clone(&self.transport),
            shared: Arc::clone(&self.shared),
        }
    }
}

/// Server-side bridge to one dispatched connection: sends Q3 frames and
/// resolves their Q4 responses. Cloning shares the pending-request map
/// (cheap `Arc` clones — the carrier endpoint is object-safe).
#[derive(Clone)]
pub struct ConnectionHandle {
    transport: Arc<dyn Transport>,
    shared: Arc<Shared>,
}

impl ConnectionHandle {
    /// Sends a Q3 `approval/request` for `turn_id` with the default 120s
    /// deadline (data-model E7).
    ///
    /// # Errors
    /// See [`ConnectionHandle::request_approval_with_timeout`].
    pub async fn request_approval(
        &self,
        turn_id: &str,
        invocation: serde_json::Value,
        reason: &str,
    ) -> ApprovalOutcome {
        self.request_approval_with_timeout(turn_id, invocation, reason, APPROVAL_TIMEOUT)
            .await
    }

    /// Sends a Q3 `approval/request` with an explicit deadline. A
    /// connection that negotiated `approvals:false` fails fast with
    /// -32010; a deadline miss yields -32011 carrying `turnId`.
    ///
    /// # Errors
    /// - [`RpcError::approval_unsupported`] (-32010) when the client
    ///   declared `approvals:false`.
    /// - [`RpcError::approval_timeout`] (-32011) when no Q4 response
    ///   arrives in time.
    /// - [`RpcError::internal`] (-32603) when the transport dies mid-flight.
    pub async fn request_approval_with_timeout(
        &self,
        turn_id: &str,
        invocation: serde_json::Value,
        reason: &str,
        timeout: Duration,
    ) -> ApprovalOutcome {
        let caps = *self.shared.capabilities.lock().await;
        if !caps.approvals {
            return Err(RpcError::approval_unsupported());
        }

        let server_id = {
            let mut n = self.shared.next_server_id.lock().await;
            *n += 1;
            format!("srv-{n}")
        };

        let (tx, rx) = oneshot::channel();
        self.shared
            .pending
            .lock()
            .await
            .insert(server_id.clone(), tx);

        let params = serde_json::json!({
            "turnId": turn_id,
            "invocation": invocation,
            "reason": reason,
        });
        let frame = Frame::server_request(server_id.clone(), "approval/request", params);
        if let Err(e) = self.transport.send(frame).await {
            self.shared.pending.lock().await.remove(&server_id);
            return Err(RpcError::internal(&format!("approval send failed: {e}")));
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => Err(RpcError::internal("approval bridge dropped")),
            Err(_) => {
                self.shared.pending.lock().await.remove(&server_id);
                Err(RpcError::approval_timeout(turn_id))
            }
        }
    }
}

/// Builds a `DispatchServer` with stub handlers for every P1 lifecycle,
/// memory, and knowledge method — contract-suite fixture returning
/// well-shaped dummy results (real business handlers arrive with the
/// daemon tasks T012-T014).
///
/// # Errors
/// Never fails in practice; the `Result` keeps the builder chain uniform
/// with [`DispatchServer::handle`].
pub fn stub_server<T: Transport + 'static>(transport: T) -> anyhow::Result<DispatchServer> {
    let server = DispatchServer::new(transport)
        .handle("health/status", |_| async {
            Ok(serde_json::json!({
                "serverVersion": env!("CARGO_PKG_VERSION"),
                "protocolVersion": SERVER_PROTOCOL_VERSION,
                "clients": 1,
                "uptimeMs": 0,
                "storeHealth": "ok",
                "activeTurns": 0,
            }))
        })?
        .handle("shutdown", |_| async {
            Ok(serde_json::json!({"drained": 0, "cancelled": 0}))
        })?
        .handle("memory/retrieve", |params| async move {
            require("memory/retrieve", &params, &["sessionId"])?;
            Ok(serde_json::json!({"entries": []}))
        })?
        .handle("memory/putEntry", |params| async move {
            require(
                "memory/putEntry",
                &params,
                &["sessionId", "role", "content", "entityType"],
            )?;
            Ok(serde_json::json!({"frameId": "frame_0001"}))
        })?
        .handle("memory/search", |params| async move {
            require("memory/search", &params, &["query"])?;
            Ok(serde_json::json!({"hits": []}))
        })?
        .handle("memory/stats", |_| async {
            Ok(serde_json::json!({"frames": 0, "capacityBytes": 0, "generation": 0}))
        })?
        .handle("knowledge/search", |params| async move {
            require("knowledge/search", &params, &["query"])?;
            Ok(serde_json::json!({"notes": []}))
        })?;
    Ok(server)
}

/// Fails with -32602 naming `method` unless every required key is present
/// in `params`. Shared minimal param validation for stub handlers.
fn require(method: &str, params: &serde_json::Value, required: &[&str]) -> Result<(), RpcError> {
    for key in required {
        if params.get(key).is_none() {
            return Err(RpcError::invalid_params(
                method,
                &format!("missing required param {key:?}"),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::transport::in_process;
    use std::sync::Mutex as StdMutex;

    use super::*;

    async fn handshake(client: &in_process::InProcessTransport) {
        client
            .send(Frame::request_with(
                0,
                "initialize",
                crate::protocol::initialize_params(
                    "1.0",
                    "test-client",
                    "0.0.0",
                    Capabilities::default(),
                ),
            ))
            .await
            .unwrap();
        let _ = client.recv().await.unwrap();
        client
            .send(Frame::notification("initialized", serde_json::json!({})))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn pre_handshake_request_rejected() {
        let (client, server_transport) = in_process::pair();
        let server = stub_server(server_transport).unwrap();
        tokio::spawn(server.run());

        client
            .send(Frame::request(1, "health/status"))
            .await
            .unwrap();
        let Frame::ServerResponse {
            error: Some(e), id, ..
        } = client.recv().await.unwrap()
        else {
            panic!("expected error response");
        };
        assert_eq!(id, 1);
        assert_eq!((e.code, e.name.as_str()), (-32000, "not-initialized"));
    }

    #[tokio::test]
    async fn unknown_notification_ignored_silently() {
        let (client, server_transport) = in_process::pair();
        let server = stub_server(server_transport).unwrap();
        tokio::spawn(server.run());

        handshake(&client).await;
        client
            .send(Frame::notification(
                "future/unknownKind",
                serde_json::json!({"x": 1}),
            ))
            .await
            .unwrap();

        // Server must not answer a notification; prove liveness by a
        // subsequent request succeeding.
        client
            .send(Frame::request(2, "health/status"))
            .await
            .unwrap();
        let Frame::ServerResponse {
            result: Some(_),
            id,
            ..
        } = client.recv().await.unwrap()
        else {
            panic!("expected success response");
        };
        assert_eq!(id, 2);
    }

    #[tokio::test]
    async fn approval_round_trip_and_timeout() {
        let (client, server_transport) = in_process::pair();
        let server = stub_server(server_transport).unwrap();
        let handle = server.connection();
        tokio::spawn(server.run());

        // Approvals default false: fail-fast -32010 pre-handshake caps.
        let err = handle
            .request_approval_with_timeout(
                "t_0",
                serde_json::json!({"name": "shell.exec"}),
                "confidential",
                Duration::from_millis(100),
            )
            .await
            .unwrap_err();
        assert_eq!((err.code, err.name), (-32010, "approval-unsupported"));

        // Re-handshake with approvals:true enables the Q3 path.
        client
            .send(Frame::request_with(
                9,
                "initialize",
                crate::protocol::initialize_params(
                    "1.0",
                    "test-client",
                    "0.0.0",
                    Capabilities {
                        approvals: true,
                        ..Default::default()
                    },
                ),
            ))
            .await
            .unwrap();
        let _ = client.recv().await.unwrap();
        client
            .send(Frame::notification("initialized", serde_json::json!({})))
            .await
            .unwrap();

        let h2 = handle.clone();
        let approver = tokio::spawn(async move {
            h2.request_approval_with_timeout(
                "t_1",
                serde_json::json!({"name": "shell.exec"}),
                "confidential",
                Duration::from_secs(5),
            )
            .await
        });
        let Frame::ServerRequest { id, method, .. } = client.recv().await.unwrap() else {
            panic!("expected server request");
        };
        assert_eq!(method, "approval/request");
        assert!(id.starts_with("srv-"));
        client
            .send(Frame::client_response(
                id,
                serde_json::json!({"decision": "approve"}),
            ))
            .await
            .unwrap();
        assert_eq!(approver.await.unwrap().unwrap()["decision"], "approve");

        let err = handle
            .request_approval_with_timeout(
                "t_2",
                serde_json::json!({"name": "shell.exec"}),
                "confidential",
                Duration::from_millis(50),
            )
            .await
            .unwrap_err();
        assert_eq!((err.code, err.name), (-32011, "approval-timeout"));
        assert_eq!(err.data.unwrap()["turnId"], "t_2");
    }

    #[tokio::test]
    async fn client_ready_hook_fires_on_initialized() {
        let seen: Arc<StdMutex<Vec<(String, String, bool)>>> = Arc::new(StdMutex::new(Vec::new()));
        let seen_for_hook = Arc::clone(&seen);

        let (client, server_transport) = in_process::pair();
        let server =
            stub_server(server_transport)
                .unwrap()
                .on_client_ready(move |identity, caps| {
                    seen_for_hook.lock().unwrap().push((
                        identity.name,
                        identity.version,
                        caps.approvals,
                    ));
                });
        tokio::spawn(server.run());

        client
            .send(Frame::request_with(
                1,
                "initialize",
                crate::protocol::initialize_params(
                    "1.0",
                    "hook-client",
                    "9.9",
                    Capabilities {
                        approvals: true,
                        ..Default::default()
                    },
                ),
            ))
            .await
            .unwrap();
        let _ = client.recv().await.unwrap();
        // Hook fires only after `initialized`, not after `initialize`.
        assert!(seen.lock().unwrap().is_empty());
        client
            .send(Frame::notification("initialized", serde_json::json!({})))
            .await
            .unwrap();

        // Give the dispatcher a beat to process the notification.
        for _ in 0..50 {
            if !seen.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            *seen.lock().unwrap(),
            vec![("hook-client".to_string(), "9.9".to_string(), true)]
        );
    }
}
