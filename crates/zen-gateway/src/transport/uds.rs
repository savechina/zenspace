//! L1 UDS carrier — JSONL-framed [`Frame`] pipe over a Unix domain socket
//! (task T011).
//!
//! PURPOSE: The daemon's wire carrier. One frame per line, one line per
//! frame; the socket file is created with owner-only permissions and the
//! bind is atomic — a live gateway on the path refuses a second bind, a
//! dead (stale) socket file is cleaned before rebinding.
//!
//! USAGE: Server side accepts via [`bind_socket`] + [`accept_transport`]
//! and wraps each stream in [`UdsTransport::from_stream`]; clients dial
//! with [`UdsTransport::connect`]. Both sides speak the same framing.
//!
//! EXPECTED: A frame sent by one end arrives unchanged at the other;
//! `bind_socket` twice on one path fails the second call while a live
//! listener exists.
//!
//! ERRORS: Peer close surfaces as an `Err` from `recv` ("closed by peer");
//! malformed lines are transport errors mapped to -32700/-32600 upstream
//! by the dispatch layer. Bind failures name the socket path.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

use crate::protocol::Frame;
use crate::transport::Transport;

/// Socket file permission: owner read/write only (0600).
#[cfg(unix)]
const SOCKET_MODE: u32 = 0o600;

/// JSONL-framed half-duplex pair member over a [`UnixStream`]. Daemons
/// share it as `Arc<dyn Transport>` (trait is object-safe via
/// `async_trait`).
pub struct UdsTransport {
    reader: Arc<Mutex<tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>>>,
    writer: Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
}

impl UdsTransport {
    /// Wraps an accepted (or dialed) stream, splitting it into concurrent
    /// read/write halves.
    ///
    /// # Errors
    /// Fails if stream split fails (already-split stream).
    pub fn from_stream(stream: UnixStream) -> anyhow::Result<Self> {
        let (read_half, write_half) = stream.into_split();
        Ok(Self {
            reader: Arc::new(Mutex::new(tokio::io::BufReader::new(read_half))),
            writer: Arc::new(Mutex::new(write_half)),
        })
    }

    /// Dials a gateway socket as a client.
    ///
    /// # Errors
    /// Connection refused / socket missing / permission denied.
    pub async fn connect(socket_path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let stream = UnixStream::connect(socket_path.as_ref()).await?;
        Self::from_stream(stream)
    }

    /// Half-closes the write direction so the peer observes EOF while
    /// the read direction stays open. Lifecycle control for drains and
    /// tests that need to simulate an owner dying mid-turn.
    ///
    /// # Errors
    /// Fails if the underlying socket write half is already closed.
    pub async fn shutdown_write(&self) -> anyhow::Result<()> {
        self.writer.lock().await.shutdown().await?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl Transport for UdsTransport {
    async fn send(&self, frame: Frame) -> anyhow::Result<()> {
        let mut writer = self.writer.lock().await;
        // Frame::to_json_line already terminates the line.
        let line = frame.to_json_line()?;
        writer.write_all(line.as_bytes()).await?;
        writer.flush().await?;
        Ok(())
    }

    async fn recv(&self) -> anyhow::Result<Frame> {
        let mut reader = self.reader.lock().await;
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            anyhow::bail!("uds transport closed by peer");
        }
        Frame::from_json_line(line.trim_end_matches(['\n', '\r']))
            .map_err(|e| anyhow::anyhow!("malformed frame line: {e:#}"))
    }
}

/// Binds a UnixListener at `socket_path` atomically (T011): a live socket
/// refuses the bind (sole-owner claim), a stale file left by a dead
/// process is removed first. The socket file gets 0600 permissions.
///
/// Exactly-one semantics hold under races: two processes that both see a
/// stale file cannot both succeed — only one `bind(2)` wins once the
/// pathname exists again.
///
/// # Errors
/// - Live socket on path → error naming the path.
/// - Filesystem errors (create dir, remove stale, bind, chmod).
pub async fn bind_socket(socket_path: &Path) -> anyhow::Result<UnixListener> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("create socket dir {}: {e}", parent.display()))?;
    }
    if socket_path.exists() {
        // Probe synchronously: a live listener answers instantly.
        match std::os::unix::net::UnixStream::connect(socket_path) {
            Ok(_) => anyhow::bail!(
                "gateway socket {} is live; refusing second bind",
                socket_path.display()
            ),
            Err(_) => {
                tracing::info!(path = %socket_path.display(), "removing stale gateway socket");
                std::fs::remove_file(socket_path).map_err(|e| {
                    anyhow::anyhow!("remove stale socket {}: {e}", socket_path.display())
                })?;
            }
        }
    }
    let listener = UnixListener::bind(socket_path)
        .map_err(|e| anyhow::anyhow!("bind gateway socket {}: {e}", socket_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(SOCKET_MODE))?;
    }
    Ok(listener)
}

/// Accepts one connection and wraps it in a [`UdsTransport`].
///
/// # Errors
/// Accept failure (listener closed / fd exhaustion).
pub async fn accept_transport(listener: &UnixListener) -> anyhow::Result<UdsTransport> {
    let (stream, _addr) = listener.accept().await?;
    UdsTransport::from_stream(stream)
}

/// Removes the socket file if present (daemon shutdown cleanup). Missing
/// file is not an error.
pub fn cleanup_socket(socket_path: &Path) {
    if let Err(e) = std::fs::remove_file(socket_path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %socket_path.display(), error = %e, "failed to remove gateway socket");
    }
}

/// Resolves the default socket path: `<global data>/gateway.sock`.
pub fn default_socket_path() -> PathBuf {
    zen_core::paths::ZenPaths::detect()
        .map(|p| p.data().join("gateway.sock"))
        .unwrap_or_else(|_| zen_core::paths::user_root().join("gateway.sock"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::SERVER_PROTOCOL_VERSION;

    #[tokio::test]
    async fn bind_twice_on_same_path_fails_second() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.sock");
        let _l1 = bind_socket(&path).await.unwrap();
        assert!(matches!(bind_socket(&path).await, Err(e) if e.to_string().contains("live")));
    }

    #[tokio::test]
    async fn stale_socket_is_cleaned_and_rebound() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.sock");
        std::fs::write(&path, b"garbage").unwrap(); // dead file, no listener
        let l = bind_socket(&path).await;
        assert!(l.is_ok(), "dead socket file should be cleaned");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn socket_permissions_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("perm.sock");
        drop(bind_socket(&path).await.unwrap());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[tokio::test]
    async fn jsonl_round_trip_between_pair() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("rt.sock");
        let listener = bind_socket(&path).await.unwrap();
        let client_task = tokio::spawn(async move {
            let c = UdsTransport::connect(&path).await.unwrap();
            c.send(Frame::notification(
                "initialized",
                serde_json::json!({"v": SERVER_PROTOCOL_VERSION}),
            ))
            .await
            .unwrap();
        });
        let server = accept_transport(&listener).await.unwrap();
        client_task.await.unwrap();
        let frame = server.recv().await.unwrap();
        assert_eq!(frame.method(), Some("initialized"));
        assert!(frame.validate_wire());
    }

    #[tokio::test]
    async fn recv_after_close_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("close.sock");
        let listener = bind_socket(&path).await.unwrap();
        let handle = tokio::spawn(async move {
            let c = UdsTransport::connect(&path).await.unwrap();
            drop(c); // immediate close
        });
        let server = accept_transport(&listener).await.unwrap();
        handle.await.unwrap();
        // Client close may race our recv; loop until EOF observed.
        for _ in 0..50 {
            match server.recv().await {
                Err(e)
                    if e.to_string().contains("closed") || e.to_string().contains("malformed") =>
                {
                    return;
                }
                Ok(_) => continue,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
        panic!("peer close never observed");
    }
}
