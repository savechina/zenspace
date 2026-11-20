//! W12.2 (eng-review N1): staleness-resolver invariant pins.
//!
//! PURPOSE: `resolve_stale_claim_secs()` must be ≥ 4× the resolved turn
//! watchdog — the `PLAN_WATCHDOG_CEILING` renewal ceiling (zen-gateway
//! guards.rs) — at defaults AND under the `ZEN_TURN_WATCHDOG_SECS` env
//! override. A smaller window stale-sweeps live plans mid-task and lets a
//! second resume steal the claim (true double-spend).
//!
//! Mirrors the env-mutation resolver pins in `zen-core/src/config.rs`
//! (SAFETY: nextest process-per-test isolates env mutation).

use zen_core::config::{GATEWAY_TURN_WATCHDOG_DEFAULT_SECS, resolve_stale_claim_secs};

/// W12.2/N1: at defaults (no env override, no config) the resolver returns
/// exactly 4× the default watchdog — satisfying `stale_secs ≥
/// PLAN_WATCHDOG_CEILING` (= 4× watchdog).
#[test]
fn stale_claim_secs_is_four_times_the_default_watchdog() {
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: nextest process-per-test isolates env mutation.
    unsafe {
        std::env::set_var("ZEN_HOME", dir.path());
        std::env::remove_var("ZEN_TURN_WATCHDOG_SECS");
    }
    let stale = resolve_stale_claim_secs();
    assert_eq!(
        stale,
        4 * GATEWAY_TURN_WATCHDOG_DEFAULT_SECS as i64,
        "default staleness = 4× default watchdog (900s ⇒ 3600s)"
    );
    assert!(
        stale >= 4 * GATEWAY_TURN_WATCHDOG_DEFAULT_SECS as i64,
        "invariant: stale_secs ≥ PLAN_WATCHDOG_CEILING (= 4× watchdog)"
    );
    // SAFETY: nextest process-per-test isolates env mutation.
    unsafe { std::env::remove_var("ZEN_HOME") };
}

/// W12.2/N1: under the env override the resolver tracks 4× the override —
/// the invariant holds on the widened AND the shrunk watchdog.
#[test]
fn stale_claim_secs_tracks_the_env_override_at_four_times() {
    // SAFETY: nextest process-per-test isolates env mutation.
    unsafe { std::env::set_var("ZEN_TURN_WATCHDOG_SECS", "1200") };
    assert_eq!(resolve_stale_claim_secs(), 4800, "4× the widened watchdog");
    assert!(
        resolve_stale_claim_secs() >= 4 * 1200,
        "invariant holds under the widened override"
    );
    // SAFETY: nextest process-per-test isolates env mutation.
    unsafe { std::env::set_var("ZEN_TURN_WATCHDOG_SECS", "60") };
    assert_eq!(resolve_stale_claim_secs(), 240, "4× the shrunk watchdog");
    assert!(
        resolve_stale_claim_secs() >= 4 * 60,
        "invariant holds under the shrunk override"
    );
    // SAFETY: nextest process-per-test isolates env mutation.
    unsafe { std::env::remove_var("ZEN_TURN_WATCHDOG_SECS") };
}
