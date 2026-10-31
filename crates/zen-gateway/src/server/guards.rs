//! Agent-loop guard infrastructure (E7) — process-level resilience ring
//! layered on zen-agents' executor retry taxonomy (design §6).
//!
//! PURPOSE: Contains runaway agent loops at the daemon boundary. Four
//! guards with data-model E7 defaults: [`TurnWatchdog`] (per-turn
//! deadline, 900s default, scaled wider when a local LLM is reachable —
//! enforced in `hosting::turn` via [`watchdog_deadline`]),
//! [`CircuitBreaker`] (consecutive failed turns
//! per session → cool-down, 5→60s), [`DoomLoop`] (turn-rate cap per
//! session window, 20/10min), and the StaleClientGC heartbeat helper.
//!
//! USAGE: `SessionHost` carries an [`Arc<Guards>`]; the turn handler
//! calls [`Guards::check_submit`] before creating a record and
//! [`Guards::record_success`]/[`record_failure`] after execution. Every
//! rejection returns -32020 `guard-rejected{guard, reason}` and appends
//! an audit line when a sink is configured.
//!
//! EXPECTED: rejections are cheap synchronous checks; breaker recovery
//! is time-based (no manual reset); doom-loop slides its window.
//!
//! ERRORS: all rejections use [`RpcError::guard_rejected`]; audit
//! failures never propagate.

use std::collections::HashMap;
use std::sync::{Mutex as StdMutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::protocol::RpcError;

/// Poison-tolerant `Mutex::lock` for request-path code (CSO T264): a
/// prior panic while holding the lock must not cascade-panic every
/// subsequent request. Recovers the guard via [`PoisonError::into_inner`]
/// (the data-intact contract — the critical section left consistent
/// state or panicked mid-update, and callers already tolerate partial
/// writes) and logs one `zen_gateway_poison` warning per recovery. The
/// non-poisoned fast path is a plain `match` on `Result` — no
/// allocation, no extra locking, no widening of lock-hold windows.
///
/// `what` names the lock site (reuses the historical `.expect(...)`
/// message strings) so poison recoveries are attributable in logs.
///
/// # Examples
/// ```
/// use std::sync::Mutex;
/// use zen_gateway::server::guards::lock_ok;
///
/// let m = Mutex::new(41);
/// *lock_ok(&m, "demo lock") += 1; // fast path, byte-equal to .lock().unwrap()
/// assert_eq!(*m.lock().unwrap(), 42);
/// ```
pub fn lock_ok<'a, T>(mutex: &'a StdMutex<T>, what: &'static str) -> MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!(
                target: "zen_gateway_poison",
                lock = what,
                "recovered poisoned mutex (prior panic while held)"
            );
            poisoned.into_inner()
        }
    }
}

/// Per-turn deadline default (data-model E7 TurnWatchdog). Must exceed
/// the client's first-token budget (zen-agents
/// `ZEN_STREAM_FIRST_TOKEN_TIMEOUT_SECS`, default 600s) so cold local
/// models finish their load+prefill before the watchdog fires.
pub const WATCHDOG_TIMEOUT: Duration = Duration::from_secs(900);
/// Consecutive failures before the breaker opens per session.
pub const BREAKER_THRESHOLD: u32 = 5;
/// Breaker cool-down once open.
pub const BREAKER_COOLDOWN: Duration = Duration::from_secs(60);
/// Max turns per session within [`DOOM_WINDOW`].
pub const DOOM_MAX_TURNS: usize = 20;
/// Doom-loop sliding window.
pub const DOOM_WINDOW: Duration = Duration::from_secs(600);
/// Client heartbeat cadence for stale-connection reaping.
pub const GC_HEARTBEAT: Duration = Duration::from_secs(30);

fn guard_rejected(guard: &'static str, reason: &str) -> RpcError {
    RpcError::guard_rejected(guard, reason)
}

/// Effective watchdog deadline: [`WATCHDOG_TIMEOUT`] unless
/// `ZEN_TURN_WATCHDOG_SECS` overrides it (parsed once per process).
pub fn watchdog_timeout() -> Duration {
    static OVERRIDE: StdMutex<Option<Duration>> = StdMutex::new(None);
    *lock_ok(&OVERRIDE, "watchdog override").get_or_insert_with(|| {
        std::env::var("ZEN_TURN_WATCHDOG_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .map(Duration::from_secs)
            .unwrap_or(WATCHDOG_TIMEOUT)
    })
}

/// Base watchdog seconds: config `[gateway].turn_watchdog_secs` (5-layer),
/// falling back to the default when config cannot be loaded. Legacy
/// `ZEN_TURN_WATCHDOG_SECS` precedence is applied by the callers, not here.
pub(crate) fn base_watchdog_secs() -> u64 {
    zen_core::config::load_config()
        .map(|c| c.gateway.turn_watchdog_secs_or_default())
        .unwrap_or(zen_core::config::GATEWAY_TURN_WATCHDOG_DEFAULT_SECS)
}

/// Local-LLM watchdog seconds from
/// `[gateway].local_turn_watchdog_secs` (same load fallback).
pub(crate) fn local_watchdog_secs() -> u64 {
    zen_core::config::load_config()
        .map(|c| c.gateway.local_turn_watchdog_secs_or_default())
        .unwrap_or(zen_core::config::GATEWAY_LOCAL_TURN_WATCHDOG_DEFAULT_SECS)
}

/// Effective per-turn watchdog deadline.
///
/// Precedence: the legacy `ZEN_TURN_WATCHDOG_SECS` env (explicit full
/// override, parsed once per process) beats everything; otherwise the base
/// budget (`[gateway].turn_watchdog_secs`, default 900s) is widened to
/// `max(base, local)` when `local_reachable` — a slow local model must
/// finish multi-round tool loops instead of dying at the base deadline.
/// The local budget never shortens below the base.
pub fn watchdog_deadline(local_reachable: bool) -> Duration {
    if let Some(t) = repo_watchdog_env_override() {
        return t;
    }
    let base = Duration::from_secs(base_watchdog_secs());
    if local_reachable {
        base.max(Duration::from_secs(local_watchdog_secs()))
    } else {
        base
    }
}

/// Process-static parse of the legacy explicit watchdog
/// (`ZEN_TURN_WATCHDOG_SECS`); `None` when unset/unparsable.
fn repo_watchdog_env_override() -> Option<Duration> {
    static PARSED: StdMutex<Option<Option<Duration>>> = StdMutex::new(None);
    *lock_ok(&PARSED, "watchdog env override").get_or_insert_with(|| {
        std::env::var("ZEN_TURN_WATCHDOG_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .map(Duration::from_secs)
    })
}

/// TTL for the cached local-LLM reachability probe (GraphCoreCache
/// precedent): an ollama started mid-session is picked up within ≤5min.
const LOCAL_PROBE_TTL: std::time::Duration = std::time::Duration::from_secs(300);

/// Shared answer holder: the override seam and the cached reader MUST be
/// the same static or the seam writes into a slot nobody reads.
static LOCAL_PROBE_OVERRIDE: StdMutex<Option<bool>> = StdMutex::new(None);

/// Cached local-LLM reachability for turn-budget scaling (and the client
/// turn ceiling): a `zen_provider` router probe over
/// `DefaultRouter::from_agentic` running the ollama `health_check`
/// (dedicated-threaded, no-proxy for loopback); process-lifetime cache
/// with [`LOCAL_PROBE_TTL`]; probe failure is fail-safe (unreachable ⇒
/// base deadline).
///
/// `#[doc(hidden)]` seam: `set_local_llm_probe_override` pins locality
/// deterministically in tests without a network. The seam and this reader
/// share one static — two module statics of the same name would be separate
/// cells and the pin would write into a slot nobody reads (caught by the
/// pin at review time).
#[must_use]
pub fn local_llm_reachable_cached() -> bool {
    if let Some(v) = *lock_ok(&LOCAL_PROBE_OVERRIDE, "local probe override") {
        return v;
    }
    static CACHE: StdMutex<Option<(std::time::Instant, bool)>> = StdMutex::new(None);
    let mut cache = lock_ok(&CACHE, "local probe cache");
    if let Some((at, reachable)) = *cache
        && at.elapsed() < LOCAL_PROBE_TTL
    {
        return reachable;
    }
    let reachable = probe_local_llm_reachable();
    *cache = Some((std::time::Instant::now(), reachable));
    reachable
}

/// One fresh ollama reachability probe. Config load failure degrades to
/// defaults (no ollama ⇒ unreachable ⇒ byte-identical base).
#[must_use]
pub fn probe_local_llm_reachable() -> bool {
    match zen_core::config::load_config() {
        Ok(cfg) => probe_router(cfg),
        Err(e) => {
            tracing::warn!(error = %e, "config load failed; probing local LLM with defaults");
            probe_router(&zen_core::config::ZenConfig::default())
        }
    }
}

fn probe_router(cfg: &zen_core::config::ZenConfig) -> bool {
    let router = zen_provider::DefaultRouter::from_agentic(cfg);
    zen_provider::is_local_llm_available(&router)
}

/// Test seam: pin the cached reachability answer (docs in
/// [`local_llm_reachable_cached`]). `None` restores live probing.
#[doc(hidden)]
pub fn set_local_llm_probe_override(v: Option<bool>) {
    *lock_ok(&LOCAL_PROBE_OVERRIDE, "local probe override") = v;
}

/// Watchdog expiry surfaced as a catalog rejection to the turn caller.
pub fn watchdog_rejected() -> RpcError {
    guard_rejected("watchdog", "turn exceeded watchdog deadline")
}

/// Guard aggregate (E7): per-session breaker + doom-loop state.
#[derive(Default)]
pub struct Guards {
    breakers: StdMutex<HashMap<String, BreakerState>>,
    doom: StdMutex<HashMap<String, Vec<Instant>>>,
}

#[derive(Default, Clone)]
struct BreakerState {
    consecutive_failures: u32,
    open_until: Option<Instant>,
}

impl Guards {
    /// Admits one submitted turn or rejects it with -32020. Runs before
    /// any side effect; also records the submission attempt in the
    /// doom-loop window.
    pub fn check_submit(&self, session_id: &str) -> Result<(), RpcError> {
        self.check_breaker(session_id)?;
        self.record_doom_turn(session_id)?;
        Ok(())
    }

    fn check_breaker(&self, session_id: &str) -> Result<(), RpcError> {
        let mut breakers = lock_ok(&self.breakers, "breaker lock");
        let state = breakers.entry(session_id.to_string()).or_default();
        if let Some(until) = state.open_until {
            if Instant::now() < until {
                let remaining = until.duration_since(Instant::now()).as_secs().max(1);
                return Err(guard_rejected(
                    "circuit-breaker",
                    &format!(
                        "session cooling down after {BREAKER_THRESHOLD} consecutive failures; retry in ~{remaining}s"
                    ),
                ));
            }
            // Cool-down elapsed: half-open (next outcome closes/opens).
            state.open_until = None;
        }
        Ok(())
    }

    fn record_doom_turn(&self, session_id: &str) -> Result<(), RpcError> {
        let now = Instant::now();
        let mut doom = lock_ok(&self.doom, "doom lock");
        let window = doom.entry(session_id.to_string()).or_default();
        window.retain(|t| now.duration_since(*t) < DOOM_WINDOW);
        if window.len() >= DOOM_MAX_TURNS {
            return Err(guard_rejected(
                "doom-loop",
                &format!(
                    "session exceeded {DOOM_MAX_TURNS} turns in {} minutes",
                    DOOM_WINDOW.as_secs() / 60
                ),
            ));
        }
        window.push(now);
        Ok(())
    }

    /// Records a completed turn: resets the failure streak and prunes
    /// the doom window entry set.
    pub fn record_success(&self, session_id: &str) {
        let mut breakers = lock_ok(&self.breakers, "breaker lock");
        if let Some(state) = breakers.get_mut(session_id) {
            state.consecutive_failures = 0;
            state.open_until = None;
        }
    }

    /// Records a failed turn; opens the breaker at threshold.
    pub fn record_failure(&self, session_id: &str) {
        let mut breakers = lock_ok(&self.breakers, "breaker lock");
        let state = breakers.entry(session_id.to_string()).or_default();
        state.consecutive_failures += 1;
        if state.consecutive_failures >= BREAKER_THRESHOLD {
            state.open_until = Some(Instant::now() + BREAKER_COOLDOWN);
            state.consecutive_failures = 0;
        }
    }
}

/// Appends one guard-rejection audit line when a sink is wired.
pub fn audit_rejection(sink: Option<&std::path::PathBuf>, guard: &str, reason: &str) {
    let Some(path) = sink else { return };
    let line = format!(
        "{}\n",
        json!({
            "ts": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis().to_string())
                .unwrap_or_else(|_| "0".to_string()),
            "kind": "gateway.guard_rejected",
            "guard": guard,
            "reason": reason,
        })
    );
    let path = path.clone();
    tokio::task::spawn_blocking(move || {
        use std::io::Write;
        let write = || -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?
                .write_all(line.as_bytes())
        };
        if let Err(e) = write() {
            tracing::warn!(path = %path.display(), "guard audit write failed: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breaker_opens_after_threshold_and_recovers_by_time() {
        let guards = Guards::default();
        for _ in 0..(BREAKER_THRESHOLD - 1) {
            assert!(guards.check_submit("s").is_ok());
            guards.record_failure("s");
        }
        // Fourth failure stays under threshold.
        assert!(guards.check_submit("s").is_ok());
        guards.record_failure("s");

        // Fifth consecutive failure opens the breaker.
        assert!(guards.check_submit("s").is_err());

        // Success after cool-down would reset; simulate by direct state.
        {
            let mut b = guards.breakers.lock().unwrap();
            let st = b.get_mut("s").unwrap();
            st.open_until = Some(Instant::now() - Duration::from_secs(1));
        }
        assert!(guards.check_submit("s").is_ok(), "half-open admits");
        guards.record_success("s");
        assert!(guards.check_submit("s").is_ok());
    }

    #[test]
    fn doom_loop_caps_rate_per_window() {
        let guards = Guards::default();
        for _ in 0..DOOM_MAX_TURNS {
            assert!(guards.check_submit("s").is_ok(), "within cap must pass");
        }
        let err = guards.check_submit("s").unwrap_err();
        assert_eq!((err.code, err.name), (-32020, "guard-rejected"));
        assert_eq!(err.data.unwrap()["guard"], "doom-loop");

        // Window slide: age out everything, capacity restored.
        {
            let mut doom = guards.doom.lock().unwrap();
            doom.values_mut()
                .for_each(|w| *w = vec![Instant::now() - DOOM_WINDOW]);
        }
        assert!(guards.check_submit("s").is_ok());
    }

    #[test]
    fn distinct_sessions_do_not_share_guard_state() {
        let guards = Guards::default();
        for _ in 0..DOOM_MAX_TURNS {
            assert!(guards.check_submit("a").is_ok());
        }
        assert!(
            guards.check_submit("b").is_ok(),
            "other sessions unaffected"
        );
    }
}
