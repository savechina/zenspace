use std::collections::HashMap as HashMapStd;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::net::UnixStream;
use tokio::sync::{RwLock, mpsc, watch};
use zen_memory::{ReplayCheckpointStore, SessionReplayer, ZenMemvidStore};
use zen_repo::MemvidReplayOffsetRepo;
use zen_vault::search::SearchService;

use crate::channel::Channel as _;
use crate::server::approval::ApprovalBroker;
use crate::server::connection::{ClientConnection, OUTBOUND_CAPACITY, OutboundFrame};
use crate::server::hosting::{
    SessionHost, TurnRegistry, cancel as hosting_cancel, resume as hosting_resume,
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

/// PID record persisted to `daemon.pid`: the process id plus an
/// OS-native token for its start instant.
///
/// The start token defeats pid reuse: `kill(pid, 0)` alone reports
/// "alive" for any unrelated process that recycled a dead daemon's pid,
/// which made `serve start` refuse to boot until the recycle window
/// passed (codex-rs stores the same pair as its PID backend).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PidRecord {
    pub pid: u32,
    /// Native start-instant repr (`ps -o lstart=` output); `None` for
    /// legacy bare-pid files or when unverifiable.
    pub start: Option<String>,
}

/// Best-effort OS-native identity of `pid`'s start instant.
///
/// Reads the process start time (seconds since epoch) via `sysinfo` —
/// no subprocess, cross-platform. The value is stable across the pid's
/// lifetime and changes when the OS recycles the pid. Returns `None`
/// when the process is gone; callers must treat `None` as "cannot
/// verify", never as proof of liveness either way.
pub fn process_start_token(pid: u32) -> Option<String> {
    with_process(pid, |p| p.start_time().to_string())
}

/// Targeted sysinfo access: refresh only this pid (cheap), then map.
fn with_process<R>(pid: u32, f: impl FnOnce(&sysinfo::Process) -> R) -> Option<R> {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let target = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::Some(&[target]), true);
    system.process(target).map(f)
}

/// True only when `pid` exists AND its current start instant matches
/// `recorded`. A live-but-recycled pid fails the match → stale.
pub fn pid_record_alive(recorded_pid: u32, recorded_start: Option<&str>) -> bool {
    if !is_pid_alive(recorded_pid) {
        return false;
    }
    match (recorded_start, process_start_token(recorded_pid)) {
        (Some(a), Some(b)) => a == b,
        // Legacy file or unverifiable platform: fall back to signal-0.
        _ => true,
    }
}

/// Liveness probe via `sysinfo` process-table lookup (jento/app_sys_ctl
/// parity): a present entry means the OS knows this pid; no signal is
/// sent, so `EPERM` ownership edge cases do not arise.
pub fn is_pid_alive(pid: u32) -> bool {
    with_process(pid, |_| ()).is_some()
}

/// Write `{pid, start}` for an arbitrary daemon pid (typically the
/// freshly spawned child, not the spawner).
///
/// Writes via temp-file + rename so a crash mid-write can never leave a
/// truncated record (codex-rs parity).
pub fn write_pid_for<P: AsRef<Path>>(path: P, pid: u32) -> std::io::Result<()> {
    let record = serde_json::json!({
        "pid": pid,
        "start": process_start_token(pid),
    });
    let body = record.to_string();
    let target = path.as_ref();
    let tmp = target.with_extension("pid.tmp");
    std::fs::write(&tmp, &body)?;
    std::fs::rename(&tmp, target)
        .inspect(|_| tracing::debug!(record = %body, path = %target.display(), "PID file written"))
}

/// Write this process's own record to `daemon.pid`.
pub fn write_pid<P: AsRef<Path>>(path: P) -> std::io::Result<()> {
    write_pid_for(path, process::id())
}

/// Read and parse a pid file (JSON record preferred, legacy bare pid).
pub fn read_pid_record<P: AsRef<Path>>(path: P) -> std::io::Result<PidRecord> {
    let content = std::fs::read_to_string(&path)?;
    parse_pid_record(&content).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("PID file {:?}: {}", path.as_ref(), e),
        )
    })
}

/// Legacy shim: bare pid from either record format.
pub fn read_pid<P: AsRef<Path>>(path: P) -> std::io::Result<u32> {
    read_pid_record(path).map(|r| r.pid)
}

fn parse_pid_record(content: &str) -> std::io::Result<PidRecord> {
    let trimmed = content.trim();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        let pid = v
            .get("pid")
            .and_then(serde_json::Value::as_u64)
            .filter(|p| *p <= u64::from(u32::MAX))
            .ok_or_else(|| invalid("missing pid"))? as u32;
        let start = v
            .get("start")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        return Ok(PidRecord { pid, start });
    }
    trimmed
        .parse::<u32>()
        .map(|pid| PidRecord { pid, start: None })
        .map_err(|e| invalid(&e.to_string()))
}

fn invalid(msg: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg.to_string())
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
    /// Self-exit after this much continuous zero-client time. `None`
    /// (default) runs forever — codex parity: only implicit spawns opt
    /// in (`ZEN_GATEWAY_IDLE_EXIT_SECS`, set by `GatewayClient::
    /// default_spawn`) so chat-booted daemons cannot accumulate.
    pub idle_exit: Option<std::time::Duration>,
    /// Graceful-drain budget after a stop signal (design §6): in-flight
    /// turns may finish inside this window before stragglers are
    /// cancelled with audits.
    pub drain_window: std::time::Duration,
    /// Ownership model advertised on `health/status`.
    pub mode: GatewayMode,
    /// Loopback HTTP carrier options; `None` keeps the carrier off
    /// (FR-019 default). Auto-enabled when `qqbot` is set — channels
    /// bridge through the HTTP surface.
    pub http: Option<crate::transport::http::HttpCarrierConfig>,
    /// QQBot channel options (FR-021 stage b); `Some` implies the
    /// loopback HTTP carrier. The daemon resolves `chat_base` from
    /// the effective HTTP carrier before spawning.
    pub qqbot: Option<crate::channel::qqbot::QqBotAdapterOptions>,
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
            idle_exit: None,
            http: None,
            qqbot: None,
            turn_executor: None,
        }
    }
}

/// RAII guard clearing the replay-tick busy flag on early return/panic.
struct ReplayTickGuard(Arc<std::sync::atomic::AtomicBool>);

impl Drop for ReplayTickGuard {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
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
    hosting: std::sync::Arc<SessionHost>,
    reads: std::sync::Arc<ReadDeps>,
    connections: RwLock<HashMapStd<String, std::sync::Arc<ClientConnection>>>,
    /// Session-replay checkpoint store (Phase 11 T052): `None` when
    /// state.db is unavailable — replay then runs per-tick without
    /// resume support (idempotency keys still prevent duplicates).
    replay_db: Option<std::sync::Arc<zen_repo::SqliteClient>>,
    /// Aggregate replay counters surfaced on `health/status`.
    replay_counters: Arc<ReplayCounters>,
    /// Replay-tick busy flag: a previous tick still running skips this
    /// one instead of piling up blocking mv2 writes.
    replay_running: Arc<std::sync::atomic::AtomicBool>,
    shutdown_tx: watch::Sender<bool>,
    _claim: SoleOwnerClaim,
}

/// Aggregate session-replay counters (Phase 11 T052) exposed via
/// `health/status.replay` as `{replayed, skipped, lastOffset}`.
#[derive(Default)]
pub struct ReplayCounters {
    replayed: std::sync::atomic::AtomicU64,
    skipped: std::sync::atomic::AtomicU64,
    last_offset: std::sync::atomic::AtomicU64,
}

impl ReplayCounters {
    fn snapshot(&self) -> serde_json::Value {
        use std::sync::atomic::Ordering::Relaxed;
        serde_json::json!({
            "replayed": self.replayed.load(Relaxed),
            "skipped": self.skipped.load(Relaxed),
            "lastOffset": self.last_offset.load(Relaxed),
        })
    }
}

/// [`ReplayCheckpointStore`] backed by the daemon's state.db
/// `memvid_replay_offsets` table (T049) — adapts the async repo API to
/// the replayer's checkpoint trait. A fresh adapter is built per replay
/// tick from the shared client (the repo holds `&SqliteClient`).
struct SqliteReplayCheckpoints {
    client: std::sync::Arc<zen_repo::SqliteClient>,
}

#[async_trait::async_trait]
impl ReplayCheckpointStore for SqliteReplayCheckpoints {
    async fn load_offset(&self, session_path: &str) -> anyhow::Result<Option<i64>> {
        Ok(MemvidReplayOffsetRepo::new(&self.client)
            .load(session_path)
            .await?)
    }

    async fn save_offset(&self, session_path: &str, applied_offset: i64) -> anyhow::Result<()> {
        Ok(MemvidReplayOffsetRepo::new(&self.client)
            .update(session_path, applied_offset)
            .await?)
    }
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
        let hosting = std::sync::Arc::new(SessionHost {
            executor,
            sessions: std::sync::Arc::default(),
            turns: std::sync::Arc::new(TurnRegistry::default()),
            guards: std::sync::Arc::new(crate::server::Guards::default()),
            approval: broker,
            audit_path: audit_path.clone(),
        });
        let reads = std::sync::Arc::new(ReadDeps {
            registry: Some(std::sync::Arc::new(zen_agents::DefaultAgentRegistry::new())),
        });

        // Replay checkpoint db (Phase 11 T052): dedicated pool over the
        // same state.db the knowledge stack uses; degraded to None when
        // unavailable (replay then runs without resume, still idempotent).
        let replay_db = match zen_repo::SqliteClient::open(
            config
                .db_path
                .clone()
                .unwrap_or_else(|| {
                    paths
                        .as_ref()
                        .map(|p| p.data().join("state.db"))
                        .expect("db path resolvable when ZenPaths available")
                })
                .as_path(),
        )
        .await
        {
            Ok(c) => Some(Arc::new(c)),
            Err(e) => {
                tracing::warn!(error = %e, "replay checkpoint db unavailable; replay runs without resume");
                None
            }
        };

        let (shutdown_tx, _) = watch::channel(false);
        Ok(Self {
            started_at: std::time::Instant::now(),
            mode: GatewayMode::Standalone,
            store,
            knowledge,
            hosting,
            reads,
            connections: RwLock::new(HashMapStd::new()),
            replay_db,
            replay_counters: Arc::new(ReplayCounters::default()),
            replay_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
            .handle("memory/rebuild", {
                let service = std::sync::Arc::clone(self);
                move |_| {
                    let service = std::sync::Arc::clone(&service);
                    async move { service.memory_rebuild().await }
                }
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
            "replay": self.replay_counters.snapshot(),
        });
        if self.mode == GatewayMode::Embedded {
            snapshot["mode"] = serde_json::json!("embedded");
        }
        Ok(snapshot)
    }

    /// Phase 11 T052: incremental jsonl→mv2 replay over the sessions
    /// root, invoked from the GC tick (30s cadence).
    ///
    /// Skips when a previous tick is still running (busy flag), when the
    /// checkpoint db or memvid store is unavailable, or when ZenPaths
    /// cannot resolve the sessions root. Aggregates per-file stats into
    /// [`Self::replay_counters`] and enforces the 5% skipped-ratio
    /// silent-failure guard (ERROR log). Live `persist_turn` writes stay
    /// immediate — same-process store state serializes against replay.
    async fn run_replay_tick(self: &std::sync::Arc<Self>) {
        use std::sync::atomic::Ordering;

        if self.replay_running.swap(true, Ordering::SeqCst) {
            return;
        }
        let _busy = ReplayTickGuard(self.replay_running.clone());

        let Some(db) = self.replay_db.as_ref() else {
            return;
        };
        let store_handle = self.store.read().await.clone();
        let Some(store) = store_handle else {
            return;
        };
        let Some(sessions_root) = zen_core::paths::ZenPaths::detect()
            .ok()
            .map(|p| p.sessions())
        else {
            return;
        };

        let replayer = SessionReplayer::new(
            store,
            Arc::new(SqliteReplayCheckpoints {
                client: Arc::clone(db),
            }),
        );
        let results = replayer.replay_dir(&sessions_root).await;

        let mut tick_replayed = 0u64;
        let mut tick_skipped = 0u64;
        let mut max_offset = 0u64;
        for (_path, stats) in results {
            tick_replayed += stats.replayed;
            tick_skipped += stats.skipped;
            max_offset = max_offset.max(stats.last_offset);
        }
        let counters = &*self.replay_counters;
        counters
            .replayed
            .fetch_add(tick_replayed, Ordering::Relaxed);
        counters.skipped.fetch_add(tick_skipped, Ordering::Relaxed);
        counters.last_offset.store(max_offset, Ordering::Relaxed);

        // Silent-failure guard (task-specified): corrupt-line ratio above
        // 5% of this tick's processed turns means the archives are being
        // mis-read somewhere — surface loudly.
        let processed = tick_replayed + tick_skipped;
        if processed > 0 && (tick_skipped as f64 / processed as f64) > 0.05 {
            tracing::error!(
                replayed = tick_replayed,
                skipped = tick_skipped,
                "session replay skipped-ratio exceeds 5% this tick"
            );
        } else if tick_replayed > 0 || tick_skipped > 0 {
            tracing::debug!(
                replayed = tick_replayed,
                skipped = tick_skipped,
                "session replay tick done"
            );
        }
    }

    /// Phase 11 T053 (`memory/rebuild`): full mv2 rebuild — reindex md
    /// sources via [`MemvidIndexer`], reset every replay checkpoint to
    /// byte 0, then run a full jsonl replay immediately (turn dedup keys
    /// were wiped with the rebuild, so every archived turn re-persists).
    ///
    /// # Errors
    /// -32002 store-unavailable when the memvid store is not open;
    /// -32603 when checkpoint reset fails.
    async fn memory_rebuild(
        self: std::sync::Arc<Self>,
    ) -> Result<serde_json::Value, crate::protocol::RpcError> {
        use zen_memory::MemvidIndexer;

        if self.store_health().await == "unavailable" {
            return Err(crate::protocol::RpcError::store_unavailable("unavailable"));
        }
        let workspace_root = zen_core::paths::ZenPaths::detect()
            .ok()
            .and_then(|p| p.workspace_root().cloned())
            .ok_or_else(|| {
                crate::protocol::RpcError::internal("workspace root unavailable for rebuild")
            })?;

        tracing::info!("memory/rebuild: full memvid reindex starting");
        let report = {
            let mut guard = self.store.write().await;
            let Some(store) = guard.as_mut() else {
                return Err(crate::protocol::RpcError::store_unavailable("unavailable"));
            };
            MemvidIndexer::new(workspace_root)
                .index_all(store)
                .map_err(|e| {
                    crate::protocol::RpcError::internal(&format!("memvid reindex failed: {e}"))
                })?
        };
        tracing::info!(
            filesScanned = report.files_scanned,
            chunksIndexed = report.chunks_indexed,
            errors = report.errors.len(),
            "memory/rebuild: reindex done"
        );

        if let Some(db) = self.replay_db.as_ref() {
            MemvidReplayOffsetRepo::new(db)
                .reset_all()
                .await
                .map_err(|e| {
                    crate::protocol::RpcError::internal(&format!("checkpoint reset failed: {e}"))
                })?;
        } else {
            tracing::warn!(
                "memory/rebuild: no checkpoint db; replay runs from stored offsets only"
            );
        }

        self.run_replay_tick().await;
        Ok(serde_json::json!({
            "filesScanned": report.files_scanned,
            "chunksIndexed": report.chunks_indexed,
            "errors": report.errors,
            "replay": self.replay_counters.snapshot(),
        }))
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
                    gc_service.run_replay_tick().await;
                }
            })
        };

        // Idle self-exit (implicit-spawn hygiene): after `idle_exit` of
        // continuous zero-client time, take the same graceful-drain path
        // as `shutdown`/SIGTERM. Explicit `zen serve start` leaves this
        // None — a user-managed daemon never disappears on its own.
        let idle_task = config.idle_exit.map(|idle| {
            let idle_service = std::sync::Arc::clone(&service);
            tokio::spawn(async move {
                let mut zero_since: Option<tokio::time::Instant> = None;
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                tick.tick().await;
                loop {
                    tick.tick().await;
                    if idle_service.client_count().await > 0 {
                        zero_since = None;
                        continue;
                    }
                    let since = *zero_since.get_or_insert_with(tokio::time::Instant::now);
                    if since.elapsed() >= idle {
                        tracing::info!(
                            idle_secs = idle.as_secs(),
                            "gateway idle with no clients; shutting down"
                        );
                        idle_service.request_shutdown();
                        return;
                    }
                }
            })
        });

        // Loopback HTTP carrier (FR-019/020, T041): same dispatcher, REST
        // + WS + MCP mounts. Off unless configured (validate_loopback
        // refuses non-loopback binds inside the task and logs). A
        // configured qqbot channel implies the carrier — channels bridge
        // through `/api/v1` like any external client.
        let effective_http = config.http.clone().or_else(|| {
            config
                .qqbot
                .as_ref()
                .map(|_| crate::transport::http::HttpCarrierConfig {
                    bind_addr: "127.0.0.1".to_string(),
                    port: 9876,
                })
        });
        let http_task = effective_http.clone().map(|http_cfg| {
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

        // QQBot channel (FR-021 stage b, Phase 13): dials QQ's WS gateway
        // and bridges events onto the loopback HTTP surface above.
        let qqbot_task = config.qqbot.clone().map(|mut opts| {
            if let Some(http_cfg) = effective_http.as_ref() {
                opts.chat_base = format!("http://{}:{}", http_cfg.bind_addr, http_cfg.port);
            }
            // One shared audit sink across hosting + channel carriers
            // so IM events correlate with turn records in one file
            // (resolved by the service constructor, same as hosting).
            opts.audit_path = service.hosting.audit_path.clone();
            let qq_shutdown = service.shutdown_tx.subscribe();
            tokio::spawn(async move {
                match crate::channel::qqbot::QqBotAdapter::new(opts) {
                    Ok(ch) => {
                        if let Err(e) = ch.run(qq_shutdown).await {
                            tracing::warn!(error = %e, "qqbot channel stopped");
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "qqbot channel setup failed"),
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
        // Stop background hygiene tasks immediately; carriers get the
        // graceful path instead: signal the service watch they all
        // subscribe to, then JOIN them with a grace budget so an
        // in-flight reply finishes its POST rather than being silently
        // dropped mid-send by an abort().
        gc_task.abort();
        if let Some(task) = idle_task {
            task.abort();
        }
        service.request_shutdown();
        let carrier_deadline =
            tokio::time::Instant::now() + config.drain_window + std::time::Duration::from_secs(5);
        if let Some(mut handle) = http_task
            && tokio::time::timeout_at(carrier_deadline, &mut handle)
                .await
                .is_err()
        {
            tracing::warn!("http carrier did not stop within grace; aborting");
            handle.abort();
        }
        if let Some(mut handle) = qqbot_task
            && tokio::time::timeout_at(carrier_deadline, &mut handle)
                .await
                .is_err()
        {
            tracing::warn!("qqbot channel did not drain within grace; aborting");
            handle.abort();
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
