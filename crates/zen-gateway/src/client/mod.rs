//! L3 client runtime (tasks T016-T019) — probe, connect-or-spawn, and
//! the [`GatewayClient`] request/notification engine over the UDS
//! carrier.
//!
//! PURPOSE: Lets every surface (TUI, chat, future plugins) reach the
//! sole-owner daemon with zero setup: dial the socket; if nothing lives
//! there, clean a stale file and launch `zen serve start --daemonized`
//! once, then wait for readiness. Authority is ALWAYS the socket probe —
//! never PID trust (PID reuse guard, spec edge case). The client tracks
//! per-request correlation ids, demultiplexes server frames, and offers
//! an optional heartbeat that flags disconnection after two misses.
//!
//! USAGE: `GatewayClient::connect_or_spawn_with_probe(&path, None,
//! "tui", version, caps).await?` — the surface dial path (T019 +
//! T055): connect-or-spawn, handshake, and a serverVersion probe that
//! restarts a protocol-incompatible daemon exactly once. Lower-level
//! callers use `GatewayClient::connect_or_spawn(&path, None).await?`
//! then `.handshake("tui", caps).await?` and
//! `.request(method, params).await`.
//! Server-initiated Q3 requests arrive on the notifications channel;
//! answer them with [`GatewayClient::respond`].
//!
//! EXPECTED: concurrent clients race safely — exactly one daemon wins the
//! atomic bind, losers' spawners fail quietly while their wait-for-ready
//! probes connect them to the winner.
//!
//! ERRORS: dial failures surface from spawn/ready timeouts as anyhow
//! errors; request failures return the server's [`RpcErrorBody`] or a
//! transport-closed error; after disconnect all pending requests fail.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, mpsc, oneshot};
use tracing::{debug, warn};

use crate::protocol::{
    Capabilities, Frame, RpcErrorBody, SERVER_PROTOCOL_VERSION, initialize_params,
};
use crate::transport::Transport;
use crate::transport::uds::UdsTransport;

/// Per-surface facade with link-state tracking and turn recovery
/// (tasks T022–T025).
pub mod surface;

pub use surface::{
    ApprovalBridgeEnds, ApprovalRequestPayload, ApprovalResponsePayload, ApprovalSink,
    GatewayLinkState, SurfaceClient, SurfaceError, TURN_TIMEOUT,
};

/// Readiness window after spawning a daemon (codex app-server-daemon
/// uses the same 10s budget: migrations and store open can legitimately
/// take seconds on a cold start).
pub const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// Delay between readiness probes.
const READY_POLL: Duration = Duration::from_millis(25);
/// Per-attempt cap on the initialize round-trip inside [`wait_for_ready`] (codex parity).
const READY_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// Cap on the best-effort `shutdown` RPC inside the version-probe
/// restart cycle (T055). The handler acknowledges immediately; the
/// daemon exits asynchronously afterwards.
const SHUTDOWN_RPC_TIMEOUT: Duration = Duration::from_secs(5);
/// Budget for the daemon to release its socket after an ACKNOWLEDGED
/// `shutdown` RPC (T055). The drain path drops the listener before
/// flushing, so release is normally sub-second; the bound only guards
/// a stuck carrier.
const SHUTDOWN_RELEASE_TIMEOUT: Duration = Duration::from_secs(10);
/// Delay between socket-release probes (T055).
const RELEASE_POLL: Duration = Duration::from_millis(50);

/// Where `default_spawn` redirects daemon stderr; on readiness timeout
/// its tail becomes the user-facing diagnosis (codex stderr-log pattern).
fn spawn_log_path() -> Option<PathBuf> {
    zen_core::paths::ZenPaths::detect()
        .ok()
        .map(|p| p.logs().join("gateway-spawn.log"))
}

/// Last `max` bytes of the spawn log, lossy-decoded for error context.
fn spawn_log_tail(max: usize) -> String {
    let Some(path) = spawn_log_path() else {
        return String::new();
    };
    let Ok(meta) = std::fs::metadata(&path) else {
        return String::new();
    };
    let start = meta.len().saturating_sub(max as u64);
    let Ok(mut f) = std::fs::File::open(&path) else {
        return String::new();
    };
    use std::io::{Read, Seek, SeekFrom};
    if f.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&buf);
    format!("\n--- gateway-spawn.log (tail) ---\n{}", text.trim_end())
}

/// Heartbeat misses before the client flags itself disconnected.
pub const HEARTBEAT_MAX_MISSES: u32 = 2;

/// Spawns the platform daemon. Injectable so test binaries (which cannot
/// exec themselves as `zen`) substitute an in-process serve loop; the
/// atomic bind still guarantees a single winner across racing callers.
pub type DaemonSpawnFn = Arc<dyn Fn() -> anyhow::Result<()> + Send + Sync>;

/// Removes `path` when it exists but no listener answers (stale file left
/// by a dead daemon). A live socket is never touched.
fn clean_stale_socket(path: &Path) {
    if !path.exists() {
        return;
    }
    let live = std::os::unix::net::UnixStream::connect(path).is_ok();
    if !live
        && let Err(e) = std::fs::remove_file(path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        warn!(path = %path.display(), error = %e, "failed to remove stale gateway socket");
    }
}

/// Extracts `serverInfo.version` from an `initialize` result
/// (contracts/01 shape). `None` when absent, empty, or non-string —
/// foreign/experimental daemons may omit it (T059b: degrade, never
/// refuse).
fn server_version_of(handshake_result: &serde_json::Value) -> Option<String> {
    handshake_result
        .pointer("/serverInfo/version")
        .and_then(serde_json::Value::as_str)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// Parses the `MAJOR.MINOR` prefix of a crate semver
/// (`MAJOR.MINOR.PATCH`, pre-release/build suffixes tolerated). The
/// patch component is deliberately ignored — patch diffs are cosmetic
/// per contracts/00 §version. Returns `None` for foreign or
/// unparseable strings (T059b).
fn parse_version_prefix(version: &str) -> Option<(u64, u64)> {
    let core = version.split(['-', '+']).next().unwrap_or_default();
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// Outcome of comparing the handshake's advertised server version
/// against this build (T055). Mirrors the contract version semantics:
/// the restart trigger is "the handshake would fail per equal-major &&
/// client-minor ≤ server-minor" — never a cosmetic patch diff.
enum ServerVersionCompat {
    /// Same major and server not older at the minor level (patch diffs
    /// ignored) — additive changes only, safe to use.
    Compatible,
    /// Major divergence, or a server older than this build beyond
    /// patch level — the same refusal shape the protocol negotiation
    /// applies. Carries the offending advertised version.
    Incompatible(String),
    /// Missing/empty/unparseable advertisement (foreign or experimental
    /// daemon); never a hard refusal (T059b).
    Unknown,
}

fn classify_server_version(server_version: Option<&str>) -> ServerVersionCompat {
    let Some(advertised) = server_version.filter(|v| !v.is_empty()) else {
        return ServerVersionCompat::Unknown;
    };
    match (
        parse_version_prefix(env!("CARGO_PKG_VERSION")),
        parse_version_prefix(advertised),
    ) {
        (Some(client), Some(server)) => {
            if client.0 != server.0 || client.1 > server.1 {
                ServerVersionCompat::Incompatible(advertised.to_string())
            } else {
                ServerVersionCompat::Compatible
            }
        }
        _ => ServerVersionCompat::Unknown,
    }
}

/// Polls until the socket stops admitting connections — the daemon
/// exited and released its bind (file removed or listener dropped) —
/// or `budget` elapses. Socket-probe authority, never PID trust
/// (T055; `wait_exit`-style bounded poll).
async fn wait_for_socket_release(path: &Path, budget: Duration) -> bool {
    let end = tokio::time::Instant::now() + budget;
    loop {
        if UdsTransport::connect(path).await.is_err() {
            return true;
        }
        if tokio::time::Instant::now() >= end {
            return false;
        }
        tokio::time::sleep(RELEASE_POLL).await;
    }
}

/// Default production spawner: relaunch this binary detached as
/// `zen serve start` with null stdio, stderr captured to
/// `<logs>/gateway-spawn.log`, and a new session.
fn default_spawn() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.args(["serve", "start"])
        // Implicitly-spawned daemons self-exit after 30 idle minutes
        // (codex THREAD_UNLOADING_DELAY parity); explicit `zen serve
        // start` never sets this and runs until `zen serve stop`.
        .env("ZEN_GATEWAY_IDLE_EXIT_SECS", "1800")
        // T16: implicit spawns run a pure gateway (sessions/hosting/guards)
        // without the 16-worker scheduler; explicit `zen serve start` or
        // launchd-managed daemons keep the scheduler.
        .env("ZEN_SERVE_NO_SCHEDULER", "1")
        .stdout(Stdio::null())
        .stdin(Stdio::null());
    match spawn_log_path() {
        Some(path) => {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|e| anyhow::anyhow!("open spawn log {}: {e}", path.display()))?;
            cmd.stderr(Stdio::from(file));
        }
        None => {
            cmd.stderr(Stdio::null());
        }
    }
    #[cfg(unix)]
    {
        // New SESSION (codex app-server-daemon pattern): the daemon must
        // survive terminal hangup entirely — process_group(0) only leaves
        // the foreground group, setsid() leaves the controlling terminal.
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("spawn zen serve: {e}"))
}

/// Dial-until-ready: socket liveness alone proves nothing (the listener
/// can bind before the dispatcher answers), so readiness is confirmed
/// with a real `initialize` round-trip on a THROWAWAY connection — the
/// returned transport stays untouched so callers run their own
/// initialize lifecycle on it (codex app-server-daemon pattern).
async fn wait_for_ready(path: &Path, deadline: Duration) -> anyhow::Result<Arc<UdsTransport>> {
    let end = tokio::time::Instant::now() + deadline;
    loop {
        if probe_ready(path).await.is_ok() {
            // Probe passed; hand the caller a fresh, session-virgin transport.
            return match UdsTransport::connect(path).await {
                Ok(t) => Ok(Arc::new(t)),
                Err(e) if tokio::time::Instant::now() < end => {
                    debug!(error = %e, "post-probe connect failed; retrying");
                    tokio::time::sleep(READY_POLL).await;
                    continue;
                }
                Err(e) => anyhow::bail!(
                    "gateway not ready at {} within {deadline:?}: {e}{}",
                    path.display(),
                    spawn_log_tail(2048)
                ),
            };
        }
        if tokio::time::Instant::now() >= end {
            anyhow::bail!(
                "gateway not ready at {} within {deadline:?}: initialize round-trip kept failing{}",
                path.display(),
                spawn_log_tail(2048)
            );
        }
        tokio::time::sleep(READY_POLL).await;
    }
}

/// One readiness attempt: connect, `initialize` round-trip under
/// [`READY_PROBE_TIMEOUT`], drop everything. Connection-level failures
/// and timeouts are retriable by the caller; they carry the error for
/// the final timeout message.
async fn probe_ready(path: &Path) -> anyhow::Result<()> {
    let transport = UdsTransport::connect(path).await?;
    let probe = GatewayClient::from_transport(Arc::new(transport), path);
    let attempt = tokio::time::timeout(
        READY_PROBE_TIMEOUT,
        probe.handshake("zen-readiness", "0.0", Default::default()),
    )
    .await;
    match attempt {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(_elapsed) => Err(anyhow::anyhow!(
            "initialize round-trip exceeded {READY_PROBE_TIMEOUT:?}"
        )),
    }
}

struct Shared {
    next_id: Mutex<u64>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<serde_json::Value, RpcErrorBody>>>>,
    notifications_tx: mpsc::Sender<Frame>,
    connected: std::sync::atomic::AtomicBool,
    socket_path: PathBuf,
}

impl Shared {
    /// Fails every in-flight request; called on transport death or close.
    async fn fail_all(&self, reason: &str) {
        let mut pending = self.pending.lock().await;
        for (_, tx) in pending.drain() {
            let _ = tx.send(Err(RpcErrorBody {
                code: -32603,
                name: "internal".into(),
                message: format!("connection closed: {reason}"),
                data: None,
            }));
        }
    }
}

/// Aborts the reader task when the LAST client handle drops. Clones
/// share one guard `Arc`, so intermediate drops (handoffs, facade
/// caching) never kill the receive direction; an explicit
/// [`GatewayClient::close`] aborts immediately.
struct ReaderAbort {
    handle: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl ReaderAbort {
    fn abort(&self) {
        if let Some(handle) = self.handle.lock().expect("reader slot poisoned").take() {
            handle.abort();
        }
    }
}

impl Drop for ReaderAbort {
    fn drop(&mut self) {
        self.abort();
    }
}

/// Client-side handle to the sole-owner daemon. Cheap clones share
/// correlation state; every clone can send concurrently (the reader task
/// owns the receive direction) and the reader lives until the last
/// clone is dropped or [`GatewayClient::close`] is called.
#[derive(Clone)]
pub struct GatewayClient {
    transport: Arc<UdsTransport>,
    shared: Arc<Shared>,
    reader_abort: Arc<ReaderAbort>,
    notifications_rx: Arc<Mutex<mpsc::Receiver<Frame>>>,
}

/// Owns the embedded gateway this surface process started (codex
/// `AppServerTarget::Embedded` pattern). Dropping it flips the shutdown
/// watch so the in-process server drains — audits flushed, socket
/// removed — instead of leaking tasks behind a runtime that is about to
/// exit. `None` link ownership means we ATTACHED to a server someone
/// else owns and must leave it running.
pub struct EmbeddedGatewayGuard {
    shutdown: Arc<tokio::sync::watch::Sender<bool>>,
}

impl Drop for EmbeddedGatewayGuard {
    fn drop(&mut self) {
        let _ = self.shutdown.send_replace(true);
    }
}

impl GatewayClient {
    /// Dials an already-running daemon.
    ///
    /// # Errors
    /// Socket missing/refused.
    pub async fn connect(socket_path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = socket_path.as_ref();
        let transport = Arc::new(UdsTransport::connect(path).await?);
        Ok(Self::from_transport(transport, path))
    }

    /// Codex-style link policy: dial → attach to a live server, else
    /// EMBED the gateway in this process (same PID — no daemon spawn).
    /// The embedded server binds the public socket first (atomic
    /// single-instance arbiter), so a racing sibling surface loses the
    /// bind, never touches the store lock, and attaches to us instead.
    /// The returned [`EmbeddedGatewayGuard`] must be held by the surface for
    /// as long as it may act as the server; dropping it drains.
    ///
    /// # Errors
    /// Dial failure plus readiness timeout (with spawn-log tail).
    pub async fn connect_or_embed(
        socket_path: impl AsRef<Path>,
    ) -> anyhow::Result<(Self, Option<EmbeddedGatewayGuard>)> {
        let path = socket_path.as_ref();
        if let Ok(client) = Self::connect(path).await {
            return Ok((client, None));
        }
        clean_stale_socket(path);

        let config = crate::daemon::GatewayDaemonConfig {
            socket_path: path.to_path_buf(),
            mode: crate::daemon::GatewayMode::Embedded,
            ..crate::daemon::GatewayDaemonConfig::default()
        };
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let shutdown = Arc::new(shutdown_tx);
        {
            let shutdown = Arc::clone(&shutdown);
            tokio::spawn(async move {
                let _ = crate::daemon::GatewayService::serve_with_shutdown(
                    config,
                    (*shutdown).clone(),
                    shutdown_rx,
                )
                .await;
            });
        }
        let transport = wait_for_ready(path, READY_TIMEOUT).await?;
        Ok((
            Self::from_transport(transport, path),
            Some(EmbeddedGatewayGuard { shutdown }),
        ))
    }

    /// T016/T017/T018 entry point: probe → stale-clean → spawn-once →
    /// wait-for-ready → connect. The atomic bind guarantees exactly one
    /// daemon regardless of how many surfaces race here; racing spawners
    /// that lose the bind exit quietly while their callers' readiness
    /// probes land on the winner.
    ///
    /// # Errors
    /// Spawn invocation failure or readiness timeout.
    pub async fn connect_or_spawn(
        socket_path: impl AsRef<Path>,
        spawn_fn: Option<DaemonSpawnFn>,
    ) -> anyhow::Result<Self> {
        let path = socket_path.as_ref();
        if let Ok(client) = Self::connect(path).await {
            return Ok(client);
        }
        clean_stale_socket(path);
        match spawn_fn {
            Some(spawn) => spawn()?,
            None => default_spawn()?,
        }
        let transport = wait_for_ready(path, READY_TIMEOUT).await?;
        Ok(Self::from_transport(transport, path))
    }

    /// T055 dial path: [`GatewayClient::connect_or_spawn`] plus the
    /// initialize handshake and a version probe on the advertised
    /// `serverInfo.version`. When the server is protocol-incompatible
    /// with this build — per contract version semantics (equal major,
    /// client minor ≤ server minor; patch diffs are cosmetic) — or the
    /// handshake was refused with -32001, the client performs exactly
    /// ONE restart cycle: best-effort `shutdown` RPC, bounded wait for
    /// socket release, then one `connect_or_spawn` retry. A second
    /// mismatch never loops: the error surfaces with the FR-004
    /// recovery hint, and a version that is STILL unknown after the
    /// retry is accepted (T059b — foreign/experimental daemons stay
    /// usable). Concurrency-safe across surfaces: the atomic UDS bind
    /// arbitrates the respawn, so racing siblings never double-spawn.
    ///
    /// # Errors
    /// Dial/spawn/readiness failures as [`GatewayClient::connect_or_spawn`];
    /// handshake failures propagate; a persistent version mismatch
    /// fails with an FR-004 recovery message instead of silently
    /// connecting to an incompatible daemon.
    ///
    /// ```no_run
    /// # async fn demo() -> anyhow::Result<()> {
    /// use zen_gateway::client::GatewayClient;
    /// use zen_gateway::protocol::Capabilities;
    /// let client = GatewayClient::connect_or_spawn_with_probe(
    ///     "/tmp/zen-gateway.sock",
    ///     None,
    ///     "zen-chat",
    ///     "0.0.8",
    ///     Capabilities::default(),
    /// )
    /// .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect_or_spawn_with_probe(
        socket_path: impl AsRef<Path>,
        spawn_fn: Option<DaemonSpawnFn>,
        client_name: &str,
        client_version: &str,
        capabilities: Capabilities,
    ) -> anyhow::Result<Self> {
        let path = socket_path.as_ref();
        let mut restarted = false;
        loop {
            let client = GatewayClient::connect_or_spawn(path, spawn_fn.clone()).await?;
            match client
                .handshake(client_name, client_version, capabilities)
                .await
            {
                Ok(result) => {
                    let server_version = server_version_of(&result);
                    match classify_server_version(server_version.as_deref()) {
                        ServerVersionCompat::Compatible => return Ok(client),
                        ServerVersionCompat::Unknown if restarted => {
                            // T059b: never loop-restart against a daemon
                            // that does not advertise a version.
                            warn!(
                                client = env!("CARGO_PKG_VERSION"),
                                "serverVersion still unknown after one restart; accepting foreign/experimental gateway"
                            );
                            return Ok(client);
                        }
                        ServerVersionCompat::Unknown => {
                            warn!(
                                client = env!("CARGO_PKG_VERSION"),
                                "serverVersion unknown/empty in handshake; attempting one daemon restart"
                            );
                        }
                        ServerVersionCompat::Incompatible(version) => {
                            if restarted {
                                client.close().await;
                                anyhow::bail!(
                                    "gateway version mismatch persists after one restart \
                                     (server {version} vs client {}): restart the gateway \
                                     with a matching version (zen serve stop, then retry)",
                                    env!("CARGO_PKG_VERSION")
                                );
                            }
                            warn!(
                                server = %version,
                                client = env!("CARGO_PKG_VERSION"),
                                "protocol-incompatible gateway detected; restarting it once"
                            );
                        }
                    }
                    Self::restart_daemon(&client, path).await;
                    restarted = true;
                }
                Err(e) => {
                    let version_refused = e
                        .downcast_ref::<RpcErrorBody>()
                        .is_some_and(|body| body.code == -32001);
                    if version_refused && !restarted {
                        warn!(
                            error = %e,
                            "handshake refused with -32001 version-mismatch; restarting gateway once"
                        );
                        Self::restart_daemon(&client, path).await;
                        restarted = true;
                        continue;
                    }
                    client.close().await;
                    if version_refused {
                        anyhow::bail!(
                            "gateway handshake refused with -32001 version-mismatch: \
                             restart the gateway with a matching version (zen serve stop, \
                             then retry) — {e:#}"
                        );
                    }
                    anyhow::bail!("gateway handshake failed: {e:#}");
                }
            }
        }
    }

    /// One restart cycle of the version probe (T055): best-effort
    /// `shutdown` RPC (local carriers only, contracts/00 §Handshake),
    /// then a bounded socket-release wait — only when the shutdown was
    /// ACKNOWLEDGED; a gate rejection (-32000 pre-handshake) or
    /// transport death leaves nothing to wait for, and the retry dial
    /// re-probes whatever still lives on the path.
    async fn restart_daemon(client: &GatewayClient, path: &Path) {
        match client
            .request_timeout("shutdown", serde_json::json!({}), SHUTDOWN_RPC_TIMEOUT)
            .await
        {
            Ok(_) => {
                if !wait_for_socket_release(path, SHUTDOWN_RELEASE_TIMEOUT).await {
                    warn!(
                        socket = %path.display(),
                        budget_secs = SHUTDOWN_RELEASE_TIMEOUT.as_secs(),
                        "gateway socket did not release after shutdown; retrying dial anyway"
                    );
                }
            }
            Err(e) => {
                debug!(
                    code = e.code,
                    "shutdown RPC during version restart not accepted: {}", e.message
                );
            }
        }
        client.close().await;
    }

    fn from_transport(transport: Arc<UdsTransport>, socket_path: &Path) -> Self {
        let (notifications_tx, notifications_rx) = mpsc::channel(256);
        let shared = Arc::new(Shared {
            next_id: 0.into(),
            pending: HashMap::new().into(),
            notifications_tx,
            connected: true.into(),
            socket_path: socket_path.to_path_buf(),
        });
        let reader_abort = Arc::new(ReaderAbort {
            handle: std::sync::Mutex::new(None),
        });
        {
            let transport = Arc::clone(&transport);
            let shared = Arc::clone(&shared);
            let handle = tokio::spawn(async move {
                loop {
                    match transport.recv().await {
                        Ok(frame) => Self::route(&shared, frame).await,
                        Err(e) => {
                            shared
                                .connected
                                .store(false, std::sync::atomic::Ordering::SeqCst);
                            shared.fail_all(&e.to_string()).await;
                            break;
                        }
                    }
                }
            });
            *reader_abort.handle.lock().expect("reader slot poisoned") = Some(handle);
        }
        Self {
            transport,
            shared,
            reader_abort,
            notifications_rx: Arc::new(Mutex::new(notifications_rx)),
        }
    }

    async fn route(shared: &Arc<Shared>, frame: Frame) {
        match frame {
            Frame::ServerResponse {
                id, result, error, ..
            } => {
                if let Some(tx) = shared.pending.lock().await.remove(&id) {
                    let _ = tx.send(match (result, error) {
                        (Some(v), None) => Ok(v),
                        (_, Some(e)) => Err(e),
                        (None, None) => Err(RpcErrorBody {
                            code: -32603,
                            name: "internal".into(),
                            message: "response carried no result".into(),
                            data: None,
                        }),
                    });
                }
            }
            // Q3 approvals and lifecycle notifications both land on the
            // demux channel; callers answer Q3 via `respond`.
            other => {
                let _ = shared.notifications_tx.send(other).await;
            }
        }
    }

    /// Performs the initialize→initialized handshake, returning the raw
    /// initialize result payload.
    ///
    /// # Errors
    /// Transport failure, version mismatch (-32001), duplicate handshake
    /// (-32000).
    pub async fn handshake(
        &self,
        client_name: &str,
        client_version: &str,
        capabilities: Capabilities,
    ) -> anyhow::Result<serde_json::Value> {
        let params = initialize_params(
            SERVER_PROTOCOL_VERSION,
            client_name,
            client_version,
            capabilities,
        );
        let result = self.request("initialize", params).await?;
        self.notify("initialized", serde_json::json!({})).await?;
        Ok(result)
    }

    /// Sends a request and awaits its correlated response.
    ///
    /// # Errors
    /// Server [`RpcErrorBody`], transport failure, or dropped reply.
    pub async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcErrorBody> {
        self.request_timeout(method, params, Duration::from_secs(30))
            .await
    }

    /// [`GatewayClient::request`] with an explicit deadline; expiry fails
    /// the call locally (the server may still complete side effects).
    pub async fn request_timeout(
        &self,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value, RpcErrorBody> {
        let id = {
            let mut next = self.shared.next_id.lock().await;
            *next += 1;
            *next
        };
        let (tx, rx) = oneshot::channel();
        self.shared.pending.lock().await.insert(id, tx);
        if let Err(e) = self
            .transport
            .send(Frame::request_with(id, method, params))
            .await
        {
            self.shared.pending.lock().await.remove(&id);
            return Err(RpcErrorBody {
                code: -32603,
                name: "internal".into(),
                message: format!("send failed: {e}"),
                data: None,
            });
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(RpcErrorBody {
                code: -32603,
                name: "internal".into(),
                message: "reply channel dropped".into(),
                data: None,
            }),
            Err(_) => {
                // Reclaim the slot; a late reply just finds no receiver.
                self.shared.pending.lock().await.remove(&id);
                Err(RpcErrorBody {
                    code: -32603,
                    name: "internal".into(),
                    message: format!("request timed out after {timeout:?}"),
                    data: None,
                })
            }
        }
    }

    /// Sends a notification (no reply expected).
    ///
    /// # Errors
    /// Transport failure.
    pub async fn notify(&self, method: &str, params: serde_json::Value) -> anyhow::Result<()> {
        self.transport
            .send(Frame::notification(method, params))
            .await
    }

    /// Answers a server-initiated Q3 request (approval flow).
    ///
    /// # Errors
    /// Transport failure.
    pub async fn respond(&self, request_id: &str, result: serde_json::Value) -> anyhow::Result<()> {
        self.transport
            .send(Frame::client_response(request_id.to_string(), result))
            .await
    }

    /// Receives the next server-initiated frame (Q3 approval request or
    /// notification). Single consumer — each surface owns one client.
    ///
    /// # Errors
    /// Channel closed (client dropped / reader dead).
    pub async fn next_notification(&mut self) -> anyhow::Result<Frame> {
        self.notifications_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("notification channel closed"))
    }

    /// Connection liveness as tracked by the reader task and heartbeats.
    pub fn is_connected(&self) -> bool {
        self.shared
            .connected
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Socket this client dials (diagnostics/reconnect support).
    pub fn socket_path(&self) -> &Path {
        &self.shared.socket_path
    }

    /// Optional heartbeat (T019): pings `health/status` every `interval`;
    /// after [`HEARTBEAT_MAX_MISSES`] consecutive failures the client is
    /// flagged disconnected and pending requests failed. Aborting the
    /// returned handle stops the heartbeat.
    pub fn start_heartbeat(&self, interval: Duration) -> tokio::task::JoinHandle<()> {
        let transport = Arc::clone(&self.transport);
        let shared = Arc::clone(&self.shared);
        tokio::spawn(async move {
            let mut misses = 0u32;
            loop {
                tokio::time::sleep(interval).await;
                let id = {
                    let mut next = shared.next_id.lock().await;
                    *next += 1;
                    *next
                };
                let (tx, rx) = oneshot::channel();
                shared.pending.lock().await.insert(id, tx);
                let sent = transport
                    .send(Frame::request_with(
                        id,
                        "health/status",
                        serde_json::json!({}),
                    ))
                    .await
                    .is_ok();
                let replied = sent && tokio::time::timeout(interval, rx).await.is_ok();
                if replied {
                    misses = 0;
                } else {
                    misses += 1;
                    if misses >= HEARTBEAT_MAX_MISSES {
                        shared
                            .connected
                            .store(false, std::sync::atomic::Ordering::SeqCst);
                        shared.fail_all("heartbeat lost").await;
                        break;
                    }
                }
            }
        })
    }

    /// Stops the reader and fails outstanding requests.
    pub async fn close(&self) {
        self.reader_abort.abort();
        self.shared
            .connected
            .store(false, std::sync::atomic::Ordering::SeqCst);
        self.shared.fail_all("client closed").await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::stub_server;
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn serve_one_connection(socket: &Path) {
        let listener = crate::transport::uds::bind_socket(socket).await.unwrap();
        tokio::spawn(async move {
            if let Ok(server_side) = crate::transport::uds::accept_transport(&listener).await
                && let Ok(server) = stub_server(server_side)
            {
                let _ = server.run().await;
            }
        });
    }

    #[tokio::test]
    async fn connect_request_round_trip_over_uds() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("c.sock");
        serve_one_connection(&sock).await;

        let client = GatewayClient::connect(&sock).await.unwrap();
        client
            .handshake("test", "0.1", Capabilities::default())
            .await
            .unwrap();

        let status = client
            .request("health/status", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(status["storeHealth"], "ok");
        client.close().await;
    }

    #[tokio::test]
    async fn unknown_method_returns_catalog_error() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("e.sock");
        serve_one_connection(&sock).await;

        let client = GatewayClient::connect(&sock).await.unwrap();
        client
            .handshake("test", "0.1", Capabilities::default())
            .await
            .unwrap();
        let err = client
            .request("nope/gone", serde_json::json!({}))
            .await
            .unwrap_err();
        assert_eq!(err.code, -32601);
        client.close().await;
    }

    #[tokio::test]
    async fn connect_or_spawn_uses_injected_spawner_and_lands_on_winner() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("s.sock");

        let config_sock = sock.clone();
        let spawns = Arc::new(AtomicUsize::new(0));
        let spawn_fn: DaemonSpawnFn = {
            let spawns = Arc::clone(&spawns);
            Arc::new(move || {
                spawns.fetch_add(1, Ordering::SeqCst);
                let sock = config_sock.clone();
                // Bind loser exits quietly; winner serves all comers.
                tokio::spawn(async move {
                    let listener = match crate::transport::uds::bind_socket(&sock).await {
                        Ok(l) => l,
                        Err(_) => return,
                    };
                    loop {
                        if let Ok(server_side) =
                            crate::transport::uds::accept_transport(&listener).await
                            && let Ok(server) = stub_server(server_side)
                        {
                            let _ = server.run().await;
                        }
                    }
                });
                Ok(())
            })
        };

        let client = GatewayClient::connect_or_spawn(&sock, Some(spawn_fn))
            .await
            .unwrap();
        client
            .handshake("race", "0.0", Capabilities::default())
            .await
            .unwrap();
        assert!(spawns.load(Ordering::SeqCst) >= 1);
        assert!(
            crate::transport::uds::bind_socket(&sock).await.is_err(),
            "exactly one daemon owns the socket"
        );
        client.close().await;
    }

    #[tokio::test]
    async fn heartbeat_flags_disconnect_when_peer_silent() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("hb.sock");
        let listener = crate::transport::uds::bind_socket(&sock).await.unwrap();
        tokio::spawn(async move {
            // Accept then drop: peer sees EOF without any reply.
            if let Ok(server_side) = crate::transport::uds::accept_transport(&listener).await {
                drop(server_side);
            }
        });
        let client = GatewayClient::connect(&sock).await.unwrap();
        let hb = client.start_heartbeat(Duration::from_millis(50));
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(!client.is_connected(), "silent peer must flag disconnect");
        hb.abort();
        client.close().await;
    }

    /// T055 test fixture: scripted initialize behavior of the fake
    /// daemon under test.
    #[derive(Clone)]
    enum InitializeScript {
        /// Successful initialize result advertising this `serverInfo`.
        Advertise(serde_json::Value),
        /// Refuse initialize with -32001 (older protocol daemon) and
        /// gate-reject `shutdown` with -32000 — the pre-handshake gate
        /// behavior a real older daemon exhibits.
        Refuse,
    }

    /// T055 test fixture: binds the socket, serves scripted
    /// initialize/shutdown/health frames per connection, and on the
    /// first `shutdown` releases the socket and exits — the release
    /// the restart cycle polls for. The shutdown REPLY is sent only
    /// after the teardown completes so the client's retry dial never
    /// races a half-torn-down listener.
    async fn scripted_version_daemon(
        sock: &Path,
        script: InitializeScript,
        shutdown_seen: Arc<AtomicUsize>,
    ) {
        let listener = crate::transport::uds::bind_socket(sock)
            .await
            .expect("scripted daemon binds");
        let released = Arc::new(tokio::sync::Notify::new());
        let torn_down = Arc::new(tokio::sync::Notify::new());
        loop {
            let side = tokio::select! {
                _ = released.notified() => break,
                accepted = crate::transport::uds::accept_transport(&listener) => match accepted {
                    Ok(side) => side,
                    Err(_) => break,
                },
            };
            let script = script.clone();
            let seen = Arc::clone(&shutdown_seen);
            let released = Arc::clone(&released);
            let torn_down = Arc::clone(&torn_down);
            tokio::spawn(async move {
                loop {
                    let Ok(frame) = side.recv().await else { break };
                    let Frame::ClientRequest { id, method, .. } = frame else {
                        continue; // `initialized` and other notifications
                    };
                    let reply = match method.as_str() {
                        "initialize" => match &script {
                            InitializeScript::Advertise(info) => Frame::response(
                                id,
                                serde_json::json!({
                                    "protocolVersion": SERVER_PROTOCOL_VERSION,
                                    "serverInfo": info,
                                    "capabilities": {},
                                }),
                            ),
                            InitializeScript::Refuse => Frame::error_response(
                                id,
                                crate::protocol::RpcError::version_mismatch(
                                    "0.0.1-old",
                                    SERVER_PROTOCOL_VERSION,
                                    "scripted refusal",
                                ),
                            ),
                        },
                        "shutdown" => {
                            seen.fetch_add(1, Ordering::SeqCst);
                            released.notify_one();
                            torn_down.notified().await;
                            match &script {
                                InitializeScript::Advertise(_) => Frame::response(
                                    id,
                                    serde_json::json!({"drained": 0, "cancelled": 0}),
                                ),
                                InitializeScript::Refuse => Frame::error_response(
                                    id,
                                    crate::protocol::RpcError::not_initialized(),
                                ),
                            }
                        }
                        "health/status" => Frame::response(
                            id,
                            serde_json::json!({
                                "serverVersion": "scripted",
                                "storeHealth": "ok",
                                "scheduler": false,
                            }),
                        ),
                        other => Frame::error_response(
                            id,
                            crate::protocol::RpcError::internal(&format!(
                                "scripted: no handler for {other}"
                            )),
                        ),
                    };
                    if side.send(reply).await.is_err() {
                        break;
                    }
                }
            });
        }
        drop(listener);
        std::fs::remove_file(sock).ok();
        torn_down.notify_one();
    }

    /// Waits until a daemon owns the socket (connect-probe style —
    /// never PID trust).
    async fn wait_socket_live(sock: &Path) {
        for _ in 0..400 {
            if std::os::unix::net::UnixStream::connect(sock).is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("scripted daemon never bound {}", sock.display());
    }

    /// Injected spawner booting a healthy stub daemon (bind-loser
    /// semantics preserved — races lose quietly); counts invocations.
    fn stub_daemon_spawner(sock: &Path) -> (DaemonSpawnFn, Arc<AtomicUsize>) {
        let spawns = Arc::new(AtomicUsize::new(0));
        let sock = sock.to_path_buf();
        let counted = {
            let spawns = Arc::clone(&spawns);
            move || {
                spawns.fetch_add(1, Ordering::SeqCst);
                let sock = sock.clone();
                tokio::spawn(async move {
                    let Ok(listener) = crate::transport::uds::bind_socket(&sock).await else {
                        return;
                    };
                    loop {
                        if let Ok(server_side) =
                            crate::transport::uds::accept_transport(&listener).await
                            && let Ok(server) = stub_server(server_side)
                        {
                            let _ = server.run().await;
                        }
                    }
                });
                Ok(())
            }
        };
        (Arc::new(counted), spawns)
    }

    /// Boots an incompatible scripted daemon on `sock`.
    fn spawn_incompatible_daemon(sock: &Path, shutdown_seen: Arc<AtomicUsize>) {
        let sock = sock.to_path_buf();
        tokio::spawn(async move {
            scripted_version_daemon(
                &sock,
                InitializeScript::Advertise(serde_json::json!({
                    "name": "zen-gateway",
                    "version": "999.0.0",
                })),
                shutdown_seen,
            )
            .await;
        });
    }

    #[tokio::test]
    async fn version_mismatch_triggers_exactly_one_restart_cycle() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("vp1.sock");

        let seen = Arc::new(AtomicUsize::new(0));
        spawn_incompatible_daemon(&sock, Arc::clone(&seen));
        wait_socket_live(&sock).await;

        let (spawn_fn, spawns) = stub_daemon_spawner(&sock);
        let client = GatewayClient::connect_or_spawn_with_probe(
            &sock,
            Some(spawn_fn),
            "probe-test",
            "0.0",
            Capabilities::default(),
        )
        .await
        .expect("one restart cycle must recover a compatible daemon");

        assert_eq!(seen.load(Ordering::SeqCst), 1, "one shutdown RPC");
        assert_eq!(
            spawns.load(Ordering::SeqCst),
            1,
            "one respawn — never a loop"
        );
        let status = client
            .request("health/status", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(status["storeHealth"], "ok", "recovered link is usable");
        client.close().await;
    }

    #[tokio::test]
    async fn persistent_mismatch_fails_with_recovery_hint_without_loop() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("vp2.sock");

        let seen_a = Arc::new(AtomicUsize::new(0));
        spawn_incompatible_daemon(&sock, Arc::clone(&seen_a));
        wait_socket_live(&sock).await;

        // The respawn lands on ANOTHER incompatible daemon (a racing
        // surface's old binary won the bind arbitration).
        let seen_b = Arc::new(AtomicUsize::new(0));
        let spawns = Arc::new(AtomicUsize::new(0));
        let b_sock = sock.clone();
        let spawn_fn: DaemonSpawnFn = {
            let spawns = Arc::clone(&spawns);
            let seen_b = Arc::clone(&seen_b);
            Arc::new(move || {
                spawns.fetch_add(1, Ordering::SeqCst);
                let sock = b_sock.clone();
                let seen = Arc::clone(&seen_b);
                spawn_incompatible_daemon(&sock, seen);
                Ok(())
            })
        };

        let err = match GatewayClient::connect_or_spawn_with_probe(
            &sock,
            Some(spawn_fn),
            "probe-test",
            "0.0",
            Capabilities::default(),
        )
        .await
        {
            Err(e) => e,
            Ok(_) => panic!("second mismatch must fail visibly, never connect silently"),
        };

        let msg = format!("{err:#}");
        assert!(msg.contains("version mismatch"), "got: {msg}");
        assert!(
            msg.contains("zen serve stop"),
            "FR-004 recovery hint, got: {msg}"
        );
        assert_eq!(
            spawns.load(Ordering::SeqCst),
            1,
            "exactly one restart cycle"
        );
        assert_eq!(seen_a.load(Ordering::SeqCst), 1, "daemon A shut down once");
        assert_eq!(
            seen_b.load(Ordering::SeqCst),
            0,
            "daemon B left running — no kill-and-loop"
        );
    }

    #[tokio::test]
    async fn unknown_server_version_accepts_after_one_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("vp3.sock");

        // Daemon A advertises an EMPTY serverVersion (T059b).
        let seen_a = Arc::new(AtomicUsize::new(0));
        {
            let sock = sock.to_path_buf();
            let seen = Arc::clone(&seen_a);
            tokio::spawn(async move {
                scripted_version_daemon(
                    &sock,
                    InitializeScript::Advertise(serde_json::json!({
                        "name": "foreign",
                        "version": "",
                    })),
                    seen,
                )
                .await;
            });
        }
        wait_socket_live(&sock).await;

        // The respawn lands on a daemon omitting serverInfo entirely.
        let spawns = Arc::new(AtomicUsize::new(0));
        let b_sock = sock.clone();
        let spawn_fn: DaemonSpawnFn = {
            let spawns = Arc::clone(&spawns);
            Arc::new(move || {
                spawns.fetch_add(1, Ordering::SeqCst);
                let sock = b_sock.clone();
                tokio::spawn(async move {
                    scripted_version_daemon(
                        &sock,
                        InitializeScript::Advertise(serde_json::json!({})),
                        Arc::new(AtomicUsize::new(0)),
                    )
                    .await;
                });
                Ok(())
            })
        };

        let client = GatewayClient::connect_or_spawn_with_probe(
            &sock,
            Some(spawn_fn),
            "probe-test",
            "0.0",
            Capabilities::default(),
        )
        .await
        .expect("still-unknown version after one restart must be accepted");

        assert_eq!(seen_a.load(Ordering::SeqCst), 1, "one restart attempted");
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
        let status = client
            .request("health/status", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(status["storeHealth"], "ok", "accepted link is usable");
        client.close().await;
    }

    #[tokio::test]
    async fn refused_handshake_triggers_restart_and_recovers() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("vp4.sock");

        // Daemon A refuses initialize with -32001 and gate-rejects
        // shutdown with -32000, yet still releases the socket.
        let seen = Arc::new(AtomicUsize::new(0));
        {
            let sock = sock.to_path_buf();
            let seen = Arc::clone(&seen);
            tokio::spawn(async move {
                scripted_version_daemon(&sock, InitializeScript::Refuse, seen).await;
            });
        }
        wait_socket_live(&sock).await;

        let (spawn_fn, spawns) = stub_daemon_spawner(&sock);
        let client = GatewayClient::connect_or_spawn_with_probe(
            &sock,
            Some(spawn_fn),
            "probe-test",
            "0.0",
            Capabilities::default(),
        )
        .await
        .expect("refused handshake must restart once and recover");

        assert_eq!(
            seen.load(Ordering::SeqCst),
            1,
            "shutdown attempted despite gate rejection"
        );
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
        let status = client
            .request("health/status", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(status["storeHealth"], "ok");
        client.close().await;
    }
}
