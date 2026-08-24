//! E2 ClientConnection — one connected surface's lifecycle record and
//! bounded outbound queue (task T010, data-model E2).
//!
//! PURPOSE: Tracks Connecting→Initialized→Closed per data-model E2 and
//! owns the bounded (256-frame) outbound queue with the design §5.2
//! two-class policy: structural frames are never dropped (backpressure),
//! delta frames may be dropped under pressure (coalescing class).
//!
//! USAGE: The daemon creates one per accepted socket around an
//! `mpsc::channel::<OutboundFrame>(OUTBOUND_CAPACITY)`; a pump task drains
//! the receiver onto the wire. Server-initiated pushes (approvals now,
//! session events later) call [`ClientConnection::enqueue`] directly.
//!
//! EXPECTED: duplicate `mark_ready` fails returning the current state;
//! `close` is idempotent from any state; a full queue drops deltas and
//! backpresses structurals.
//!
//! ERRORS: Enqueue fails when the queue is closed (pump gone = peer
//! disconnected); callers treat that as connection death.

use tokio::sync::{Mutex, mpsc};

use crate::protocol::{Capabilities, Frame};

/// Bounded outbound queue depth (design §5.2).
pub const OUTBOUND_CAPACITY: usize = 256;

/// E2 lifecycle states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Pre-handshake: no successful `initialize`+`initialized` yet.
    Connecting,
    /// Handshake complete; requests accepted.
    Initialized,
    /// Terminal: disconnect or timeout. Triggers E2-GC upstream.
    Closed,
}

/// Outbound priority class (design §5.2): deltas coalesce (may drop),
/// structural frames never drop (backpressure instead).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameClass {
    /// Streaming token/coalescible event.
    Delta,
    /// Lifecycle/response frames that must not be lost.
    Structural,
}

/// One queued outbound frame plus its drop-policy class.
#[derive(Debug, Clone)]
pub struct OutboundFrame {
    pub frame: Frame,
    pub class: FrameClass,
}

impl OutboundFrame {
    /// Wraps `frame` with the never-drop class.
    pub fn structural(frame: Frame) -> Self {
        Self {
            frame,
            class: FrameClass::Structural,
        }
    }

    /// Wraps `frame` with the droppable-under-pressure class.
    pub fn delta(frame: Frame) -> Self {
        Self {
            frame,
            class: FrameClass::Delta,
        }
    }
}

/// Handshake-reported client identity (`clientInfo` from `initialize`).
#[derive(Debug, Clone)]
pub struct ClientIdentity {
    pub name: String,
    pub version: String,
}

/// E2 record for one connected surface.
pub struct ClientConnection {
    /// Server-minted UUIDv7 connection id.
    pub id: String,
    state: Mutex<ConnectionState>,
    identity: Mutex<Option<ClientIdentity>>,
    capabilities: Mutex<Capabilities>,
    outbound: mpsc::Sender<OutboundFrame>,
}

impl ClientConnection {
    /// Creates a `Connecting` connection over `outbound`.
    pub fn new(outbound: mpsc::Sender<OutboundFrame>) -> Self {
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            state: Mutex::new(ConnectionState::Connecting),
            identity: Mutex::new(None),
            capabilities: Mutex::new(Capabilities::default()),
            outbound,
        }
    }

    /// Cloned queue sender for transports/pumps sharing this connection.
    pub fn queue(&self) -> mpsc::Sender<OutboundFrame> {
        self.outbound.clone()
    }

    /// Current lifecycle state.
    pub async fn state(&self) -> ConnectionState {
        *self.state.lock().await
    }

    /// Capabilities negotiated at handshake (defaults pre-handshake).
    pub async fn capabilities(&self) -> Capabilities {
        *self.capabilities.lock().await
    }

    /// Client identity reported by `initialize`; `None` until ready.
    pub async fn identity(&self) -> Option<ClientIdentity> {
        self.identity.lock().await.clone()
    }

    /// Connecting→Initialized transition carrying handshake artifacts.
    ///
    /// # Errors
    /// Returns the current state when the connection is not `Connecting`
    /// (duplicate initialize / post-close).
    pub async fn mark_ready(
        &self,
        identity: ClientIdentity,
        capabilities: Capabilities,
    ) -> Result<ConnectionState, ConnectionState> {
        let mut state = self.state.lock().await;
        if *state != ConnectionState::Connecting {
            return Err(*state);
        }
        *self.identity.lock().await = Some(identity);
        *self.capabilities.lock().await = capabilities;
        *state = ConnectionState::Initialized;
        Ok(*state)
    }

    /// Any-state → Closed; idempotent (E2 terminal transition).
    pub async fn close(&self) {
        *self.state.lock().await = ConnectionState::Closed;
    }

    /// Enqueues with frame-class policy: deltas are dropped (warned) when
    /// the queue is full; structurals await capacity instead of dropping.
    ///
    /// # Errors
    /// Queue closed (peer gone).
    pub async fn enqueue(&self, outbound: OutboundFrame) -> anyhow::Result<()> {
        match self.outbound.try_send(outbound) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(of)) => {
                if matches!(of.class, FrameClass::Delta) {
                    tracing::warn!(
                        connection_id = %self.id,
                        "outbound queue full; dropping delta frame"
                    );
                    Ok(())
                } else {
                    self.outbound
                        .send(of)
                        .await
                        .map_err(|e| anyhow::anyhow!("outbound queue closed: {e}"))
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                anyhow::bail!("outbound queue closed")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn frame_ping() -> Frame {
        Frame::notification("initialized", json!({}))
    }

    #[tokio::test]
    async fn lifecycle_transitions_match_e2() {
        let (tx, _rx) = mpsc::channel::<OutboundFrame>(4);
        let conn = ClientConnection::new(tx);
        assert_eq!(conn.state().await, ConnectionState::Connecting);

        conn.mark_ready(
            ClientIdentity {
                name: "tui".into(),
                version: "0.1".into(),
            },
            Capabilities::default(),
        )
        .await
        .unwrap();
        assert_eq!(conn.state().await, ConnectionState::Initialized);

        // Duplicate ready rejected with current state.
        assert_eq!(
            conn.mark_ready(
                ClientIdentity {
                    name: "again".into(),
                    version: "0".into()
                },
                Capabilities::default(),
            )
            .await,
            Err(ConnectionState::Initialized)
        );

        conn.close().await;
        conn.close().await; // idempotent
        assert_eq!(conn.state().await, ConnectionState::Closed);

        // Ready-after-close also rejected.
        assert_eq!(
            conn.mark_ready(
                ClientIdentity {
                    name: "x".into(),
                    version: "x".into()
                },
                Capabilities::default(),
            )
            .await,
            Err(ConnectionState::Closed)
        );
    }

    #[tokio::test]
    async fn delta_frames_drop_when_queue_full_structurals_backpressure() {
        let (tx, mut rx) = mpsc::channel::<OutboundFrame>(1);
        let conn = std::sync::Arc::new(ClientConnection::new(tx));

        conn.enqueue(OutboundFrame::delta(frame_ping()))
            .await
            .unwrap();

        // Structural enqueue parks (queue at capacity); spawned so the
        // test task stays free to drain.
        let structural_task = tokio::spawn({
            let c = std::sync::Arc::clone(&conn);
            async move { c.enqueue(OutboundFrame::structural(frame_ping())).await }
        });
        let delta_task = tokio::spawn({
            let c = std::sync::Arc::clone(&conn);
            async move { c.enqueue(OutboundFrame::delta(frame_ping())).await }
        });

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            !structural_task.is_finished(),
            "structural enqueue must await capacity"
        );
        assert!(
            delta_task.is_finished(),
            "delta enqueue must drop-and-return under pressure"
        );

        rx.recv().await;
        delta_task.await.unwrap().unwrap();
        structural_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn enqueue_fails_after_receiver_dropped() {
        let (tx, rx) = mpsc::channel::<OutboundFrame>(4);
        let conn = ClientConnection::new(tx);
        drop(rx);
        assert!(
            conn.enqueue(OutboundFrame::structural(frame_ping()))
                .await
                .is_err()
        );
    }
}
