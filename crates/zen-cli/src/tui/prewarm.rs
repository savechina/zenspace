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
/// Cached DB client for the lifetime of the process (review-agent P2 fix:
/// opening a fresh `SqliteClient` per chat turn caused connection churn).
static DB_CLIENT: OnceLock<Mutex<Option<zen_repo::SqliteClient>>> = OnceLock::new();

/// Start background pre-warming. Cheap to call more than once (the stores
/// only accept the first value), but intended to run once at session start.
pub(crate) fn spawn(config: &'static zen_core::config::ZenConfig) {
    tokio::spawn(async move {
        // Orchestrator construction is synchronous (memvid store load) — keep
        // it off the async worker thread.
        let built = tokio::task::spawn_blocking(move || super::app::build_orchestrator(config))
            .await
            .ok();
        if let Some(orch) = built {
            let _ = ORCHESTRATOR.set(Arc::new(orch));
            tracing::debug!("prewarm: orchestrator ready");
        }
        // Pre-open the knowledge DB so migrations run here, not on first Enter.
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
