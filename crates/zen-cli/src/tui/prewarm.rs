//! Background pre-warming for the TUI session (T053).
//!
//! The first chat submission otherwise pays the full cold-start price
//! synchronously on the event-loop thread: orchestrator construction
//! (including the memvid store load) plus knowledge-DB open/migrations —
//! the ~10s Enter-to-LLM lag reported 2026-08-16 (input-display-plan.md).
//!
//! `spawn` runs both in the background at session start. The hot paths
//! (`App::init_orchestrator`, `App::auto_search_knowledge`) take the
//! pre-warmed artifacts when ready and fall back to synchronous
//! construction otherwise, so correctness never depends on the race.

use std::sync::{Arc, Mutex, OnceLock};

use zen_agents::orchestrator::AgentOrchestrator;

static ORCHESTRATOR: OnceLock<Arc<AgentOrchestrator>> = OnceLock::new();
/// Broadcasts completion of the detached prewarm build so `resolve` can await
/// it instead of racing a second `build_orchestrator`.
static ORCHESTRATOR_WATCH: OnceLock<tokio::sync::watch::Receiver<Option<Arc<AgentOrchestrator>>>> =
    OnceLock::new();
/// Cached DB client for the lifetime of the process (review-agent P2 fix:
/// opening a fresh `SqliteClient` per chat turn caused connection churn).
static DB_CLIENT: OnceLock<Mutex<Option<zen_repo::SqliteClient>>> = OnceLock::new();

/// Start background pre-warming. Cheap to call more than once (the stores
/// only accept the first value), but intended to run once at session start.
pub(crate) fn spawn(config: &'static zen_core::config::ZenConfig) {
    // Orchestrator construction is synchronous (memvid store load) — run it
    // on a DETACHED thread. A tokio `spawn_blocking` would be joined by the
    // runtime on drop, delaying Ctrl+D exit by the full build (~10s local,
    // >12s on CI — PTY wait_exit timeouts). A detached thread keeps the
    // process exit instant while still pre-warming the cache.
    let (tx, rx) = tokio::sync::watch::channel(None);
    let _ = ORCHESTRATOR_WATCH.set(rx);
    std::thread::spawn(move || {
        let built = Arc::new(super::app::build_orchestrator(config));
        let _ = ORCHESTRATOR.set(built.clone());
        let _ = tx.send(Some(built));
        tracing::debug!("prewarm: orchestrator ready");
    });

    // Pre-open the knowledge DB so migrations run here, not on first Enter.
    // Async tasks are aborted on runtime drop (no exit delay).
    tokio::spawn(async move {
        if let Ok(paths) = zen_core::paths::ZenPaths::detect() {
            let db_path = paths.data().join("state.db");
            match zen_repo::SqliteClient::open_lazy(&db_path).await {
                Ok(client) => {
                    let slot = DB_CLIENT.get_or_init(|| Mutex::new(None));
                    match slot.lock() {
                        Ok(mut guard) => *guard = Some(client),
                        Err(poisoned) => *poisoned.into_inner() = Some(client),
                    }
                    tracing::debug!("prewarm: knowledge DB ready");
                }
                Err(e) => tracing::warn!(error = %e, "prewarm: failed to open knowledge DB"),
            }
        }
    });
}

/// Take the pre-warmed orchestrator, if ready.
pub(crate) fn take_orchestrator() -> Option<Arc<AgentOrchestrator>> {
    ORCHESTRATOR.get().cloned()
}

/// Resolve the orchestrator, awaiting the in-flight prewarm build when one is
/// running. Avoids a second `build_orchestrator` and never uses
/// `spawn_blocking` (the runtime joins the blocking pool on drop, delaying
/// Ctrl+D exit by the full build).
pub(crate) async fn resolve(
    config: &'static zen_core::config::ZenConfig,
) -> Option<Arc<AgentOrchestrator>> {
    if let Some(o) = take_orchestrator() {
        return Some(o);
    }
    if let Some(rx) = ORCHESTRATOR_WATCH.get() {
        let mut rx = rx.clone();
        while rx.changed().await.is_ok() {
            if let Some(o) = rx.borrow().clone() {
                return Some(o);
            }
        }
        // Sender dropped without a value (build thread panicked) — re-check
        // the cache in case `set` happened just before drop.
        return take_orchestrator();
    }
    // Prewarm never ran (unusual) — build once on a detached thread.
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let built = Arc::new(super::app::build_orchestrator(config));
        let _ = ORCHESTRATOR.set(built.clone());
        let _ = tx.send(built);
    });
    rx.await.ok()
}

/// Run `f` with the process-wide knowledge-DB client, opening it lazily on
/// first use if prewarm has not already (must run in a tokio context, e.g.
/// `spawn_blocking`). The client is cached for the process lifetime — no
/// per-turn `SqliteClient::open_lazy` churn (review-agent P2 fix).
pub(crate) fn with_db_client<R>(f: impl FnOnce(&zen_repo::SqliteClient) -> R) -> Option<R> {
    let slot = DB_CLIENT.get_or_init(|| Mutex::new(None));
    let mut guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.is_none() {
        let db_path = zen_core::paths::ZenPaths::detect()
            .ok()?
            .data()
            .join("state.db");
        *guard = tokio::runtime::Handle::current()
            .block_on(zen_repo::SqliteClient::open_lazy(&db_path))
            .ok();
    }
    guard.as_ref().map(f)
}
