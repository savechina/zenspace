//! Gateway channel carriers — outbound platform surfaces (FR-021).
//!
//! # PURPOSE
//! A [`Channel`] is an outbound-connecting carrier: the gateway dials a
//! platform (QQ official bot today; future IM/webhook surfaces) and
//! bridges platform events onto the SAME dispatcher-backed session
//! stack the UDS/HTTP carriers serve (contracts/05 carrier model —
//! "carrier-layer addition with zero protocol change").
//!
//! # USAGE
//! Implement [`Channel`], construct from daemon config, spawn via
//! [`Channel::run`] in `serve_with_shutdown` beside the UDS accept
//! loop. Each inbound platform message opens a virtual dispatch
//! session (`SessionWire` + `ClientConnection` + dispatcher) exactly
//! like the HTTP carrier; outbound notifications are rendered as
//! platform replies by the channel implementation.
//!
//! # EXPECTED
//! Channels connect with `capabilities {approvals: false}` — turns
//! requiring approval fail fast with `-32010` and are rendered as
//! user-facing fallback text by the channel. Method surface is
//! deny-by-default: only `session/*`, `knowledge/search`,
//! `health/status` (contracts/05 security guards).
//!
//! # ERRORS
//! `run` returns `Err` only on unrecoverable setup failure;
//! connection drops are handled internally with backoff reconnect.

pub mod qqbot;

/// One outbound platform carrier bridged onto the gateway's own HTTP
/// transport server (user decision 2026-08-24: channels dial the
/// loopback `/api/v1` surface like any external client — FR-019/021
/// coexistence by construction).
#[async_trait::async_trait]
pub trait Channel: Send + Sync {
    /// Stable channel identifier (logs/audit correlation).
    fn name(&self) -> &'static str;

    /// Runs the carrier until `shutdown` fires or a fatal setup error
    /// occurs. Connection-level failures are retried internally —
    /// returning `Err` takes the carrier task down with a loud log,
    /// never the daemon itself.
    ///
    /// # Errors
    /// Fatal configuration/setup failure only.
    async fn run(&self, shutdown: tokio::sync::watch::Receiver<bool>) -> anyhow::Result<()>;
}
