use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use rig_compose::registry::{KernelError, ToolRegistry};
use rig_compose::tool::{Tool, ToolSchema};
use rig_mcp::McpTransport;
use rmcp::ClientHandler;
use rmcp::model::{CallToolRequestParams, Tool as RmcpTool};
use rmcp::service::{NotificationContext, Peer, RoleClient, RunningService, ServiceExt};
use rmcp::transport::TokioChildProcess;
use serde_json::Value;
use tokio::sync::{Mutex, mpsc};
use tracing::{info, warn};

const MAX_RECONNECT_ATTEMPTS: u32 = 3;
/// Exponential backoff schedule (FR-013): one delay per attempt — 1s, 2s, 4s.
/// Single home for the schedule is `zen_core::retry::BACKOFF_SECS`.
const BACKOFF_SECS: &[u64] = zen_core::retry::BACKOFF_SECS;

type SpawnFuture = Pin<Box<dyn Future<Output = Result<Arc<dyn McpTransport>, String>> + Send>>;

/// Re-spawns the MCP subprocess on demand. A factory is needed because the
/// transport exposes neither its child handle nor a `clone`/`restart` method —
/// the only way to recover from a crash is to spawn again with the original
/// command, args, and scrubbed env, which this closure captures.
type SpawnFactory = Arc<dyn Fn() -> SpawnFuture + Send + Sync>;

/// First-run trust prompt (FR-018): called with a server config, returns
/// `true` to trust it. `None` means no prompt is available (headless
/// orchestrator) — untrusted servers are then skipped non-fatally.
pub type TrustPrompt =
    Option<Arc<dyn Fn(&zen_core::config::McpServerConfig) -> bool + Send + Sync>>;

/// One registered MCP tool. The schema is behind an `RwLock` so a
/// `notifications/tools/list_changed` refresh can update the live
/// registration in place (T079) without re-registering or restarting the
/// session; `removed` tombstones a tool the server no longer offers because
/// rig-compose's `ToolRegistry` has no unregister.
struct McpProxyTool {
    transport: Arc<dyn McpTransport>,
    server_name: String,
    original_name: String,
    schema: RwLock<ToolSchema>,
    removed: AtomicBool,
}

impl McpProxyTool {
    fn new(
        transport: Arc<dyn McpTransport>,
        server_name: impl Into<String>,
        original_name: impl Into<String>,
        schema: ToolSchema,
    ) -> Self {
        Self {
            transport,
            server_name: server_name.into(),
            original_name: original_name.into(),
            schema: RwLock::new(schema),
            removed: AtomicBool::new(false),
        }
    }

    fn schema_snapshot(&self) -> ToolSchema {
        self.schema
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn replace_schema(&self, schema: ToolSchema) {
        *self
            .schema
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = schema;
    }

    fn mark_removed(&self) {
        self.removed.store(true, Ordering::Relaxed);
    }

    fn mark_active(&self) {
        self.removed.store(false, Ordering::Relaxed);
    }

    fn is_removed(&self) -> bool {
        self.removed.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl Tool for McpProxyTool {
    fn schema(&self) -> ToolSchema {
        self.schema_snapshot()
    }

    async fn invoke(&self, args: Value) -> Result<Value, KernelError> {
        if self.is_removed() {
            return Err(KernelError::ToolFailed(format!(
                "MCP tool '{}' is no longer offered by server '{}' (tools/list_changed)",
                self.original_name, self.server_name
            )));
        }
        self.transport.call_tool(&self.original_name, args).await
    }
}

/// Spec naming convention (spec.md §naming): MCP tools are namespaced by
/// their source server **without** an `mcp.` prefix, e.g. `brave.web_search`.
fn namespaced_tool_name(server: &str, tool: &str) -> String {
    format!("{}.{}", server, tool)
}

/// Delta produced by applying a fresh `tools/list` snapshot to a live
/// registry. All-zero means the server's tool catalog is byte-identical.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct ToolUpdate {
    added: usize,
    removed: usize,
    updated: usize,
}

impl ToolUpdate {
    fn changed(&self) -> bool {
        self.added + self.removed + self.updated > 0
    }
}

/// Deterministic digest of a `tools/list` snapshot (names + schemas) used to
/// drop `list_changed` notifications that carry no actual change (T079).
fn tool_list_fingerprint(schemas: &[ToolSchema]) -> u64 {
    let mut items: Vec<(&str, &str, String, String)> = schemas
        .iter()
        .map(|s| {
            (
                s.name.as_str(),
                s.description.as_str(),
                s.args_schema.to_string(),
                s.result_schema.to_string(),
            )
        })
        .collect();
    items.sort();
    let mut hasher = DefaultHasher::new();
    items.hash(&mut hasher);
    hasher.finish()
}

/// Live registration surface for one MCP server (T079). Owns the proxy
/// handles so a `notifications/tools/list_changed` can re-list and hot-swap
/// schemas, register newly discovered tools, and tombstone removed ones —
/// without restarting the session or re-bootstrapping the client.
pub struct McpToolSet {
    server_name: String,
    registry: ToolRegistry,
    transport: Arc<dyn McpTransport>,
    proxies: Mutex<HashMap<String, Arc<McpProxyTool>>>,
    fingerprint: Mutex<Option<u64>>,
    /// Set by the refresh worker when the transport reports itself unavailable;
    /// cleared by the next successful catalog apply (T244).
    degraded: AtomicBool,
}

impl McpToolSet {
    fn new(
        server_name: impl Into<String>,
        registry: ToolRegistry,
        transport: Arc<dyn McpTransport>,
    ) -> Self {
        Self {
            server_name: server_name.into(),
            registry,
            transport,
            proxies: Mutex::new(HashMap::new()),
            fingerprint: Mutex::new(None),
            degraded: AtomicBool::new(false),
        }
    }

    /// Whether the last refresh attempt left this server's transport
    /// unavailable (T244). Cleared on the next successful catalog apply.
    pub fn is_degraded(&self) -> bool {
        self.degraded.load(Ordering::Relaxed)
    }

    fn mark_degraded(&self) {
        self.degraded.store(true, Ordering::Relaxed);
    }

    fn mark_healthy(&self) {
        self.degraded.store(false, Ordering::Relaxed);
    }

    async fn list_and_apply(&self) -> Result<ToolUpdate, KernelError> {
        let schemas = self.transport.list_tools().await?;
        Ok(self.apply(schemas).await)
    }

    async fn apply(&self, schemas: Vec<ToolSchema>) -> ToolUpdate {
        let digest = tool_list_fingerprint(&schemas);
        let mut last = self.fingerprint.lock().await;
        if *last == Some(digest) {
            return ToolUpdate::default();
        }
        *last = Some(digest);

        let mut proxies = self.proxies.lock().await;
        let mut seen: HashSet<String> = HashSet::with_capacity(schemas.len());
        let mut update = ToolUpdate::default();

        for schema in schemas {
            let original = schema.name.clone();
            seen.insert(original.clone());
            let namespaced = ToolSchema {
                name: namespaced_tool_name(&self.server_name, &original),
                description: schema.description,
                args_schema: schema.args_schema,
                result_schema: schema.result_schema,
            };

            match proxies.get(&original) {
                Some(proxy) => {
                    if proxy.is_removed() {
                        proxy.replace_schema(namespaced);
                        proxy.mark_active();
                        update.updated += 1;
                    } else if proxy.schema_snapshot() != namespaced {
                        proxy.replace_schema(namespaced);
                        update.updated += 1;
                    }
                }
                None => {
                    let proxy = Arc::new(McpProxyTool::new(
                        self.transport.clone(),
                        self.server_name.clone(),
                        original.clone(),
                        namespaced,
                    ));
                    self.registry.register(proxy.clone());
                    proxies.insert(original, proxy);
                    update.added += 1;
                }
            }
        }

        for (name, proxy) in proxies.iter() {
            if !seen.contains(name) && !proxy.is_removed() {
                proxy.mark_removed();
                update.removed += 1;
            }
        }

        update
    }
}

/// Background consumer for MCP `tools/list_changed` signals (T079). The rmcp
/// client handler only forwards a wake-up; this worker owns the re-list so the
/// protocol reader is never blocked on `list_tools` round-trips (D5: all pipe
/// I/O stays with the rmcp service task, never inline on the call path).
///
/// T243: the worker holds only a [`Weak`] reference. The wiring keeps the sole
/// strong `Arc<McpToolSet>`; when it is dropped the transport (and with it the
/// spawn factory's notification sender) is freed, the channel closes, and the
/// loop exits naturally — no `JoinHandle` management or shutdown channel.
fn spawn_tool_refresh_worker(
    mut signals: mpsc::Receiver<()>,
    tools: Weak<McpToolSet>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while signals.recv().await.is_some() {
            // A signal can outlive the toolset; stop rather than refresh a
            // registration surface nobody owns anymore.
            let Some(tools) = tools.upgrade() else {
                break;
            };

            // Signals are edge-triggered: one bounded retry keeps a transient
            // failure from swallowing a wake-up until the server next changes.
            let mut result = tools.list_and_apply().await;
            if result.is_err() {
                result = tools.list_and_apply().await;
            }

            match result {
                Ok(update) if update.changed() => {
                    tools.mark_healthy();
                    info!(
                        server = %tools.server_name,
                        added = update.added,
                        removed = update.removed,
                        updated = update.updated,
                        "MCP tools/list_changed applied in place"
                    );
                }
                Ok(_) => {
                    tools.mark_healthy();
                    info!(
                        server = %tools.server_name,
                        "MCP tools/list_changed with identical catalog ignored"
                    );
                }
                Err(e) => {
                    let unavailable = is_transport_unavailable(&e);
                    if unavailable {
                        tools.mark_degraded();
                    }
                    warn!(
                        target: "zen_mcp_degraded",
                        server = %tools.server_name,
                        error = %e,
                        degraded = unavailable,
                        "MCP tools/list_changed refresh failed"
                    );
                }
            }
        }
    })
}

/// Whether a refresh error came from the fail-fast gate in
/// [`ReconnectingMcpTransport::current_transport`] or from reconnect
/// exhaustion — both name the server as "unavailable".
fn is_transport_unavailable(error: &KernelError) -> bool {
    error.to_string().contains("unavailable")
}

/// Crash-recovering MCP transport (FR-013). On a `call_tool`/`list_tools`
/// failure it re-spawns via the stored [`SpawnFactory`] with exponential
/// backoff 1s→2s→4s (max 3 attempts); the original op is retried once on
/// success. After 3 failed re-spawn attempts the server is marked
/// *unavailable* (`current = None`) and subsequent calls return a clear
/// error naming the server. Lifecycle events are emitted via `tracing`.
struct ReconnectingMcpTransport {
    server_name: String,
    endpoint: String,
    spawn: SpawnFactory,
    current: Mutex<Option<Arc<dyn McpTransport>>>,
    /// Per-attempt delays before each re-spawn (FR-013). Injectable so tests
    /// can exercise the full retry sequence without real sleeping.
    backoffs: Vec<Duration>,
}

impl ReconnectingMcpTransport {
    fn new(
        server_name: impl Into<String>,
        endpoint: impl Into<String>,
        spawn: SpawnFactory,
        initial: Arc<dyn McpTransport>,
    ) -> Self {
        let backoffs = BACKOFF_SECS
            .iter()
            .map(|s| Duration::from_secs(*s))
            .collect();
        Self {
            server_name: server_name.into(),
            endpoint: endpoint.into(),
            spawn,
            current: Mutex::new(Some(initial)),
            backoffs,
        }
    }

    async fn current_transport(&self) -> Result<Arc<dyn McpTransport>, KernelError> {
        match self.current.lock().await.clone() {
            Some(t) => Ok(t),
            None => Err(KernelError::ToolFailed(format!(
                "MCP server '{}' is unavailable (marked down after {} failed re-spawn attempts)",
                self.server_name, MAX_RECONNECT_ATTEMPTS
            ))),
        }
    }

    async fn reconnect(&self) -> Result<Arc<dyn McpTransport>, KernelError> {
        let mut last_err = String::from("no spawn attempted");

        for (i, delay) in self.backoffs.iter().enumerate() {
            let attempt = (i as u32) + 1;
            tokio::time::sleep(*delay).await;
            info!(
                server = %self.server_name,
                attempt, backoff_ms = delay.as_millis() as u64, "MCP reconnect attempt"
            );
            match (self.spawn)().await {
                Ok(t) => {
                    *self.current.lock().await = Some(t.clone());
                    info!(server = %self.server_name, attempt, "MCP server recovered");
                    return Ok(t);
                }
                Err(e) => {
                    warn!(
                        server = %self.server_name,
                        attempt, error = %e, "MCP re-spawn attempt failed"
                    );
                    last_err = e;
                }
            }
        }

        *self.current.lock().await = None;
        warn!(
            server = %self.server_name,
            "MCP server marked unavailable after {} failed re-spawn attempts",
            MAX_RECONNECT_ATTEMPTS
        );
        Err(KernelError::ToolFailed(format!(
            "MCP server '{}' unavailable after {} re-spawn attempts (1s/2s/4s backoff): {}",
            self.server_name, MAX_RECONNECT_ATTEMPTS, last_err
        )))
    }
}

#[async_trait]
impl McpTransport for ReconnectingMcpTransport {
    fn endpoint(&self) -> &str {
        &self.endpoint
    }

    async fn list_tools(&self) -> Result<Vec<ToolSchema>, KernelError> {
        let transport = self.current_transport().await?;
        match transport.list_tools().await {
            Ok(v) => Ok(v),
            Err(e) => {
                warn!(
                    server = %self.server_name,
                    error = %e, "MCP list_tools failed, attempting reconnect"
                );
                let fresh = self.reconnect().await?;
                fresh.list_tools().await
            }
        }
    }

    async fn call_tool(&self, name: &str, args: Value) -> Result<Value, KernelError> {
        let transport = self.current_transport().await?;
        match transport.call_tool(name, args.clone()).await {
            Ok(v) => Ok(v),
            Err(e) => {
                warn!(
                    server = %self.server_name,
                    tool = name, error = %e, "MCP call_tool failed, attempting reconnect"
                );
                match self.reconnect().await {
                    Ok(fresh) => fresh.call_tool(name, args).await,
                    Err(reconnect_err) => Err(reconnect_err),
                }
            }
        }
    }
}

// =============================================================================
// HttpMcpTransport — Streamable HTTP client transport (D25 / FR-014)
// =============================================================================
//
// Design-First & Reuse note (Constitution Principle XI):
// rig-mcp 0.2.5 ships only StdioTransport + LoopbackTransport — NO HTTP/SSE
// client transport (verified: `rig-mcp-0.2.5/src/transport.rs` defines the
// McpTransport trait with no HTTP impl; `src/stdio.rs` is the only wire
// transport). The MCP Streamable HTTP spec (2024-11-05 revision) requires:
// POST JSON-RPC to the server endpoint with
// `Accept: application/json, text/event-stream`, parse the response as either
// a single JSON document or an SSE event stream, and echo back the
// `Mcp-Session-Id` header if the server issues one.
//
// This implementation reuses `reqwest` (already a workspace dep with json +
// stream + rustls features) — no new crate dependencies. It is intentionally
// minimal: one POST per JSON-RPC call, single response parsed. Servers that
// require server-initiated SSE notifications (e.g.
// `notifications/tools/list_changed`) are better served by the stdio
// transport; this HTTP path targets the common request-response MCP-over-HTTP
// pattern.

/// Minimal Streamable HTTP MCP client transport (D25 / FR-014).
struct HttpMcpTransport {
    endpoint: String,
    client: reqwest::Client,
    extra_headers: Vec<(String, String)>,
    /// Stored after the first response, echoed on subsequent requests per
    /// the MCP Streamable HTTP spec's optional session management.
    session_id: Mutex<Option<String>>,
    next_id: AtomicU64,
}

impl HttpMcpTransport {
    /// Send a JSON-RPC request and return the `result` object. Handles
    /// both `application/json` (single envelope) and `text/event-stream`
    /// (SSE-parsed) response content types per the MCP Streamable HTTP spec.
    async fn rpc(&self, method: &str, params: Option<Value>) -> Result<Value, KernelError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let mut request_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
        });
        if let Some(p) = params {
            request_body["params"] = p;
        }

        let mut req = self
            .client
            .post(&self.endpoint)
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .json(&request_body);

        if let Some(sid) = self.session_id.lock().await.as_ref() {
            req = req.header("Mcp-Session-Id", sid);
        }
        for (k, v) in &self.extra_headers {
            req = req.header(k, v);
        }

        let resp = req.send().await.map_err(|e| {
            KernelError::ToolFailed(format!(
                "MCP HTTP request to '{}' failed: {}",
                self.endpoint, e
            ))
        })?;

        // Capture Mcp-Session-Id if the server issues one.
        if let Some(sid) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            *self.session_id.lock().await = Some(sid.to_string());
        }

        let status = resp.status();
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = resp
            .text()
            .await
            .map_err(|e| KernelError::ToolFailed(format!("MCP HTTP body read failed: {}", e)))?;

        if !status.is_success() {
            let snippet = body.chars().take(300).collect::<String>();
            return Err(KernelError::ToolFailed(format!(
                "MCP HTTP {} from '{}' (Content-Type: {}): {}",
                status, self.endpoint, content_type, snippet
            )));
        }

        if content_type.contains("text/event-stream") {
            parse_sse_response(&body, id)
        } else {
            let envelope: Value = serde_json::from_str(&body).map_err(|e| {
                KernelError::ToolFailed(format!("MCP HTTP JSON parse failed: {}", e))
            })?;
            extract_rpc_result(&envelope, id)
        }
    }
}

/// Extract the `result` field from a JSON-RPC envelope, validating the
/// response ID and surfacing any `error` object.
fn extract_rpc_result(envelope: &Value, expected_id: u64) -> Result<Value, KernelError> {
    if let Some(resp_id) = envelope.get("id")
        && resp_id.as_u64() != Some(expected_id)
    {
        return Err(KernelError::ToolFailed(format!(
            "MCP JSON-RPC id mismatch: expected {}, got {:?}",
            expected_id, resp_id
        )));
    }
    if let Some(err) = envelope.get("error") {
        return Err(KernelError::ToolFailed(format!(
            "MCP JSON-RPC error from server: {}",
            err
        )));
    }
    envelope
        .get("result")
        .cloned()
        .ok_or_else(|| KernelError::ToolFailed("MCP JSON-RPC response missing 'result'".into()))
}

/// Parse a `text/event-stream` body and return the JSON-RPC `result`
/// matching `expected_id`. Iterates all event blocks, collecting `data:`
/// lines, and returns the first response (result/error) with a matching id.
/// Notifications (no id match, or events without result/error) are ignored.
fn parse_sse_response(body: &str, expected_id: u64) -> Result<Value, KernelError> {
    let mut last_err: Option<String> = None;

    for event_block in body.split("\n\n") {
        let data: String = event_block
            .lines()
            .filter_map(|line| line.strip_prefix("data:").map(|d| d.trim().to_string()))
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            continue;
        }
        if let Ok(envelope) = serde_json::from_str::<Value>(&data)
            && (envelope.get("result").is_some() || envelope.get("error").is_some())
        {
            match extract_rpc_result(&envelope, expected_id) {
                Ok(r) => return Ok(r),
                Err(e) => last_err = Some(e.to_string()),
            }
            // Notifications in a request-response stream are ignored — this
            // transport has no long-lived SSE subscription (T079 covers stdio).
        }
    }

    Err(KernelError::ToolFailed(format!(
        "MCP SSE stream had no result for request id {}{}",
        expected_id,
        last_err
            .map(|e| format!(" (last error: {})", e))
            .unwrap_or_default()
    )))
}

#[async_trait]
impl McpTransport for HttpMcpTransport {
    fn endpoint(&self) -> &str {
        &self.endpoint
    }

    async fn list_tools(&self) -> Result<Vec<ToolSchema>, KernelError> {
        let result = self.rpc("tools/list", None).await?;
        let tools = result
            .get("tools")
            .and_then(|t| t.as_array())
            .ok_or_else(|| {
                KernelError::ToolFailed("MCP tools/list: missing 'tools' array".into())
            })?;
        Ok(tools.iter().map(http_tool_to_schema).collect())
    }

    async fn call_tool(&self, name: &str, args: Value) -> Result<Value, KernelError> {
        let arguments = if args.is_null() {
            Value::Object(serde_json::Map::new())
        } else {
            args
        };
        let params = serde_json::json!({
            "name": name,
            "arguments": arguments,
        });
        let result = self.rpc("tools/call", Some(params)).await?;

        // MCP spec: prefer structuredContent, then text content parsed as JSON.
        if let Some(sc) = result.get("structuredContent") {
            return Ok(sc.clone());
        }
        if result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let msg = result
                .get("content")
                .and_then(|c| c.as_array())
                .and_then(|arr| {
                    arr.iter().find_map(|c| {
                        c.get("text")
                            .and_then(|t| t.as_str())
                            .map(|s| s.to_string())
                    })
                })
                .unwrap_or_else(|| "MCP tool returned error".to_string());
            return Err(KernelError::ToolFailed(msg));
        }
        if let Some(text) = result
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|arr| {
                arr.iter().find_map(|c| {
                    c.get("text")
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string())
                })
            })
        {
            if let Ok(parsed) = serde_json::from_str::<Value>(&text) {
                return Ok(parsed);
            }
            return Ok(Value::String(text));
        }
        Ok(result)
    }
}

/// Map a raw MCP tool descriptor (JSON from `tools/list`) to a [`ToolSchema`].
fn http_tool_to_schema(t: &Value) -> ToolSchema {
    ToolSchema {
        name: t
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        description: t
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        args_schema: t.get("inputSchema").cloned().unwrap_or(Value::Null),
        result_schema: t.get("outputSchema").cloned().unwrap_or(Value::Null),
    }
}

/// Construct an [`HttpMcpTransport`] for the given endpoint URL.
async fn spawn_http(
    url: &str,
    extra_headers: Option<&HashMap<String, String>>,
) -> Result<HttpMcpTransport, KernelError> {
    // FR-036: network egress policy — block SSRF vectors (metadata endpoints,
    // private ranges) before connecting to a configured MCP HTTP endpoint.
    let policy = zen_core::network_policy::NetworkPolicy::with_allow_hosts(vec![
        "localhost".into(),
        "127.0.0.1".into(),
    ]);
    if let Err(reason) = policy.validate_url(url) {
        return Err(KernelError::ToolFailed(format!(
            "MCP HTTP endpoint '{}' blocked by network policy: {}",
            url, reason
        )));
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| KernelError::ToolFailed(format!("HTTP client build failed: {}", e)))?;
    let headers_vec: Vec<(String, String)> = extra_headers
        .map(|h| h.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    Ok(HttpMcpTransport {
        endpoint: url.to_string(),
        client,
        extra_headers: headers_vec,
        session_id: Mutex::new(None),
        next_id: AtomicU64::new(0),
    })
}

/// Bootstrap MCP clients for every enabled, trusted server (FR-013/FR-018).
///
/// Untrusted servers are skipped unless `prompt` returns `true`, in which
/// case the decision is persisted via `trust_store.save(paths)`. When
/// `prompt` is `None` (headless orchestrator), untrusted servers are
/// skipped with a warning — never fatal.
///
/// Returns the strong stdio toolset handles (T243). The refresh worker only
/// holds a `Weak`; the caller is responsible for retaining these for as long
/// as it wants `tools/list_changed` refreshes to keep working.
pub async fn bootstrap_mcp_clients(
    registry: &ToolRegistry,
    mcp_servers: &[zen_core::config::McpServerConfig],
    trust_store: &mut zen_core::config::McpTrustStore,
    paths: &zen_core::paths::ZenPaths,
    prompt: TrustPrompt,
) -> Vec<Arc<McpToolSet>> {
    // Phase 1 (sequential, no network): trust gate + config validation.
    let mut plans = Vec::new();
    for server in mcp_servers {
        if !server.enabled {
            continue;
        }

        // FR-018: first-run trust gate.
        if !trust_store.is_trusted(&server.name) {
            let trusted = match &prompt {
                Some(ask) => ask(server),
                None => false,
            };
            if trusted {
                trust_store.set_trusted(&server.name, true);
                if let Err(e) = trust_store.save(paths) {
                    warn!(
                        server = %server.name,
                        error = %e, "Failed to persist MCP trust decision"
                    );
                }
                info!(server = %server.name, "MCP server trusted via first-run prompt");
            } else {
                // D27: when prompt is None (headless / library default),
                // untrusted servers are skipped with a clear actionable
                // hint — never a blocking prompt.
                warn!(
                    server = %server.name,
                    "MCP server not trusted, skipping — run `zen plugin mcp trust {}` to trust it",
                    server.name
                );
                continue;
            }
        }

        match server.transport.as_str() {
            "stdio" => {
                let command = match &server.command {
                    Some(c) => c.clone(),
                    None => {
                        warn!(server = %server.name, "MCP stdio server missing 'command'");
                        continue;
                    }
                };
                plans.push(ConnectPlan::Stdio {
                    name: server.name.clone(),
                    endpoint: format!("stdio://{}", server.name),
                    command,
                    args: server.args.clone().unwrap_or_default(),
                    env: server.env.clone().unwrap_or_default(),
                    auto_refresh: server.auto_refresh,
                });
            }
            "http" | "https" => {
                // D25 / FR-014: HTTP transport via HttpMcpTransport (see above).
                let url = match &server.url {
                    Some(u) => u.clone(),
                    None => {
                        warn!(
                            server = %server.name,
                            transport = %server.transport,
                            "MCP {} server missing 'url'",
                            server.transport
                        );
                        continue;
                    }
                };
                plans.push(ConnectPlan::Http {
                    name: server.name.clone(),
                    url,
                    headers: server.headers.clone(),
                });
            }
            other => {
                warn!(
                    server = %server.name,
                    transport = %other, "Unknown MCP transport"
                );
            }
        }
    }

    // Phase 2 (parallel): sub-process spawn / TCP connect + initialize
    // handshake + tools/list. The wall-clock collapses from sum(N) to
    // max(N); a hung server is bounded by the stdio handshake timeout.
    let opened = futures_util::future::join_all(plans.iter().map(|plan| async move {
        match plan {
            ConnectPlan::Stdio {
                name,
                endpoint,
                command,
                args,
                env,
                ..
            } => open_stdio_initial(name, endpoint, command, args, env).await,
            ConnectPlan::Http { name, url, headers } => {
                open_http_initial(name, url, headers.as_ref()).await
            }
        }
    }))
    .await;

    // Phase 3 (sequential, plan order preserved): registry apply + reconnect
    // wrapper + refresh worker. Registry mutation stays off the concurrent
    // path on purpose — only the network phase above is parallel.
    let mut toolsets = Vec::new();
    for (plan, open) in plans.into_iter().zip(opened) {
        match (plan, open) {
            (
                ConnectPlan::Stdio {
                    name,
                    endpoint,
                    auto_refresh,
                    ..
                },
                Ok(Opened::Stdio(opened)),
            ) => {
                let result = connect_stdio(registry, &name, &endpoint, opened, auto_refresh).await;
                match result {
                    Ok((count, toolset)) => {
                        info!(server = %name, tools = count, "MCP client connected");
                        toolsets.push(toolset);
                    }
                    Err(e) => {
                        warn!(server = %name, error = %e, "MCP connection failed");
                    }
                }
            }
            (ConnectPlan::Stdio { name, .. }, Err(e)) => {
                warn!(server = %name, error = %e, "MCP connection failed");
            }
            (ConnectPlan::Http { name, url, .. }, Ok(Opened::Http(opened))) => {
                match connect_http(registry, &name, &url, opened).await {
                    Ok(count) => {
                        info!(server = %name, url = %url, tools = count, "MCP HTTP client connected");
                    }
                    Err(e) => {
                        warn!(server = %name, url = %url, error = %e, "MCP HTTP connection failed");
                    }
                }
            }
            (ConnectPlan::Http { name, url, .. }, Err(e)) => {
                warn!(server = %name, url = %url, error = %e, "MCP HTTP connection failed");
            }
            (ConnectPlan::Stdio { .. }, Ok(Opened::Http(_)))
            | (ConnectPlan::Http { .. }, Ok(Opened::Stdio(_))) => {
                unreachable!("open phase mirrors the plan variant");
            }
        }
    }
    toolsets
}

enum ConnectPlan {
    Stdio {
        name: String,
        endpoint: String,
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
        auto_refresh: bool,
    },
    Http {
        name: String,
        url: String,
        headers: Option<HashMap<String, String>>,
    },
}

enum Opened {
    Stdio(StdioOpen),
    Http(HttpOpen),
}

/// Parallel-open artifacts for one stdio server: the live transport plus the
/// pieces [`connect_stdio`] needs to materialize the reconnecting wrapper.
struct StdioOpen {
    transport: Arc<dyn McpTransport>,
    spawn: SpawnFactory,
    schemas: Vec<ToolSchema>,
    signals: tokio::sync::mpsc::Receiver<()>,
    spawn_ms: u64,
    list_ms: u64,
}

/// Parallel-open artifacts for the HTTP path (mirror of [`StdioOpen`]).
struct HttpOpen {
    transport: Arc<dyn McpTransport>,
    spawn: SpawnFactory,
    schemas: Vec<ToolSchema>,
    connect_ms: u64,
    list_ms: u64,
}

/// Spawn the stdio child and fetch its initial `tools/list` — the two slow
/// awaits of [`connect_stdio`], extracted so the bootstrap phase can run
/// them concurrently across servers (FR-041).
#[allow(clippy::too_many_arguments)]
async fn open_stdio_initial(
    server_name: &str,
    endpoint: &str,
    program: &str,
    args: &[String],
    env: &HashMap<String, String>,
) -> Result<Opened, String> {
    let (notify, signals) = mpsc::channel(1);
    let started = Instant::now();
    let transport: Arc<dyn McpTransport> = Arc::new(
        ZenStdioTransport::spawn(endpoint, program, args, env, Some(notify.clone()))
            .await
            .map_err(|e| format!("spawn failed: {}", e))?,
    );
    let spawn_ms = started.elapsed().as_millis() as u64;
    info!(server = server_name, spawn_ms, "MCP subprocess spawned");

    let list_started = Instant::now();
    let schemas = transport
        .list_tools()
        .await
        .map_err(|e| format!("list_tools failed: {}", e))?;
    let list_ms = list_started.elapsed().as_millis() as u64;

    let endpoint_factory = endpoint.to_string();
    let program_factory = program.to_string();
    let args_factory = args.to_vec();
    let env_factory = env.clone();
    let spawn: SpawnFactory = Arc::new(move || {
        let endpoint = endpoint_factory.clone();
        let program = program_factory.clone();
        let args = args_factory.clone();
        let env = env_factory.clone();
        let notify = notify.clone();
        Box::pin(async move {
            let transport =
                ZenStdioTransport::spawn(&endpoint, &program, &args, &env, Some(notify.clone()))
                    .await
                    .map_err(|e| e.to_string())?;
            // T244: a re-spawn gets a fresh peer, so re-apply the catalog now
            // rather than waiting for the server to emit its own
            // `tools/list_changed`. Failed spawns never send. Coalescing:
            // try_send drops the signal if one is already queued — the
            // worker will relist regardless.
            let _ = notify.try_send(());
            let transport: Arc<dyn McpTransport> = Arc::new(transport);
            Ok(transport)
        })
    });

    Ok(Opened::Stdio(StdioOpen {
        transport,
        spawn,
        schemas,
        signals,
        spawn_ms,
        list_ms,
    }))
}

/// Spawn the HTTP transport and fetch its initial `tools/list` — the two
/// slow awaits of [`connect_http`], extracted for the parallel bootstrap
/// phase (FR-041).
async fn open_http_initial(
    server_name: &str,
    url: &str,
    extra_headers: Option<&HashMap<String, String>>,
) -> Result<Opened, String> {
    let started = Instant::now();
    let transport: Arc<dyn McpTransport> = Arc::new(
        spawn_http(url, extra_headers)
            .await
            .map_err(|e| format!("spawn failed: {}", e))?,
    );
    let connect_ms = started.elapsed().as_millis() as u64;
    info!(server = server_name, url = %url, connect_ms, "MCP HTTP transport connected");

    let list_started = Instant::now();
    let schemas = transport
        .list_tools()
        .await
        .map_err(|e| format!("list_tools failed: {}", e))?;
    let list_ms = list_started.elapsed().as_millis() as u64;

    let url_factory = url.to_string();
    let headers_factory: HashMap<String, String> = extra_headers.cloned().unwrap_or_default();
    let spawn: SpawnFactory = Arc::new(move || {
        let url = url_factory.clone();
        let headers = headers_factory.clone();
        Box::pin(async move {
            spawn_http(&url, Some(&headers))
                .await
                .map(|t| Arc::new(t) as Arc<dyn McpTransport>)
                .map_err(|e| e.to_string())
        })
    });

    Ok(Opened::Http(HttpOpen {
        transport,
        spawn,
        schemas,
        connect_ms,
        list_ms,
    }))
}

async fn connect_stdio(
    registry: &ToolRegistry,
    server_name: &str,
    endpoint: &str,
    opened: StdioOpen,
    auto_refresh: bool,
) -> Result<(usize, Arc<McpToolSet>), String> {
    info!(
        server = server_name,
        spawn_ms = opened.spawn_ms,
        list_ms = opened.list_ms,
        tools = opened.schemas.len(),
        "MCP stdio opened (FR-041 hot-path metrics)"
    );

    let reconnecting = Arc::new(ReconnectingMcpTransport::new(
        server_name,
        endpoint,
        opened.spawn,
        opened.transport,
    ));

    let tools = Arc::new(McpToolSet::new(
        server_name,
        registry.clone(),
        reconnecting.clone(),
    ));
    let update = tools.apply(opened.schemas).await;
    if auto_refresh {
        let worker = spawn_tool_refresh_worker(opened.signals, Arc::downgrade(&tools));

        // Intentionally detached: the worker holds a `Weak` toolset and exits
        // once the caller drops its strong ref — no handle plumbing needed.
        drop(worker);
    }

    Ok((update.added, tools))
}

/// Materialize the HTTP/HTTPS MCP server connection: wrap the opened
/// transport in [`ReconnectingMcpTransport`] (so FR-013 crash recovery
/// applies) and register all discovered tools. Mirrors [`connect_stdio`]
/// structurally.
async fn connect_http(
    registry: &ToolRegistry,
    server_name: &str,
    url: &str,
    opened: HttpOpen,
) -> Result<usize, String> {
    info!(
        server = server_name,
        url = %url,
        connect_ms = opened.connect_ms,
        list_ms = opened.list_ms,
        tools = opened.schemas.len(),
        "MCP HTTP opened (FR-041 hot-path metrics)"
    );

    let reconnecting = Arc::new(ReconnectingMcpTransport::new(
        server_name,
        url,
        opened.spawn,
        opened.transport,
    ));

    // HTTP carries no server-initiated `tools/list_changed` stream in this
    // minimal transport (one POST per call), so the set is seeded once.
    let tools = McpToolSet::new(server_name, registry.clone(), reconnecting.clone());
    let update = tools.apply(opened.schemas).await;

    Ok(update.added)
}

/// Smoke-test connectivity to a single MCP server (D4 / `zen plugin mcp
/// reconnect <name>`). Spawns (stdio) or connects (HTTP), calls
/// `tools/list`, and returns the discovered tool count. Does NOT register
/// tools — tool registration happens in the running agent process via
/// [`bootstrap_mcp_clients`]. This exercises the same spawn + list_tools
/// path that auto-reconnect (FR-013) uses on failure, giving the user
/// immediate feedback on whether a server is reachable and correctly
/// configured.
pub async fn reconnect_mcp_server(
    server: &zen_core::config::McpServerConfig,
) -> Result<usize, String> {
    match server.transport.as_str() {
        "stdio" => {
            let command = server
                .command
                .clone()
                .ok_or_else(|| format!("server '{}' (stdio) has no 'command'", server.name))?;
            let args = server.args.clone().unwrap_or_default();
            let env = server.env.clone().unwrap_or_default();
            let endpoint = format!("stdio://{}", server.name);
            let transport: Arc<dyn McpTransport> = Arc::new(
                ZenStdioTransport::spawn(&endpoint, &command, &args, &env, None)
                    .await
                    .map_err(|e| format!("spawn failed: {}", e))?,
            );
            let schemas = transport
                .list_tools()
                .await
                .map_err(|e| format!("list_tools failed: {}", e))?;
            Ok(schemas.len())
        }
        "http" | "https" => {
            let url = server.url.clone().ok_or_else(|| {
                format!(
                    "server '{}' ({}) has no 'url'",
                    server.name, server.transport
                )
            })?;
            let transport: Arc<dyn McpTransport> = Arc::new(
                spawn_http(&url, server.headers.as_ref())
                    .await
                    .map_err(|e| format!("connect failed: {}", e))?,
            );
            let schemas = transport
                .list_tools()
                .await
                .map_err(|e| format!("list_tools failed: {}", e))?;
            Ok(schemas.len())
        }
        other => Err(format!(
            "unknown transport '{}' for server '{}'",
            other, server.name
        )),
    }
}

/// Environment for a spawned MCP stdio child (FR-037 / T074).
///
/// Parent secrets are stripped by the shared `zen_core::env_scrub` rules.
/// Per-server `env` entries from `[mcp_servers]` are user-authoritative: they
/// are allowlisted so a legitimately secret-named variable configured for the
/// server (e.g. `GITHUB_TOKEN`) survives the pattern sweep, while
/// loader-injection vars stay blocked even when configured (SC-017).
fn child_env(config_env: &HashMap<String, String>) -> HashMap<String, String> {
    let scrub = zen_core::env_scrub::EnvScrubConfig {
        extra_scrub: Vec::new(),
        allowlist: config_env.keys().cloned().collect(),
    };
    zen_core::env_scrub::scrubbed_env(config_env, &scrub)
}

/// rmcp client handler that only forwards `notifications/tools/list_changed`
/// as a wake-up for [`spawn_tool_refresh_worker`].
struct ToolChangeNotifier {
    notify: Option<mpsc::Sender<()>>,
}

impl ClientHandler for ToolChangeNotifier {
    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        if let Some(tx) = &self.notify {
            // A dropped receiver means no refresh worker is attached (one-shot
            // connectivity probes): discarding the wake-up is correct there.
            // Bounded(1) + try_send: the worker contract is edge-triggered, so
            // the queued signal already guarantees a relist; a second signal
            // adds nothing — drop it instead of buffering a flood from a
            // misbehaving server.
            let _ = tx.try_send(());
        }
    }
}

/// Stdio MCP transport that owns its client layer so the child can be
/// spawned with a scrubbed environment (T074) and can surface
/// `notifications/tools/list_changed` (T079) — neither is possible through
/// rig-mcp 0.2.5's `StdioTransport`, which exposes no env control and no
/// notification hook. Protocol framing and the stdout drain remain rmcp's.
struct ZenStdioTransport {
    endpoint: String,
    peer: Peer<RoleClient>,
    _service: Arc<RunningService<RoleClient, ToolChangeNotifier>>,
}

const MCP_STDIO_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);

impl ZenStdioTransport {
    async fn spawn(
        endpoint: &str,
        program: &str,
        args: &[String],
        env: &HashMap<String, String>,
        notify: Option<mpsc::Sender<()>>,
    ) -> Result<Self, KernelError> {
        let mut command = tokio::process::Command::new(program);
        command.args(args);
        command.env_clear();
        command.envs(child_env(env));

        let transport = TokioChildProcess::new(command)
            .map_err(|e| KernelError::ToolFailed(format!("mcp.spawn: {}", e)))?;
        // Bounded handshake, mirrored from the HTTP path's 60s reqwest
        // timeout: rmcp's plain `serve()` has no startup deadline, so a hung
        // stdio server would otherwise stall bootstrap unboundedly.
        let service = tokio::time::timeout(
            MCP_STDIO_HANDSHAKE_TIMEOUT,
            ToolChangeNotifier { notify }.serve(transport),
        )
        .await
        .map_err(|_| {
            KernelError::ToolFailed(format!(
                "mcp.connect: no initialize reply within {MCP_STDIO_HANDSHAKE_TIMEOUT:?}"
            ))
        })?
        .map_err(|e| KernelError::ToolFailed(format!("mcp.connect: {}", e)))?;
        let peer = service.peer().clone();
        Ok(Self {
            endpoint: endpoint.to_string(),
            peer,
            _service: Arc::new(service),
        })
    }
}

fn rmcp_tool_to_schema(tool: RmcpTool) -> ToolSchema {
    ToolSchema {
        name: tool.name.to_string(),
        description: tool.description.map(|d| d.to_string()).unwrap_or_default(),
        args_schema: Value::Object((*tool.input_schema).clone()),
        result_schema: tool
            .output_schema
            .map(|s| Value::Object((*s).clone()))
            .unwrap_or(Value::Null),
    }
}

#[async_trait]
impl McpTransport for ZenStdioTransport {
    fn endpoint(&self) -> &str {
        &self.endpoint
    }

    async fn list_tools(&self) -> Result<Vec<ToolSchema>, KernelError> {
        let tools = self
            .peer
            .list_all_tools()
            .await
            .map_err(|e| KernelError::ToolFailed(format!("tools/list: {}", e)))?;
        Ok(tools.into_iter().map(rmcp_tool_to_schema).collect())
    }

    async fn call_tool(&self, name: &str, args: Value) -> Result<Value, KernelError> {
        let arguments = match args {
            Value::Object(map) => Some(map),
            Value::Null => None,
            other => {
                return Err(KernelError::InvalidArgument(format!(
                    "tools/call requires an object or null arguments, got {other}"
                )));
            }
        };
        let mut params = CallToolRequestParams::new(name.to_string());
        params.arguments = arguments;
        let result = self
            .peer
            .call_tool(params)
            .await
            .map_err(|e| KernelError::ToolFailed(format!("tools/call: {}", e)))?;

        if result.is_error.unwrap_or(false) {
            let msg = result
                .content
                .iter()
                .find_map(|c| c.as_text().map(|t| t.text.clone()))
                .unwrap_or_else(|| "tool returned error".to_string());
            return Err(KernelError::ToolFailed(msg));
        }
        if let Some(value) = result.structured_content {
            return Ok(value);
        }
        if let Some(text) = result
            .content
            .iter()
            .find_map(|c| c.as_text().map(|t| t.text.clone()))
        {
            if let Ok(parsed) = serde_json::from_str::<Value>(&text) {
                return Ok(parsed);
            }
            return Ok(Value::String(text.to_string()));
        }
        Ok(Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Mock transport whose calls succeed or fail based on `fail`. Used
    /// instead of `LoopbackTransport` because the latter requires a
    /// `ToolRegistry` and cannot inject the failures needed to exercise
    /// reconnect.
    struct MockTransport {
        fail: bool,
    }

    #[async_trait]
    impl McpTransport for MockTransport {
        fn endpoint(&self) -> &str {
            "mock://test"
        }

        async fn list_tools(&self) -> Result<Vec<ToolSchema>, KernelError> {
            if self.fail {
                Err(KernelError::ToolFailed("mock list_tools failure".into()))
            } else {
                Ok(vec![])
            }
        }

        async fn call_tool(&self, _name: &str, _args: Value) -> Result<Value, KernelError> {
            if self.fail {
                Err(KernelError::ToolFailed("mock call_tool failure".into()))
            } else {
                Ok(Value::Null)
            }
        }
    }

    fn failing_transport() -> Arc<dyn McpTransport> {
        Arc::new(MockTransport { fail: true })
    }

    fn healthy_transport() -> Arc<dyn McpTransport> {
        Arc::new(MockTransport { fail: false })
    }

    #[test]
    fn namespaced_tool_name_produces_server_dot_tool() {
        assert_eq!(
            namespaced_tool_name("brave", "web_search"),
            "brave.web_search"
        );
        assert_eq!(
            namespaced_tool_name("github", "create_issue"),
            "github.create_issue"
        );
        assert!(!namespaced_tool_name("brave", "web_search").starts_with("mcp."));
    }

    #[test]
    fn backoff_schedule_is_one_two_four() {
        assert_eq!(BACKOFF_SECS, [1, 2, 4]);
        assert_eq!(MAX_RECONNECT_ATTEMPTS, 3);
    }

    fn instant_backoffs() -> Vec<Duration> {
        vec![Duration::ZERO, Duration::ZERO, Duration::ZERO]
    }

    // (a) 3 failed re-spawn attempts → marked unavailable, clear error.
    #[tokio::test]
    async fn marks_unavailable_after_three_spawn_failures() {
        let spawn: SpawnFactory = Arc::new(|| {
            Box::pin(async { Err::<Arc<dyn McpTransport>, String>("spawn boom".into()) })
        });
        let transport = ReconnectingMcpTransport {
            server_name: "srv-x".into(),
            endpoint: "stdio://srv-x".into(),
            spawn,
            current: Mutex::new(Some(failing_transport())),
            backoffs: instant_backoffs(),
        };

        let err = transport.call_tool("ping", Value::Null).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unavailable"), "got: {msg}");
        assert!(msg.contains("srv-x"), "error must name the server: {msg}");

        let err2 = transport.call_tool("ping", Value::Null).await.unwrap_err();
        assert!(err2.to_string().contains("unavailable"));
    }

    // (b) A successful re-spawn retries the call and recovers.
    #[tokio::test]
    async fn recovers_when_a_reconnect_succeeds() {
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();
        let spawn: SpawnFactory = Arc::new(move || {
            let c = counter_clone.clone();
            Box::pin(async move {
                let n = c.fetch_add(1, Ordering::Relaxed);
                if n == 0 {
                    Err("first spawn fails".into())
                } else {
                    Ok(healthy_transport())
                }
            })
        });
        let transport = ReconnectingMcpTransport {
            server_name: "srv-y".into(),
            endpoint: "stdio://srv-y".into(),
            spawn,
            current: Mutex::new(Some(failing_transport())),
            backoffs: instant_backoffs(),
        };

        let result = transport.call_tool("ping", Value::Null).await;
        assert!(result.is_ok(), "should recover: {:?}", result);
        assert_eq!(counter.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn healthy_server_never_reconnects() {
        let attempts = Arc::new(AtomicU32::new(0));
        let attempts_clone = attempts.clone();
        let spawn: SpawnFactory = Arc::new(move || {
            let c = attempts_clone.clone();
            Box::pin(async move {
                c.fetch_add(1, Ordering::Relaxed);
                Ok(healthy_transport())
            })
        });
        let transport = ReconnectingMcpTransport {
            server_name: "srv-ok".into(),
            endpoint: "stdio://srv-ok".into(),
            spawn,
            current: Mutex::new(Some(healthy_transport())),
            backoffs: instant_backoffs(),
        };

        let result = transport.call_tool("ping", Value::Null).await;
        assert!(result.is_ok());
        assert_eq!(attempts.load(Ordering::Relaxed), 0, "no spawn should occur");
    }

    /// In-memory transport whose `tools/list` is driven by a shared snapshot,
    /// so a test can play the server's `list_changed` role.
    struct ListMock {
        tools: Arc<Mutex<Vec<ToolSchema>>>,
    }

    #[async_trait]
    impl McpTransport for ListMock {
        fn endpoint(&self) -> &str {
            "mock://list"
        }

        async fn list_tools(&self) -> Result<Vec<ToolSchema>, KernelError> {
            Ok(self.tools.lock().await.clone())
        }

        async fn call_tool(&self, _name: &str, _args: Value) -> Result<Value, KernelError> {
            Ok(Value::Null)
        }
    }

    fn schema(name: &str, description: &str) -> ToolSchema {
        ToolSchema {
            name: name.to_string(),
            description: description.to_string(),
            args_schema: Value::Null,
            result_schema: Value::Null,
        }
    }

    struct SeededSet {
        schemas: Arc<Mutex<Vec<ToolSchema>>>,
        registry: ToolRegistry,
        tools: Arc<McpToolSet>,
    }

    fn seeded_set(server: &str, initial: Vec<ToolSchema>) -> SeededSet {
        let schemas = Arc::new(Mutex::new(initial));
        let registry = ToolRegistry::new();
        let tools = Arc::new(McpToolSet::new(
            server,
            registry.clone(),
            Arc::new(ListMock {
                tools: schemas.clone(),
            }),
        ));
        SeededSet {
            schemas,
            registry,
            tools,
        }
    }

    #[test]
    fn child_env_scrubs_parent_secrets_and_keeps_config_env() {
        // GIVEN a parent process carrying a secret and a per-server env map
        // SAFETY: unique process-global key mutated only by this test.
        unsafe {
            std::env::set_var("ZEN_MCP_TEST_PARENT_API_KEY", "leak-me");
        }
        let mut config_env = HashMap::new();
        config_env.insert("ZEN_MCP_FEATURE".to_string(), "on".to_string());
        config_env.insert("GITHUB_TOKEN".to_string(), "configured".to_string());

        // WHEN the child environment is derived
        let env = child_env(&config_env);
        unsafe {
            std::env::remove_var("ZEN_MCP_TEST_PARENT_API_KEY");
        }

        // THEN parent secrets are gone and configured entries are authoritative
        assert!(!env.contains_key("ZEN_MCP_TEST_PARENT_API_KEY"));
        assert_eq!(env.get("ZEN_MCP_FEATURE").map(String::as_str), Some("on"));
        assert_eq!(
            env.get("GITHUB_TOKEN").map(String::as_str),
            Some("configured"),
            "per-server config env must survive the pattern sweep"
        );
    }

    #[test]
    fn child_env_rejects_loader_injection_even_when_configured() {
        // GIVEN a server env map trying to re-arm a loader hook
        let mut config_env = HashMap::new();
        config_env.insert("LD_PRELOAD".to_string(), "/tmp/evil.so".to_string());

        // WHEN the child environment is derived
        let env = child_env(&config_env);

        // THEN the loader var is dropped despite the config allowlist
        assert!(!env.contains_key("LD_PRELOAD"));
    }

    #[tokio::test]
    async fn list_changed_refreshes_proxy_registry_in_place() {
        // GIVEN a server exposing a and b, both registered
        let SeededSet {
            schemas,
            registry,
            tools,
        } = seeded_set("srv", vec![schema("a", ""), schema("b", "")]);
        let seeded = tools.list_and_apply().await.unwrap();
        assert_eq!(seeded.added, 2);
        assert!(registry.get("srv.a").is_ok());

        // WHEN the server drops a and adds c
        *schemas.lock().await = vec![schema("b", ""), schema("c", "")];
        let update = tools.list_and_apply().await.unwrap();

        // THEN c is registered live, a is tombstoned, b is untouched
        let expected = ToolUpdate {
            added: 1,
            removed: 1,
            updated: 0,
        };
        assert_eq!(update, expected);
        assert!(registry.get("srv.c").is_ok());
        let removed = registry.get("srv.a").unwrap();
        let err = removed.invoke(Value::Null).await.unwrap_err();
        assert!(err.to_string().contains("no longer offered"), "got: {err}");
    }

    #[tokio::test]
    async fn list_changed_updates_schema_in_place() {
        // GIVEN a registered tool with an old description
        let SeededSet {
            schemas,
            registry,
            tools,
        } = seeded_set("srv", vec![schema("a", "old")]);
        tools.list_and_apply().await.unwrap();

        // WHEN the server changes only the schema
        *schemas.lock().await = vec![schema("a", "new")];
        let update = tools.list_and_apply().await.unwrap();

        // THEN the same registry entry serves the refreshed schema
        assert_eq!(update.updated, 1);
        assert_eq!(registry.get("srv.a").unwrap().schema().description, "new");
    }

    #[tokio::test]
    async fn unchanged_tool_list_is_a_noop() {
        // GIVEN a seeded tool set
        let SeededSet { tools, .. } = seeded_set("srv", vec![schema("a", "same")]);
        tools.list_and_apply().await.unwrap();

        // WHEN the server re-lists an identical catalog
        let second = tools.list_and_apply().await.unwrap();

        // THEN the refresh reports no delta
        assert_eq!(second, ToolUpdate::default());
        assert!(!second.changed());
    }

    #[tokio::test]
    async fn notification_signal_triggers_relist_and_registration() {
        // GIVEN a live tool set and an attached refresh worker
        let SeededSet {
            schemas,
            registry,
            tools,
        } = seeded_set("srv", vec![schema("a", "")]);
        tools.list_and_apply().await.unwrap();
        let (tx, rx) = mpsc::channel(1);
        let worker = spawn_tool_refresh_worker(rx, Arc::downgrade(&tools));

        // WHEN the server gains tool b and emits tools/list_changed
        *schemas.lock().await = vec![schema("a", ""), schema("b", "")];
        tx.try_send(()).unwrap();

        // THEN the worker re-lists and registers b without a session restart
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while registry.get("srv.b").is_err() {
            assert!(
                std::time::Instant::now() < deadline,
                "refresh worker never registered the new tool"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        worker.abort();
    }

    /// Dead subprocess: both listing and calls fail, so a reconnect must
    /// replace it wholesale.
    struct DeadMock;

    #[async_trait]
    impl McpTransport for DeadMock {
        fn endpoint(&self) -> &str {
            "mock://dead"
        }

        async fn list_tools(&self) -> Result<Vec<ToolSchema>, KernelError> {
            Err(KernelError::ToolFailed("mock transport dead".into()))
        }

        async fn call_tool(&self, _name: &str, _args: Value) -> Result<Value, KernelError> {
            Err(KernelError::ToolFailed("mock transport dead".into()))
        }
    }

    /// List mock whose availability can be flipped at runtime, so a test can
    /// drive the worker through unavailable → healthy transitions.
    struct FlakyListMock {
        tools: Arc<Mutex<Vec<ToolSchema>>>,
        fail: Arc<AtomicBool>,
    }

    #[async_trait]
    impl McpTransport for FlakyListMock {
        fn endpoint(&self) -> &str {
            "mock://flaky"
        }

        async fn list_tools(&self) -> Result<Vec<ToolSchema>, KernelError> {
            if self.fail.load(Ordering::Relaxed) {
                Err(KernelError::ToolFailed(
                    "MCP server 'srv' is unavailable (marked down after 3 failed re-spawn attempts)"
                        .into(),
                ))
            } else {
                Ok(self.tools.lock().await.clone())
            }
        }

        async fn call_tool(&self, _name: &str, _args: Value) -> Result<Value, KernelError> {
            Ok(Value::Null)
        }
    }

    #[tokio::test]
    async fn worker_exits_when_toolset_dropped() {
        // GIVEN a worker holding only a Weak toolset ref
        let SeededSet { tools, .. } = seeded_set("srv", vec![schema("a", "")]);
        let (tx, rx) = mpsc::channel(1);
        let worker = spawn_tool_refresh_worker(rx, Arc::downgrade(&tools));

        // WHEN the strong owner and the signal sender are both dropped
        drop(tools);
        drop(tx);

        // THEN the channel closes and the worker exits instead of leaking
        tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .expect("refresh worker must exit after the toolset is dropped")
            .expect("refresh worker task must not panic");
    }

    #[tokio::test]
    async fn respawn_notify_triggers_relist() {
        // GIVEN a catalog seeded from before the crash
        let registry = ToolRegistry::new();
        let (tx, rx) = mpsc::channel(1);
        let initial: Arc<dyn McpTransport> = Arc::new(DeadMock);
        let respawned: Arc<dyn McpTransport> = Arc::new(ListMock {
            tools: Arc::new(Mutex::new(vec![schema("b", "")])),
        });
        let notify = tx.clone();
        let spawn: SpawnFactory = Arc::new(move || {
            let fresh = respawned.clone();
            let notify = notify.clone();
            Box::pin(async move {
                // Mirrors connect_stdio's factory (T244): a successful
                // re-spawn wakes the worker so the catalog is re-applied.
                // Bounded(1): coalesces like the production send site.
                let _ = notify.try_send(());
                Ok(fresh)
            })
        });
        let reconnecting = Arc::new(ReconnectingMcpTransport {
            server_name: "srv".into(),
            endpoint: "stdio://srv".into(),
            spawn,
            current: Mutex::new(Some(initial)),
            backoffs: instant_backoffs(),
        });
        let tools = Arc::new(McpToolSet::new(
            "srv",
            registry.clone(),
            reconnecting.clone(),
        ));
        tools.apply(vec![schema("a", "")]).await;
        assert!(registry.get("srv.a").is_ok());
        let worker = spawn_tool_refresh_worker(rx, Arc::downgrade(&tools));

        // WHEN a call crashes, reconnects, and the transport is re-spawned
        // (no tools/list_changed is emitted by the test)
        reconnecting.call_tool("ping", Value::Null).await.unwrap();

        // THEN the respawn wake-up alone makes the worker apply the new catalog
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while registry.get("srv.b").is_err() {
            assert!(
                std::time::Instant::now() < deadline,
                "respawn did not re-apply the catalog"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let removed = registry.get("srv.a").unwrap();
        let err = removed.invoke(Value::Null).await.unwrap_err();
        assert!(err.to_string().contains("no longer offered"), "got: {err}");
        worker.abort();
    }

    #[tokio::test]
    async fn bounded_notify_channel_coalesces_signals() {
        // D7 pin: the notify channel is bounded(1); try_send DROPS a signal
        // when one is already queued instead of buffering a flood from a
        // misbehaving server. The refresh worker relists on every received
        // signal, so the dropped duplicate is redundant by contract.
        let (tx, mut rx) = mpsc::channel(1);
        tx.try_send(())
            .expect("first signal fits the empty channel");
        tx.try_send(())
            .expect_err("second unprocessed signal coalesces (Full)");
        // Exactly one wake-up survives for the worker to consume.
        rx.recv().await.expect("the queued signal is delivered");
        tx.try_send(())
            .expect("channel frees again after the worker drains");
        assert!(
            rx.try_recv().is_ok(),
            "the post-drain signal is buffered and will wake the worker"
        );
    }

    #[tokio::test]
    async fn degraded_set_after_unavailable() {
        // GIVEN a toolset whose transport reports itself unavailable
        let fail = Arc::new(AtomicBool::new(true));
        let tools = Arc::new(McpToolSet::new(
            "srv",
            ToolRegistry::new(),
            Arc::new(FlakyListMock {
                tools: Arc::new(Mutex::new(vec![schema("a", "")])),
                fail: fail.clone(),
            }),
        ));
        let (tx, rx) = mpsc::channel(1);
        let worker = spawn_tool_refresh_worker(rx, Arc::downgrade(&tools));

        // WHEN a refresh fails on the unavailable transport
        tx.try_send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !tools.is_degraded() {
            assert!(
                std::time::Instant::now() < deadline,
                "worker did not mark the server degraded"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // THEN the next successful apply clears the flag
        fail.store(false, Ordering::Relaxed);
        tx.try_send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while tools.is_degraded() {
            assert!(
                std::time::Instant::now() < deadline,
                "worker did not clear the degraded flag"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        worker.abort();
    }

    #[test]
    fn parse_sse_handles_list_changed_notification_without_panic() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}\n\n";
        let result = parse_sse_response(body, 1);
        assert!(
            result.is_err(),
            "a notification-only stream has no result for the request id"
        );
    }
}
