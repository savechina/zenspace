use colored::Colorize;
use serde::Serialize;
use std::path::Path;
use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;

#[derive(Serialize)]
pub struct CheckResult {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Serialize)]
pub struct DoctorReport {
    pub checks: Vec<CheckResult>,
    pub ok: bool,
}

pub fn execute_command(json: bool) -> Result<(), ZenError> {
    let checks = run_all_checks();
    let all_ok = checks.iter().all(|c| c.ok);
    let report = DoctorReport { checks, ok: all_ok };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|e| ZenError::Service(format!("json serialization: {e}")))?
        );
    } else {
        for check in &report.checks {
            let icon = if check.ok { "✓".green() } else { "✗".red() };
            println!("{} {} — {}", icon, check.name, check.detail);
        }
        if report.ok {
            println!("\n{}", "All checks passed".green());
        } else {
            println!("\n{}", "Some checks failed".red());
        }
    }

    if all_ok {
        Ok(())
    } else {
        Err(ZenError::Service(
            "doctor checks failed (see output above)".to_string(),
        ))
    }
}

fn run_all_checks() -> Vec<CheckResult> {
    vec![
        check_config(),
        check_state_db(),
        check_memories(),
        check_daemon(),
        check_loop_liveness(),
        check_provider(),
        check_vault(),
        check_outbox(),
    ]
}

fn check_config() -> CheckResult {
    match zen_core::config::load_config() {
        Ok(_) => CheckResult {
            name: "config".into(),
            ok: true,
            detail: "4-layer config loads successfully".into(),
        },
        Err(e) => CheckResult {
            name: "config".into(),
            ok: false,
            detail: format!("config load failed: {e}"),
        },
    }
}

fn check_state_db() -> CheckResult {
    let paths = match ZenPaths::detect() {
        Ok(p) => p,
        Err(e) => {
            return CheckResult {
                name: "state.db".into(),
                ok: false,
                detail: format!("paths unavailable: {e}"),
            };
        }
    };
    let db_path = paths.data().join("state.db");
    if !db_path.exists() {
        return CheckResult {
            name: "state.db".into(),
            ok: false,
            detail: "state.db does not exist".into(),
        };
    }
    match std::fs::File::open(&db_path) {
        Ok(f) => {
            let meta = f.metadata().map(|m| m.len()).unwrap_or(0);
            CheckResult {
                name: "state.db".into(),
                ok: true,
                detail: format!("{} bytes", meta),
            }
        }
        Err(e) => CheckResult {
            name: "state.db".into(),
            ok: false,
            detail: format!("cannot read: {e}"),
        },
    }
}

fn check_memories() -> CheckResult {
    let paths = match ZenPaths::detect() {
        Ok(p) => p,
        Err(e) => {
            return CheckResult {
                name: "memories".into(),
                ok: false,
                detail: format!("paths unavailable: {e}"),
            };
        }
    };
    let mem_dir = paths.memory();
    if !mem_dir.exists() {
        return CheckResult {
            name: "memories".into(),
            ok: false,
            detail: "memories/ directory does not exist".into(),
        };
    }
    let count = count_files_recursive(&mem_dir);
    let size = dir_size_recursive(&mem_dir);
    CheckResult {
        name: "memories".into(),
        ok: true,
        detail: format!("{} files, {}", count, human_size(size)),
    }
}

fn check_daemon() -> CheckResult {
    let socket = zen_gateway::transport::uds::default_socket_path();
    if socket.exists() {
        match std::os::unix::net::UnixStream::connect(&socket) {
            Ok(_) => CheckResult {
                name: "daemon".into(),
                ok: true,
                detail: format!("socket {}", socket.display()),
            },
            Err(e) => CheckResult {
                name: "daemon".into(),
                ok: false,
                detail: format!("socket exists but connect failed: {e}"),
            },
        }
    } else {
        CheckResult {
            name: "daemon".into(),
            ok: false,
            detail: "gateway socket not found".into(),
        }
    }
}

fn check_loop_liveness() -> CheckResult {
    let paths = match ZenPaths::detect() {
        Ok(p) => p,
        Err(e) => {
            return CheckResult {
                name: "loop".into(),
                ok: false,
                detail: format!("paths unavailable: {e}"),
            };
        }
    };
    let report_path = paths.logs().join("loop-last-report.json");
    if !report_path.exists() {
        return CheckResult {
            name: "loop".into(),
            ok: false,
            detail: "never ran (no loop-last-report.json)".into(),
        };
    }
    match std::fs::metadata(&report_path) {
        Ok(meta) => {
            let modified = meta.modified().ok().and_then(|t| {
                let elapsed = t.elapsed().ok()?;
                Some(elapsed)
            });
            match modified {
                Some(elapsed) => {
                    let hours = elapsed.as_secs() / 3600;
                    let ok = elapsed.as_secs() < 7200;
                    CheckResult {
                        name: "loop".into(),
                        ok,
                        detail: format!("last report {hours}h ago"),
                    }
                }
                None => CheckResult {
                    name: "loop".into(),
                    ok: true,
                    detail: "last report exists (mtime unknown)".into(),
                },
            }
        }
        Err(e) => CheckResult {
            name: "loop".into(),
            ok: false,
            detail: format!("cannot read mtime: {e}"),
        },
    }
}

fn check_provider() -> CheckResult {
    let config = match zen_core::config::load_config() {
        Ok(c) => c,
        Err(e) => {
            return CheckResult {
                name: "provider".into(),
                ok: false,
                detail: format!("config load failed: {e}"),
            };
        }
    };

    for (name, agent) in &config.agents {
        if let Some(provider) = &agent.provider {
            if provider == "ollama" {
                return CheckResult {
                    name: "provider".into(),
                    ok: true,
                    detail: format!("{provider} (local) configured via {name}"),
                };
            }
            let env_key = format!("{}_API_KEY", provider.to_uppercase());
            if std::env::var(&env_key).is_ok() {
                return CheckResult {
                    name: "provider".into(),
                    ok: true,
                    detail: format!("{provider} API key set via {env_key}"),
                };
            }
        }
    }

    for (name, prov) in &config.providers {
        if name == "ollama" {
            return CheckResult {
                name: "provider".into(),
                ok: true,
                detail: "ollama (local) provider configured".into(),
            };
        }
        if let Some(key_ref) = &prov.api_key {
            return CheckResult {
                name: "provider".into(),
                ok: true,
                detail: format!("{name} API key configured ({key_ref})"),
            };
        }
    }

    CheckResult {
        name: "provider".into(),
        ok: false,
        detail: "no provider with API key or ollama found".into(),
    }
}

fn check_vault() -> CheckResult {
    let paths = match ZenPaths::detect() {
        Ok(p) => p,
        Err(e) => {
            return CheckResult {
                name: "vault".into(),
                ok: false,
                detail: format!("paths unavailable: {e}"),
            };
        }
    };
    let vault = paths.vault();
    if !vault.exists() {
        return CheckResult {
            name: "vault".into(),
            ok: false,
            detail: "vault/ directory does not exist".into(),
        };
    }

    let is_git = vault.join(".git").exists();
    let inbox = count_files_recursive(&paths.inbox());
    let wiki = count_files_recursive(&paths.wiki());
    let raw = count_files_recursive(&paths.raw());

    CheckResult {
        name: "vault".into(),
        ok: true,
        detail: format!("git={} inbox={} wiki={} raw={}", is_git, inbox, wiki, raw),
    }
}

/// Probe 8 (T189): morning-brief outbox deliverability.
///
/// Staged briefs in `<logs>/outbox/` are drained only by the qqbot channel's
/// outbox drainer; with `[channels.qqbot]` unconfigured they accumulate
/// forever with no other signal. Resolves paths + config, then delegates to
/// the pure [`outbox_probe`].
///
/// # Returns
/// PASS when the outbox is empty (nothing to deliver) or the qqbot channel is
/// live; FAIL with an actionable message when staged briefs are undeliverable.
fn check_outbox() -> CheckResult {
    let paths = match ZenPaths::detect() {
        Ok(p) => p,
        Err(e) => {
            return CheckResult {
                name: "outbox".into(),
                ok: false,
                detail: format!("paths unavailable: {e}"),
            };
        }
    };
    let qqbot_live = zen_core::config::load_config()
        .map(qqbot_channel_live)
        .unwrap_or(false);
    outbox_probe(&paths.logs().join("outbox"), qqbot_live)
}

/// Whether `[channels.qqbot]` carries usable credentials — mirrors
/// `serve_command::resolve_qqbot_channel` (present-but-incomplete ⇒ disabled).
fn qqbot_channel_live(config: &zen_core::config::ZenConfig) -> bool {
    config
        .channels
        .qqbot
        .as_ref()
        .map(|q| !q.app_id.is_empty() && !q.client_secret.is_empty())
        .unwrap_or(false)
}

/// Pure half of the outbox probe (testable without env/config).
///
/// # Parameters
/// - `outbox_dir` — `<logs>/outbox/`; absent or empty ⇒ PASS.
/// - `qqbot_live` — whether a delivery channel is configured.
///
/// # Returns
/// `CheckResult` named `outbox`: PASS when there is nothing staged or a live
/// channel can drain it; FAIL listing the count and the two remedies
/// (configure `[channels.qqbot]` or clear the directory).
fn outbox_probe(outbox_dir: &Path, qqbot_live: bool) -> CheckResult {
    let staged = count_files_recursive(outbox_dir);
    if staged == 0 {
        return CheckResult {
            name: "outbox".into(),
            ok: true,
            detail: "no staged briefs".into(),
        };
    }
    if qqbot_live {
        return CheckResult {
            name: "outbox".into(),
            ok: true,
            detail: format!("{staged} staged brief(s), qqbot channel configured"),
        };
    }
    CheckResult {
        name: "outbox".into(),
        ok: false,
        detail: format!(
            "{staged} staged briefs undeliverable: configure [channels.qqbot] or clear logs/outbox/"
        ),
    }
}

fn count_files_recursive(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| {
                    let p = e.path();
                    if p.is_dir() {
                        count_files_recursive(&p)
                    } else {
                        1
                    }
                })
                .sum()
        })
        .unwrap_or(0)
}

fn dir_size_recursive(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| {
                    let p = e.path();
                    if p.is_dir() {
                        dir_size_recursive(&p)
                    } else {
                        e.metadata().map(|m| m.len()).unwrap_or(0)
                    }
                })
                .sum()
        })
        .unwrap_or(0)
}

fn human_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_config_loads() {
        let result = check_config();
        assert!(result.ok, "config check should pass: {}", result.detail);
        assert_eq!(result.name, "config");
    }

    #[test]
    fn check_state_db_missing_path() {
        let result = check_state_db();
        assert_eq!(result.name, "state.db");
    }

    #[test]
    fn check_memories_missing() {
        let result = check_memories();
        assert_eq!(result.name, "memories");
    }

    #[test]
    fn human_size_formatting() {
        assert_eq!(human_size(0), "0B");
        assert_eq!(human_size(512), "512B");
        assert_eq!(human_size(1536), "1.5KB");
        assert_eq!(human_size(2 * 1024 * 1024), "2.0MB");
    }

    #[test]
    fn t189_outbox_staged_without_channel_fails() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = dir.path().join("logs").join("outbox");
        std::fs::create_dir_all(&outbox).unwrap();
        std::fs::write(outbox.join("morning-brief-2026-09-20.json"), "{}").unwrap();
        std::fs::write(outbox.join("morning-brief-2026-09-19.json"), "{}").unwrap();

        let result = outbox_probe(&outbox, false);
        assert_eq!(result.name, "outbox");
        assert!(!result.ok, "staged briefs with no channel must FAIL");
        assert!(
            result.detail.contains("2 staged briefs undeliverable"),
            "detail must name the count: {}",
            result.detail
        );
        assert!(
            result.detail.contains("[channels.qqbot]") && result.detail.contains("logs/outbox/"),
            "detail must be actionable: {}",
            result.detail
        );
    }

    #[test]
    fn t189_outbox_empty_passes() {
        let dir = tempfile::tempdir().unwrap();
        let result = outbox_probe(&dir.path().join("logs").join("outbox"), false);
        assert_eq!(result.name, "outbox");
        assert!(result.ok, "absent/empty outbox must PASS");
    }

    #[test]
    fn t189_outbox_staged_with_live_channel_passes() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = dir.path().join("outbox");
        std::fs::create_dir_all(&outbox).unwrap();
        std::fs::write(outbox.join("morning-brief-2026-09-20.json"), "{}").unwrap();

        let result = outbox_probe(&outbox, true);
        assert!(result.ok, "live qqbot channel drains the outbox");
        assert!(result.detail.contains("qqbot channel configured"));
    }

    #[test]
    fn t189_qqbot_live_requires_complete_credentials() {
        let mut config = zen_core::config::ZenConfig::default();
        assert!(!qqbot_channel_live(&config));

        config.channels.qqbot = Some(zen_core::config::QqBotChannelConfig {
            app_id: String::new(),
            client_secret: "secret".into(),
            allowed_users: Vec::new(),
            home_channel: None,
            outbox_drain_interval_secs: None,
        });
        assert!(
            !qqbot_channel_live(&config),
            "present-but-incomplete credentials ⇒ disabled (serve parity)"
        );

        config.channels.qqbot.as_mut().unwrap().app_id = "qq-1".into();
        assert!(qqbot_channel_live(&config));
    }

    #[test]
    fn t189_check_outbox_uses_zen_home_outbox_dir() {
        use std::sync::{Mutex, MutexGuard};
        static ENV_LOCK: Mutex<()> = Mutex::new(());
        let _guard: MutexGuard<'static, ()> = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved_home = std::env::var("ZEN_HOME").ok();
        let saved_app = std::env::var("ZEN_QQBOT_APP_ID").ok();
        let saved_secret = std::env::var("ZEN_QQBOT_CLIENT_SECRET").ok();

        let dir = tempfile::tempdir().unwrap();
        let outbox = dir.path().join("logs").join("outbox");
        std::fs::create_dir_all(&outbox).unwrap();
        std::fs::write(outbox.join("morning-brief-2026-09-20.json"), "{}").unwrap();

        // SAFETY: ENV_LOCK serializes env mutation within this module.
        unsafe { std::env::set_var("ZEN_HOME", dir.path()) };
        unsafe { std::env::remove_var("ZEN_QQBOT_APP_ID") };
        unsafe { std::env::remove_var("ZEN_QQBOT_CLIENT_SECRET") };
        zen_core::config::invalidate_config_cache();

        let result = check_outbox();
        assert_eq!(result.name, "outbox");
        assert!(
            !result.ok && result.detail.contains("undeliverable"),
            "staged brief under temp ZEN_HOME with no channel must FAIL, got: {}",
            result.detail
        );

        unsafe { std::env::remove_var("ZEN_HOME") };
        if let Some(v) = saved_home {
            unsafe { std::env::set_var("ZEN_HOME", v) };
        }
        if let Some(v) = saved_app {
            unsafe { std::env::set_var("ZEN_QQBOT_APP_ID", v) };
        }
        if let Some(v) = saved_secret {
            unsafe { std::env::set_var("ZEN_QQBOT_CLIENT_SECRET", v) };
        }
        zen_core::config::invalidate_config_cache();
    }

    #[test]
    fn render_report_json() {
        let report = DoctorReport {
            checks: vec![CheckResult {
                name: "test".into(),
                ok: true,
                detail: "ok".into(),
            }],
            ok: true,
        };
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"ok\":true"));
        assert!(json.contains("\"name\":\"test\""));
    }
}
