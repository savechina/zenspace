use std::collections::HashMap as HashMapStd;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::net::UnixStream;
use tokio::sync::{RwLock, mpsc, watch};
use zen_memory::ZenMemvidStore;
use zen_vault::search::SearchService;

use crate::server::approval::ApprovalBroker;
use crate::server::connection::{ClientConnection, OUTBOUND_CAPACITY, OutboundFrame};
use crate::server::hosting::{
    HostingDeps, TurnRegistry, cancel as hosting_cancel, resume as hosting_resume,
    start as hosting_start, turn_with as hosting_turn_with,
};

/// HTTP configuration for the gateway daemon.
#[derive(Clone)]
pub struct HttpConfig {
    pub bind_addr: String,
    pub port: u16,
    pub jwt_secret: Option<String>,
    pub rate_limit_rpm: u32,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1".to_string(),
            port: 9876,
            jwt_secret: None,
            rate_limit_rpm: 100,
        }
    }
}

/// Write the current process PID to daemon.pid.
pub fn write_pid<P: AsRef<Path>>(path: P) -> std::io::Result<()> {
    let pid = process::id().to_string();
    std::fs::write(&path, &pid).inspect(
        |_| tracing::debug!(pid = pid, path = %path.as_ref().display(), "PID file written"),
    )
}

/// Read and parse the PID from an existing pid file.
pub fn read_pid<P: AsRef<Path>>(path: P) -> std::io::Result<u32> {
    let content = std::fs::read_to_string(&path)?;
    content.trim().parse::<u32>().map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("PID file {:?}: {}", path.as_ref(), e),
        )
    })
}

pub fn remove_pid<P: AsRef<Path>>(path: P) -> std::io::Result<()> {
    std::fs::remove_file(&path)
        .inspect(|_| tracing::debug!(path = %path.as_ref().display(), "PID file removed"))
}

// ─────────────────── UDS gateway service (US1, T012) ───────────────────

use crate::server::knowledge::{KnowledgeState, search as knowledge_search};
use crate::server::memory::{self as memory_handlers, SharedStore};
use crate::server::readouts::{ReadDeps, agent_list, agent_status, skill_list};
use crate::server::{DispatchServer, QueueFrontTransport};
use crate::transport::uds::{self, UdsTransport};

/// Process-wide sole-owner claim: only one [`GatewayService`] may hold
/// the memory store per process; cross-process exclusivity comes from the
/// memvid file lock itself.
static GATEWAY_OPEN: AtomicBool = AtomicBool::new(false);

/// Drop guard releasing the process-wide claim.
struct SoleOwnerClaim;

impl SoleOwnerClaim {
    /// Claims sole ownership or fails if already claimed.
    fn acquire() -> Option<Self> {
        GATEWAY_OPEN
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
            .then_some(Self)
    }
}

impl Drop for SoleOwnerClaim {
    fn drop(&mut self) {
        GATEWAY_OPEN.store(false, Ordering::SeqCst);
    }
}

/// How this server instance came to life. Embedded servers are owned
/// by the surface process that spawned them and refuse sibling
/// attachment (sole-owner store semantics); standalone daemons share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GatewayMode {
    /// Explicit `zen serve start` daemon (default).
    #[default]
    Standalone,
    /// In-process gateway embedded in a surface (`connect_or_embed`).
    Embedded,
}

/// Configuration for the UDS gateway daemon. Test overrides allow
/// fully isolated temp-dir operation.
#[derive(Clone)]
pub struct GatewayDaemonConfig {
    /// Unix socket path clients dial (default `<global data>/gateway.sock`).
    pub socket_path: PathBuf,
    /// Memory archive override (default `<global>/memory/<MEMVID_STORE_FILE>`).
    pub memory_path: Option<PathBuf>,
    /// Knowledge DB override (default `<global data>/state.db`).
    pub db_path: Option<PathBuf>,
    /// Audit JSONL sink override (default `<global logs>/audit.jsonl`).
    pub audit_path: Option<PathBuf>,
    /// Agent-loop sandbox mode override (`ZEN_SANDBOX_MODE` env fallback).
    /// `ask` enables interactive Q3 approval routing for hosted turns.
    pub sandbox_mode: Option<zen_core::sandbox::SandboxMode>,
    /// Graceful-drain budget after a stop signal (design §6): in-flight
    /// turns may finish inside this window before stragglers are
    /// cancelled with audits.
    pub drain_window: std::time::Duration,
    /// Ownership model advertised on `health/status`.
    pub mode: GatewayMode,
    /// Loopback HTTP carrier options; `None` keeps the carrier off
    /// (FR-019 default).
    pub http: Option<crate::transport::http::HttpCarrierConfig>,
    /// Hosted-turn executor override (P4 in-process isomorphic testing,
    /// design P4): `None` builds the real orchestrator stack.
    pub turn_executor: Option<std::sync::Arc<dyn crate::server::hosting::TurnExecutor>>,
}

impl Default for GatewayDaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: uds::default_socket_path(),
            mode: GatewayMode::Standalone,
            memory_path: None,
            db_path: None,
            audit_path: None,
            sandbox_mode: None,
            drain_window: std::time::Duration::from_secs(10),
            http: None,
            turn_executor: None,
        }
    }
}

/// E1 GatewayService — the sole-owner daemon singleton. Owns the memvid
/// store read-write (the only opener in any process), the knowledge
/// search stack, hosted-turn infrastructure, and the live connection
/// registry.
pub struct GatewayService {
    started_at: std::time::Instant,
    mode: GatewayMode,
    store: SharedStore,
    knowledge: Option<std::sync::Arc<KnowledgeState>>,
    hosting: std::sync::Arc<HostingDeps>,
    reads: std::sync::Arc<ReadDeps>,
    connections: RwLock<HashMapStd<String, std::sync::Arc<ClientConnection>>>,
    shutdown_tx: watch::Sender<bool>,
    _claim: SoleOwnerClaim,
}

impl GatewayService {
    /// Opens the store read-write and assembles the knowledge stack.
    ///
    /// # Errors
    /// - Another service instance holds the process claim.
    /// - The memvid store is locked/unopenable (another process owns it —
    ///   sole-owner semantics refuse to start rather than contend).
    pub async fn open(config: &GatewayDaemonConfig) -> anyhow::Result<Self> {
        let claim = SoleOwnerClaim::acquire()
            .ok_or_else(|| anyhow::anyhow!("gateway service already owns this process"))?;

        let paths = zen_core::paths::ZenPaths::detect().ok();
        let memory_path = config
            .memory_path
            .clone()
            .or_else(|| {
                paths
                    .as_ref()
                    .map(|p| p.memory().join(zen_core::constants::MEMVID_STORE_FILE))
            })
            .ok_or_else(|| anyhow::anyhow!("no memory path configured and ZenPaths unavailable"))?;
        if let Some(parent) = memory_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("create memory dir {}: {e}", parent.display()))?;
        }

        let store: SharedStore = match ZenMemvidStore::new(memory_path.clone()) {
            Ok(s) => Arc::new(Some(s).into()),
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "memory store {} unavailable ({e}) — another gateway may own it",
                    memory_path.display()
                ));
            }
        };
        let knowledge = Self::build_knowledge(config, paths.as_ref()).await;

        // Hosted-session stack: orchestrator (Ask-capable) + approval
        // broker + guards + audit sink.
        let broker = Arc::new(ApprovalBroker::default());
        let executor: Option<std::sync::Arc<dyn crate::server::hosting::TurnExecutor>> =
            match config.turn_executor.clone() {
                Some(injected) => Some(injected),
                None => Self::build_orchestrator(
                    &memory_path,
                    config.sandbox_mode,
                    Some(Arc::clone(&broker)),
                )
                .map(|e| e as std::sync::Arc<dyn crate::server::hosting::TurnExecutor>),
            };
        let audit_path = config
            .audit_path
            .clone()
            .or_else(|| paths.as_ref().map(|p| p.logs().join("audit.jsonl")));
        let hosting = std::sync::Arc::new(HostingDeps {
            executor,
            sessions: std::sync::Arc::default(),
            turns: std::sync::Arc::new(TurnRegistry::default()),
            guards: std::sync::Arc::new(crate::server::Guards::default()),
            approval: broker,
            audit_path,
        });
        let reads = std::sync::Arc::new(ReadDeps {
            registry: Some(std::sync::Arc::new(zen_agents::DefaultAgentRegistry::new())),
        });

        let (shutdown_tx, _) = watch::channel(false);
        Ok(Self {
            started_at: std::time::Instant::now(),
            mode: GatewayMode::Standalone,
            store,
            knowledge,
            hosting,
            reads,
            connections: RwLock::new(HashMapStd::new()),
            shutdown_tx,
            _claim: claim,
        })
    }

    /// Builds the hosted-turn executor exactly as `zen chat` does
    /// (`load_config` → `DefaultRouter::from_agentic` → `with_memory`).
    /// The memory path is the store we already opened: the process-wide
    /// memvid singleton hands back our own read-write handle, so no
    /// second file opener exists. Any failure degrades to `None`
    /// (session/turn fails fast; memory/knowledge keep serving).
    ///
    /// `SandboxMode::Ask` additionally installs the approval broker as
    /// the interactive callback, routing gated tool invocations to the
    /// originating surface (FR-010). Other modes keep the silent
    /// seatbelt behavior of zen-cli.
    fn build_orchestrator(
        memory_path: &std::path::Path,
        sandbox_mode: Option<zen_core::sandbox::SandboxMode>,
        broker: Option<std::sync::Arc<ApprovalBroker>>,
    ) -> Option<std::sync::Arc<zen_agents::AgentOrchestrator>> {
        let config = match zen_core::config::load_config() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "config load failed; hosted sessions degraded");
                return None;
            }
        };
        let mode = sandbox_mode.or_else(|| {
            std::env::var("ZEN_SANDBOX_MODE")
                .ok()
                .and_then(|v| zen_core::sandbox::SandboxMode::parse_str(&v))
        });
        let router = zen_provider::DefaultRouter::from_agentic(config);
        let built = match mode {
            Some(m) if m == zen_core::sandbox::SandboxMode::Ask => {
                let mut orch =
                    zen_agents::AgentOrchestrator::new(router.clone()).with_sandbox_mode(m);
                if let Some(broker) = broker.as_ref() {
                    orch = orch.with_approval_callback(broker.callback());
                }
                orch
            }
            _ => zen_agents::AgentOrchestrator::new(router.clone()),
        };
        match built.with_memory(memory_path.to_path_buf()) {
            Ok(o) => Some(Arc::new(o)),
            Err(e) => {
                tracing::warn!(error = %e, "orchestrator memory attach failed; plain mode");
                Some(Arc::new(zen_agents::AgentOrchestrator::new(router)))
            }
        }
    }

    /// Assembles SearchService + SqliteClient; any failure degrades to
    /// `None` (daemon still serves memory ops).
    async fn build_knowledge(
        config: &GatewayDaemonConfig,
        paths: Option<&zen_core::paths::ZenPaths>,
    ) -> Option<std::sync::Arc<KnowledgeState>> {
        let Some(paths) = paths else {
            tracing::warn!("ZenPaths unavailable; knowledge/search degraded");
            return None;
        };
        let db_path = config
            .db_path
            .clone()
            .unwrap_or_else(|| paths.data().join("state.db"));
        let client = match zen_repo::SqliteClient::open(&db_path).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "state.db unavailable; knowledge/search degraded");
                return None;
            }
        };
        let service = match zen_core::config::load_config() {
            Ok(cfg) => Some(SearchService::new(
                zen_provider::DefaultRouter::from_agentic(cfg),
            )),
            Err(e) => {
                tracing::warn!(error = %e, "config load failed; tier5 synthesis degraded");
                None
            }
        };
        Some(Arc::new(KnowledgeState::new(
            service,
            Some(client),
            vec![paths.inbox(), paths.wiki()],
        )))
    }

    /// Live connection count (health/status `clients` field).
    pub async fn client_count(&self) -> usize {
        self.connections.read().await.len()
    }

    /// Registers an HTTP-carrier dispatch session's connection so
    /// health/status client counts include carrier peers.
    pub(crate) async fn register_connection(&self, conn: Arc<ClientConnection>) {
        self.connections.write().await.insert(conn.id.clone(), conn);
    }

    /// Removes a closed HTTP-carrier connection by id.
    pub(crate) async fn unregister_connection(&self, conn_id: &str) {
        self.connections.write().await.remove(conn_id);
    }

    /// Registered agent count (legacy `/health` `agents` field).
    pub(crate) async fn agents_count(&self) -> usize {
        match self.reads.registry.as_ref() {
            Some(r) => r.list_all().len(),
            None => 0,
        }
    }

    /// Store health classification for health/status: `unavailable`
    /// (no store handle), `degraded` (store ok, knowledge stack down),
    /// `ok`.
    pub async fn store_health(&self) -> &'static str {
        let has_store = self.store.read().await.is_some();
        if !has_store {
            "unavailable"
        } else if self.knowledge.is_some() {
            "ok"
        } else {
            "degraded"
        }
    }

    /// Requests a graceful accept-loop stop; returns current client count
    /// as the `drained` figure.
    pub fn request_shutdown(&self) -> usize {
        self.shutdown_tx.send_replace(true);
        0 // exact drained count reported by serve(); US1 keeps this simple
    }

    /// Builds the dispatcher with real business handlers for one
    /// connection.
    pub(crate) fn build_dispatcher(
        self: &std::sync::Arc<Self>,
        conn: std::sync::Arc<ClientConnection>,
        transport: QueueFrontTransport,
    ) -> anyhow::Result<DispatchServer> {
        let conn_for_hook = Arc::clone(&conn);
        let server = DispatchServer::new(transport).on_client_ready(move |identity, caps| {
            let conn = Arc::clone(&conn_for_hook);
            tokio::spawn(async move {
                if conn.mark_ready(identity, caps).await.is_err() {
                    tracing::warn!("client_ready on non-Connecting connection");
                }
            });
        });
        // Q3 bridge for this connection's hosted turns.
        let origin_handle = server.connection();
        let server = server
            .handle("health/status", {
                let service = std::sync::Arc::clone(self);
                move |_| {
                    let service = std::sync::Arc::clone(&service);
                    async move { service.health_snapshot().await }
                }
            })?
            .handle("shutdown", {
                let service = std::sync::Arc::clone(self);
                move |_| {
                    let service = std::sync::Arc::clone(&service);
                    async move {
                        let drained = service.request_shutdown();
                        Ok(serde_json::json!({"drained": drained, "cancelled": 0}))
                    }
                }
            })?
            .handle("memory/retrieve", {
                let store = Arc::clone(&self.store);
                move |p| memory_handlers::retrieve(Arc::clone(&store), p)
            })?
            .handle("memory/putEntry", {
                let store = Arc::clone(&self.store);
                move |p| memory_handlers::put_entry(Arc::clone(&store), p)
            })?
            .handle("memory/search", {
                let store = Arc::clone(&self.store);
                move |p| memory_handlers::search(Arc::clone(&store), p)
            })?
            .handle("memory/stats", {
                let store = Arc::clone(&self.store);
                move |p| memory_handlers::stats(Arc::clone(&store), p)
            })?
            .handle("knowledge/search", {
                let knowledge = self.knowledge.clone();
                let service = std::sync::Arc::clone(self);
                move |p| {
                    let knowledge = knowledge.clone();
                    let service = std::sync::Arc::clone(&service);
                    async move {
                        knowledge_search(
                            knowledge_required(knowledge, service.store_health().await)?,
                            p,
                        )
                        .await
                    }
                }
            })?
            .handle("session/start", {
                let hosting = std::sync::Arc::clone(&self.hosting);
                move |p| hosting_start(std::sync::Arc::clone(&hosting), p)
            })?
            .handle("session/turn", {
                let hosting = std::sync::Arc::clone(&self.hosting);
                move |p| {
                    let hosting = std::sync::Arc::clone(&hosting);
                    let origin = conn.queue();
                    let handle = origin_handle.clone();
                    async move { hosting_turn_with(hosting, Some(origin), Some(handle), p).await }
                }
            })?
            .handle("session/cancel", {
                let hosting = std::sync::Arc::clone(&self.hosting);
                move |p| hosting_cancel(std::sync::Arc::clone(&hosting), p)
            })?
            .handle("session/resume", {
                let hosting = std::sync::Arc::clone(&self.hosting);
                move |p| hosting_resume(std::sync::Arc::clone(&hosting), p)
            })?
            .handle("agent/list", {
                let reads = std::sync::Arc::clone(&self.reads);
                move |p| {
                    let reads = std::sync::Arc::clone(&reads);
                    async move { agent_list(reads, p) }
                }
            })?
            .handle("agent/status", {
                let reads = std::sync::Arc::clone(&self.reads);
                move |p| {
                    let reads = std::sync::Arc::clone(&reads);
                    async move { agent_status(reads, p) }
                }
            })?
            .handle("skill/list", {
                move |p| async move {
                    let paths = zen_core::paths::ZenPaths::detect().ok();
                    skill_list(paths.as_ref(), p)
                }
            })?;
        Ok(server)
    }

    async fn health_snapshot(
        self: std::sync::Arc<Self>,
    ) -> Result<serde_json::Value, crate::protocol::RpcError> {
        let mut snapshot = serde_json::json!({
            "serverVersion": env!("CARGO_PKG_VERSION"),
            "protocolVersion": crate::protocol::SERVER_PROTOCOL_VERSION,
            "clients": self.client_count().await,
            "uptimeMs": self.started_at.elapsed().as_millis() as u64,
            "storeHealth": self.store_health().await,
            "activeTurns": self.hosting.turns.active_count(),
        });
        if self.mode == GatewayMode::Embedded {
            snapshot["mode"] = serde_json::json!("embedded");
        }
        Ok(snapshot)
    }

    /// Accepts one socket: registers the E2 record, pumps the outbound
    /// queue onto the wire, runs the dispatcher until close.
    fn spawn_connection(self_: &std::sync::Arc<Self>, stream: UnixStream) {
        let service = std::sync::Arc::clone(self_);
        tokio::spawn(async move {
            let wire: std::sync::Arc<dyn crate::transport::Transport> =
                match UdsTransport::from_stream(stream) {
                    Ok(w) => std::sync::Arc::new(w),
                    Err(e) => {
                        tracing::warn!(error = %e, "connection setup failed");
                        return;
                    }
                };
            let (tx, mut rx) = mpsc::channel::<OutboundFrame>(OUTBOUND_CAPACITY);
            let conn = Arc::new(ClientConnection::new(tx));
            service
                .connections
                .write()
                .await
                .insert(conn.id.clone(), Arc::clone(&conn));
            tracing::debug!(connection_id = %conn.id, "connection registered");

            let pump_wire = std::sync::Arc::clone(&wire);
            let pump = tokio::spawn(async move {
                while let Some(of) = rx.recv().await {
                    if pump_wire.send(of.frame).await.is_err() {
                        break;
                    }
                }
            });

            let front = QueueFrontTransport::new(conn.queue(), std::sync::Arc::clone(&wire));
            match service.build_dispatcher(Arc::clone(&conn), front) {
                Ok(dispatcher) => {
                    if let Err(e) = dispatcher.run().await {
                        tracing::debug!(error = %e, "dispatcher ended with error");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "dispatcher build failed"),
            }

            pump.abort();
            service.connections.write().await.remove(&conn.id);
            conn.close().await;
            tracing::debug!(connection_id = %conn.id, "connection closed");
        });
    }

    /// Runs the accept loop until [`GatewayService::request_shutdown`]
    /// (the `shutdown` RPC) is signalled, then drains per design §6.
    ///
    /// # Errors
    /// Service open failure or socket bind failure (live gateway present).
    pub async fn serve(config: GatewayDaemonConfig) -> anyhow::Result<()> {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        Self::serve_with_shutdown(config, shutdown_tx, shutdown_rx).await
    }

    /// [`GatewayService::serve`] with an external shutdown channel so a
    /// signal handler (SIGTERM/SIGINT in `zen serve`) can trigger the
    /// same graceful-drain path as the `shutdown` RPC.
    pub async fn serve_with_shutdown(
        config: GatewayDaemonConfig,
        _external_tx: watch::Sender<bool>,
        mut external_rx: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        // Bind FIRST: the atomic socket bind is the single-instance
        // arbiter (embedded mode races two surfaces here — the loser
        // never touches the store lock; its readiness poll lands on the
        // winner).
        let listener = uds::bind_socket(&config.socket_path).await?;
        let service = match GatewayService::open(&config).await {
            Ok(mut s) => {
                s.mode = config.mode;
                std::sync::Arc::new(s)
            }
            Err(e) => {
                uds::cleanup_socket(&config.socket_path);
                tracing::error!(error = %e, "gateway service open FAILED");
                return Err(e);
            }
        };
        let mut shutdown_rx = service.shutdown_tx.subscribe();
        tracing::info!(
            socket = %config.socket_path.display(),
            protocolVersion = crate::protocol::SERVER_PROTOCOL_VERSION,
            serverVersion = env!("CARGO_PKG_VERSION"),
            "gateway listening"
        );

        // StaleClientGC (E7): heartbeat pings at GC_HEARTBEAT cadence so
        // half-dead peers surface as send errors and get reaped by their
        // dispatcher-exit path; also bounds the turn idempotency map.
        let gc_task = {
            let gc_service = std::sync::Arc::clone(&service);
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(crate::server::guards::GC_HEARTBEAT);
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                tick.tick().await; // consume the immediate first tick
                loop {
                    tick.tick().await;
                    let conns = gc_service.connections.read().await;
                    for conn in conns.values() {
                        if conn.state().await == crate::server::connection::ConnectionState::Closed
                        {
                            continue;
                        }
                        let _ = conn
                            .enqueue(OutboundFrame::delta(crate::protocol::Frame::notification(
                                "health/ping",
                                serde_json::json!({}),
                            )))
                            .await;
                    }
                    drop(conns);
                    gc_service.hosting.turns.reap_terminal(64);
                }
            })
        };

        // Loopback HTTP carrier (FR-019/020, T041): same dispatcher, REST
        // + WS + MCP mounts. Off unless configured (validate_loopback
        // refuses non-loopback binds inside the task and logs).
        let http_task = config.http.clone().map(|http_cfg| {
            let http_service = std::sync::Arc::clone(&service);
            let http_shutdown = service.shutdown_tx.subscribe();
            tokio::spawn(async move {
                if let Err(e) =
                    crate::transport::http::serve_http(http_service, http_cfg, http_shutdown).await
                {
                    tracing::warn!(error = %e, "loopback http carrier stopped");
                }
            })
        });

        loop {
            tokio::select! {
                _ = external_rx.changed() => break,
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow_and_update() {
                        break;
                    }
                }
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, _)) => GatewayService::spawn_connection(&service, stream),
                        Err(e) => {
                            tracing::warn!(error = %e, "accept failed");
                            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        }
                    }
                }
            }
        }

        // Stop the GC heartbeat and close the listener BEFORE draining:
        // the task holds an Arc to the service (and thereby the store's
        // exclusive flock), and a still-bound-but-unaccepting listener
        // lets redialing surfaces connect into the backlog and stall on
        // the handshake until their 30s timeout instead of failing fast.
        gc_task.abort();
        if let Some(http_task) = http_task {
            http_task.abort();
        }
        drop(listener);

        // Graceful drain (design §6): let in-flight turns finish inside
        // the window, then cancel stragglers — their finalize path writes
        // the `outcome:"cancelled"` audits — and give the audit tasks a
        // short grace before the store drops.
        let drained = service.client_count().await;
        let deadline = tokio::time::Instant::now() + config.drain_window;
        while service.hosting.turns.active_count() > 0 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let cancelled = service.hosting.turns.active_count();
        if cancelled > 0 {
            tracing::info!(cancelled, "drain window elapsed; cancelling stragglers");
            service.hosting.turns.cancel_all_active().await;
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        uds::cleanup_socket(&config.socket_path);
        tracing::info!(drained, cancelled, "gateway drained; store closing cleanly");
        Ok(())
    }
}

/// Unwraps an owned `KnowledgeState` or fails the handler with -32002
/// carrying the live storeHealth classification (T037 transitions).
fn knowledge_required(
    state: Option<std::sync::Arc<KnowledgeState>>,
    store_health: &'static str,
) -> Result<std::sync::Arc<KnowledgeState>, crate::protocol::RpcError> {
    state.ok_or_else(|| crate::protocol::RpcError::store_unavailable(store_health))
}
