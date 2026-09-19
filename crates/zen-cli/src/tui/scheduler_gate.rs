//! In-app scheduler gate: decides whether the TUI hosts the
//! learning-core scheduler for its lifetime (in-app background
//! learning, daemon-optional) and spawns it when safe.
//!
//! Coexistence rule: spawn unless `[cron] tui_scheduler` is off, a live
//! daemon reports `scheduler: true` via health/status (an explicit
//! `zen serve start` already hosts the full scheduler), or the
//! cross-process scheduler lease is held (another TUI got there first —
//! probe-then-spawn alone is TOCTOU). Implicit daemons (TUI/chat-spawned,
//! `ZEN_SERVE_NO_SCHEDULER=1`) report false and never suppress the
//! in-app spawn; an unreachable/absent daemon means the TUI is the only
//! learner and must spawn.

use std::time::Duration;

use zen_core::config::ZenConfig;
use zen_core::paths::ZenPaths;

/// Probe budget: bounded wait for the prewarmed surface + one health
/// RPC. Beyond this, treat the daemon as absent (fail open — the
/// scheduler lease + worker markers make double-fire safe, but learning
/// delay is real).
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Poll cadence for the daemon-arrival watcher (T154): an explicit
/// `zen serve start` daemon waiting on the lease is detected within one
/// interval, then the in-app scheduler shuts down and releases the lease.
const YIELD_POLL_INTERVAL: Duration = Duration::from_secs(15);

/// Pure coexistence decision (unit-testable).
///
/// - `config_enabled == false` → never spawn (kill switch)
/// - `daemon_scheduler == Some(true)` → never spawn (daemon hosts it)
/// - otherwise (no daemon / implicit daemon / unknown) → spawn
fn should_spawn_in_app_scheduler(config_enabled: bool, daemon_scheduler: Option<bool>) -> bool {
    config_enabled && daemon_scheduler != Some(true)
}

/// Pure yield decision (unit-testable): the TUI yields its in-app
/// scheduler when an explicit daemon intends to host one
/// (`scheduler_pending: true`) or already hosts one (`scheduler: true`).
/// Only an explicit daemon (`scheduler_hosted`) sets `scheduler_pending`,
/// so implicit daemons never cause a yield.
fn should_yield(health: &serde_json::Value) -> bool {
    health
        .get("scheduler_pending")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
        || health
            .get("scheduler")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
}

/// Read `scheduler` from a health/status snapshot. Absent field
/// (pre-1.2 daemon) → `Some(false)` — an old daemon without the field
/// runs no scheduler, so spawning in-app is correct.
fn scheduler_flag(snapshot: &serde_json::Value) -> Option<bool> {
    snapshot
        .get("scheduler")
        .and_then(serde_json::Value::as_bool)
        .or(Some(false))
}

/// Watches for an explicit daemon that wants the scheduler lease and
/// signals the in-app scheduler to shut down when one appears (T154).
///
/// Polls `health/status` every [`YIELD_POLL_INTERVAL`]; on
/// `scheduler_pending: true` (daemon waiting for the lease) or
/// `scheduler: true` (daemon already hosting), flips the watch and
/// returns. The caller then awaits the scheduler task and drops the
/// lease so the daemon's Full profile can take over.
fn spawn_yield_watcher(
    shutdown_tx: tokio::sync::watch::Sender<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(YIELD_POLL_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick.tick().await; // consume the immediate first tick
        loop {
            tick.tick().await;
            let Some(surface) = super::prewarm::resolve_client().await else {
                continue;
            };
            match surface.health_status().await {
                Ok(snapshot) if should_yield(&snapshot) => {
                    tracing::info!(
                        scheduler_pending = snapshot
                            .get("scheduler_pending")
                            .and_then(serde_json::Value::as_bool),
                        scheduler = snapshot
                            .get("scheduler")
                            .and_then(serde_json::Value::as_bool),
                        "tui scheduler: explicit daemon wants the scheduler lease, handing off"
                    );
                    shutdown_tx.send_replace(true);
                    return;
                }
                Ok(_) => {}
                Err(e) => tracing::debug!(error = %e, "tui scheduler: yield probe failed"),
            }
        }
    })
}

/// Entry point for both TUI paths (inline + fullscreen). Kill switch is
/// checked synchronously; the probe + decision + spawn run in a
/// detached task so the event loop never blocks on daemon I/O.
pub(crate) fn spawn(config: &ZenConfig) {
    if !config.cron.tui_scheduler_or_default() {
        tracing::info!("tui scheduler: disabled by [cron] tui_scheduler");
        return;
    }
    let cron = config.cron.clone();
    tokio::spawn(async move {
        let daemon_scheduler = match super::prewarm::resolve_client().await {
            Some(surface) => {
                let probe = tokio::time::timeout(PROBE_TIMEOUT, surface.health_status()).await;
                match probe {
                    Ok(Ok(snapshot)) => scheduler_flag(&snapshot),
                    Ok(Err(e)) => {
                        tracing::debug!(error = %e, "tui scheduler: health probe failed");
                        None
                    }
                    Err(_) => {
                        tracing::debug!("tui scheduler: health probe timed out");
                        None
                    }
                }
            }
            None => None,
        };
        if !should_spawn_in_app_scheduler(true, daemon_scheduler) {
            tracing::info!(
                ?daemon_scheduler,
                "tui scheduler: daemon hosts a scheduler, skipping in-app spawn"
            );
            return;
        }
        // Cross-process mutual exclusion: another TUI (or a daemon that
        // has not yet surfaced `scheduler: true`) may already hold the
        // lease. Fail closed here — learning resumes in the next TUI
        // session once the current holder exits.
        let lease = match ZenPaths::detect() {
            Ok(paths) => match zen_agents::scheduler::SchedulerLease::try_acquire(&paths) {
                Ok(lease) => lease,
                Err(zen_agents::scheduler::LeaseError::Contention) => {
                    tracing::info!(
                        "tui scheduler: lease held by another scheduler host, skipping in-app spawn"
                    );
                    return;
                }
                Err(zen_agents::scheduler::LeaseError::OpenFailed(e)) => {
                    // T164: a real filesystem problem, not coexistence —
                    // warn and skip rather than pretend another host owns it.
                    tracing::warn!(
                        error = %e,
                        "tui scheduler: lease lock file could not be opened, skipping in-app spawn"
                    );
                    return;
                }
            },
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "tui scheduler: cannot resolve paths, skipping in-app spawn"
                );
                return;
            }
        };
        tracing::info!(
            ?daemon_scheduler,
            "tui scheduler: spawning learning-core scheduler in-process"
        );
        let scheduler = zen_agents::scheduler::create_configured_scheduler_with(
            &cron,
            zen_agents::scheduler::SchedulerProfile::InApp,
        );
        // T154: an explicit `zen serve start` daemon appearing mid-session
        // signals `scheduler_pending` (or `scheduler` once it acquires the
        // lease); the watcher flips the watch, we stop the in-app scheduler
        // BEFORE dropping the lease so the daemon's Full profile takes over
        // without double-firing cron workers.
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let scheduler_task = tokio::spawn(scheduler.run_with_shutdown(shutdown_rx));
        let watcher = spawn_yield_watcher(shutdown_tx);
        let _ = scheduler_task.await;
        watcher.abort();
        drop(lease);
        tracing::info!("tui scheduler: in-app scheduler stopped, lease released");
    });
}

#[cfg(test)]
mod tests {
    use super::should_spawn_in_app_scheduler as decide;

    #[test]
    fn kill_switch_wins() {
        assert!(!decide(false, None));
        assert!(!decide(false, Some(true)));
        assert!(!decide(false, Some(false)));
    }

    #[test]
    fn daemon_hosted_scheduler_blocks_spawn() {
        assert!(!decide(true, Some(true)));
    }

    #[test]
    fn absent_or_implicit_daemon_spawns() {
        assert!(decide(true, None));
        assert!(decide(true, Some(false)));
    }

    #[test]
    fn missing_scheduler_field_means_false() {
        assert_eq!(super::scheduler_flag(&serde_json::json!({})), Some(false));
        assert_eq!(
            super::scheduler_flag(&serde_json::json!({"scheduler": true})),
            Some(true)
        );
        assert_eq!(
            super::scheduler_flag(&serde_json::json!({"scheduler": false})),
            Some(false)
        );
    }

    #[test]
    fn yield_on_pending_or_hosted() {
        assert!(super::should_yield(
            &serde_json::json!({"scheduler_pending": true})
        ));
        assert!(super::should_yield(&serde_json::json!({"scheduler": true})));
        assert!(super::should_yield(&serde_json::json!({
            "scheduler_pending": true,
            "scheduler": false
        })));
        assert!(super::should_yield(&serde_json::json!({
            "scheduler_pending": true,
            "scheduler": true
        })));
    }

    #[test]
    fn no_yield_for_implicit_or_absent_daemon() {
        assert!(!super::should_yield(&serde_json::json!({})));
        assert!(!super::should_yield(
            &serde_json::json!({"scheduler": false})
        ));
        assert!(!super::should_yield(&serde_json::json!({
            "scheduler_pending": false,
            "scheduler": false
        })));
        // Non-bool junk must not panic or yield.
        assert!(!super::should_yield(
            &serde_json::json!({"scheduler_pending": "yes"})
        ));
    }
}
