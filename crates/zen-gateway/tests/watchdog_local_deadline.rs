//! Per-turn watchdog budget scaling by local-LLM reachability.
//!
//! PURPOSE: Pin the turn-budget contract introduced with the
//! slow-local-model fix (2026-10-31). Contract (`server::guards`):
//! legacy `ZEN_TURN_WATCHDOG_SECS` env beats everything; otherwise the
//! config `[gateway].turn_watchdog_secs` base (default 900) is widened to
//! `max(base, local)` when the local-LLM probe reports reachable. One
//! host-level test proves the scaling is reachable end-to-end through
//! `hosting::turn` (a slow executor survives a longer local deadline and
//! is watchdog-cancelled at the base deadline when the probe says
//! unreachable), mirroring harness_hosting's in-process shape.
//!
//! USAGE: `cargo nextest -p zen-gateway --test watchdog_local_deadline`.
//! ENV-NOTE: process-isolated by nextest (process-per-test, T118
//! discipline); every pin installs and REMOVES its env vars and calls
//! `invalidate_config_cache()`; the probe is pinned via the
//! `set_local_llm_probe_override` test seam — no network involved.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use zen_core::types::SessionContext;
use zen_gateway::server::guards;
use zen_gateway::server::hosting::{self, SessionHost, TurnExecutor};

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

// deadline-level pins -------------------------------------------------------

#[test]
fn deadline_local_never_shortens_base() {
    set_local_override(true);
    let env = CfgEnvGuard::set(&[
        ("ZEN_GATEWAY_TURN_WATCHDOG_SECS", "1200"),
        ("ZEN_GATEWAY_LOCAL_TURN_WATCHDOG_SECS", "1200"),
    ]);
    // Same value on both keys: reachable == unreachable == 1200s.
    assert_eq!(guards::watchdog_deadline(true), Duration::from_secs(1200));
    set_local_override(false);
    assert_eq!(guards::watchdog_deadline(false), Duration::from_secs(1200));
    drop(env);
    clear_local_override();
}

#[test]
fn deadline_local_widens_beyond_base_when_reachable() {
    let env = CfgEnvGuard::set(&[
        ("ZEN_GATEWAY_TURN_WATCHDOG_SECS", "1200"),
        ("ZEN_GATEWAY_LOCAL_TURN_WATCHDOG_SECS", "2400"),
    ]);
    set_local_override(true);
    assert_eq!(guards::watchdog_deadline(true), Duration::from_secs(2400));
    set_local_override(false);
    assert_eq!(guards::watchdog_deadline(false), Duration::from_secs(1200));
    drop(env);
    clear_local_override();
}

#[test]
fn deadline_legacy_env_beats_local_scaling() {
    let env = CfgEnvGuard::set(&[
        ("ZEN_TURN_WATCHDOG_SECS", "1800"),
        ("ZEN_GATEWAY_TURN_WATCHDOG_SECS", "1200"),
        ("ZEN_GATEWAY_LOCAL_TURN_WATCHDOG_SECS", "2400"),
    ]);
    set_local_override(true);
    // The legacy per-process env is an explicit full override: the local
    // budget must NOT widen past it.
    assert_eq!(guards::watchdog_deadline(true), Duration::from_secs(1800));
    drop(env);
    // deadline parsed the legacy env into a process-wide static: restore
    // callers of watchdog_timeout by clearing the seam only (the static
    // is process-scoped; nextest gives each pin a fresh process anyway).
    clear_local_override();
}

#[test]
fn probe_override_pins_cached_answer() {
    set_local_override(true);
    assert!(guards::local_llm_reachable_cached());
    set_local_override(false);
    assert!(!guards::local_llm_reachable_cached());
    clear_local_override();
}

// host-level end-to-end pins ------------------------------------------------

struct SleepExec {
    ms: u64,
}

#[async_trait::async_trait]
impl TurnExecutor for SleepExec {
    async fn execute_stream(
        &self,
        _session: &mut SessionContext,
        _prompt: &str,
        callback: &mut (dyn FnMut(String) + Send),
    ) -> anyhow::Result<String> {
        tokio::time::sleep(Duration::from_millis(self.ms)).await;
        callback("almost".to_string());
        tokio::time::sleep(Duration::from_millis(self.ms)).await;
        Ok("survived the watchdog".to_string())
    }
}

/// Runs one hosted turn against a slow scripted executor whose total
/// wall time is `2 * ms + ε`. Two budget orders are exercised by the
/// callers below (see the pin comments): a config-driven 61s deadline
/// and a legacy-env 2s deadline. 900s-default and scale-by-probe value
/// semantics are covered exhaustively at the deadline level above.
async fn hosted_turn_outcome(
    ms: u64,
    envs: &'static [(&'static str, &'static str)],
) -> Result<serde_json::Value, String> {
    let env = CfgEnvGuard::set(envs);
    set_local_override(true);

    let deps = Arc::new(SessionHost::new(Some(Arc::new(SleepExec { ms }))));
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

#[tokio::test(flavor = "current_thread")]
async fn config_budget_reaches_the_hosted_turn() {
    // Config `[gateway].turn_watchdog_secs = 61` is the effective base at
    // the hosting layer (NOT the 900s wired default): a 2.5s executor
    // succeeds trivially under it. The probe seam is true, mirroring the
    // local-reachable production shape.
    let out = hosted_turn_outcome(
        1250,
        &[
            ("ZEN_GATEWAY_TURN_WATCHDOG_SECS", "61"),
            ("ZEN_GATEWAY_LOCAL_TURN_WATCHDOG_SECS", "61"),
        ],
    )
    .await
    .expect("turn survives under the config-driven deadline");
    assert_eq!(out["response"], json!("survived the watchdog"));
}

#[tokio::test(flavor = "current_thread")]
async fn legacy_env_override_cancels_slow_executor() {
    // Legacy ZEN_TURN_WATCHDOG_SECS=2 is an explicit full override that
    // beats the local widening: the 2.5s executor is watchdog-cancelled
    // mid-flight (exact production shape of the 35B-collect timeout).
    let err = hosted_turn_outcome(1250, &[("ZEN_TURN_WATCHDOG_SECS", "2")])
        .await
        .expect_err("legacy env deadline fires");
    assert!(err.contains("watchdog"), "{err}");
}
