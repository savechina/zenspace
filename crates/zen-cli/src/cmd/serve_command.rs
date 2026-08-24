use clap::Subcommand;
use colored::Colorize;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use tracing::info;

use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;
use zen_gateway::{HttpConfig, read_pid, remove_pid, write_pid};

#[derive(Subcommand)]
pub enum ServeCommands {
    /// Start the gateway server (UDS sole-owner daemon by default)
    Start {
        /// Run in foreground (blocks)
        #[arg(long)]
        foreground: bool,
        /// Legacy HTTP gateway instead of the UDS daemon
        #[arg(long)]
        http: bool,
        /// Quiet mode used by client-side auto-spawn (`--daemonized`)
        #[arg(long)]
        daemonized: bool,
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
    /// Show gateway server status
    Status,
    /// Test MCP server connectivity
    Test {
        /// Port of the gateway (default: 9876)
        #[arg(long)]
        port: Option<u16>,
    },
}

const PID_FILE_NAME: &str = "daemon.pid";

fn uds_socket_path() -> std::path::PathBuf {
    zen_gateway::transport::uds::default_socket_path()
}

fn pid_path() -> Result<std::path::PathBuf, ZenError> {
    let paths = ZenPaths::detect()?;
    Ok(paths.global_root().join(PID_FILE_NAME))
}

fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn check_stale_pid(path: &Path) -> Result<(), ZenError> {
    if path.exists() {
        let pid = read_pid(path).map_err(|e| ZenError::Service(e.to_string()))?;
        if is_process_alive(pid) {
            return Err(ZenError::Service(format!(
                "Gateway already running (pid: {}). Run 'zen serve stop' first.",
                pid
            )));
        }
        println!(
            "{} Cleaned up stale PID file (pid: {} was dead)",
            "🧹".yellow(),
            pid
        );
        remove_pid(path).ok();
    }
    Ok(())
}

fn ensure_pid_dir(path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
}

pub async fn execute_command(operation: &ServeCommands) -> Result<(), ZenError> {
    match operation {
        ServeCommands::Start {
            foreground,
            http,
            daemonized,
            bind,
            port,
            mcp,
        } => {
            if *mcp {
                return run_mcp_stdio().await;
            }
            let path = pid_path()?;
            check_stale_pid(&path)?;
            ensure_pid_dir(&path);
            // `--http` now enables the loopback HTTP carrier alongside the
            // UDS daemon (T046: legacy HttpGateway retired); env opt-in
            // also honored per FR-019 config layering.
            let http_cfg = resolve_http_carrier(*http, bind.as_deref(), *port);
            write_pid(&path).map_err(|e| ZenError::Service(e.to_string()))?;
            if *foreground {
                return run_uds_foreground(*daemonized, http_cfg).await;
            }
            run_background(&path, http_cfg)
        }
        ServeCommands::Stop => {
            // Preferred path: graceful `shutdown` RPC over the UDS socket.
            if let Ok(client) = zen_gateway::client::GatewayClient::connect(uds_socket_path()).await
            {
                let _ = client
                    .handshake("cli-stop", "0.0", Default::default())
                    .await;
                if let Ok(result) = client.request("shutdown", serde_json::json!({})).await {
                    remove_pid(&pid_path()?).ok();
                    println!(
                        "{} Gateway stopped via socket (drained: {}, cancelled: {})",
                        "✅".green(),
                        result["drained"],
                        result["cancelled"]
                    );
                    return Ok(());
                }
            }

            let path = pid_path()?;
            if !path.exists() {
                println!("Gateway not running (no PID file)");
                return Ok(());
            }

            let pid = read_pid(&path).map_err(|e| ZenError::Service(e.to_string()))?;

            if !is_process_alive(pid) {
                println!("Gateway process not responding (pid: {})", pid);
                remove_pid(&path).map_err(|e| ZenError::Service(e.to_string()))?;
                return Ok(());
            }

            #[cfg(unix)]
            {
                let result = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
                if result == 0 {
                    info!("Sent SIGTERM to gateway (pid: {})", pid);
                    println!("Sent stop signal to gateway (pid: {})", pid);
                } else {
                    println!("Failed to send signal to gateway (pid: {})", pid);
                }

                for i in 0..20 {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    if !is_process_alive(pid) {
                        break;
                    }
                    if i == 10 {
                        println!("Process not responding, force killing...");
                        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        break;
                    }
                }
            }

            #[cfg(not(unix))]
            {
                println!("Stop signal sent (pid: {})", pid);
            }

            remove_pid(&path).map_err(|e| ZenError::Service(e.to_string()))?;
            println!("{} Gateway stopped", "✅".green());
            Ok(())
        }
        ServeCommands::Status => {
            // Preferred path: live health/status over the UDS socket.
            if let Ok(client) = zen_gateway::client::GatewayClient::connect(uds_socket_path()).await
                && client
                    .handshake("cli-status", "0.0", Default::default())
                    .await
                    .is_ok()
                && let Ok(s) = client.request("health/status", serde_json::json!({})).await
            {
                let pid = pid_path().ok().and_then(|p| read_pid(&p).ok());
                println!("{} Gateway running (UDS)", "✅".green());
                if let Some(p) = pid {
                    println!("  PID: {}", p);
                }
                println!("  Socket: {}", uds_socket_path().display());
                println!(
                    "  Version: {} (protocol {})",
                    s["serverVersion"], s["protocolVersion"]
                );
                println!("  Clients: {}", s["clients"]);
                println!("  Store:   {}", s["storeHealth"]);
                println!("  Uptime:  {}ms", s["uptimeMs"]);
                println!("  Turns:   {}", s["activeTurns"]);
                print_process_stats(pid.unwrap_or(0));
                return Ok(());
            }

            let path = pid_path().ok();
            let config = HttpConfig::default();
            let health_url = format!("http://{}:{}/health", config.bind_addr, config.port);

            let addr =
                format!("{}:{}", config.bind_addr, config.port).parse::<std::net::SocketAddr>();

            let http_ok = addr
                .ok()
                .and_then(|addr| {
                    std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(2))
                        .ok()
                })
                .is_some();

            if http_ok {
                let pid = path.as_ref().and_then(|p| read_pid(p).ok());

                println!("{} Gateway running", "✅".green());
                if let Some(p) = pid {
                    println!("  PID: {}", p);
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
            } else if let Some(path) = path {
                if path.exists()
                    && let Ok(pid) = read_pid(&path)
                    && is_process_alive(pid)
                {
                    println!(
                        "{} Gateway process alive (pid: {}) but HTTP not responding",
                        "⚠️".yellow(),
                        pid
                    );
                    return Ok(());
                }
                println!("{} Gateway not running", "⛔".red());
            } else {
                println!("{} Gateway not running", "⛔".red());
            }
            Ok(())
        }
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
    }
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

async fn run_uds_foreground(
    quiet: bool,
    http_cfg: Option<zen_gateway::transport::http::HttpCarrierConfig>,
) -> Result<(), ZenError> {
    use zen_gateway::{GatewayDaemonConfig, GatewayService};

    let config = GatewayDaemonConfig {
        http: http_cfg,
        ..GatewayDaemonConfig::default()
    };
    let socket = config.socket_path.display().to_string();

    let zen_config = zen_core::config::load_config()?;
    let scheduler = zen_agents::scheduler::create_configured_scheduler(&zen_config.cron);
    tokio::spawn(async move {
        scheduler.run().await;
    });
    info!("Background scheduler started");

    if !quiet {
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
            match serve_task.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => return Err(ZenError::Service(format!("gateway drain failed: {e}"))),
                Err(e) => return Err(ZenError::Service(format!("gateway task panicked: {e}"))),
            }
            remove_pid(&pid_file).ok();
            if !quiet {
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

fn run_background(
    path: &Path,
    http_cfg: Option<zen_gateway::transport::http::HttpCarrierConfig>,
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

    std::thread::sleep(std::time::Duration::from_millis(500));

    if is_process_alive(child_pid) {
        ensure_pid_dir(path);
        write_pid(path).map_err(|e| ZenError::Service(e.to_string()))?;

        println!(
            "{} Gateway started (background, pid: {})",
            "✅".green(),
            child_pid
        );
        if let Some(cfg) = &http_cfg {
            println!("  HTTP:     http://{}:{}/health", cfg.bind_addr, cfg.port);
        }
        println!("  Socket:   {}", uds_socket_path().display());
        println!("  PID file: {}", path.display());
        println!("  Run 'zen serve stop' to stop");
    } else {
        return Err(ZenError::Service(
            "Gateway daemon failed to start. Check logs for details.".to_string(),
        ));
    }

    Ok(())
}

fn print_process_stats(pid: u32) {
    if pid == 0 {
        return;
    }
    #[cfg(unix)]
    {
        use std::fs;
        let stat_path = format!("/proc/{}/stat", pid);
        if let Ok(content) = fs::read_to_string(&stat_path) {
            let parts: Vec<&str> = content.split_whitespace().collect();
            if parts.len() > 22 {
                let utime: u64 = parts[13].parse().unwrap_or(0);
                let stime: u64 = parts[14].parse().unwrap_or(0);
                let starttime: u64 = parts[21].parse().unwrap_or(0);
                let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
                let uptime_secs = fs::read_to_string("/proc/uptime")
                    .ok()
                    .and_then(|s| s.split_whitespace().next().map(|v| v.to_string()))
                    .and_then(|s| s.parse::<f64>().ok())
                    .unwrap_or(0.0) as u64;
                let process_start_secs = starttime / clk_tck;
                let run_time = uptime_secs.saturating_sub(process_start_secs);

                println!(
                    "  CPU time: {}s user + {}s system",
                    utime / clk_tck,
                    stime / clk_tck
                );
                println!("  Run time: {}s", run_time);

                let status_path = format!("/proc/{}/status", pid);
                if let Ok(status) = fs::read_to_string(&status_path) {
                    for line in status.lines() {
                        if line.starts_with("VmRSS:") {
                            println!("  Memory: {}", line.trim());
                            break;
                        }
                    }
                }
            }
        }
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
