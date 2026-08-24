//! Server-side gateway modules: connection management (T010) and
//! registry-driven dispatch. Handlers, guards, and event fan-out arrive
//! with Phase 4+ tasks.
//!
//! PURPOSE: Owns everything the daemon does per-connection. The E2
//! [`ClientConnection`] record tracks the Connecting→Initialized→Closed
//! lifecycle and owns the bounded outbound queue; [`QueueFrontTransport`]
//! routes every dispatcher write through that queue so backpressure and
//! frame-class policy apply to all server output. The dispatch loop is
//! transport-generic — the UDS daemon spawns one
//! [`DispatchServer`](dispatch::DispatchServer) per accepted socket with
//! real business handlers.
//!
//! USAGE: Per accepted socket, build a channel pair
//! `mpsc::channel::<OutboundFrame>(OUTBOUND_CAPACITY)`, wrap the sender in
//! a `ClientConnection`, register it in the daemon's connection map, and
//! hand a `QueueFrontTransport` to `DispatchServer::new`. Server-initiated
//! pushes bypass the dispatcher via `ClientConnection::enqueue`.
//!
//! EXPECTED: pre-initialize requests → -32000; structural frames are
//! never dropped (backpressure instead); delta frames may be dropped when
//! the queue is full (design §5.2 coalescing class).
//!
//! ERRORS: Queue closed (peer gone) surfaces as `Err` from enqueue/send;
//! the accept loop reaps the connection record on dispatch exit.

pub mod approval;
pub mod connection;
pub mod dispatch;
pub mod guards;
pub mod hosting;
pub mod knowledge;
pub mod memory;
pub mod readouts;

pub use connection::{
    ClientConnection, ClientIdentity, ConnectionState, FrameClass, OUTBOUND_CAPACITY, OutboundFrame,
};
pub use dispatch::{
    APPROVAL_TIMEOUT, ApprovalOutcome, ConnectionHandle, DispatchServer, HandlerFn, stub_server,
};
pub use guards::{Guards, WATCHDOG_TIMEOUT};
pub use hosting::{HostingDeps, TurnRegistry, TurnState};

use crate::protocol::Frame;
use crate::transport::Transport;

/// Dispatcher-facing transport front-end: sends land in the owning
/// [`ClientConnection`] outbound queue (always structural — responses are
/// never dropped); receives pass through to the shared wire endpoint.
pub struct QueueFrontTransport {
    queue: tokio::sync::mpsc::Sender<OutboundFrame>,
    upstream: std::sync::Arc<dyn Transport>,
}

impl QueueFrontTransport {
    /// Wraps a shared wire endpoint, routing sends through `queue`.
    pub fn new(
        queue: tokio::sync::mpsc::Sender<OutboundFrame>,
        upstream: std::sync::Arc<dyn Transport>,
    ) -> Self {
        Self { queue, upstream }
    }
}

#[async_trait::async_trait]
impl Transport for QueueFrontTransport {
    async fn send(&self, frame: Frame) -> anyhow::Result<()> {
        self.queue
            .send(OutboundFrame::structural(frame))
            .await
            .map_err(|e| anyhow::anyhow!("outbound queue closed: {e}"))
    }

    async fn recv(&self) -> anyhow::Result<Frame> {
        self.upstream.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn queued_send_reaches_upstream_pump() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<OutboundFrame>(4);
        let (wire_client, wire_server) = crate::transport::in_process::pair();
        let wire: std::sync::Arc<dyn Transport> = std::sync::Arc::new(wire_client);

        // Pump: forwards every queued frame onto the real wire.
        let pump_wire = std::sync::Arc::clone(&wire);
        tokio::spawn(async move {
            while let Some(of) = rx.recv().await {
                if pump_wire.send(of.frame).await.is_err() {
                    break;
                }
            }
        });

        let front = QueueFrontTransport::new(tx, wire);
        front
            .send(Frame::notification("initialized", serde_json::json!({})))
            .await
            .unwrap();
        drop(front);

        let got = wire_server.recv().await.unwrap();
        assert_eq!(got.method(), Some("initialized"));
    }
}
