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
//! USAGE: `GatewayClient::connect_or_spawn(&path, None).await?` then
//! `.handshake("tui", caps).await?` and `.request(method, params).await`.
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
use tracing::warn;

use crate::protocol::{
    Capabilities, Frame, RpcErrorBody, SERVER_PROTOCOL_VERSION, initialize_params,
};
use crate::transport::Transport;
use crate::transport::uds::UdsTransport;

/// Per-surface facade with link-state tracking and turn recovery
/// (tasks T022–T025).
pub mod surface;

pub use surface::{GatewayLinkState, SurfaceClient, SurfaceError, TURN_TIMEOUT};

/// Readiness window after spawning a daemon (codex app-server-daemon
/// uses the same 10s budget: migrations and store open can legitimately
/// take seconds on a cold start).
pub const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// Delay between readiness probes.
const READY_POLL: Duration = Duration::from_millis(25);

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

/// Default production spawner: relaunch this binary detached as
/// `zen serve start --daemonized` with null stdio, stderr captured to
/// `<logs>/gateway-spawn.log`, and a new session.
fn default_spawn() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.args(["serve", "start", "--daemonized"])
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
        // Detach from the caller's process group without pulling libc in.
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("spawn zen serve: {e}"))
}

/// Dials until the socket answers or `deadline` passes.
async fn wait_for_ready(path: &Path, deadline: Duration) -> anyhow::Result<UdsTransport> {
    let end = tokio::time::Instant::now() + deadline;
    loop {
        match UdsTransport::connect(path).await {
            Ok(t) => return Ok(t),
            Err(_) if tokio::time::Instant::now() < end => {
                tokio::time::sleep(READY_POLL).await;
            }
            Err(e) => anyhow::bail!(
                "gateway not ready at {} within {deadline:?}: {e}{}",
                path.display(),
                spawn_log_tail(2048)
            ),
        }
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
pub struct EmbeddedServer {
    shutdown: Arc<tokio::sync::watch::Sender<bool>>,
}

impl Drop for EmbeddedServer {
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
    /// The returned [`EmbeddedServer`] must be held by the surface for
    /// as long as it may act as the server; dropping it drains.
    ///
    /// # Errors
    /// Dial failure plus readiness timeout (with spawn-log tail).
    pub async fn connect_or_embed(
        socket_path: impl AsRef<Path>,
    ) -> anyhow::Result<(Self, Option<EmbeddedServer>)> {
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
        let transport = Arc::new(wait_for_ready(path, READY_TIMEOUT).await?);
        Ok((
            Self::from_transport(transport, path),
            Some(EmbeddedServer { shutdown }),
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
        let transport = Arc::new(wait_for_ready(path, READY_TIMEOUT).await?);
        Ok(Self::from_transport(transport, path))
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
}
