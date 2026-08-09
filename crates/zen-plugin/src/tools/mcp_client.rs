use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
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
const BACKOFF_SECS: [u64; MAX_RECONNECT_ATTEMPTS as usize] = [1, 2, 4];

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
type TrustPrompt = Option<Arc<dyn Fn(&zen_core::config::McpServerConfig) -> bool + Send + Sync>>;

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
                warn!(
                    server = %server.name,
                    hint = format!("zen plugin mcp trust {}", server.name),
                    "MCP server not trusted, skipping"
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
                // TODO(FR-014): rig-mcp 0.2.5 ships no HTTP/SSE client transport
                // (only stdio + in-process loopback). When a future rig-mcp adds
                // one, wire it here and reuse `ReconnectingMcpTransport` with an
                // HTTP-backed spawn factory so FR-013 crash recovery applies.
                warn!(
                    server = %server.name,
                    transport = "http",
                    "HTTP MCP transport not yet supported (FR-014 pending rig-mcp HTTP client)"
                );
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

async fn spawn_stdio(
    endpoint: &str,
    program: &str,
    args: &[String],
) -> Result<StdioTransport, KernelError> {
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
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
}
