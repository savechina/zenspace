//! Agent-loop guard infrastructure (E7) — process-level resilience ring
//! layered on zen-agents' executor retry taxonomy (design §6).
//!
//! PURPOSE: Contains runaway agent loops at the daemon boundary. Four
//! guards with data-model E7 defaults: [`TurnWatchdog`] (per-turn
//! deadline, 900s default — enforced in `hosting::turn` via
//! [`watchdog_timeout`]), [`CircuitBreaker`] (consecutive failed turns
//! per session → cool-down, 5→60s), [`DoomLoop`] (turn-rate cap per
//! session window, 20/10min), and the StaleClientGC heartbeat helper.
//!
//! USAGE: `HostingDeps` carries an [`Arc<Guards>`]; the turn handler
//! calls [`Guards::check_submit`] before creating a record and
//! [`Guards::record_success`]/[`record_failure`] after execution. Every
//! rejection returns -32020 `guardRejected{guard, reason}` and appends
//! an audit line when a sink is configured.
//!
//! EXPECTED: rejections are cheap synchronous checks; breaker recovery
//! is time-based (no manual reset); doom-loop slides its window.
//!
//! ERRORS: all rejections use [`RpcError::guard_rejected`]; audit
//! failures never propagate.

use std::collections::HashMap;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::protocol::RpcError;

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
    *OVERRIDE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get_or_insert_with(|| {
            std::env::var("ZEN_TURN_WATCHDOG_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .map(Duration::from_secs)
                .unwrap_or(WATCHDOG_TIMEOUT)
        })
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
        let mut breakers = self.breakers.lock().expect("breaker lock");
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
        let mut doom = self.doom.lock().expect("doom lock");
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
        let mut breakers = self.breakers.lock().expect("breaker lock");
        if let Some(state) = breakers.get_mut(session_id) {
            state.consecutive_failures = 0;
            state.open_until = None;
        }
    }

    /// Records a failed turn; opens the breaker at threshold.
    pub fn record_failure(&self, session_id: &str) {
        let mut breakers = self.breakers.lock().expect("breaker lock");
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
            "kind": "gateway.guardRejected",
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
