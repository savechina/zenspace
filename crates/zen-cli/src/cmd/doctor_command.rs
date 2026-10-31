use colored::Colorize;
use serde::Serialize;
use sqlx::sqlite::SqlitePoolOptions;
use std::path::Path;
use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;

#[derive(Serialize, Debug)]
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
        check_daemon_version(),
        check_loop_liveness(),
        check_provider(),
        check_vault(),
        check_outbox(),
        check_schema_compat(),
        check_background_tasks(),
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

/// Probe 9 (Phase 30 G3): stale-daemon diagnosis — the post-`brew upgrade`
/// case where an old gateway daemon keeps running under the new CLI.
/// Diagnose-only: this probe NEVER acts on `upgrade_policy` (the acting
/// detection points are `zen serve status` and the TUI prewarm path).
///
/// Version source: the `daemon.pid` record's `version` field (written by
/// the daemon itself after winning the startup lock). A live record
/// without the field predates version reporting ⇒ unknown ⇒ FAIL (it is
/// by definition an older build). No live record while the socket answers
/// means an implicit (TUI-spawned) daemon, which runs the current binary
/// by construction ⇒ PASS.
fn check_daemon_version() -> CheckResult {
    let socket = zen_gateway::transport::uds::default_socket_path();
    let socket_alive = socket.exists() && std::os::unix::net::UnixStream::connect(&socket).is_ok();
    let live_record = crate::cmd::serve_command::pid_path()
        .ok()
        .and_then(|p| zen_gateway::read_pid_record(p).ok())
        .filter(|r| zen_gateway::pid_record_alive(r.pid, r.start.as_deref()));
    daemon_version_probe(
        socket_alive,
        live_record.is_some(),
        live_record.as_ref().and_then(|r| r.version.as_deref()),
        env!("CARGO_PKG_VERSION"),
    )
}

/// Pure half of the daemon-version probe (testable without sockets/pids).
fn daemon_version_probe(
    socket_alive: bool,
    live_record: bool,
    record_version: Option<&str>,
    binary_version: &str,
) -> CheckResult {
    const NAME: &str = "daemon-version";
    if !socket_alive {
        return CheckResult {
            name: NAME.into(),
            ok: true,
            detail: "daemon not running (liveness covered by the daemon probe)".into(),
        };
    }
    if !live_record {
        return CheckResult {
            name: NAME.into(),
            ok: true,
            detail: "no explicit-daemon pid record (implicit daemons run the current binary)"
                .into(),
        };
    }
    match record_version {
        None => CheckResult {
            name: NAME.into(),
            ok: false,
            detail: format!(
                "daemon version unknown (predates version records, binary {binary_version}) — run `zen serve restart`"
            ),
        },
        Some(v) if v == binary_version => CheckResult {
            name: NAME.into(),
            ok: true,
            detail: format!("daemon {v} matches binary {binary_version}"),
        },
        Some(v) => CheckResult {
            name: NAME.into(),
            ok: false,
            detail: format!(
                "daemon {v} != binary {binary_version} (STALE) — run `zen serve restart`"
            ),
        },
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

    // Deterministic probe order: config maps are HashMaps whose iteration
    // order is randomized per process — without sorting, the first
    // resolvable provider (and therefore this check's detail string)
    // flips between runs (observed: ollama vs deepseek alternating).
    let mut agents: Vec<_> = config.agents.iter().collect();
    agents.sort_by(|a, b| a.0.cmp(b.0));
    for (name, agent) in agents {
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

    let mut providers: Vec<_> = config.providers.iter().collect();
    providers.sort_by(|a, b| a.0.cmp(b.0));
    for (name, prov) in providers {
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

/// Probe 10 (W4.3, spec D17): read-only applied-vs-embedded schema
/// comparison.
///
/// Resolves the state.db path and delegates to the async, non-migrating
/// [`schema_compat_for_path`]. An absent state.db is the normal
/// fresh-install state (PASS), never a failure.
fn check_schema_compat() -> CheckResult {
    let paths = match ZenPaths::detect() {
        Ok(p) => p,
        Err(e) => {
            return CheckResult {
                name: "schema-compat".into(),
                ok: false,
                detail: format!("paths unavailable: {e}"),
            };
        }
    };
    let db_path = paths.data().join("state.db");
    let probe = schema_compat_for_path(&db_path);
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => crate::runtime_util::block_in_place(|| handle.block_on(probe)),
        Err(_) => match tokio::runtime::Runtime::new() {
            Ok(rt) => rt.block_on(probe),
            Err(e) => CheckResult {
                name: "schema-compat".into(),
                ok: false,
                detail: format!("runtime unavailable: {e}"),
            },
        },
    }
}

/// Probe 11 (background wave 2026-10-31): orphaned background-tool metas.
///
/// Reads `<logs>/tasks/*/meta.json` (the on-disk mirror written at spawn and
/// at every terminal transition). A daemon crash between spawn and
/// completion leaves metas stuck in `status:"running"` forever — that is the
/// state this probe surfaces.
///
/// Daemon-alive distinction: a live daemon owns genuinely running tasks
/// that look byte-identical to orphans on disk, so a live daemon ⇒
/// informational PASS (count only). Daemon down with running metas ⇒ FAIL
/// with the actionable remedy (`zen serve start` settles them via the boot
/// sweep: verifiably-live groups get killed, dead groups get marked).
///
/// # Returns
/// PASS when no metas are on disk, none are running, or the daemon is live;
/// FAIL when the daemon is down and ≥1 meta still says `running`.
fn check_background_tasks() -> CheckResult {
    let dir = match ZenPaths::detect() {
        Ok(p) => p.logs().join("tasks"),
        Err(e) => {
            return CheckResult {
                name: "background-tasks".into(),
                ok: false,
                detail: format!("paths unavailable: {e}"),
            };
        }
    };
    let socket = zen_gateway::transport::uds::default_socket_path();
    let daemon_running =
        socket.exists() && std::os::unix::net::UnixStream::connect(&socket).is_ok();
    background_tasks_probe(&dir, daemon_running)
}

/// Pure half of the background-tasks probe (testable without env/sockets).
/// Status is parsed from raw meta JSON — no zen-plugin dependency: the
/// doctor observes the same on-disk contract (`"running"` until a
/// terminal transition is written), never the in-memory registry.
fn background_tasks_probe(task_dir: &Path, daemon_running: bool) -> CheckResult {
    const NAME: &str = "background-tasks";
    let Ok(entries) = std::fs::read_dir(task_dir) else {
        return CheckResult {
            name: NAME.into(),
            ok: true,
            detail: "no background task records".into(),
        };
    };
    let mut recorded = 0u64;
    let mut running: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        recorded += 1;
        let meta = entry.path().join("meta.json");
        let Ok(text) = std::fs::read_to_string(&meta) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            // Deliberately visible: corrupt metas are the exact shape the
            // on-disk contract warns about; a record that cannot prove it
            // terminated still counts, but cannot be called running.
            continue;
        };
        if value.get("status").and_then(|s| s.as_str()) == Some("running") {
            let id = entry.file_name().to_string_lossy().into_owned();
            running.push(id.chars().take(8).collect());
        }
    }
    if running.is_empty() {
        return CheckResult {
            name: NAME.into(),
            ok: true,
            detail: format!("{recorded} recorded task(s), none running"),
        };
    }
    if daemon_running {
        return CheckResult {
            name: NAME.into(),
            ok: true,
            detail: format!(
                "{} running task(s) while the daemon is live (live tasks look identical on disk)",
                running.len()
            ),
        };
    }
    CheckResult {
        name: NAME.into(),
        ok: false,
        detail: format!(
            "daemon down: {} task(s) stuck in running ({}…) — outputs under logs/tasks/, `zen serve start` settles them",
            running.len(),
            running.join(", ")
        ),
    }
}

/// Read-only schema-version probe for one state.db path.
///
/// Opens `sqlite://<path>?mode=ro` — deliberately NOT
/// [`zen_repo::SqliteClient::open`], which migrates and takes the
/// cross-process migration flock: the doctor must observe, never mutate,
/// and must not stall behind a live migration. Errors are classified by
/// message: a missing `_sqlx_migrations` table is a fresh install (PASS);
/// any other query/open failure (corrupt file, unreadable) is a genuine
/// FAIL naming the error.
async fn schema_compat_for_path(db_path: &Path) -> CheckResult {
    const NAME: &str = "schema-compat";
    if !db_path.exists() {
        return CheckResult {
            name: NAME.into(),
            ok: true,
            detail: "state.db not created yet (fresh install)".into(),
        };
    }
    let url = format!("sqlite://{}?mode=ro", db_path.display());
    let pool = match SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
    {
        Ok(pool) => pool,
        Err(e) => {
            return CheckResult {
                name: NAME.into(),
                ok: false,
                detail: format!("cannot open state.db read-only: {e}"),
            };
        }
    };
    let applied = sqlx::query_scalar::<_, i64>("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await;
    pool.close().await;

    match applied {
        Ok(db_max) => {
            let embedded_max = zen_repo::embedded_migration_max();
            if db_max > embedded_max {
                CheckResult {
                    name: NAME.into(),
                    ok: true,
                    detail: format!(
                        "db schema v{db_max} ahead of binary v{embedded_max} — \
                         tolerated (additive-only); upgrade zen when convenient"
                    ),
                }
            } else if db_max < embedded_max {
                // Binary ahead of the db (reachable right after an upgrade,
                // before any open() applies migrations): the db does NOT
                // match — migrations are merely pending, so still a PASS.
                CheckResult {
                    name: NAME.into(),
                    ok: true,
                    detail: format!(
                        "db schema v{db_max} pending migrations to v{embedded_max} — \
                         applied on next open"
                    ),
                }
            } else {
                CheckResult {
                    name: NAME.into(),
                    ok: true,
                    detail: format!("schema v{db_max} matches binary v{embedded_max}"),
                }
            }
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("no such table") {
                CheckResult {
                    name: NAME.into(),
                    ok: true,
                    detail: "state.db not created yet (fresh install)".into(),
                }
            } else {
                CheckResult {
                    name: NAME.into(),
                    ok: false,
                    detail: format!("schema probe failed: {msg}"),
                }
            }
        }
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
    fn daemon_version_probe_match_passes() {
        let r = daemon_version_probe(true, true, Some("0.0.9"), "0.0.9");
        assert_eq!(r.name, "daemon-version");
        assert!(r.ok, "matching versions must PASS: {}", r.detail);
    }

    #[test]
    fn daemon_version_probe_mismatch_fails_actionable() {
        let r = daemon_version_probe(true, true, Some("0.0.8"), "0.0.9");
        assert!(!r.ok, "version mismatch must FAIL");
        assert!(
            r.detail.contains("zen serve restart"),
            "detail must be actionable: {}",
            r.detail
        );
        assert!(
            r.detail.contains("0.0.8") && r.detail.contains("0.0.9"),
            "detail must name both versions: {}",
            r.detail
        );
    }

    #[test]
    fn daemon_version_probe_unknown_fails() {
        let r = daemon_version_probe(true, true, None, "0.0.9");
        assert!(!r.ok, "unknown version (legacy record) must FAIL");
        assert!(r.detail.contains("zen serve restart"));
    }

    #[test]
    fn daemon_version_probe_not_running_passes() {
        let r = daemon_version_probe(false, false, None, "0.0.9");
        assert!(r.ok, "daemon-not-running is the daemon probe's job");
    }

    #[test]
    fn daemon_version_probe_implicit_daemon_passes() {
        let r = daemon_version_probe(true, false, None, "0.0.9");
        assert!(
            r.ok,
            "no pid record ⇒ implicit daemon ⇒ current binary by construction"
        );
        assert!(r.detail.contains("implicit"));
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

    #[test]
    fn doctor_runs_eleven_probes_including_background_tasks() {
        let checks = run_all_checks();
        assert_eq!(checks.len(), 11, "doctor must run 11 liveness probes");
        assert!(
            checks.iter().any(|c| c.name == "schema-compat"),
            "probe #10 `schema-compat` must be present"
        );
        assert!(
            checks.iter().any(|c| c.name == "background-tasks"),
            "probe #11 `background-tasks` must be present"
        );
    }

    #[test]
    fn background_tasks_probe_states() {
        let dir = tempfile::tempdir().unwrap();
        let r = background_tasks_probe(&dir.path().join("absent"), false);
        assert!(
            r.ok && r.detail.contains("no background task records"),
            "absent dir is a trivial PASS, got {r:?}"
        );

        let r = background_tasks_probe(dir.path(), false);
        assert!(r.ok, "empty dir PASS, got {r:?}");
        assert_eq!(r.detail, "0 recorded task(s), none running");
    }

    #[test]
    fn background_tasks_probe_surfaces_stale_running_metas() {
        let dir = tempfile::tempdir().unwrap();
        let write_meta = |id: &str, status: &str| {
            let d = dir.path().join(id);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(
                d.join("meta.json"),
                format!(r#"{{"status": "{status}", "task_id": "{id}"}}"#),
            )
            .unwrap();
        };
        write_meta("aaaaaaaa", "running");
        write_meta("bbbbbbbb", "running");
        write_meta("cccccccc", "completed");
        write_meta("dddddddd", "not-json");

        // Daemon down: running metas are orphans — FAIL with id8s only.
        let r = background_tasks_probe(dir.path(), false);
        assert!(
            !r.ok
                && r.detail.contains("2 task(s) stuck")
                && r.detail.contains("aaaaaaaa")
                && r.detail.contains("bbbbbbbb")
                && !r.detail.contains("cccccccc")
                && !r.detail.contains("dddddddd"),
            "stale metas must FAIL naming id8s (order unstable), got {r:?}"
        );

        // Daemon live: live tasks look identical on disk — informational PASS.
        let r = background_tasks_probe(dir.path(), true);
        assert!(
            r.ok && r.detail.contains("2 running task(s)"),
            "daemon-live running metas PASS, got {r:?}"
        );
    }

    #[tokio::test]
    async fn schema_compat_missing_db_is_fresh_ok() {
        let dir = tempfile::tempdir().unwrap();
        let r = schema_compat_for_path(&dir.path().join("state.db")).await;
        assert_eq!(r.name, "schema-compat");
        assert!(r.ok, "a missing state.db is a fresh install: {}", r.detail);
        assert!(r.detail.contains("fresh install"), "{}", r.detail);
    }

    #[tokio::test]
    async fn schema_compat_equal_db_matches_binary() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        let client = zen_repo::SqliteClient::open(&db).await.unwrap();
        drop(client);

        let r = schema_compat_for_path(&db).await;
        assert_eq!(r.name, "schema-compat");
        assert!(r.ok, "equal schema must PASS: {}", r.detail);
        assert!(
            r.detail.contains("matches binary"),
            "detail must state the match: {}",
            r.detail
        );
    }

    #[tokio::test]
    async fn schema_compat_ahead_db_is_tolerated_ok() {
        let dir = tempfile::tempdir().unwrap();
        let db = build_skew_db(dir.path()).await;

        let r = schema_compat_for_path(&db).await;
        assert_eq!(r.name, "schema-compat");
        assert!(
            r.ok,
            "applied-ahead schema is tolerated (ok=true), not refused: {}",
            r.detail
        );
        assert!(
            r.detail.contains("ahead")
                && r.detail.contains("tolerated")
                && r.detail.contains("upgrade zen when convenient"),
            "detail must name the skew and the remedy: {}",
            r.detail
        );
    }

    #[tokio::test]
    async fn schema_compat_corrupt_db_fails() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        std::fs::write(&db, b"definitely not a sqlite database").unwrap();

        let r = schema_compat_for_path(&db).await;
        assert_eq!(r.name, "schema-compat");
        assert!(
            !r.ok,
            "an unreadable/corrupt state.db must FAIL: {}",
            r.detail
        );
    }

    /// Materializes a state.db whose applied schema is one migration ahead
    /// of this binary's embedded set: the real migration set plus a
    /// fabricated next-version probe applied through a fixture `Migrator`
    /// (mirrors `zen-repo/tests/schema_compat_tolerance.rs`). The warm open
    /// registers sqlite-vec process-wide so `002_vec` can create the vec0 table.
    async fn build_skew_db(dir: &Path) -> std::path::PathBuf {
        let mig_dir = dir.join("fixture_migrations");
        std::fs::create_dir(&mig_dir).unwrap();
        let real = Path::new(env!("CARGO_MANIFEST_DIR")).join("../zen-repo/migrations");
        for entry in std::fs::read_dir(&real).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            if name.to_string_lossy().ends_with(".sql") {
                std::fs::copy(entry.path(), mig_dir.join(&name)).unwrap();
            }
        }
        // Derived, never hardcoded: a literal version collides with the real
        // migration of that number (`UNIQUE constraint failed:
        // _sqlx_migrations.version`) and stops exercising the skew path.
        let probe_version = zen_repo::embedded_migration_max() + 1;
        std::fs::write(
            mig_dir.join(format!("{probe_version:03}_future_probe.sql")),
            "CREATE TABLE future_probe(x);\n",
        )
        .unwrap();

        let warm = dir.join("warm.db");
        let _warm = zen_repo::SqliteClient::open(&warm).await.unwrap();

        let db = dir.join("state.db");
        let url = format!("sqlite://{}?mode=rwc", db.display());
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap();
        let migrator = sqlx::migrate::Migrator::new(mig_dir).await.unwrap();
        migrator.run(&pool).await.unwrap();
        pool.close().await;
        db
    }
}
