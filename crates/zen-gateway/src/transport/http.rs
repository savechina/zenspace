//! Loopback HTTP carrier (FR-019/020) — legacy `/api/v1` REST surface and
//! the MCP tool offering mounted on the SAME MethodRegistry dispatcher as
//! UDS (tasks T041/T042).
//!
//! PURPOSE: One dispatcher, two carriers. Legacy HTTP clients keep their
//! wire shapes (`ChatRequest`/`ChatResponse`, token-streaming WS) while
//! every operation executes through hosted protocol handlers; MCP clients
//! get `tools/list` + `tools/call` over Streamable-HTTP-style JSON-RPC
//! POSTs against the identical Confidential-filtered registry the stdio
//! server serves.
//!
//! USAGE: [`GatewayDaemonConfig::http`] carries `Some(HttpCarrierConfig)`
//! to enable; [`serve_http`] binds loopback-only and shuts down with the
//! daemon's drain watch.
//!
//! EXPECTED: `GET /health` returns the legacy health shape;
//! `POST /api/v1/chat` returns `{reply, agent}`; gated tools fail-fast on
//! the carrier (`approvals:false` → `-32010 approval-unsupported`).
//!
//! ERRORS: handler failures surface as catalog errors mapped to HTTP
//! 4xx/5xx; non-loopback bind addresses are refused at startup.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{RawQuery, Request, State, WebSocketUpgrade},
    http::{HeaderMap, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};
use tracing::warn;

use crate::protocol::{Frame, RpcErrorBody};
use crate::server::{ClientConnection, OUTBOUND_CAPACITY, OutboundFrame};
use crate::transport::Transport;

/// Per-request dispatch ceiling: hosted chat turns are long (cold local
/// models), simple reads are fast; one generous ceiling keeps parity with
/// the UDS client budget without per-method tuning here.
const HTTP_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(960);

/// Server-side endpoint of one in-memory dispatch session: `recv()` yields
/// client frames pushed via [`SessionWire::push_client_frame`]; `send()`
/// correlates Q2 responses to their pending rpcId and relays notifications
/// to stream subscribers.
struct SessionWire {
    inbound: std::sync::Mutex<VecDeque<Frame>>,
    inbound_ready: tokio::sync::Notify,
    pending: Mutex<HashMap<u64, oneshot::Sender<Frame>>>,
    events: broadcast::Sender<Frame>,
}

impl SessionWire {
    fn new() -> Arc<Self> {
        let (events, _) = broadcast::channel(256);
        Arc::new(Self {
            inbound: std::sync::Mutex::new(VecDeque::new()),
            inbound_ready: tokio::sync::Notify::new(),
            pending: Mutex::new(HashMap::new()),
            events,
        })
    }

    fn push_client_frame(&self, frame: Frame) {
        self.inbound.lock().unwrap().push_back(frame);
        self.inbound_ready.notify_one();
    }

    /// Registers a response waiter, then queues the request frame.
    /// Registration MUST precede the push: the dispatcher may answer
    /// within the same tick.
    async fn call_frame(&self, id: u64, frame: Frame) -> Result<serde_json::Value, RpcErrorBody> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        self.push_client_frame(frame);
        match rx.await {
            Ok(frame) => match frame {
                Frame::ServerResponse { result, error, .. } => match error {
                    Some(e) => Err(e),
                    None => result.ok_or_else(|| RpcErrorBody {
                        code: -32603,
                        name: "internal".to_string(),
                        message: "empty result slot".to_string(),
                        data: None,
                    }),
                },
                _ => Err(RpcErrorBody {
                    code: -32603,
                    name: "internal".to_string(),
                    message: "non-response frame correlated to request id".to_string(),
                    data: None,
                }),
            },
            Err(_) => Err(RpcErrorBody {
                code: -32603,
                name: "internal".to_string(),
                message: "dispatch session dropped before responding".to_string(),
                data: None,
            }),
        }
    }
}

#[async_trait::async_trait]
impl crate::transport::Transport for SessionWire {
    async fn send(&self, frame: Frame) -> anyhow::Result<()> {
        match &frame {
            Frame::ServerResponse { id, .. } => {
                if let Some(waiter) = self.pending.lock().await.remove(id) {
                    let _ = waiter.send(frame);
                }
            }
            Frame::Notification { .. } => {
                let _ = self.events.send(frame);
            }
            // approvals:false carriers never receive Q3 requests; anything
            // else is a protocol violation on this synthetic wire.
            _ => warn!("http session wire received unexpected frame quadrant"),
        }
        Ok(())
    }

    async fn recv(&self) -> anyhow::Result<Frame> {
        loop {
            if let Some(frame) = self.inbound.lock().unwrap().pop_front() {
                return Ok(frame);
            }
            self.inbound_ready.notified().await;
        }
    }
}

/// Live in-memory dispatch session handle: one `ClientConnection` +
/// dispatcher wired through [`SessionWire`], registered in the service's
/// connection map exactly like a UDS peer.
pub(crate) struct HttpDispatchSession {
    service: Arc<crate::GatewayService>,
    conn_id: String,
    wire: Arc<SessionWire>,
    next_rpc_id: u64,
}

impl HttpDispatchSession {
    /// Opens a session: registers the connection and spawns the real
    /// dispatcher (handshake gate + all business handlers).
    pub(crate) async fn open(service: &Arc<crate::GatewayService>) -> anyhow::Result<Self> {
        let (tx, mut rx) = mpsc::channel::<OutboundFrame>(OUTBOUND_CAPACITY);
        let conn = Arc::new(ClientConnection::new(tx));
        service.register_connection(Arc::clone(&conn)).await;
        let conn_id = conn.id.clone();

        let wire = SessionWire::new();
        let pump_wire = Arc::clone(&wire);
        tokio::spawn(async move {
            while let Some(of) = rx.recv().await {
                if pump_wire.send(of.frame).await.is_err() {
                    break;
                }
            }
        });

        let wire_dyn: std::sync::Arc<dyn crate::transport::Transport> =
            std::sync::Arc::<SessionWire>::clone(&wire);
        let front = crate::server::QueueFrontTransport::new(conn.queue(), wire_dyn);
        let dispatcher = service.build_dispatcher(Arc::clone(&conn), front)?;
        tokio::spawn(async move {
            if let Err(e) = dispatcher.run().await {
                tracing::debug!(error = %e, "http dispatch session ended");
            }
        });

        Ok(Self {
            service: Arc::clone(service),
            conn_id,
            wire,
            next_rpc_id: 0,
        })
    }

    fn next_id(&mut self) -> u64 {
        self.next_rpc_id += 1;
        self.next_rpc_id
    }

    /// Performs the initialize/initialized handshake with
    /// `approvals:false` capabilities, then issues one method call and
    /// awaits its correlated response.
    pub(crate) async fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcErrorBody> {
        if self.next_rpc_id == 0 {
            let id = self.next_id();
            self.wire
                .call_frame(
                    id,
                    Frame::request_with(
                        id,
                        "initialize",
                        serde_json::json!({
                            "protocolVersion": crate::protocol::SERVER_PROTOCOL_VERSION,
                            "clientInfo": {"name": "zen-http-carrier", "version": env!("CARGO_PKG_VERSION")},
                            "capabilities": {"approvals": false, "streaming": true, "deliveryAck": false}
                        }),
                    ),
                )
                .await?;
            self.wire
                .push_client_frame(Frame::notification("initialized", serde_json::json!({})));
        }
        let id = self.next_id();
        self.wire
            .call_frame(id, Frame::request_with(id, method, params))
            .await
    }

    /// Subscribes to notification frames relayed from the dispatcher
    /// (WS delta streaming).
    pub(crate) fn subscribe_events(&self) -> broadcast::Receiver<Frame> {
        self.wire.events.subscribe()
    }
}

impl Drop for HttpDispatchSession {
    fn drop(&mut self) {
        let service = Arc::clone(&self.service);
        let conn_id = self.conn_id.clone();
        tokio::spawn(async move {
            service.unregister_connection(&conn_id).await;
        });
    }
}

/// Maps catalog error codes to HTTP statuses. `-32004
/// turn-already-completed` is deliberately `OK`: it is the idempotent-
/// replay success shape — the caller already holds the final reply in
/// `RpcErrorBody.data` (unpacked at the call site), so surfacing 500
/// would turn a redelivery into a client-visible failure.
fn rpc_error_to_status(code: i32) -> axum::http::StatusCode {
    match code {
        -32004 => axum::http::StatusCode::OK,
        -32602 | -32600 | -32700 => axum::http::StatusCode::BAD_REQUEST,
        -32002 | -32000 => axum::http::StatusCode::SERVICE_UNAVAILABLE,
        _ => axum::http::StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// Unpacks an idempotent replay (`-32004`) into the original final
/// reply carried in `data`; `None` for any other error shape. The
/// canonical wire shape is `{"response": "..."}` (protocol/error.rs);
/// a bare string is accepted defensively.
fn replayed_reply(error: &RpcErrorBody) -> Option<String> {
    if error.code != -32004 {
        return None;
    }
    let data = error.data.as_ref()?;
    data.get("response")
        .and_then(serde_json::Value::as_str)
        .or_else(|| data.as_str())
        .map(str::to_string)
}

// ─────────────────────── Carrier auth (CSO audit #1) ───────────────────────

/// Origins allowed to send cross-origin requests at the carrier. Exact
/// match only: a browser page served from any other origin (including
/// other ports) is rejected — drive-by defense independent of token
/// auth. Loopback-only bind means same-origin tooling (curl, reqwest,
/// the qqbot bridge) sends no `Origin` header at all and is unaffected.
const ALLOWED_ORIGINS: [&str; 3] = ["http://127.0.0.1", "http://localhost", "http://[::1]"];

/// Short-circuit-free byte comparison for bearer secrets (timing-safe
/// equality; `subtle`-crate parity without the dependency). Length
/// mismatch returns early — length is not secret-bearing (standard
/// practice, documented in the `subtle` crate itself).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// `Some` only for non-empty secrets: an empty string would otherwise
/// demand `Bearer ` with nothing after it and match any empty token.
fn normalize_secret(secret: Option<String>) -> Option<String> {
    secret.filter(|s| !s.is_empty())
}

/// Extracts the raw bearer token from an `Authorization: Bearer <token>`
/// header (exact scheme spelling, per the carrier contract).
fn bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string)
}

/// Extracts `?token=` from a raw query string (WS clients — browsers —
/// cannot set custom headers on upgrades). Values are taken verbatim:
/// secrets are generated hex/uuid shapes with no percent-encoding.
fn query_token(query: Option<&str>) -> Option<String> {
    query.and_then(|q| {
        q.split('&').find_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            (k == "token").then(|| v.to_string())
        })
    })
}

/// Carrier auth gate (CSO audit #1), applied to every route:
///
/// 1. `/health` is the unauthenticated liveness probe — exempt from
///    both gates.
/// 2. A foreign `Origin` header is rejected regardless of auth state
///    (browser drive-by defense, independent of token).
/// 3. `/api/v1/ws` enforces its own token check in [`ws_handler`]
///    (header OR `?token=` — browsers cannot set headers on upgrades),
///    so the header-only gate here would strand the query-param path.
/// 4. When `jwt_secret` is `Some(non-empty)`, every other route
///    requires `Authorization: Bearer <secret>` (constant-time
///    compare); `None` keeps the historical loopback-only posture
///    byte-for-byte.
async fn auth_middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    if path == "/health" {
        return next.run(req).await;
    }
    let origin_header = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok());
    if let Some(origin) = origin_header
        && !ALLOWED_ORIGINS.contains(&origin)
    {
        tracing::warn!(origin, "http carrier rejected cross-origin request");
        return (
            axum::http::StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "forbidden origin"})),
        )
            .into_response();
    }
    if path == "/api/v1/ws" {
        return next.run(req).await;
    }
    if let Some(secret) = &state.jwt_secret {
        let ok = bearer_token(req.headers())
            .map(|tok| constant_time_eq(tok.as_bytes(), secret.as_bytes()))
            .unwrap_or(false);
        if !ok {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "unauthorized"})),
            )
                .into_response();
        }
    }
    next.run(req).await
}

// ─────────────────────── Legacy REST shims (T041 parity) ───────────────────────

/// Shared router state: the live service plus the filtered MCP registry.
#[derive(Clone)]
pub(crate) struct AppState {
    pub service: Arc<crate::GatewayService>,
    pub mcp: Arc<rig_compose::registry::ToolRegistry>,
    /// Bearer secret for [`auth_middleware`]; `None` = loopback-only
    /// posture (no auth required).
    pub jwt_secret: Option<String>,
}

#[derive(serde::Deserialize)]
struct ChatRequest {
    message: String,
    #[allow(dead_code)]
    agent: Option<String>,
    session_id: Option<String>,
    /// Caller-supplied idempotency key forwarded as the hosted
    /// `turnId`; absent → server mints one. Carriers derive it
    /// deterministically from platform message ids so redeliveries
    /// replay instead of re-executing.
    #[serde(default)]
    turn_id: Option<String>,
}

#[derive(serde::Serialize)]
struct ChatResponse {
    reply: String,
    agent: Option<String>,
    /// Hosted-session id for conversation continuity; `None` when the
    /// turn failed before `session/start` completed. Additive field —
    /// legacy clients ignore unknown JSON keys.
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
}

async fn health_handler(State(state): State<AppState>) -> impl IntoResponse {
    let service = &state.service;
    Json(serde_json::json!({
        "status": if service.store_health().await == "unavailable" { "degraded" } else { "healthy" },
        "version": env!("CARGO_PKG_VERSION"),
        "agents": service.agents_count().await,
        "scheduler": service.is_scheduler_hosted(),
        "scheduler_pending": service.is_scheduler_waiting(),
    }))
}

async fn chat_handler(
    State(state): State<AppState>,
    Json(req): Json<ChatRequest>,
) -> impl IntoResponse {
    let service = &state.service;
    if req.message.is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(ChatResponse {
                reply: "Empty message".to_string(),
                agent: None,
                session_id: None,
            }),
        );
    }
    let mut session = match HttpDispatchSession::open(service).await {
        Ok(s) => s,
        Err(e) => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(ChatResponse {
                    reply: format!("carrier error: {e}"),
                    agent: None,
                    session_id: None,
                }),
            );
        }
    };
    let call = async {
        let started = session
            .call(
                "session/start",
                serde_json::json!({ "sessionId": req.session_id }),
            )
            .await?;
        let session_id = started["sessionId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let turn_id = req
            .turn_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
        let turn = session
            .call(
                "session/turn",
                serde_json::json!({
                    "turnId": turn_id,
                    "sessionId": session_id,
                    "prompt": req.message,
                }),
            )
            .await?;
        let agent = turn["agent"]
            .is_null()
            .then(|| started["agent"].as_str().map(str::to_string))
            .flatten();
        Ok::<_, RpcErrorBody>((session_id, turn, agent))
    };
    match tokio::time::timeout(HTTP_CALL_TIMEOUT, call).await {
        Ok(Ok((session_id, turn, agent))) => (
            axum::http::StatusCode::OK,
            Json(ChatResponse {
                reply: turn["response"].as_str().unwrap_or_default().to_string(),
                agent,
                session_id: Some(session_id),
            }),
        ),
        Ok(Err(e)) => match replayed_reply(&e) {
            Some(reply) => (
                axum::http::StatusCode::OK,
                Json(ChatResponse {
                    reply,
                    agent: None,
                    session_id: None,
                }),
            ),
            None => (
                rpc_error_to_status(e.code),
                Json(ChatResponse {
                    reply: format!("{} ({})", e.message, e.name),
                    agent: None,
                    session_id: None,
                }),
            ),
        },
        Err(_) => (
            axum::http::StatusCode::GATEWAY_TIMEOUT,
            Json(ChatResponse {
                reply: "turn exceeded carrier timeout".to_string(),
                agent: None,
                session_id: None,
            }),
        ),
    }
}

async fn agents_handler(State(state): State<AppState>) -> impl IntoResponse {
    let mut session = match HttpDispatchSession::open(&state.service).await {
        Ok(s) => s,
        Err(e) => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("carrier error: {e}") })),
            );
        }
    };
    match session.call("agent/list", serde_json::json!({})).await {
        Ok(result) => {
            let agents: Vec<serde_json::Value> = result["agents"]
                .as_array()
                .map(|list| {
                    list.iter()
                        .map(|a| {
                            serde_json::json!({
                                "name": a["name"],
                                "role": a["role"],
                                "capabilities": [],
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            (
                axum::http::StatusCode::OK,
                Json(serde_json::json!({ "agents": agents })),
            )
        }
        Err(e) => (
            rpc_error_to_status(e.code),
            Json(serde_json::json!({ "error": format!("{} ({})", e.message, e.name) })),
        ),
    }
}

// ─────────────────────── WS bridge (token streaming parity) ───────────────────────

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Response {
    if let Some(secret) = &state.jwt_secret {
        let ok = bearer_token(&headers)
            .or_else(|| query_token(query.as_deref()))
            .map(|tok| constant_time_eq(tok.as_bytes(), secret.as_bytes()))
            .unwrap_or(false);
        if !ok {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "unauthorized"})),
            )
                .into_response();
        }
    }
    ws.on_upgrade(move |socket| run_ws_bridge(socket, state.service))
        .into_response()
}

async fn run_ws_bridge(socket: axum::extract::ws::WebSocket, service: Arc<crate::GatewayService>) {
    let mut session = match HttpDispatchSession::open(&service).await {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "ws bridge session open failed");
            return;
        }
    };
    let (mut sink, mut stream) = socket.split();
    let mut hosted_session_id: Option<String> = None;
    let mut events = session.subscribe_events();

    loop {
        tokio::select! {
            frame = events.recv() => {
                let Ok(frame) = frame else { break };
                let Frame::Notification { params, .. } = frame else { continue };
                let kind = params["kind"].as_str().unwrap_or_default();
                let msg = match kind {
                    "delta" => serde_json::json!({"type": "token", "content": params["payload"]["text"]}),
                    "turn_completed" => serde_json::json!({"type": "done", "content": params["payload"]["response"]}),
                    "turn_error" => serde_json::json!({"type": "error", "content": params["payload"]["message"]}),
                    _ => continue,
                };
                if sink.send(axum::extract::ws::Message::Text(msg.to_string().into())).await.is_err() {
                    break;
                }
            }
            incoming = stream.next() => {
                let Some(Ok(axum::extract::ws::Message::Text(text))) = incoming else { break };
                #[derive(serde::Deserialize)]
                struct WsRequest {
                    message: String,
                    session_id: Option<String>,
                }
                let req: WsRequest = match serde_json::from_str(&text) {
                    Ok(r) => r,
                    Err(_) => {
                        let reply = serde_json::json!({"type": "error", "content": "Invalid JSON. Expected {\"message\": \"...\", \"session_id\": \"...\"}"});
                        let _ = sink.send(axum::extract::ws::Message::Text(reply.to_string().into())).await;
                        continue;
                    }
                };
                if req.message.is_empty() {
                    let reply = serde_json::json!({"type": "error", "content": "Empty message"});
                    let _ = sink.send(axum::extract::ws::Message::Text(reply.to_string().into())).await;
                    continue;
                }
                if hosted_session_id.is_none() {
                    match session
                        .call(
                            "session/start",
                            serde_json::json!({ "sessionId": req.session_id }),
                        )
                        .await
                    {
                        Ok(started) => {
                            hosted_session_id = started["sessionId"].as_str().map(str::to_string);
                        }
                        Err(e) => {
                            let reply = serde_json::json!({"type": "error", "content": format!("{} ({})", e.message, e.name)});
                            let _ = sink.send(axum::extract::ws::Message::Text(reply.to_string().into())).await;
                            continue;
                        }
                    }
                }
                let turn_params = serde_json::json!({
                    "turnId": uuid::Uuid::now_v7().to_string(),
                    "sessionId": hosted_session_id.clone().unwrap_or_default(),
                    "prompt": req.message,
                });
                if let Err(e) = session.call("session/turn", turn_params).await {
                    let reply = match replayed_reply(&e) {
                        Some(content) => serde_json::json!({"type": "done", "content": content}),
                        None => serde_json::json!(
                            {"type": "error",
                             "content": format!("Execution error: {} ({})", e.message, e.name)}
                        ),
                    };
                    let _ = sink.send(axum::extract::ws::Message::Text(reply.to_string().into())).await;
                }
            }
        }
    }
}

// ─────────────────────── MCP over Streamable HTTP (T042) ───────────────────────

/// Minimal Streamable-HTTP MCP endpoint: single-shot JSON-RPC POSTs
/// (`initialize`, `tools/list`, `tools/call`) against the same
/// Confidential-filtered registry the stdio server serves (FR-020).
async fn mcp_handler(
    State(state): State<AppState>,
    body: Option<Json<serde_json::Value>>,
) -> impl IntoResponse {
    let registry = &state.mcp;
    let Some(Json(body)) = body else {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(
                serde_json::json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "parse error"}}),
            ),
        );
    };
    let id = body.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let method = body.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = body.get("params").cloned().unwrap_or(serde_json::json!({}));

    let result: Result<serde_json::Value, (i64, String)> = match method {
        "initialize" => Ok(serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": "zen-gateway", "version": env!("CARGO_PKG_VERSION")}
        })),
        "notifications/initialized" | "initialized" => {
            return (
                axum::http::StatusCode::ACCEPTED,
                Json(serde_json::Value::Null),
            );
        }
        "tools/list" => {
            let tools: Vec<serde_json::Value> = registry
                .schemas()
                .into_iter()
                .map(|schema| {
                    serde_json::json!({
                        "name": schema.name,
                        "description": schema.description,
                        "inputSchema": schema.args_schema,
                    })
                })
                .collect();
            Ok(serde_json::json!({ "tools": tools }))
        }
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or_default().to_string();
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            match registry.get(&name) {
                Ok(tool) => match tool.invoke(args).await {
                    Ok(text) => Ok(serde_json::json!({
                        "content": [{"type": "text", "text": text}],
                        "isError": false
                    })),
                    Err(e) => Ok(serde_json::json!({
                        "content": [{"type": "text", "text": e.to_string()}],
                        "isError": true
                    })),
                },
                Err(_) => Err((-32602, format!("unknown tool: {name}"))),
            }
        }
        "" => Err((-32600, "invalid request: missing method".to_string())),
        other => Err((-32601, format!("method not found: {other}"))),
    };

    match result {
        Ok(value) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({"jsonrpc": "2.0", "id": id, "result": value})),
        ),
        Err((code, message)) => (
            axum::http::StatusCode::OK,
            Json(
                serde_json::json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
            ),
        ),
    }
}

// ─────────────────────── Router + server lifecycle ───────────────────────

/// Loopback-only HTTP carrier options (config key `[gateway.http]`).
///
/// The bearer secret is NOT carried here on purpose: the CLI
/// construction sites build this struct with field-exact literals, so
/// the auth secret rides [`crate::daemon::GatewayDaemonConfig::
/// http_jwt_secret`] instead and is passed to [`router_with_auth`] /
/// [`serve_http`] separately.
#[derive(Clone, Debug)]
pub struct HttpCarrierConfig {
    pub bind_addr: String,
    pub port: u16,
}

impl HttpCarrierConfig {
    /// Refuses non-loopback bind targets (FR-019: loopback only).
    ///
    /// # Errors
    /// Address parse failure or a non-loopback IP.
    pub fn validate_loopback(&self) -> anyhow::Result<()> {
        let ip: IpAddr = self
            .bind_addr
            .parse()
            .map_err(|e| anyhow::anyhow!("gateway.http.bind_addr {}: {e}", self.bind_addr))?;
        if !ip.is_loopback() {
            anyhow::bail!(
                "gateway.http.bind_addr must be loopback (127.0.0.1/::1); got {ip} — non-local exposure requires auth + ADR addendum"
            );
        }
        Ok(())
    }
}

/// Builds the carrier router over a live [`GatewayService`] with no
/// auth (historical loopback-only posture).
pub fn router(service: Arc<crate::GatewayService>) -> Router {
    router_with_auth(service, None)
}

/// Builds the carrier router with the auth gate armed when `jwt_secret`
/// is `Some(non-empty)`: every route except `/health` requires
/// `Authorization: Bearer <secret>` (constant-time compare), and any
/// request bearing a foreign `Origin` header is rejected regardless of
/// auth state. `None` keeps the loopback-only posture byte-for-byte.
pub fn router_with_auth(service: Arc<crate::GatewayService>, jwt_secret: Option<String>) -> Router {
    let mcp = Arc::new(crate::mcp_registry_for_http());
    let state = AppState {
        service,
        mcp,
        jwt_secret: normalize_secret(jwt_secret),
    };
    Router::new()
        .route("/health", get(health_handler))
        .route("/api/v1/chat", post(chat_handler))
        .route("/api/v1/ws", get(ws_handler))
        .route("/api/v1/agents", get(agents_handler))
        .route("/api/v1/mcp", post(mcp_handler))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}

/// Serves the loopback HTTP carrier until `shutdown` fires.
///
/// # Errors
/// Loopback validation failure or TCP bind failure.
pub async fn serve_http(
    service: Arc<crate::GatewayService>,
    config: HttpCarrierConfig,
    jwt_secret: Option<String>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    config.validate_loopback()?;
    let addr: std::net::SocketAddr = format!("{}:{}", config.bind_addr, config.port)
        .parse()
        .map_err(|e| anyhow::anyhow!("http bind addr: {e}"))?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "loopback http carrier listening");
    axum::serve(
        listener,
        router_with_auth(service, jwt_secret).into_make_service(),
    )
    .with_graceful_shutdown(async move {
        let _ = shutdown.changed().await;
    })
    .await?;
    Ok(())
}
