use clap::Subcommand;
use colored::Colorize;
use std::io::{Read, Write};
use std::path::Path;
// macOS-only usage (launchd helpers below) — un-gating breaks Linux CI under -D warnings.
#[cfg(target_os = "macos")]
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use tracing::info;

use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;
use zen_gateway::{
    HttpConfig, is_pid_alive, pid_record_alive, read_pid, read_pid_record, remove_pid,
};

#[derive(Subcommand)]
pub enum ServeCommands {
    /// Start the gateway server (UDS sole-owner daemon by default)
    ///
    /// Without flags this detaches immediately (codex app-server pattern:
    /// the spawner owns reporting; the daemon process stays machine-quiet).
    /// Startup output prints only when stdout is a terminal.
    Start {
        /// Run in foreground (blocks)
        #[arg(long)]
        foreground: bool,
        /// Legacy HTTP gateway instead of the UDS daemon
        #[arg(long)]
        http: bool,
        /// Bind address (default: 127.0.0.1, HTTP mode only)
        #[arg(long)]
        bind: Option<String>,
        /// Port (default: 9876, HTTP mode only)
        #[arg(long)]
        port: Option<u16>,
        /// Start as MCP stdio server (for external MCP clients)
        #[arg(long)]
        mcp: bool,
    },
    /// Stop the gateway server
    Stop,
    /// Restart the gateway daemon under the current binary (Phase 30 G2)
    ///
    /// Functionality: launchd-installed (plist present) ⇒ `launchctl
    ///   kickstart -k gui/{uid}/dev.zen.serve` (KeepAlive relaunches and
    ///   PATH resolves the NEW binary); otherwise graceful stop (SIGTERM,
    ///   drain ≤10s, audited cancels) then a background start under the
    ///   existing StartupLock/readiness semantics.
    /// User impact: in-flight daemon turns drain for ≤10s then cancel with
    ///   audit lines; this is the recommended follow-up to `brew upgrade`.
    /// Default: manual stop+start (launchd path only when the plist exists).
    /// Interaction: a stopped daemon makes restart a plain start; idempotent
    ///   like start (a live post-kickstart daemon answers the readiness
    ///   probe). `[gateway] upgrade_policy = "auto-restart"` runs this flow
    ///   automatically at the status/TUI stale-detection points.
    Restart,
    /// Show gateway server status
    ///
    /// Functionality: binary version vs running daemon version with a STALE
    ///   marker when they differ (unknown daemon version ⇒ STALE-unknown).
    /// User impact: `--json` emits the machine-readable status object.
    /// Default: human-readable output.
    /// Interaction: with `[gateway] upgrade_policy = "auto-restart"`, a
    ///   STALE daemon is drain-restarted on the spot (human mode only —
    ///   `--json` reports without acting so machine consumers decide).
    Status {
        /// Machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
    /// Test MCP server connectivity
    Test {
        /// Port of the gateway (default: 9876)
        #[arg(long)]
        port: Option<u16>,
    },
    /// Install zen serve as a macOS launchd LaunchAgent (macOS only)
    ///
    /// Functionality: writes a LaunchAgent plist to ~/Library/LaunchAgents/
    ///   and bootstraps it via launchctl so `zen serve start --foreground`
    ///   runs persistently with KeepAlive + crash-loop throttle.
    /// User impact: after install, the daemon starts automatically on login.
    /// Default: not installed — user must run `zen serve install` explicitly.
    /// Interaction: `zen serve uninstall` reverses; `zen serve start` while
    ///   installed causes a second instance (single-instance guard is socket-based).
    Install,
    /// Uninstall the zen serve launchd LaunchAgent (macOS only)
    ///
    /// Functionality: boots out the LaunchAgent and removes the plist file.
    /// User impact: daemon stops starting on login; running instance unaffected.
    /// Default: no-op when not installed (tolerant).
    /// Interaction: after uninstall, `zen serve start` works as manual start.
    Uninstall,
}

pub(crate) const PID_FILE_NAME: &str = "daemon.pid";

/// Readiness probe cadence/budget shared by `run_background` and the
/// launchd kickstart path (same values, one definition).
const READY_POLL: Duration = Duration::from_millis(250);
const READY_BUDGET: Duration = Duration::from_secs(10);

/// Human-output reporter: `quiet` mode (TUI-triggered restart) routes the
/// same messages to the tracing log instead of stdout, which the inline
/// viewport owns.
fn report(quiet: bool, msg: String) {
    if quiet {
        tracing::info!(target: "zen_serve", "{msg}");
    } else {
        println!("{msg}");
    }
}

fn uds_socket_path() -> std::path::PathBuf {
    zen_gateway::transport::uds::default_socket_path()
}

pub(crate) fn pid_path() -> Result<std::path::PathBuf, ZenError> {
    let paths = ZenPaths::detect()?;
    Ok(paths.global_root().join(PID_FILE_NAME))
}

/// Polls until `pid` exits or the grace window elapses.
///
/// Returns `true` when the process is gone, `false` if it survived the
/// full grace period (caller decides whether to escalate or fail).
async fn wait_exit(pid: u32, grace: Duration) -> bool {
    const POLL: Duration = Duration::from_millis(250);
    let deadline = tokio::time::Instant::now() + grace;
    while is_pid_alive(pid) {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(POLL).await;
    }
    true
}

/// Best-effort cleanup of a stale or recycled-pid pid file.
///
/// Never rejects startup: the daemon's socket bind is the single-instance
/// arbiter (codex parity — probe/socket first, pid file advisory only).
/// True when a gateway answers the socket handshake right now. Used to make
/// `serve start` idempotent without touching the running daemon's pid file.
async fn gateway_is_live() -> bool {
    match zen_gateway::client::GatewayClient::connect(uds_socket_path()).await {
        Ok(client) => client
            .handshake("cli-start", "0.0", Default::default())
            .await
            .is_ok(),
        Err(_) => false,
    }
}

fn clean_stale_pid(path: &Path, quiet: bool) {
    let Ok(record) = read_pid_record(path) else {
        if path.exists() {
            report(
                quiet,
                format!("{} Removed unreadable PID file", "🧹".yellow()),
            );
            remove_pid(path).ok();
        }
        return;
    };
    if pid_record_alive(record.pid, record.start.as_deref()) {
        return;
    }
    let recycled = is_pid_alive(record.pid);
    report(
        quiet,
        format!(
            "{} Cleaned up stale PID file (pid: {} {})",
            "🧹".yellow(),
            record.pid,
            if recycled {
                "was recycled by another process"
            } else {
                "is dead"
            }
        ),
    );
    remove_pid(path).ok();
}

fn ensure_pid_dir(path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
}

/// Returns `true` when the scheduler should be started.
/// Implicit gateway spawns set `ZEN_SERVE_NO_SCHEDULER=1` so the daemon
/// runs as a pure gateway (sessions/hosting/guards only). Explicit
/// `zen serve start` leaves the env unset and gets the full scheduler.
pub(crate) fn scheduler_enabled() -> bool {
    std::env::var("ZEN_SERVE_NO_SCHEDULER")
        .map(|v| v != "1")
        .unwrap_or(true)
}

#[cfg(target_os = "macos")]
const LAUNCHD_LABEL: &str = "dev.zen.serve";

#[cfg(target_os = "macos")]
pub(crate) fn render_plist(zen_bin: &Path, logs_dir: &Path) -> String {
    let out_log = logs_dir.join("serve.out.log");
    let err_log = logs_dir.join("serve.err.log");
    let home = std::env::var("HOME").unwrap_or_else(|_| "/Users/user".into());
    let path = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin";

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin}</string>
        <string>serve</string>
        <string>start</string>
        <string>--foreground</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ThrottleInterval</key>
    <integer>60</integer>
    <key>EnvironmentVariables</key>
    <dict>
        <key>HOME</key>
        <string>{home}</string>
        <key>PATH</key>
        <string>{path}</string>
    </dict>
    <key>StandardOutPath</key>
    <string>{out}</string>
    <key>StandardErrorPath</key>
    <string>{err}</string>
</dict>
</plist>"#,
        label = LAUNCHD_LABEL,
        bin = zen_bin.display(),
        home = home,
        path = path,
        out = out_log.display(),
        err = err_log.display(),
    )
}

#[cfg(target_os = "macos")]
fn plist_path() -> Result<PathBuf, ZenError> {
    let home =
        std::env::var("HOME").map_err(|e| ZenError::Service(format!("HOME not set: {e}")))?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist")))
}

#[cfg(target_os = "macos")]
fn gui_domain() -> Result<String, ZenError> {
    let out = std::process::Command::new("id")
        .arg("-u")
        .output()
        .map_err(|e| ZenError::Service(format!("id -u: {e}")))?;
    let uid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || uid.is_empty() || !uid.chars().all(|c| c.is_ascii_digit()) {
        return Err(ZenError::Service(
            "cannot resolve user UID via `id -u`".into(),
        ));
    }
    Ok(format!("gui/{uid}"))
}

#[cfg(not(target_os = "macos"))]
fn install_launchd() -> Result<(), ZenError> {
    Err(ZenError::Service(
        "launchd persistence is macOS-only".to_string(),
    ))
}

#[cfg(target_os = "macos")]
fn install_launchd() -> Result<(), ZenError> {
    let zen_bin = std::env::current_exe()
        .map_err(|e| ZenError::Service(format!("cannot find zen binary: {e}")))?;
    let paths = ZenPaths::detect()?;
    let logs_dir = paths.logs();
    std::fs::create_dir_all(&logs_dir)
        .map_err(|e| ZenError::Service(format!("create logs dir: {e}")))?;

    let plist = render_plist(&zen_bin, &logs_dir);
    let dest = plist_path()?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ZenError::Service(format!("create LaunchAgents dir: {e}")))?;
    }
    std::fs::write(&dest, &plist).map_err(|e| ZenError::Service(format!("write plist: {e}")))?;

    let domain = gui_domain()?;

    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &format!("{}/{}", domain, LAUNCHD_LABEL)])
        .output();

    let output = std::process::Command::new("launchctl")
        .args(["bootstrap", &domain, dest.to_str().unwrap_or("")])
        .output()
        .map_err(|e| ZenError::Service(format!("launchctl bootstrap: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Load failed") || stderr.contains("already loaded") {
            println!("{} LaunchAgent reloaded", "✅".green());
        } else {
            return Err(ZenError::Service(format!(
                "launchctl bootstrap failed: {}",
                stderr.trim()
            )));
        }
    } else {
        println!("{} LaunchAgent installed", "✅".green());
    }
    println!("  Label:  {}", LAUNCHD_LABEL);
    println!("  Plist:  {}", dest.display());
    println!("  Binary: {}", zen_bin.display());
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn uninstall_launchd() -> Result<(), ZenError> {
    Err(ZenError::Service(
        "launchd persistence is macOS-only".to_string(),
    ))
}

#[cfg(target_os = "macos")]
fn uninstall_launchd() -> Result<(), ZenError> {
    let domain = gui_domain()?;

    let output = std::process::Command::new("launchctl")
        .args(["bootout", &format!("{}/{}", domain, LAUNCHD_LABEL)])
        .output()
        .map_err(|e| ZenError::Service(format!("launchctl bootout: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Could not find specified service") {
            println!("{} LaunchAgent not loaded (no-op)", "ℹ️".blue());
        } else {
            eprintln!("{} bootout warning: {}", "⚠️".yellow(), stderr.trim());
        }
    } else {
        println!("{} LaunchAgent unloaded", "✅".green());
    }

    let dest = plist_path()?;
    if dest.exists() {
        std::fs::remove_file(&dest).map_err(|e| ZenError::Service(format!("remove plist: {e}")))?;
        println!("  Removed: {}", dest.display());
    }
    Ok(())
}

pub async fn execute_command(operation: &ServeCommands) -> Result<(), ZenError> {
    match operation {
        ServeCommands::Start {
            foreground,
            http,
            bind,
            port,
            mcp,
        } => {
            if *mcp {
                return run_mcp_stdio().await;
            }
            start_daemon(false, *foreground, *http, bind.as_deref(), *port).await
        }
        ServeCommands::Stop => {
            stop_daemon(false).await?;
            Ok(())
        }
        ServeCommands::Restart => restart_daemon().await,
        ServeCommands::Status { json } => status_command(*json).await,
        ServeCommands::Test { port } => {
            let config = HttpConfig::default();
            let port = port.unwrap_or(config.port);
            let bind_addr = &config.bind_addr;

            println!("{} Gateway Connectivity Test", "━━━".bold());
            println!();

            let http_addr = format!("{}:{}", bind_addr, port);
            print!("  HTTP Gateway ({}) ... ", http_addr);
            std::io::stdout().flush().ok();

            let http_result = std::net::TcpStream::connect_timeout(
                &http_addr
                    .parse::<std::net::SocketAddr>()
                    .map_err(|e| ZenError::Service(e.to_string()))?,
                std::time::Duration::from_secs(3),
            );

            match http_result {
                Ok(_) => {
                    println!("{}", "OK".green());
                    println!("    Port {} is open", port);

                    if let Some(body) = fetch_http_body(bind_addr, port, "/health") {
                        println!("    Health: {}", body);
                    }
                }
                Err(e) => {
                    println!("{} {}", "FAIL".red(), e);
                    println!("    Gateway may not be running");
                    println!("    Run 'zen serve start' to launch gateway");
                }
            }

            println!();
            println!("Complete report: {} zen serve status", "→".blue().italic());

            Ok(())
        }
        ServeCommands::Install => install_launchd(),
        ServeCommands::Uninstall => uninstall_launchd(),
    }
}

/// Start path shared by `zen serve start` and the restart flow (Phase 30
/// G2 — reuse, never duplicate). `quiet` routes human output to tracing.
async fn start_daemon(
    quiet: bool,
    foreground: bool,
    http: bool,
    bind: Option<&str>,
    port: Option<u16>,
) -> Result<(), ZenError> {
    let path = pid_path()?;
    clean_stale_pid(&path, quiet);
    ensure_pid_dir(&path);
    // Probe BEFORE spawning: if a gateway is already serving, this
    // start is a no-op and, critically, must not touch the pid file —
    // the running daemon owns that record now.
    if gateway_is_live().await {
        match read_pid_record(&path)
            .ok()
            .filter(|r| pid_record_alive(r.pid, r.start.as_deref()))
            .map(|r| r.pid)
        {
            Some(pid) => report(
                quiet,
                format!("{} Gateway already running (pid: {pid})", "✅".green()),
            ),
            None => report(quiet, format!("{} Gateway already running", "✅".green())),
        }
        report(
            quiet,
            format!("  Socket:   {}", uds_socket_path().display()),
        );
        return Ok(());
    }
    // `--http` now enables the loopback HTTP carrier alongside the
    // UDS daemon (T046: legacy HttpGateway retired); env opt-in
    // also honored per FR-019 config layering.
    let http_cfg = resolve_http_carrier(http, bind, port);
    // QQBot channel (Phase 13): config.toml-only (`[channels.qqbot]`);
    // its presence implies the loopback HTTP carrier it bridges to.
    let zen_config = zen_core::config::load_config()?;
    let qqbot_cfg = resolve_qqbot_channel(zen_config);
    let http_cfg = http_cfg.or_else(|| {
        qqbot_cfg.as_ref().map(|_| HttpConfig::default()).map(|d| {
            zen_gateway::transport::http::HttpCarrierConfig {
                bind_addr: d.bind_addr,
                port: d.port,
            }
        })
    });
    if foreground {
        return run_uds_foreground(http_cfg, qqbot_cfg).await;
    }
    run_background(&path, http_cfg, quiet).await
}

/// Stop path shared by `zen serve stop` and the restart flow. Returns
/// whether a running daemon was actually stopped (`false` = not running —
/// restart callers announce the plain-start case).
async fn stop_daemon(quiet: bool) -> Result<bool, ZenError> {
    // Preferred path: graceful `shutdown` RPC over the UDS socket.
    if let Ok(client) = zen_gateway::client::GatewayClient::connect(uds_socket_path()).await {
        let _ = client
            .handshake("cli-stop", "0.0", Default::default())
            .await;
        if let Ok(result) = client.request("shutdown", serde_json::json!({})).await {
            remove_pid(&pid_path()?).ok();
            report(
                quiet,
                format!(
                    "{} Gateway stopped via socket (drained: {}, cancelled: {})",
                    "✅".green(),
                    result["drained"],
                    result["cancelled"]
                ),
            );
            return Ok(true);
        }
    }

    let path = pid_path()?;
    if !path.exists() {
        report(quiet, "Gateway not running (no PID file)".to_string());
        return Ok(false);
    }

    let record = match read_pid_record(&path) {
        Ok(r) => r,
        Err(e) => return Err(ZenError::Service(e.to_string())),
    };

    if !pid_record_alive(record.pid, record.start.as_deref()) {
        if is_pid_alive(record.pid) {
            report(
                quiet,
                format!(
                    "PID file is stale (pid: {} was recycled by another process)",
                    record.pid
                ),
            );
        } else {
            report(
                quiet,
                format!("Gateway process not responding (pid: {})", record.pid),
            );
        }
        remove_pid(&path).map_err(|e| ZenError::Service(e.to_string()))?;
        report(quiet, "Gateway not running".to_string());
        return Ok(false);
    }
    let pid = record.pid;

    #[cfg(unix)]
    {
        let result = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        if result == 0 {
            info!("Sent SIGTERM to gateway (pid: {})", pid);
            report(quiet, format!("Sent stop signal to gateway (pid: {})", pid));
        } else {
            report(
                quiet,
                format!("Failed to send signal to gateway (pid: {})", pid),
            );
        }

        // Escalation chain (codex app-server-daemon pattern): the
        // daemon drains in-flight turns for ≤10s after SIGTERM, so
        // grace slightly beyond that before a forced kill — and
        // never report success over a wedged process.
        const TERM_GRACE: Duration = Duration::from_secs(15);
        const KILL_GRACE: Duration = Duration::from_secs(5);
        if !wait_exit(pid, TERM_GRACE).await {
            report(
                quiet,
                "Process not responding, force killing...".to_string(),
            );
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
            if !wait_exit(pid, KILL_GRACE).await {
                return Err(ZenError::Service(format!(
                    "gateway pid {pid} survived SIGKILL"
                )));
            }
        }
    }

    #[cfg(not(unix))]
    {
        report(quiet, format!("Stop signal sent (pid: {})", pid));
    }

    remove_pid(&path).map_err(|e| ZenError::Service(e.to_string()))?;
    report(quiet, format!("{} Gateway stopped", "✅".green()));
    Ok(true)
}

/// Version-match state of a running daemon against the invoking binary
/// (Phase 30 G1/G3). `Unknown` = the daemon answered without a version
/// (predates version reporting) ⇒ treated as STALE-unknown: it is by
/// definition an older build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DaemonVersionState {
    Current,
    Stale,
    Unknown,
}

impl DaemonVersionState {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Stale => "stale",
            Self::Unknown => "unknown",
        }
    }

    /// Whether the upgrade policy acts on this state (stale and unknown
    /// both mean "not provably the current binary").
    pub(crate) fn is_stale(&self) -> bool {
        !matches!(self, Self::Current)
    }
}

/// Pure STALE classification: `None`/blank daemon version ⇒ `Unknown`,
/// equal ⇒ `Current`, different ⇒ `Stale`.
pub(crate) fn classify_daemon_version(
    binary_version: &str,
    daemon_version: Option<&str>,
) -> DaemonVersionState {
    match daemon_version.map(str::trim).filter(|v| !v.is_empty()) {
        None => DaemonVersionState::Unknown,
        Some(v) if v == binary_version => DaemonVersionState::Current,
        Some(_) => DaemonVersionState::Stale,
    }
}

/// Effective `[gateway] upgrade_policy` auto-restart switch. Config load
/// failure falls back to warn (never restart on an unreadable policy).
pub(crate) fn upgrade_policy_auto_restart() -> bool {
    match zen_core::config::load_config() {
        Ok(c) => c.gateway.auto_restart(),
        Err(e) => {
            tracing::warn!(error = %e, "config load failed; upgrade_policy stays at \"warn\"");
            false
        }
    }
}

async fn status_command(json: bool) -> Result<(), ZenError> {
    let binary_version = env!("CARGO_PKG_VERSION");

    // Preferred path: live health/status over the UDS socket.
    if let Ok(client) = zen_gateway::client::GatewayClient::connect(uds_socket_path()).await
        && client
            .handshake("cli-status", "0.0", Default::default())
            .await
            .is_ok()
        && let Ok(s) = client.request("health/status", serde_json::json!({})).await
    {
        let pid = pid_path().ok().and_then(|p| read_pid(&p).ok());
        let daemon_version = s["serverVersion"].as_str();
        let state = classify_daemon_version(binary_version, daemon_version);

        if json {
            let payload = serde_json::json!({
                "running": true,
                "transport": "uds",
                "pid": pid,
                "socket": uds_socket_path().display().to_string(),
                "binaryVersion": binary_version,
                "daemonVersion": daemon_version,
                "daemonProtocol": s["protocolVersion"].as_str(),
                "versionState": state.as_str(),
                "stale": state.is_stale(),
                "clients": s["clients"],
                "storeHealth": s["storeHealth"],
                "uptimeMs": s["uptimeMs"],
                "activeTurns": s["activeTurns"],
            });
            println!("{payload}");
            return Ok(());
        }

        println!("{} Gateway running (UDS)", "✅".green());
        if let Some(p) = pid {
            println!("  PID: {}", p);
        }
        println!("  Socket: {}", uds_socket_path().display());
        println!("  Binary:  {}", binary_version);
        match state {
            DaemonVersionState::Current => println!(
                "  Daemon:  {} (protocol {})",
                daemon_version.unwrap_or_default(),
                s["protocolVersion"]
            ),
            DaemonVersionState::Stale => println!(
                "  Daemon:  {} (protocol {}) {}",
                daemon_version.unwrap_or_default(),
                s["protocolVersion"],
                "STALE — run `zen serve restart`".yellow()
            ),
            DaemonVersionState::Unknown => println!(
                "  Daemon:  unknown (predates version reporting) {}",
                "STALE-unknown — run `zen serve restart`".yellow()
            ),
        }
        println!("  Clients: {}", s["clients"]);
        println!("  Store:   {}", s["storeHealth"]);
        println!("  Uptime:  {}ms", s["uptimeMs"]);
        println!("  Turns:   {}", s["activeTurns"]);
        print_process_stats(pid.unwrap_or(0));

        // G3 detection point (a): policy action — human mode only, so
        // `--json` output stays parseable and machine consumers decide.
        if state.is_stale() && upgrade_policy_auto_restart() {
            println!(
                "{} upgrade_policy=auto-restart: restarting the stale daemon",
                "🔄".blue()
            );
            return restart_daemon().await;
        }
        return Ok(());
    }

    let path = pid_path().ok();
    let config = HttpConfig::default();
    let health_url = format!("http://{}:{}/health", config.bind_addr, config.port);

    let addr = format!("{}:{}", config.bind_addr, config.port).parse::<std::net::SocketAddr>();

    let http_ok = addr
        .ok()
        .and_then(|addr| {
            std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(2)).ok()
        })
        .is_some();

    if http_ok {
        let pid = path.as_ref().and_then(|p| read_pid(p).ok());
        // HTTP fallback has no live version RPC; the pid record's version
        // field (when the daemon wrote one) is the only source.
        let record_version = path
            .as_ref()
            .and_then(|p| read_pid_record(p).ok())
            .filter(|r| pid_record_alive(r.pid, r.start.as_deref()))
            .and_then(|r| r.version);
        let state = classify_daemon_version(binary_version, record_version.as_deref());

        if json {
            let payload = serde_json::json!({
                "running": true,
                "transport": "http",
                "pid": pid,
                "binaryVersion": binary_version,
                "daemonVersion": record_version,
                "versionState": state.as_str(),
                "stale": state.is_stale(),
                "health": health_url,
            });
            println!("{payload}");
            return Ok(());
        }

        println!("{} Gateway running", "✅".green());
        if let Some(p) = pid {
            println!("  PID: {}", p);
        }
        println!("  Binary:  {}", binary_version);
        match state {
            DaemonVersionState::Current => {
                println!(
                    "  Daemon:  {}",
                    record_version.as_deref().unwrap_or_default()
                )
            }
            DaemonVersionState::Stale => println!(
                "  Daemon:  {} {}",
                record_version.as_deref().unwrap_or_default(),
                "STALE — run `zen serve restart`".yellow()
            ),
            DaemonVersionState::Unknown => println!(
                "  Daemon:  unknown {}",
                "STALE-unknown — run `zen serve restart`".yellow()
            ),
        }
        println!("  Health: {}", health_url);
        println!(
            "  API:    http://{}:{}/api/v1/",
            config.bind_addr, config.port
        );

        let body = fetch_http_body(&config.bind_addr, config.port, "/health");
        if let Some(b) = body {
            println!("  Status: {}", b);
        }

        print_process_stats(pid.unwrap_or(0));
        if state.is_stale() && upgrade_policy_auto_restart() {
            println!(
                "{} upgrade_policy=auto-restart: restarting the stale daemon",
                "🔄".blue()
            );
            return restart_daemon().await;
        }
    } else if let Some(path) = path {
        if path.exists()
            && let Ok(pid) = read_pid(&path)
            && is_pid_alive(pid)
        {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "running": false,
                        "transport": "none",
                        "pid": pid,
                        "binaryVersion": binary_version,
                        "versionState": serde_json::Value::Null,
                        "stale": false,
                        "detail": "process alive but not answering health probes",
                    })
                );
                return Ok(());
            }
            println!(
                "{} Gateway process alive (pid: {}) but HTTP not responding",
                "⚠️".yellow(),
                pid
            );
            return Ok(());
        }
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "running": false,
                    "transport": "none",
                    "binaryVersion": binary_version,
                    "versionState": serde_json::Value::Null,
                    "stale": false,
                })
            );
            return Ok(());
        }
        println!("{} Gateway not running", "⛔".red());
    } else if json {
        println!(
            "{}",
            serde_json::json!({
                "running": false,
                "transport": "none",
                "binaryVersion": binary_version,
                "versionState": serde_json::Value::Null,
                "stale": false,
            })
        );
        return Ok(());
    } else {
        println!("{} Gateway not running", "⛔".red());
    }
    Ok(())
}

/// CLI entry for `zen serve restart` (human output).
pub(crate) async fn restart_daemon() -> Result<(), ZenError> {
    restart_inner(false).await
}

/// TUI entry (Phase 30 G3 detection point c): same flow, output routed to
/// tracing so the inline viewport is not corrupted.
pub(crate) async fn restart_daemon_quiet() -> Result<(), ZenError> {
    restart_inner(true).await
}

async fn restart_inner(quiet: bool) -> Result<(), ZenError> {
    #[cfg(target_os = "macos")]
    if restart_mode(plist_path().map(|p| p.exists()).unwrap_or(false))
        == RestartMode::LaunchdKickstart
    {
        return restart_via_launchd(quiet).await;
    }

    let was_running = stop_daemon(quiet).await?;
    if !was_running {
        report(
            quiet,
            format!(
                "{} Daemon was not running — restart is a plain start",
                "ℹ️".blue()
            ),
        );
    }
    start_daemon(quiet, false, false, None, None).await
}

/// Restart decision (pure): a launchd-installed plist means KeepAlive owns
/// the lifecycle, so `kickstart -k` is the only correct relaunch path —
/// a manual stop would be immediately respawned by launchd, racing the
/// manual start.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RestartMode {
    LaunchdKickstart,
    ManualStopStart,
}

#[cfg(target_os = "macos")]
pub(crate) fn restart_mode(plist_installed: bool) -> RestartMode {
    if plist_installed {
        RestartMode::LaunchdKickstart
    } else {
        RestartMode::ManualStopStart
    }
}

/// Pure launchctl kickstart target: `gui/{uid}/dev.zen.serve`.
#[cfg(target_os = "macos")]
pub(crate) fn kickstart_target(gui_domain: &str) -> String {
    format!("{gui_domain}/{LAUNCHD_LABEL}")
}

/// Pure launchctl argv for a forced service restart (`-k` kills the
/// running instance first; KeepAlive relaunches via PATH ⇒ new binary).
#[cfg(target_os = "macos")]
pub(crate) fn kickstart_args(target: &str) -> Vec<String> {
    vec!["kickstart".into(), "-k".into(), target.into()]
}

#[cfg(target_os = "macos")]
async fn restart_via_launchd(quiet: bool) -> Result<(), ZenError> {
    let domain = gui_domain()?;
    let target = kickstart_target(&domain);
    let args = kickstart_args(&target);

    let output = Command::new("launchctl")
        .args(&args)
        .output()
        .map_err(|e| ZenError::Service(format!("launchctl kickstart: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ZenError::Service(format!(
            "launchctl kickstart failed: {}",
            stderr.trim()
        )));
    }
    report(
        quiet,
        format!(
            "{} Gateway restart requested via launchd (kickstart -k {})",
            "✅".green(),
            target
        ),
    );

    // Readiness = handshake answer, same budget as the start path.
    let deadline = tokio::time::Instant::now() + READY_BUDGET;
    loop {
        if gateway_is_live().await {
            report(
                quiet,
                format!(
                    "{} Gateway ready (relaunched by launchd under the current binary {})",
                    "✅".green(),
                    env!("CARGO_PKG_VERSION")
                ),
            );
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(READY_POLL).await;
    }
    // Kickstart succeeded but the daemon has not answered yet — launchd
    // may still be relaunching (ThrottleInterval). Report honestly.
    report(
        quiet,
        format!(
            "{} Gateway did not answer within {}s after kickstart; launchd may still be relaunching — check `zen serve status`",
            "⚠️".yellow(),
            READY_BUDGET.as_secs()
        ),
    );
    Ok(())
}

/// Resolves the loopback HTTP carrier config from the `--http` flag,
/// `--bind`/`--port` args, and the `ZEN_GATEWAY_HTTP_*` env layer.
/// Explicit flag wins; env enables when the flag is absent.
fn resolve_http_carrier(
    http_flag: bool,
    bind: Option<&str>,
    port: Option<u16>,
) -> Option<zen_gateway::transport::http::HttpCarrierConfig> {
    let defaults = HttpConfig::default();
    let env_enabled = std::env::var("ZEN_GATEWAY_HTTP_ENABLED")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let enabled = http_flag || env_enabled;
    if !enabled {
        return None;
    }
    Some(zen_gateway::transport::http::HttpCarrierConfig {
        bind_addr: bind
            .map(str::to_string)
            .or_else(|| std::env::var("ZEN_GATEWAY_HTTP_BIND_ADDR").ok())
            .unwrap_or_else(|| defaults.bind_addr.clone()),
        port: port
            .or_else(|| {
                std::env::var("ZEN_GATEWAY_HTTP_PORT")
                    .ok()
                    .and_then(|v| v.parse().ok())
            })
            .unwrap_or(defaults.port),
    })
}

/// Resolves the QQBot channel from `[channels.qqbot]` (config.toml
/// only, per Phase 13 decision — no CLI/env surface of its own).
///
/// Returns `None` when unset or when credentials are incomplete;
/// endpoint URLs stay at official defaults (tests override via
/// `GatewayDaemonConfig` directly).
fn resolve_qqbot_channel(
    zen_config: &zen_core::config::ZenConfig,
) -> Option<zen_gateway::channel::qqbot::QqBotAdapterOptions> {
    let q = zen_config.channels.qqbot.as_ref()?;
    if q.app_id.is_empty() || q.client_secret.is_empty() {
        tracing::warn!("channels.qqbot set but app_id/client_secret incomplete; channel disabled");
        return None;
    }
    let bindings_db = ZenPaths::detect().ok().map(|p| p.data().join("state.db"))?;
    Some(zen_gateway::channel::qqbot::QqBotAdapterOptions {
        app_id: q.app_id.clone(),
        client_secret: q.client_secret.clone(),
        chat_base: String::new(),
        ws_url: zen_gateway::channel::qqbot::DEFAULT_WS_URL.to_string(),
        api_base: zen_gateway::channel::qqbot::DEFAULT_API_BASE.to_string(),
        token_url: zen_gateway::channel::qqbot::DEFAULT_TOKEN_URL.to_string(),
        allowed_users: q.allowed_users.clone(),
        bindings_db,
        // T101: drain tick from config, clamped 60..=3600 (default 300s).
        outbox_drain_interval: std::time::Duration::from_secs(
            q.outbox_drain_interval_secs.unwrap_or(300).clamp(60, 3600),
        ),
        // Daemon resolves the shared audit sink; CLI side stays None
        // (overridden in serve_with_shutdown).
        audit_path: None,
    })
}

async fn run_uds_foreground(
    http_cfg: Option<zen_gateway::transport::http::HttpCarrierConfig>,
    qqbot_cfg: Option<zen_gateway::channel::qqbot::QqBotAdapterOptions>,
) -> Result<(), ZenError> {
    use std::io::IsTerminal;
    use zen_gateway::{GatewayDaemonConfig, GatewayService};

    // Banner output only for humans at a terminal; spawned/redirected
    // runs (auto-start, pipes) stay machine-quiet.
    let interactive = std::io::stdout().is_terminal();

    // Idle self-exit is an IMPLICIT-spawn-only affordance: chat-booted
    // daemons clean themselves up; explicit `serve start` (no env set)
    // runs until `zen serve stop`. Default mirrors codex's 30-min
    // THREAD_UNLOADING_DELAY constant.
    let idle_exit = std::env::var("ZEN_GATEWAY_IDLE_EXIT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(std::time::Duration::from_secs);
    // Lease-gated scheduler presence: health/status must not claim a
    // scheduler that is still waiting out a TUI-held lease, so the flag
    // flips only after the daemon actually acquires the lease.
    let scheduler_live = Arc::new(AtomicBool::new(false));
    // T154: while the daemon waits for the lease, `scheduler_pending`
    // tells the TUI "an explicit daemon intends to host" so it can yield
    // its in-app scheduler instead of starving the Full profile forever.
    let scheduler_waiting = Arc::new(AtomicBool::new(true));
    let config = GatewayDaemonConfig {
        http: http_cfg,
        qqbot: qqbot_cfg,
        pid_path: Some(pid_path()?),
        idle_exit,
        scheduler_hosted: scheduler_enabled(),
        scheduler_live: Some(scheduler_live.clone()),
        scheduler_waiting: Some(scheduler_waiting.clone()),
        ..GatewayDaemonConfig::default()
    };
    let socket = config.socket_path.display().to_string();

    if scheduler_enabled() {
        let zen_config = zen_core::config::load_config()?;
        let cron = zen_config.cron.clone();
        tokio::spawn(async move {
            // The daemon outlives TUIs, so it retries until the lease is
            // free (typical acquire is immediate: no TUI holds it).
            // `_lease` must stay alive for the scheduler's lifetime —
            // dropping it releases the mutual exclusion.
            let _lease = loop {
                let acquired = match ZenPaths::detect() {
                    Ok(paths) => zen_agents::scheduler::SchedulerLease::try_acquire(&paths),
                    Err(e) => Err(zen_agents::scheduler::LeaseError::OpenFailed(
                        std::io::Error::other(e.to_string()),
                    )),
                };
                match acquired {
                    Ok(lease) => break lease,
                    Err(zen_agents::scheduler::LeaseError::Contention) => {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                    }
                    Err(zen_agents::scheduler::LeaseError::OpenFailed(e)) => {
                        // T164: a real filesystem problem, not coexistence —
                        // surface loudly instead of spinning silently.
                        tracing::error!(
                            error = %e,
                            "scheduler: lease lock file could not be opened; retrying in 30s"
                        );
                        tokio::time::sleep(Duration::from_secs(30)).await;
                    }
                }
            };
            scheduler_live.store(true, std::sync::atomic::Ordering::Relaxed);
            scheduler_waiting.store(false, std::sync::atomic::Ordering::Relaxed);
            let scheduler = zen_agents::scheduler::create_configured_scheduler(&cron);
            scheduler.run().await;
        });
        info!("Background scheduler started");
    } else {
        info!("scheduler disabled (implicit spawn)");
    }

    if interactive {
        println!("{} Gateway daemon started", "✅".green());
        println!("  Socket: {}", socket);
        println!("\nPress Ctrl+C to stop");
    }

    // Graceful drain (T035): the signal flips the external shutdown
    // watch; serve_with_shutdown drains in-flight turns inside its
    // window, cancels stragglers with audits, then returns so we exit 0.
    // Startup failure must surface IMMEDIATELY (codex app-server-daemon
    // pattern) — never idle until a signal arrives.
    let pid_file = pid_path()?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let (done_tx, mut done_rx) = tokio::sync::oneshot::channel::<()>();
    let signal_tx = shutdown_tx.clone();
    let serve_task = tokio::spawn(async move {
        let result = GatewayService::serve_with_shutdown(config, shutdown_tx, shutdown_rx).await;
        let _ = done_tx.send(());
        result
    });
    tokio::select! {
        _ = &mut done_rx => {
            remove_pid(&pid_file).ok();
            match serve_task.await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(ZenError::Service(format!(
                    "gateway failed to start: {e} — fix the cause or run 'zen serve start' in foreground"
                ))),
                Err(e) => Err(ZenError::Service(format!("gateway task panicked: {e}"))),
            }
        }
        _ = wait_for_stop_signal() => {
            signal_tx.send_replace(true);
            // B1 two-tier stop: a SECOND signal during the drain window
            // toggles the watch once more; serve_with_shutdown's drain
            // wait treats any post-shutdown change as force-immediate
            // cancellation (codex ShutdownSignal::Forceable precedent).
            let escalate_tx = signal_tx.clone();
            let escalator = tokio::spawn(async move {
                wait_for_stop_signal().await;
                info!("second stop signal received; escalating to immediate cancel");
                escalate_tx.send_replace(false);
            });
            let served = serve_task.await;
            escalator.abort();
            match served {
                Ok(Ok(())) => {}
                Ok(Err(e)) => return Err(ZenError::Service(format!("gateway drain failed: {e}"))),
                Err(e) => return Err(ZenError::Service(format!("gateway task panicked: {e}"))),
            }
            remove_pid(&pid_file).ok();
            if interactive {
                println!("\nGateway stopped");
            }
            Ok(())
        }
    }
}

/// Async twin of [`block_until_signal`]: resolves on SIGINT/SIGTERM so
/// the surrounding `select!` can race it against early task completion.
async fn wait_for_stop_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        let mut int = signal(SignalKind::interrupt()).expect("install SIGINT handler");
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn run_background(
    path: &Path,
    http_cfg: Option<zen_gateway::transport::http::HttpCarrierConfig>,
    quiet: bool,
) -> Result<(), ZenError> {
    let exe = std::env::current_exe().map_err(|e| ZenError::Service(e.to_string()))?;

    let mut cmd = Command::new(&exe);
    cmd.arg("serve").arg("start").arg("--foreground");

    if http_cfg.is_some() {
        cmd.arg("--http");
    }
    cmd.env(
        "ZEN_GATEWAY_HTTP_ENABLED",
        if http_cfg.is_some() { "1" } else { "0" },
    );

    cmd.stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    let child = cmd
        .spawn()
        .map_err(|e| ZenError::Service(format!("Failed to spawn gateway daemon: {}", e)))?;

    let child_pid = child.id();
    drop(child);

    // Readiness probe instead of liveness guessing: the socket bind is
    // the single-instance arbiter; a losing or crashed child never
    // answers, so failure is reported honestly (codex parity — the
    // probe is ground truth, pid files advisory).
    let deadline = tokio::time::Instant::now() + READY_BUDGET;
    loop {
        // Liveness FIRST: a child that lost the start race exits within
        // milliseconds, and accepting a handshake before noticing would
        // report success with a corpse's pid. The pid-file identity check
        // below is what makes the handshake proof of OUR child rather than of
        // some other daemon that happens to answer the shared socket.
        if !is_pid_alive(child_pid) {
            break;
        }
        let ready = match zen_gateway::client::GatewayClient::connect(uds_socket_path()).await {
            Ok(client) => client
                .handshake("cli-start", "0.0", Default::default())
                .await
                .is_ok(),
            Err(_) => false,
        };
        if ready {
            match read_pid_record(path) {
                Ok(record) if record.pid == child_pid => {
                    ensure_pid_dir(path);
                    report(
                        quiet,
                        format!(
                            "{} Gateway started (background, pid: {})",
                            "✅".green(),
                            child_pid
                        ),
                    );
                    if let Some(cfg) = &http_cfg {
                        report(
                            quiet,
                            format!("  HTTP:     http://{}:{}/health", cfg.bind_addr, cfg.port),
                        );
                    }
                    report(
                        quiet,
                        format!("  Socket:   {}", uds_socket_path().display()),
                    );
                    report(quiet, format!("  PID file: {}", path.display()));
                    report(quiet, "  Run 'zen serve stop' to stop".to_string());
                    return Ok(());
                }
                Ok(record) if pid_record_alive(record.pid, record.start.as_deref()) => {
                    // A different, live daemon owns the socket: our child lost
                    // the race and exited. Report the truth and do not claim
                    // (or clobber) the pid record.
                    report(
                        quiet,
                        format!(
                            "{} Gateway already running (pid: {}); this start attempt exited",
                            "⚠️".yellow(),
                            record.pid
                        ),
                    );
                    return Ok(());
                }
                // Socket answers but the pid file is missing/stale: the daemon
                // may still be finishing startup, so keep polling to the
                // deadline rather than reporting a false verdict.
                _ => {}
            }
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(READY_POLL).await;
    }

    Err(ZenError::Service(
        "Gateway daemon failed to become ready within 10s. Check ~/.zen/logs/gateway-spawn.log"
            .to_string(),
    ))
}

fn print_process_stats(pid: u32) {
    use sysinfo::Pid;
    if pid == 0 {
        return;
    }
    let target = Pid::from_u32(pid);
    let mut system = sysinfo::System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[target]), true);
    if let Some(process) = system.process(target) {
        println!("  CPU:     {:.1}%", process.cpu_usage());
        println!("  Memory:  {} MB", process.memory() / 1024 / 1024);
        println!(
            "  Started: {} (up {}s)",
            chrono::DateTime::from_timestamp(process.start_time() as i64, 0)
                .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_else(|| "unknown".into()),
            process.run_time()
        );
    }
}

async fn run_mcp_stdio() -> Result<(), ZenError> {
    use zen_gateway::McpServer;

    let wiring = zen_agents::wiring::ZenWiring::new();
    let registry = wiring.build_mcp_registry();
    let server = McpServer::with_registry(Default::default(), registry);

    println!(
        "Starting MCP stdio server ({} tools)",
        server.registry().len()
    );
    server
        .start_stdio()
        .await
        .map_err(|e| ZenError::Service(e.to_string()))
}

fn fetch_http_body(host: &str, port: u16, path: &str) -> Option<String> {
    let addr = format!("{}:{}", host, port);
    let mut stream = std::net::TcpStream::connect_timeout(
        &addr.parse::<std::net::SocketAddr>().ok()?,
        std::time::Duration::from_secs(2),
    )
    .ok()?;

    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        path, addr
    );
    stream.write_all(request.as_bytes()).ok()?;

    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;

    if let Some(idx) = response.find("\r\n\r\n") {
        Some(response[idx + 4..].to_string())
    } else {
        Some(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serialize env-mutating tests to prevent cross-thread races on ZEN_SERVE_NO_SCHEDULER.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn scheduler_enabled_by_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("ZEN_SERVE_NO_SCHEDULER") };
        assert!(scheduler_enabled());
    }

    #[test]
    fn scheduler_disabled_when_flag_set() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("ZEN_SERVE_NO_SCHEDULER", "1") };
        assert!(!scheduler_enabled());
        unsafe { std::env::remove_var("ZEN_SERVE_NO_SCHEDULER") };
    }

    #[test]
    fn scheduler_enabled_for_empty_string() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("ZEN_SERVE_NO_SCHEDULER", "") };
        assert!(scheduler_enabled());
        unsafe { std::env::remove_var("ZEN_SERVE_NO_SCHEDULER") };
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn render_plist_contains_required_keys() {
        let plist = render_plist(
            Path::new("/usr/local/bin/zen"),
            Path::new("/home/user/.zen/logs"),
        );
        assert!(plist.contains("<string>dev.zen.serve</string>"));
        assert!(plist.contains("<true/>"));
        assert!(plist.contains("<integer>60</integer>"));
        assert!(plist.contains("/usr/local/bin/zen"));
        assert!(plist.contains("serve"));
        assert!(plist.contains("start"));
        assert!(plist.contains("--foreground"));
        assert!(plist.contains("/home/user/.zen/logs/serve.out.log"));
        assert!(plist.contains("/home/user/.zen/logs/serve.err.log"));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn render_plist_has_environment_variables() {
        let plist = render_plist(Path::new("/opt/homebrew/bin/zen"), Path::new("/tmp/logs"));
        assert!(plist.contains("HOME"));
        assert!(plist.contains("PATH"));
        assert!(plist.contains("/opt/homebrew/bin"));
    }

    #[test]
    fn classify_daemon_version_states() {
        assert_eq!(
            classify_daemon_version("0.0.9", Some("0.0.9")),
            DaemonVersionState::Current
        );
        assert_eq!(
            classify_daemon_version("0.0.9", Some("0.0.8")),
            DaemonVersionState::Stale
        );
        assert_eq!(
            classify_daemon_version("0.0.9", None),
            DaemonVersionState::Unknown
        );
        assert_eq!(
            classify_daemon_version("0.0.9", Some("")),
            DaemonVersionState::Unknown
        );
        assert_eq!(
            classify_daemon_version("0.0.9", Some("  ")),
            DaemonVersionState::Unknown
        );
    }

    #[test]
    fn version_state_staleness_and_wire_names() {
        assert!(!DaemonVersionState::Current.is_stale());
        assert!(DaemonVersionState::Stale.is_stale());
        assert!(
            DaemonVersionState::Unknown.is_stale(),
            "unknown ⇒ STALE-unknown (pre-version build is by definition older)"
        );
        assert_eq!(DaemonVersionState::Current.as_str(), "current");
        assert_eq!(DaemonVersionState::Stale.as_str(), "stale");
        assert_eq!(DaemonVersionState::Unknown.as_str(), "unknown");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn restart_mode_decides_launchd_vs_manual() {
        assert_eq!(restart_mode(true), RestartMode::LaunchdKickstart);
        assert_eq!(restart_mode(false), RestartMode::ManualStopStart);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn kickstart_args_force_restart_the_labeled_service() {
        assert_eq!(kickstart_target("gui/501"), "gui/501/dev.zen.serve");
        assert_eq!(
            kickstart_args("gui/501/dev.zen.serve"),
            vec![
                "kickstart".to_string(),
                "-k".to_string(),
                "gui/501/dev.zen.serve".to_string()
            ]
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn gui_domain_returns_valid_format() {
        let result = gui_domain();
        assert!(result.is_ok(), "gui_domain failed: {:?}", result.err());
        let domain = result.unwrap();
        assert!(domain.starts_with("gui/"));
        let uid_str = domain.strip_prefix("gui/").unwrap();
        assert!(
            !uid_str.is_empty() && uid_str.chars().all(|c| c.is_ascii_digit()),
            "UID must be numeric, got: {uid_str}"
        );
    }
}
