//! In-process carrier — mpsc-backed [`Transport`] pairs for tests,
//! `zen --local`, and embedded use (contracts/00 carrier table).
//!
//! PURPOSE: Provides a zero-serialization carrier whose frames can be
//! forced through serde JSON round-trips in test mode, proving the typed
//! enum is wire-faithful (isomorphic point per the dsh InProcessApiClient
//! precedent).
//!
//! USAGE: `let (client, server) = InProcessTransport::pair();` — the first
//! element is the client-side endpoint (client sends Q1, receives Q2), the
//! second the server-side. `pair_wire_checked()` additionally validates
//! every exchanged frame round-trips through JSON.
//!
//! EXPECTED: Frames arrive in FIFO order per direction; dropping both
//! endpoints of a direction closes it and `recv` errors.
//!
//! ERRORS: `recv` returns an error once the peer endpoint is dropped.

use std::sync::Arc;

use tokio::sync::Mutex;

use crate::protocol::Frame;
use crate::transport::Transport;

/// Bounded capacity per direction — matches the 256-frame outbound queue
/// budget from data-model E2 so backpressure behavior is representative.
pub const CHANNEL_CAPACITY: usize = 256;

/// One endpoint of an in-process transport pair. Holds the sender toward
/// the peer and the receiver from the peer; both behind `Arc` so the
/// endpoint can be cloned for multi-task use (server loops + approval
/// bridges).
#[derive(Clone)]
pub struct InProcessTransport {
    tx: Arc<tokio::sync::mpsc::Sender<Frame>>,
    rx: Arc<Mutex<tokio::sync::mpsc::Receiver<Frame>>>,
    wire_checked: bool,
}

/// Creates a connected client/server endpoint pair over two mpsc channels.
///
/// The first endpoint's `send` delivers to the second endpoint's `recv`,
/// and vice versa. Neither endpoint serializes frames (fast path).
pub fn pair() -> (InProcessTransport, InProcessTransport) {
    build_pair(false)
}

/// Like [`pair()`], but every frame crossing either endpoint is first
/// serialized to JSON and parsed back — proving each exchanged frame is
/// exactly representable on the wire (contract-suite mode).
pub fn pair_wire_checked() -> (InProcessTransport, InProcessTransport) {
    build_pair(true)
}

fn build_pair(wire_checked: bool) -> (InProcessTransport, InProcessTransport) {
    let (c2s_tx, c2s_rx) = tokio::sync::mpsc::channel(CHANNEL_CAPACITY);
    let (s2c_tx, s2c_rx) = tokio::sync::mpsc::channel(CHANNEL_CAPACITY);
    let client = InProcessTransport {
        tx: Arc::new(c2s_tx),
        rx: Arc::new(Mutex::new(s2c_rx)),
        wire_checked,
    };
    let server = InProcessTransport {
        tx: Arc::new(s2c_tx),
        rx: Arc::new(Mutex::new(c2s_rx)),
        wire_checked,
    };
    (client, server)
}

impl InProcessTransport {
    /// Serializes then deserializes `frame`, returning the wire-validated
    /// copy. Panics on non-representable frames — test-mode invariant.
    fn wire_round_trip(frame: &Frame) -> Frame {
        let json = serde_json::to_string(frame).expect("frame serializes");
        serde_json::from_str(&json).expect("frame parses back")
    }
}

#[async_trait::async_trait]
impl Transport for InProcessTransport {
    async fn send(&self, mut frame: Frame) -> anyhow::Result<()> {
        if self.wire_checked {
            frame = Self::wire_round_trip(&frame);
        }
        self.tx
            .send(frame)
            .await
            .map_err(|_| anyhow::anyhow!("in-process channel closed"))
    }

    async fn recv(&self) -> anyhow::Result<Frame> {
        let mut rx = self.rx.lock().await;
        rx.recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("in-process channel closed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pair_round_trips_fifo() {
        let (client, server) = pair();
        client
            .send(Frame::request(1, "health/status"))
            .await
            .unwrap();
        client
            .send(Frame::request(2, "health/status"))
            .await
            .unwrap();
        let f1 = server.recv().await.unwrap();
        let f2 = server.recv().await.unwrap();
        assert_eq!(f1, Frame::request(1, "health/status"));
        assert_eq!(f2, Frame::request(2, "health/status"));
    }

    #[tokio::test]
    async fn bidirectional() {
        let (client, server) = pair();
        server
            .send(Frame::response(1, serde_json::json!({"ok":true})))
            .await
            .unwrap();
        let got = client.recv().await.unwrap();
        assert_eq!(got, Frame::response(1, serde_json::json!({"ok":true})));
    }

    #[tokio::test]
    async fn wire_checked_mode_proves_json_faithfulness() {
        let (client, server) = pair_wire_checked();
        client
            .send(Frame::server_request(
                "srv-9".into(),
                "approval/request",
                serde_json::json!({
                    "turnId": "t_1", "invocation": {"name": "shell.exec"}, "reason": "risk"
                }),
            ))
            .await
            .unwrap();
        let got = server.recv().await.unwrap();
        assert!(matches!(got, Frame::ServerRequest { .. }));
    }

    #[tokio::test]
    async fn recv_errors_when_peer_dropped() {
        let (client, server) = pair();
        drop(client);
        assert!(server.recv().await.is_err());
    }
}
