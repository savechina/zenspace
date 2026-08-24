//! L1 carrier layer — transports move [`Frame`]s and inspect nothing else
//! (contracts/00: "L1 never inspects `method`").
//!
//! PURPOSE: The [`Transport`] trait is the entire carrier contract; every
//! carrier (in-process today; UDS, HTTP, qqbot later) implements only
//! async send/recv of frames.
//!
//! USAGE: Tests and embedded surfaces use
//! [`InProcessTransport`](in_process::InProcessTransport)::pair(); the UDS
//! daemon (later task) will implement this trait over UnixStream JSONL.
//!
//! EXPECTED: A pair round-trips a frame send→recv unchanged; in test mode
//! frames additionally pass through serde to prove wire-validity.
//!
//! ERRORS: Recv on a closed channel returns an error (peer gone); the L3
//! client treats this as a disconnect event.

pub mod http;
pub mod in_process;
pub mod uds;

use crate::protocol::Frame;

/// Carrier contract: asynchronous frame pipe in one direction of a
/// connection. Implementations hold one queue per direction; a full
/// connection pairs two implementations back-to-back.
///
/// The trait is [`async_trait`]-boxed so it is object-safe: daemons hold
/// `Arc<dyn Transport>` and stay carrier-agnostic without generic
/// plumbing (user decision 2026-08-23).
#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    /// Queues `frame` for delivery to the peer. Completes when accepted
    /// by the carrier (bounded channels may await capacity).
    ///
    /// # Errors
    /// Carrier-specific failures: closed channel, I/O error.
    async fn send(&self, frame: Frame) -> anyhow::Result<()>;

    /// Awaits the next frame from the peer.
    ///
    /// # Errors
    /// Carrier-specific failures: closed channel, I/O error, malformed
    /// wire line (UDS carriers map these to -32700/-32600 upstream).
    async fn recv(&self) -> anyhow::Result<Frame>;
}
