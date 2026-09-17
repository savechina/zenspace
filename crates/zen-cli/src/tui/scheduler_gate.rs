//! In-app scheduler gate: decides whether the TUI hosts the
//! learning-core scheduler for its lifetime (in-app background
//! learning, daemon-optional) and spawns it when safe.
//!
//! Coexistence rule: spawn unless `[cron] tui_scheduler` is off OR a
//! live daemon reports `scheduler: true` via health/status (an explicit
//! `zen serve start` already hosts the full scheduler). Implicit
//! daemons (TUI/chat-spawned, `ZEN_SERVE_NO_SCHEDULER=1`) report false
//! and never suppress the in-app spawn; an unreachable/absent daemon
//! means the TUI is the only learner and must spawn.

use std::time::Duration;

use zen_core::config::ZenConfig;

/// Probe budget: bounded wait for the prewarmed surface + one health
/// RPC. Beyond this, treat the daemon as absent (fail open — the
/// scheduler markers make double-fire idempotent, but learning delay
/// is real).
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Pure coexistence decision (unit-testable).
///
/// - `config_enabled == false` → never spawn (kill switch)
/// - `daemon_scheduler == Some(true)` → never spawn (daemon hosts it)
/// - otherwise (no daemon / implicit daemon / unknown) → spawn
fn should_spawn_in_app_scheduler(config_enabled: bool, daemon_scheduler: Option<bool>) -> bool {
    config_enabled && daemon_scheduler != Some(true)
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
        tracing::info!(
            ?daemon_scheduler,
            "tui scheduler: spawning learning-core scheduler in-process"
        );
        let scheduler = zen_agents::scheduler::create_configured_scheduler_with(
            &cron,
            zen_agents::scheduler::SchedulerProfile::InApp,
        );
        scheduler.run().await;
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
}
