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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use zen_gateway::client::SurfaceClient;
use zen_gateway::client::surface::ApprovalBridgeEnds;

use crate::cmd::serve_command::{
    DaemonVersionState, classify_daemon_version, restart_daemon_quiet, upgrade_policy_auto_restart,
};

/// FR-023: last handshake refusal (reason, recovery) seen during a dial —
/// recorded so the turn producer can surface a version-mismatch verbatim
/// when no link is available (the generic no-link path would otherwise
/// flatten it into OfflineDegraded).
static LAST_DIAL_REFUSAL: Mutex<Option<(String, String)>> = Mutex::new(None);

/// FR-024: TUI-side ends of the approval channel pair, constructed once at
/// session start and consumed once by [`App`] construction. Follows the same
/// `OnceLock`/`Mutex<Option<_>>` pattern as `LAST_DIAL_REFUSAL`.
///
/// PURPOSE: Bridges the gateway notification pump (tokio async) to the TUI
///   tick loop (sync crossterm poll). The pump sends `ApprovalRequestPayload`
///   on `request_rx`; the TUI drains via `try_recv` in `inline_tick`.
///   Decisions go back on `response_tx`.
///
/// USAGE: Set by `spawn()`/`resolve_client()` after a successful open;
///   consumed once by `take_approval_bridge()`.
///
/// EXPECTED: One bridge per TUI session; the pump reads the sink via
///   `Arc::clone` + lock, so setting it after open is race-free.
///
/// ERRORS: None — pure data type.
static APPROVAL_BRIDGE: Mutex<Option<ApprovalBridgeEnds>> = Mutex::new(None);

/// Phase 30 G3 detection point (c): stale-daemon recovery hint, recorded
/// ONCE per session (guard) and rendered as a suffix on the gateway banner
/// (`Ok` state) so the TUI viewport is never corrupted by a stray println.
static STALE_HINT_SHOWN: AtomicBool = AtomicBool::new(false);
static STALE_DAEMON_HINT: Mutex<Option<String>> = Mutex::new(None);

/// Pure hint text: `None` when the daemon version matches this binary.
pub(crate) fn stale_hint_text(
    daemon_version: Option<&str>,
    binary_version: &str,
) -> Option<String> {
    match classify_daemon_version(binary_version, daemon_version) {
        DaemonVersionState::Current => None,
        DaemonVersionState::Stale => Some(format!(
            "STALE daemon {} != binary {binary_version} — run `zen serve restart`",
            daemon_version.unwrap_or_default().trim()
        )),
        DaemonVersionState::Unknown => Some(format!(
            "STALE-unknown daemon version (binary {binary_version}) — run `zen serve restart`"
        )),
    }
}

fn note_stale_once(hint: String) {
    if STALE_HINT_SHOWN.swap(true, Ordering::SeqCst) {
        return;
    }
    tracing::warn!(target: "zen_tui", "{hint}");
    if let Ok(mut g) = STALE_DAEMON_HINT.lock() {
        *g = Some(hint);
    }
}

/// Peek (non-consuming) the recorded stale-daemon hint for banner display.
pub(crate) fn stale_hint() -> Option<String> {
    STALE_DAEMON_HINT.lock().ok().and_then(|g| g.clone())
}

/// Test seam: clears the once-guard and the recorded hint.
#[cfg(test)]
fn reset_stale_hint() {
    STALE_HINT_SHOWN.store(false, Ordering::SeqCst);
    if let Ok(mut g) = STALE_DAEMON_HINT.lock() {
        *g = None;
    }
}

fn is_version_refusal(e: &zen_gateway::client::SurfaceError) -> bool {
    matches!(e, zen_gateway::client::SurfaceError::Rpc(rpc) if rpc.code == -32001)
}

fn record_dial_error(e: &zen_gateway::client::SurfaceError) {
    if let zen_gateway::client::SurfaceError::Rpc(rpc) = e
        && rpc.code == -32001
    {
        let reason = rpc
            .data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(|v| v.as_str())
            .unwrap_or(rpc.message.as_str())
            .to_string();
        let recovery = rpc
            .data
            .as_ref()
            .and_then(|d| d.get("recovery"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if let Ok(mut g) = LAST_DIAL_REFUSAL.lock() {
            *g = Some((reason, recovery));
        }
    }
}

/// Take (consume) the last recorded dial refusal, if any.
pub(crate) fn take_dial_refusal() -> Option<(String, String)> {
    LAST_DIAL_REFUSAL.lock().ok().and_then(|mut g| g.take())
}

/// Take (consume) the FR-024 approval bridge ends, if available.
/// Called once at TUI [`App`] construction. The bridge is consumed (not cloned)
/// so exactly one TUI drains the request channel.
pub(crate) fn take_approval_bridge() -> Option<ApprovalBridgeEnds> {
    APPROVAL_BRIDGE.lock().ok().and_then(|mut g| g.take())
}

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
        match open_surface().await {
            Some(surface) => {
                let _ = SURFACE.set(surface.clone());
                let _ = tx.send(Some(surface));
                tracing::debug!("prewarm: gateway surface ready (approval sink installed)");
            }
            None => {
                tracing::warn!("prewarm: gateway connect-or-spawn failed (first turn will retry)");
            }
        }
    });
}

/// FR-024: create the approval channel pair and install the sink on a
/// freshly opened surface. The pump reads the sink per-request via
/// Arc::clone (not take), so setting it after open is race-free — the pump
/// loop has not yet reached the approval branch when the first request can
/// arrive (the handshake must complete first).
fn adopt(surface: SurfaceClient) -> Arc<SurfaceClient> {
    let (sink, bridge) = SurfaceClient::approval_channel();
    surface.set_approval_sink(sink);
    if let Ok(mut g) = APPROVAL_BRIDGE.lock() {
        *g = Some(bridge);
    }
    Arc::new(surface)
}

/// Connect-or-spawn plus the Phase 30 G3 upgrade gate: a stale daemon is
/// hinted once, and under `upgrade_policy = "auto-restart"` drain-restarted
/// with exactly ONE reconnect attempt (no loops).
async fn open_surface() -> Option<Arc<SurfaceClient>> {
    match SurfaceClient::open_default("zen-tui", env!("CARGO_PKG_VERSION")).await {
        Ok(surface) => Some(adopt(upgrade_gate(surface).await?)),
        Err(e) => {
            let version_refusal = is_version_refusal(&e);
            record_dial_error(&e);
            tracing::warn!(error = %e, "prewarm: gateway dial failed");
            if version_refusal {
                note_stale_once(
                    "gateway handshake refused (version mismatch) — run `zen serve restart`"
                        .to_string(),
                );
                if upgrade_policy_auto_restart() {
                    tracing::info!(
                        "prewarm: upgrade_policy=auto-restart — restarting after -32001 refusal"
                    );
                    return restart_and_reopen_once().await.map(adopt);
                }
            }
            None
        }
    }
}

/// Stale-build check on a live surface via the daemon's own
/// `health/status.serverVersion` — authoritative for BOTH explicit and
/// implicit daemons (implicit ones always report the current version, so
/// they can never trigger the policy). A failed probe is NOT staleness —
/// it degrades to no action.
async fn upgrade_gate(surface: SurfaceClient) -> Option<SurfaceClient> {
    let health = match surface.health_status().await {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(error = %e, "prewarm: health probe failed; skipping stale check");
            return Some(surface);
        }
    };
    let daemon_version = health
        .get("serverVersion")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty());
    if classify_daemon_version(env!("CARGO_PKG_VERSION"), daemon_version)
        == DaemonVersionState::Current
    {
        return Some(surface);
    }
    if let Some(hint) = stale_hint_text(daemon_version, env!("CARGO_PKG_VERSION")) {
        note_stale_once(hint);
    }
    if !upgrade_policy_auto_restart() {
        return Some(surface);
    }
    tracing::info!("prewarm: upgrade_policy=auto-restart — restarting the stale daemon");
    restart_and_reopen_once().await
}

/// One drain-restart + one reconnect; `None` on either failure (the caller
/// degrades — the next turn's `resolve_client` retries the dial).
async fn restart_and_reopen_once() -> Option<SurfaceClient> {
    if let Err(e) = restart_daemon_quiet().await {
        tracing::warn!(error = %e, "prewarm: auto-restart failed");
        return None;
    }
    match SurfaceClient::open_default("zen-tui", env!("CARGO_PKG_VERSION")).await {
        Ok(fresh) => Some(fresh),
        Err(e) => {
            record_dial_error(&e);
            tracing::warn!(error = %e, "prewarm: reconnect after auto-restart failed");
            None
        }
    }
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
    // Direct dial goes through the same upgrade gate as the prewarm
    // (FR-024 sink install included via `adopt`).
    open_surface().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hint_text_none_when_current() {
        assert_eq!(stale_hint_text(Some("0.0.9"), "0.0.9"), None);
    }

    #[test]
    fn hint_text_names_versions_and_remedy_when_stale() {
        let hint = stale_hint_text(Some("0.0.8"), "0.0.9").expect("stale ⇒ hint");
        assert!(hint.contains("0.0.8") && hint.contains("0.0.9"), "{hint}");
        assert!(hint.contains("zen serve restart"), "{hint}");
    }

    #[test]
    fn hint_text_marks_unknown_version() {
        let hint = stale_hint_text(None, "0.0.9").expect("unknown ⇒ hint");
        assert!(hint.contains("STALE-unknown"), "{hint}");
        assert!(hint.contains("zen serve restart"), "{hint}");
    }

    #[test]
    fn note_stale_records_once() {
        reset_stale_hint();
        note_stale_once("first hint".to_string());
        note_stale_once("second hint".to_string());
        assert_eq!(stale_hint().as_deref(), Some("first hint"));
        reset_stale_hint();
        assert_eq!(stale_hint(), None);
    }

    #[test]
    fn version_refusal_detection_matches_only_minus_32001() {
        use zen_gateway::client::SurfaceError;
        use zen_gateway::protocol::RpcErrorBody;
        let refusal = SurfaceError::Rpc(RpcErrorBody {
            code: -32001,
            name: "version-mismatch".into(),
            message: "major mismatch".into(),
            data: None,
        });
        assert!(is_version_refusal(&refusal));
        let other = SurfaceError::Rpc(RpcErrorBody {
            code: -32020,
            name: "guard-rejected".into(),
            message: "guard".into(),
            data: None,
        });
        assert!(!is_version_refusal(&other));
    }
}
