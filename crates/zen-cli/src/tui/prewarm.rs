//! Background pre-warming for the TUI session (T053/T024).
//!
//! The first chat submission otherwise pays the full cold-start price
//! synchronously: gateway connect-or-spawn (including daemon launch on
//! a cold machine) — the ~10s Enter-to-LLM lag reported 2026-08-16
//! (input-display-plan.md).
//!
//! `spawn` dials the daemon in the background at session start. The hot
//! path (`start_async_chat`) takes the pre-warmed surface when ready
//! and falls back to dialing synchronously otherwise, so correctness
//! never depends on the race. Legacy orchestrator/DB caches were
//! removed with the pre-gateway execution paths (US3 cleanup).

use std::sync::{Arc, OnceLock};

use zen_gateway::client::SurfaceClient;

/// Gateway surface warmed at session start (T024): connect-or-spawn the
/// daemon before the first Enter instead of during it.
static SURFACE: OnceLock<Arc<SurfaceClient>> = OnceLock::new();
static SURFACE_WATCH: OnceLock<tokio::sync::watch::Receiver<Option<Arc<SurfaceClient>>>> =
    OnceLock::new();

/// Start background pre-warming. Cheap to call more than once (the
/// store only accepts the first value), but intended to run once at
/// session start.
pub(crate) fn spawn() {
    let (tx, rx) = tokio::sync::watch::channel(None);
    let _ = SURFACE_WATCH.set(rx);
    tokio::spawn(async move {
        match SurfaceClient::open_default("zen-tui", env!("CARGO_PKG_VERSION")).await {
            Ok(surface) => {
                let surface = Arc::new(surface);
                let _ = SURFACE.set(surface.clone());
                let _ = tx.send(Some(surface));
                tracing::debug!("prewarm: gateway surface ready");
            }
            Err(e) => tracing::warn!(
                error = %e,
                "prewarm: gateway connect-or-spawn failed (first turn will retry)"
            ),
        }
    });
}

/// Take the pre-warmed gateway surface, if ready.
pub(crate) fn take_client() -> Option<Arc<SurfaceClient>> {
    SURFACE.get().cloned()
}

/// Resolve the gateway surface: prewarm cache → await the in-flight
/// warm-up (bounded by the daemon readiness budget) → dial directly.
pub(crate) async fn resolve_client() -> Option<Arc<SurfaceClient>> {
    if let Some(surface) = take_client() {
        return Some(surface);
    }
    if let Some(rx) = SURFACE_WATCH.get() {
        let mut rx = rx.clone();
        if tokio::time::timeout(std::time::Duration::from_secs(3), rx.changed())
            .await
            .is_ok()
            && let Some(surface) = rx.borrow().clone()
        {
            return Some(surface);
        }
    }
    SurfaceClient::open_default("zen-tui", env!("CARGO_PKG_VERSION"))
        .await
        .ok()
        .map(Arc::new)
}
