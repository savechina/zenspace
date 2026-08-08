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
    /// Start the gateway server
    Start {
        /// Run in foreground (blocks)
        #[arg(long)]
        foreground: bool,
        /// Bind address (default: 127.0.0.1)
        #[arg(long)]
        bind: Option<String>,
        /// Port (default: 9876)
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
            bind,
            port,
            mcp,
        } => {
            if *mcp {
                return run_mcp_stdio().await;
            }
            let path = pid_path()?;
            check_stale_pid(&path)?;

            if *foreground {
                run_foreground(&path, bind.as_deref(), *port)
            } else {
                run_background(&path, bind.as_deref(), *port)
            }
        }
        ServeCommands::Stop => {
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

fn run_foreground(path: &Path, bind: Option<&str>, port: Option<u16>) -> Result<(), ZenError> {
    use zen_gateway::{Gateway, HttpGateway};

    let mut config = HttpConfig::default();
    if let Some(b) = bind {
        config.bind_addr = b.to_string();
    }
    if let Some(p) = port {
        config.port = p;
    }

    let port = config.port;
    let bind_addr = config.bind_addr.clone();

    let mut gw = HttpGateway::new(config);
    gw.start(port)
        .map_err(|e| ZenError::Service(e.to_string()))?;

    write_pid(path).ok();

    let zen_config = zen_core::config::load_config()?;
    let scheduler = zen_agents::scheduler::create_configured_scheduler(&zen_config.cron);
    tokio::spawn(async move {
        scheduler.run().await;
    });
    info!("Background scheduler started");

    println!("{} Gateway started", "✅".green());
    println!("  Listening on http://{}:{}", bind_addr, port);
    println!("  Health: http://{}:{}/health", bind_addr, port);
    println!("  API:    http://{}:{}/api/v1/", bind_addr, port);
    println!("  WS:     ws://{}:{}/api/v1/ws", bind_addr, port);
    println!("\nPress Ctrl+C to stop");

    block_until_signal();

    if let Err(e) = gw.stop() {
        tracing::warn!(error = %e, "failed to stop gateway cleanly");
    }
    remove_pid(path).ok();
    println!("\nGateway stopped");
    Ok(())
}

fn run_background(path: &Path, bind: Option<&str>, port: Option<u16>) -> Result<(), ZenError> {
    let exe = std::env::current_exe().map_err(|e| ZenError::Service(e.to_string()))?;

    let mut cmd = Command::new(&exe);
    cmd.arg("serve").arg("start").arg("--foreground");

    if let Some(b) = bind {
        cmd.arg("--bind").arg(b);
    }
    if let Some(p) = port {
        cmd.arg("--port").arg(p.to_string());
    }

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

        let config = HttpConfig::default();
        let port = port.unwrap_or(config.port);
        let bind_addr = bind.unwrap_or(&config.bind_addr);

        println!(
            "{} Gateway started (background, pid: {})",
            "✅".green(),
            child_pid
        );
        println!("  Listening on http://{}:{}", bind_addr, port);
        println!("  Health: http://{}:{}/health", bind_addr, port);
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

#[cfg(unix)]
fn block_until_signal() {
    use std::sync::atomic::{AtomicBool, Ordering};

    static STOP: AtomicBool = AtomicBool::new(false);

    unsafe {
        libc::signal(
            libc::SIGINT,
            signal_handler as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            signal_handler as *const () as libc::sighandler_t,
        );
    }

    while !STOP.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    extern "C" fn signal_handler(_sig: i32) {
        STOP.store(true, Ordering::Relaxed);
    }
}

#[cfg(not(unix))]
fn block_until_signal() {
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
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
