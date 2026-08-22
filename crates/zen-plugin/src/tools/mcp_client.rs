use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use rig_compose::registry::{KernelError, ToolRegistry};
use rig_compose::tool::{Tool, ToolSchema};
use rig_mcp::{McpTransport, StdioTransport};
use serde_json::Value;
use tokio::sync::Mutex;
use tracing::{info, warn};

const MAX_RECONNECT_ATTEMPTS: u32 = 3;
/// Exponential backoff schedule (FR-013): one delay per attempt — 1s, 2s, 4s.
/// Single home for the schedule is `zen_core::retry::BACKOFF_SECS`.
const BACKOFF_SECS: &[u64] = zen_core::retry::BACKOFF_SECS;

type SpawnFuture = Pin<Box<dyn Future<Output = Result<Arc<dyn McpTransport>, String>> + Send>>;

/// Re-spawns the MCP subprocess on demand. A factory is needed because
/// `StdioTransport` exposes neither its child handle nor a `clone`/`restart`
/// method — the only way to recover from a crash is to call
/// `StdioTransport::spawn` again with the original args, which this closure
/// captures.
type SpawnFactory = Arc<dyn Fn() -> SpawnFuture + Send + Sync>;

/// First-run trust prompt (FR-018): called with a server config, returns
/// `true` to trust it. `None` means no prompt is available (headless
/// orchestrator) — untrusted servers are then skipped non-fatally.
pub type TrustPrompt =
    Option<Arc<dyn Fn(&zen_core::config::McpServerConfig) -> bool + Send + Sync>>;

struct McpProxyTool {
    transport: Arc<dyn McpTransport>,
    original_name: String,
    schema: ToolSchema,
}

#[async_trait]
impl Tool for McpProxyTool {
    fn schema(&self) -> ToolSchema {
        self.schema.clone()
    }

    async fn invoke(&self, args: Value) -> Result<Value, KernelError> {
        self.transport.call_tool(&self.original_name, args).await
    }
}

/// Spec naming convention (spec.md §naming): MCP tools are namespaced by
/// their source server **without** an `mcp.` prefix, e.g. `brave.web_search`.
fn namespaced_tool_name(server: &str, tool: &str) -> String {
    format!("{}.{}", server, tool)
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
    /// Last-known tool set from `tools/list`, used by [`Self::refresh_tools`]
    /// to diff added/removed tools after a `notifications/tools/list_changed`.
    ///
    /// `#[allow(dead_code)]`: only read by `refresh_tools`, which is exercised
    /// by tests but not yet wired to a notification reader (FR-015 hook point).
    #[allow(dead_code)]
    last_tools: Mutex<Vec<ToolSchema>>,
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
            last_tools: Mutex::new(Vec::new()),
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

    /// Re-issue `tools/list` and diff the result against the last-known tool
    /// set, logging added/removed tools at info level (FR-015).
    ///
    /// The registered `McpProxyTool` set lives in the shared `ToolRegistry`,
    /// which rig-compose exposes no `unregister` for — so this method cannot
    /// mutate the live tool set. It is the honest partial: it detects and
    /// reports the change; a full re-registration requires a reconnect +
    /// re-bootstrap of the server.
    ///
    /// `#[allow(dead_code)]`: exercised by tests but not yet wired to a
    /// notification reader (FR-015 hook point).
    #[allow(dead_code)]
    async fn refresh_tools(&self) -> Result<Vec<ToolSchema>, KernelError> {
        let transport = self.current_transport().await?;
        let schemas = transport.list_tools().await?;

        let mut known = self.last_tools.lock().await;
        let prev: HashSet<String> = known.iter().map(|s| s.name.clone()).collect();
        let next: HashSet<String> = schemas.iter().map(|s| s.name.clone()).collect();
        for added in next.difference(&prev) {
            info!(
                server = %self.server_name,
                tool = %added,
                "MCP tool added after list_changed — refresh requires reconnect"
            );
        }
        for removed in prev.difference(&next) {
            info!(
                server = %self.server_name,
                tool = %removed,
                "MCP tool removed after list_changed — refresh requires reconnect"
            );
        }
        *known = schemas.clone();
        Ok(schemas)
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
            // Notifications are ignored — FR-015 future work.
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
pub async fn bootstrap_mcp_clients(
    registry: &ToolRegistry,
    mcp_servers: &[zen_core::config::McpServerConfig],
    trust_store: &mut zen_core::config::McpTrustStore,
    paths: &zen_core::paths::ZenPaths,
    prompt: TrustPrompt,
) {
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
                let args: Vec<String> = server.args.clone().unwrap_or_default();
                let endpoint = format!("stdio://{}", server.name);

                match connect_stdio(registry, &server.name, &endpoint, &command, &args).await {
                    Ok(count) => {
                        info!(server = %server.name, tools = count, "MCP client connected");
                    }
                    Err(e) => {
                        warn!(server = %server.name, error = %e, "MCP connection failed");
                    }
                }
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
                match connect_http(registry, &server.name, &url, server.headers.as_ref()).await {
                    Ok(count) => {
                        info!(
                            server = %server.name,
                            url = %url,
                            tools = count,
                            "MCP HTTP client connected"
                        );
                    }
                    Err(e) => {
                        warn!(
                            server = %server.name,
                            url = %url,
                            error = %e,
                            "MCP HTTP connection failed"
                        );
                    }
                }
            }
            other => {
                warn!(
                    server = %server.name,
                    transport = %other, "Unknown MCP transport"
                );
            }
        }
    }
}

async fn connect_stdio(
    registry: &ToolRegistry,
    server_name: &str,
    endpoint: &str,
    program: &str,
    args: &[String],
) -> Result<usize, String> {
    let initial: Arc<dyn McpTransport> = Arc::new(
        spawn_stdio(endpoint, program, args)
            .await
            .map_err(|e| format!("spawn failed: {}", e))?,
    );
    info!(server = server_name, "MCP subprocess spawned");

    let schemas = initial
        .list_tools()
        .await
        .map_err(|e| format!("list_tools failed: {}", e))?;

    let endpoint_factory = endpoint.to_string();
    let program_factory = program.to_string();
    let args_factory = args.to_vec();
    let spawn: SpawnFactory = Arc::new(move || {
        let endpoint = endpoint_factory.clone();
        let program = program_factory.clone();
        let args = args_factory.clone();
        Box::pin(async move {
            spawn_stdio(&endpoint, &program, &args)
                .await
                .map(|t| Arc::new(t) as Arc<dyn McpTransport>)
                .map_err(|e| e.to_string())
        })
    });

    let reconnecting = Arc::new(ReconnectingMcpTransport::new(
        server_name,
        endpoint,
        spawn,
        initial,
    ));

    // TODO(FR-015): list_changed auto-refresh. rig-mcp 0.2.5's
    // `McpTransport` has no subscription/notification API (no
    // `notifications/tools/list_changed`, no `subscribe`). When a future
    // rig-mcp adds it: expose a watch channel on the transport, spawn a
    // background task that re-runs `list_tools()`, diffs old vs new
    // schemas, and registers/unregisters `McpProxyTool` entries to keep
    // the tool set live without a restart.
    //
    // D5 note — stdout deadlock avoidance: `ReconnectingMcpTransport`
    // deliberately does NOT read child stdout directly; all pipe I/O is
    // delegated to `StdioTransport` (rig-mcp), which owns the child
    // handle and its stdout drain. Any future implementation that
    // subscribes to server notifications (FR-015) MUST spawn a dedicated
    // tokio reader task that continuously drains the child stdout pipe
    // into a channel — reading inline on the call path would block on
    // pipe-buffer fill (64 KiB on most OSes) and deadlock the transport.
    let count = schemas.len();
    for schema in schemas {
        let original_name = schema.name.clone();
        let namespaced_schema = ToolSchema {
            name: namespaced_tool_name(server_name, &original_name),
            description: schema.description,
            args_schema: schema.args_schema,
            result_schema: schema.result_schema,
        };

        let proxy = Arc::new(McpProxyTool {
            transport: reconnecting.clone(),
            original_name,
            schema: namespaced_schema,
        });

        registry.register(proxy);
    }

    Ok(count)
}

/// Connect to an HTTP/HTTPS MCP server, wrap in [`ReconnectingMcpTransport`]
/// (so FR-013 crash recovery applies), and register all discovered tools.
/// Mirrors [`connect_stdio`] structurally.
async fn connect_http(
    registry: &ToolRegistry,
    server_name: &str,
    url: &str,
    extra_headers: Option<&HashMap<String, String>>,
) -> Result<usize, String> {
    let initial: Arc<dyn McpTransport> = Arc::new(
        spawn_http(url, extra_headers)
            .await
            .map_err(|e| format!("spawn failed: {}", e))?,
    );
    info!(server = server_name, url = %url, "MCP HTTP transport connected");

    let schemas = initial
        .list_tools()
        .await
        .map_err(|e| format!("list_tools failed: {}", e))?;

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

    let reconnecting = Arc::new(ReconnectingMcpTransport::new(
        server_name,
        url,
        spawn,
        initial,
    ));

    let count = schemas.len();
    for schema in schemas {
        let original_name = schema.name.clone();
        let namespaced_schema = ToolSchema {
            name: namespaced_tool_name(server_name, &original_name),
            description: schema.description,
            args_schema: schema.args_schema,
            result_schema: schema.result_schema,
        };

        let proxy = Arc::new(McpProxyTool {
            transport: reconnecting.clone(),
            original_name,
            schema: namespaced_schema,
        });

        registry.register(proxy);
    }

    Ok(count)
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
            let endpoint = format!("stdio://{}", server.name);
            let transport: Arc<dyn McpTransport> = Arc::new(
                spawn_stdio(&endpoint, &command, &args)
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

async fn spawn_stdio(
    endpoint: &str,
    program: &str,
    args: &[String],
) -> Result<StdioTransport, KernelError> {
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    // TODO(FR-037): rig-mcp StdioTransport does not expose env control; the
    // child inherits the parent env (secrets included). Blocked upstream —
    // rig-mcp 0.2.5 `StdioTransport::spawn` builds its own
    // `tokio::process::Command` with no env parameter. Revisit when rig-mcp
    // adds env support.
    StdioTransport::spawn(endpoint, program, &arg_refs).await
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
            last_tools: Mutex::new(Vec::new()),
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
            last_tools: Mutex::new(Vec::new()),
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
            last_tools: Mutex::new(Vec::new()),
        };

        let result = transport.call_tool("ping", Value::Null).await;
        assert!(result.is_ok());
        assert_eq!(attempts.load(Ordering::Relaxed), 0, "no spawn should occur");
    }

    /// Mock transport whose `tools/list` result is driven by a shared list of
    /// tool names, so a test can mutate the server's tool set between calls.
    struct ListMock {
        tools: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl McpTransport for ListMock {
        fn endpoint(&self) -> &str {
            "mock://list"
        }

        async fn list_tools(&self) -> Result<Vec<ToolSchema>, KernelError> {
            Ok(self
                .tools
                .lock()
                .await
                .iter()
                .map(|n| ToolSchema {
                    name: n.clone(),
                    description: String::new(),
                    args_schema: Value::Null,
                    result_schema: Value::Null,
                })
                .collect())
        }

        async fn call_tool(&self, _name: &str, _args: Value) -> Result<Value, KernelError> {
            Ok(Value::Null)
        }
    }

    #[tokio::test]
    async fn refresh_tools_diffs_and_updates_known_set() {
        let tools = Arc::new(Mutex::new(vec!["a".to_string(), "b".to_string()]));
        let transport = ReconnectingMcpTransport {
            server_name: "srv-list".into(),
            endpoint: "stdio://srv-list".into(),
            spawn: Arc::new(|| {
                Box::pin(async { Err::<Arc<dyn McpTransport>, String>("unused".into()) })
            }),
            current: Mutex::new(Some(Arc::new(ListMock {
                tools: tools.clone(),
            }) as Arc<dyn McpTransport>)),
            backoffs: instant_backoffs(),
            last_tools: Mutex::new(Vec::new()),
        };

        let first = transport.refresh_tools().await.unwrap();
        assert_eq!(first.len(), 2);

        *tools.lock().await = vec!["b".to_string(), "c".to_string()];
        let second = transport.refresh_tools().await.unwrap();
        let names: Vec<String> = second.iter().map(|s| s.name.clone()).collect();
        assert_eq!(names, vec!["b", "c"]);

        let known: Vec<String> = transport
            .last_tools
            .lock()
            .await
            .iter()
            .map(|s| s.name.clone())
            .collect();
        assert_eq!(
            known,
            vec!["b", "c"],
            "last-known set must track the latest list"
        );
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
