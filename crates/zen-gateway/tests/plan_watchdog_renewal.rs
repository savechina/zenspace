//! W9.5-F3 plan-turn watchdog heartbeat (Wave-6 precondition).
//!
//! PURPOSE: Pin the F3 contract (spec 006 D22 last paragraph + D24 §2/§3):
//! the hosted-turn watchdog is a shared deadline handle the guard loop
//! re-reads each tick (replacing the single `tokio::time::timeout`
//! snapshot); plan-task completions renew it per settled task (never per
//! wave), extend-only, clamped to the named ceiling
//! `PLAN_WATCHDOG_CEILING = 4 × watchdog` from turn start; the client
//! turn-ceiling derivation covers the renewal bound so
//! `client ceiling > watchdog` holds on both sides; non-plan turns keep
//! byte-identical single-deadline semantics; cancellation wins over any
//! live renewal.
//!
//! USAGE: `cargo nextest -p zen-gateway --test plan_watchdog_renewal`.
//! ENV-NOTE: process-isolated by nextest (process-per-test, T118
//! discipline); every host-level pin installs and REMOVES its env vars
//! and calls `invalidate_config_cache()`; the local probe is pinned via
//! `set_local_llm_probe_override` — no network involved. Wall-clock
//! sleeps stay ≤2s per pin (the ceiling clamp is pinned at the handle
//! level with millisecond durations, no end-to-end 4s wait).

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use zen_core::types::SessionContext;
use zen_gateway::server::guards;
use zen_gateway::server::hosting::{self, SessionHost, TurnExecutor};
use zen_plugin::tools::approval_hook::WATCHDOG_RENEWAL;

fn set_local_override(v: bool) {
    guards::set_local_llm_probe_override(Some(v));
}

fn clear_local_override() {
    guards::set_local_llm_probe_override(None);
}

/// Installs `[gateway]` env injections for one pin, reloads the config
/// cache, and restores pristine env + cache on drop.
struct CfgEnvGuard {
    keys: &'static [(&'static str, &'static str)],
}

impl CfgEnvGuard {
    fn set(keys: &'static [(&'static str, &'static str)]) -> Self {
        for (k, v) in keys {
            // SAFETY: pin-isolated under nextest; removed in drop below.
            unsafe { std::env::set_var(k, v) };
        }
        zen_core::config::invalidate_config_cache();
        Self { keys }
    }
}

impl Drop for CfgEnvGuard {
    fn drop(&mut self) {
        for (k, _) in self.keys {
            // SAFETY: paired with the installation above.
            unsafe { std::env::remove_var(k) };
        }
        zen_core::config::invalidate_config_cache();
    }
}

// ── handle-level pins (millisecond durations, no env) ──────────────────────

#[tokio::test(flavor = "current_thread")]
async fn renewal_extends_only_and_clamps_at_the_named_ceiling() {
    // watchdog = 200ms ⇒ PLAN_WATCHDOG_CEILING = 800ms from start.
    let deadline = guards::WatchdogDeadline::start(Duration::from_millis(200));
    let ceiling = deadline.current() + Duration::from_millis(600);

    // Renewals below the ceiling extend (fresh watchdog from now).
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        deadline.renew(Duration::from_millis(200)),
        "first renewal extends"
    );
    assert!(deadline.current() > ceiling - Duration::from_millis(600));

    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        deadline.renew(Duration::from_millis(200)),
        "second renewal extends"
    );

    tokio::time::sleep(Duration::from_millis(250)).await;
    // now ≈ 750ms; candidate = now + 200ms ≥ ceiling ⇒ clamped to ceiling.
    assert!(
        deadline.renew(Duration::from_millis(200)),
        "clamped renewal still moves to the ceiling"
    );
    assert_eq!(
        deadline.current(),
        ceiling,
        "deadline sits exactly at the ceiling"
    );

    // A renewal attempt past the ceiling is a no-op (never extends past it).
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        !deadline.renew(Duration::from_millis(200)),
        "past-ceiling renewal is a no-op"
    );
    assert_eq!(
        deadline.current(),
        ceiling,
        "peak deadline never exceeds start + 4 × watchdog"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn renewal_never_shortens_the_deadline() {
    let deadline = guards::WatchdogDeadline::start(Duration::from_millis(500));
    let initial = deadline.current();
    // A tiny watchdog renewal cannot pull the deadline earlier.
    assert!(
        !deadline.renew(Duration::from_millis(1)),
        "shorter candidate never shortens"
    );
    assert!(deadline.current() >= initial);
}

// ── host-level pins (1s watchdog via the legacy env override) ──────────────

/// Scripted plan simulation: sleeps in `step_ms` chunks; after each of
/// the first `renew_steps` chunks it fires the WATCHDOG_RENEWAL task-local
/// (the exact seam plan_task.rs fires after each settled task). The
/// executor runs inside hosting's renewal scope, so the task-local is
/// reachable — this is the end-to-end proof the seam is wired.
struct PlanSimExec {
    step_ms: u64,
    steps: usize,
    renew_steps: usize,
}

#[async_trait::async_trait]
impl TurnExecutor for PlanSimExec {
    async fn execute_stream(
        &self,
        _session: &mut SessionContext,
        _prompt: &str,
        callback: &mut (dyn FnMut(String) + Send),
    ) -> anyhow::Result<String> {
        for step in 0..self.steps {
            tokio::time::sleep(Duration::from_millis(self.step_ms)).await;
            if step < self.renew_steps {
                WATCHDOG_RENEWAL.with(|cell| {
                    if let Some(renew) = cell.borrow().clone() {
                        renew();
                    }
                });
            }
        }
        callback("plan deliverable".to_string());
        Ok("plan survived".to_string())
    }
}

async fn hosted_turn_outcome(
    exec: Arc<PlanSimExec>,
    envs: &'static [(&'static str, &'static str)],
) -> Result<serde_json::Value, String> {
    let env = CfgEnvGuard::set(envs);
    set_local_override(true);

    let deps = Arc::new(SessionHost::new(Some(exec)));
    deps.sessions.lock().await.insert(
        "s1".to_string(),
        SessionContext::new("s1".to_string(), String::new()),
    );
    let out = hosting::turn(
        Arc::clone(&deps),
        None,
        json!({"turnId": "tt", "sessionId": "s1", "prompt": "hi"}),
    )
    .await;

    clear_local_override();
    drop(env);
    match out {
        Ok(v) => Ok(v),
        Err(e) => Err(e.to_string()),
    }
}

/// RENEWAL PIN: a hosted turn whose plan tasks complete progressively
/// survives BEYOND the original watchdog deadline. Watchdog = 1s; the
/// simulated plan runs 3 × 600ms steps (1.8s total) with renewals after
/// the first two steps — without renewal it dies at 1s, with renewal the
/// deadline extends to ~2.2s and the turn completes.
#[tokio::test(flavor = "current_thread")]
async fn plan_renewals_survive_beyond_the_initial_watchdog_deadline() {
    let out = hosted_turn_outcome(
        Arc::new(PlanSimExec {
            step_ms: 600,
            steps: 3,
            renew_steps: 2,
        }),
        &[("ZEN_TURN_WATCHDOG_SECS", "1")],
    )
    .await
    .expect("renewed plan turn survives past the 1s watchdog");
    assert_eq!(out["response"], json!("plan survived"));
}

/// BYTE-IDENTITY PIN: a non-plan turn (no renewals) times out exactly at
/// the watchdog deadline as today — the guard loop fires at the initial
/// snapshot when the deadline never moves.
#[tokio::test(flavor = "current_thread")]
async fn non_plan_turn_still_times_out_at_the_watchdog_deadline() {
    let err = hosted_turn_outcome(
        Arc::new(PlanSimExec {
            step_ms: 600,
            steps: 3,
            renew_steps: 0,
        }),
        &[("ZEN_TURN_WATCHDOG_SECS", "1")],
    )
    .await
    .expect_err("unrenewed 1.8s executor dies at the 1s watchdog");
    assert!(err.contains("watchdog"), "{err}");
}

/// CANCEL-DRAIN PIN: cancellation wins immediately over a live renewed
/// deadline — the executor renews forever, the client cancel resolves
/// the turn well before the extended deadline.
#[tokio::test(flavor = "current_thread")]
async fn cancel_wins_over_a_renewed_deadline() {
    let env = CfgEnvGuard::set(&[("ZEN_TURN_WATCHDOG_SECS", "1")]);
    set_local_override(true);

    let deps = Arc::new(SessionHost::new(Some(Arc::new(PlanSimExec {
        step_ms: 200,
        steps: 60, // renews forever (12s of simulated plan work)
        renew_steps: 60,
    }))));
    deps.sessions.lock().await.insert(
        "s1".to_string(),
        SessionContext::new("s1".to_string(), String::new()),
    );

    let turn_deps = Arc::clone(&deps);
    let turn = tokio::spawn(async move {
        hosting::turn(
            turn_deps,
            None,
            json!({"turnId": "tt", "sessionId": "s1", "prompt": "hi"}),
        )
        .await
    });

    // Let at least one renewal land (deadline extended to ~1.2s+), then cancel.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let started = std::time::Instant::now();
    hosting::cancel(Arc::clone(&deps), json!({"turnId": "tt"}))
        .await
        .expect("cancel is advisory-success");
    let out = turn.await.expect("turn task joins");
    let elapsed = started.elapsed();

    clear_local_override();
    drop(env);

    let err = out.expect_err("cancelled turn errors");
    assert!(err.to_string().contains("cancelled"), "{err}");
    // Cancellation must resolve promptly — NOT at the renewed deadline
    // (which sits ≥1.2s past the cancel point). Generous bound still far
    // below any renewed deadline.
    assert!(
        elapsed < Duration::from_millis(900),
        "cancel took {elapsed:?}; renewal must not fight the drain"
    );
}

// ── client-ceiling invariant pins (pure derivation, no sleeps) ─────────────

use zen_gateway::client::surface::derive_turn_ceiling;

/// INVARIANT PIN: `client ceiling > watchdog` at defaults — both the
/// unreachable (900s) and local-reachable (2700s) shapes, and the
/// derivation covers the renewal bound `PLAN_WATCHDOG_CEILING + margin`.
#[test]
fn client_ceiling_covers_the_renewal_bound_at_defaults() {
    let margin = Duration::from_secs(60);

    // Cloud-only machine: watchdog 900s ⇒ ceiling 4 × 900 + 60.
    let ceiling = derive_turn_ceiling(900, 2700, false);
    assert_eq!(
        ceiling,
        guards::plan_watchdog_ceiling(Duration::from_secs(900)) + margin
    );
    assert!(
        ceiling > Duration::from_secs(900),
        "ceiling must exceed the base watchdog"
    );

    // Local-reachable machine: watchdog widens to 2700s ⇒ ceiling 4 × 2700 + 60.
    let ceiling_local = derive_turn_ceiling(900, 2700, true);
    assert_eq!(
        ceiling_local,
        guards::plan_watchdog_ceiling(Duration::from_secs(2700)) + margin
    );
    assert!(ceiling_local > Duration::from_secs(2700));
}

/// INVARIANT PIN: the derivation is exactly `multiplier × effective +
/// margin` — the same named multiplier the server ceiling uses, so the
/// two sides cannot drift apart.
#[test]
fn client_derivation_shares_the_server_ceiling_multiplier() {
    for (base, local, reachable) in [(61u64, 61u64, true), (900, 2700, true), (900, 2700, false)] {
        let effective = if reachable { base.max(local) } else { base };
        let expected = Duration::from_secs(effective) * guards::PLAN_WATCHDOG_CEILING_MULTIPLIER
            + Duration::from_secs(60);
        assert_eq!(derive_turn_ceiling(base, local, reachable), expected);
    }
}

/// WIRING PIN: `turn_timeout()` (the value actually handed to
/// `session/turn` request_timeout) flows through the renewal-aware
/// derivation — config keys + probe seam in, `4 × watchdog + margin` out.
#[tokio::test(flavor = "current_thread")]
async fn turn_timeout_uses_the_renewal_aware_derivation() {
    let env = CfgEnvGuard::set(&[
        ("ZEN_GATEWAY_TURN_WATCHDOG_SECS", "61"),
        ("ZEN_GATEWAY_LOCAL_TURN_WATCHDOG_SECS", "61"),
    ]);
    set_local_override(true);
    // Same value on both keys: reachable == unreachable == 61s watchdog
    // ⇒ ceiling 4 × 61 + 60 = 304s.
    let ceiling = zen_gateway::client::surface::turn_timeout();
    assert_eq!(ceiling, Duration::from_secs(304));
    assert!(
        ceiling > guards::watchdog_deadline(true),
        "client ceiling > watchdog under renewal"
    );
    drop(env);
    clear_local_override();
}
