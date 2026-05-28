use clap::Subcommand;
use colored::Colorize;
use std::io::Write;
use tracing::info;

use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;
use zen_gateway::{Gateway, HttpConfig, HttpGateway, McpConfig, McpServer, read_pid, remove_pid, write_pid};

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

fn pid_path() -> Result<std::path::PathBuf, ZenError> {
    let paths = ZenPaths::detect()?;
    Ok(paths.global_root().join("daemon.pid"))
}

pub async fn execute_command(operation: &ServeCommands) -> Result<(), ZenError> {
    match operation {
        ServeCommands::Start {
            foreground,
            bind,
            port,
        } => {
            let mut config = HttpConfig::default();
            if let Some(b) = bind {
                config.bind_addr = b.clone();
            }
            if let Some(p) = port {
                config.port = *p;
            }

            let port = config.port;
            let bind_addr = config.bind_addr.clone();

            let mut gw = HttpGateway::new(config);
            gw.start(port)
                .map_err(|e| ZenError::Service(e.to_string()))?;

            if *foreground {
                println!("{} Gateway started", "✅".green());
                println!("  Listening on http://{}:{}", bind_addr, port);
                println!("  Health: http://{}:{}/health", bind_addr, port);
                println!("  API:    http://{}:{}/api/v1/", bind_addr, port);
                println!("  WS:     ws://{}:{}/api/v1/ws", bind_addr, port);
                println!("\nPress Ctrl+C to stop");

                let path = pid_path()?;
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                write_pid(&path).ok();

                block_until_signal();

                let _ = gw.stop();
                remove_pid(&path).ok();
                println!("\nGateway stopped");
            } else {
                let path = pid_path()?;
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                write_pid(&path).map_err(|e| ZenError::Service(e.to_string()))?;
                println!("{} Gateway started (background)", "✅".green());
                println!("  Listening on http://{}:{}", bind_addr, port);
                println!("  PID file: {}", path.display());
                println!("  Run 'zen serve stop' to stop");
            }
            Ok(())
        },
        ServeCommands::Stop => {
            let path = pid_path()?;
            if path.exists() {
                let pid = read_pid(&path).map_err(|e| ZenError::Service(e.to_string()))?;

                #[cfg(unix)]
                {
                    let result = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
                    if result == 0 {
                        info!("Sent SIGTERM to gateway (pid: {})", pid);
                        println!("Sent stop signal to gateway (pid: {})", pid);
                    } else {
                        println!("Gateway process not responding (pid: {})", pid);
                    }
                }

                #[cfg(not(unix))]
                {
                    println!("Stop signal sent (pid: {})", pid);
                }

                remove_pid(&path).map_err(|e| ZenError::Service(e.to_string()))?;
            } else {
                println!("Gateway not running (no PID file)");
            }
            Ok(())
        },
        ServeCommands::Status => {
            let path = pid_path()?;
            if path.exists() {
                let pid = read_pid(&path).map_err(|e| ZenError::Service(e.to_string()))?;

                #[cfg(unix)]
                {
                    let alive = unsafe { libc::kill(pid as i32, 0) == 0 };
                    if alive {
                        println!("{} Gateway running (pid: {})", "✅".green(), pid);

                        let config = HttpConfig::default();
                        println!(
                            "  Health: http://{}:{}/health",
                            config.bind_addr, config.port
                        );
                    } else {
                        println!(
                            "{} Gateway stale (pid file exists, process dead)",
                            "⚠️".yellow()
                        );
                        println!("  Run 'zen serve stop' to clean up");
                    }
                }

                #[cfg(not(unix))]
                {
                    println!("Gateway PID: {}", pid);
                }
            } else {
                println!("{} Gateway not running", "⛔".red());
            }
            Ok(())
        },
        ServeCommands::Test { port } => {
            let config = HttpConfig::default();
            let port = port.unwrap_or(config.port);
            let bind_addr = &config.bind_addr;

            println!("{} MCP Connectivity Test", "━━━".bold());
            println!();

            let http_addr = format!("{}:{}", bind_addr, port);
            print!("  HTTP Gateway ({}) ... ", http_addr);
            std::io::stdout().flush().ok();

            let http_result = std::net::TcpStream::connect_timeout(
                &http_addr.parse::<std::net::SocketAddr>().map_err(|e| ZenError::Service(e.to_string()))?,
                std::time::Duration::from_secs(3),
            );

            match http_result {
                Ok(_) => {
                    println!("{}", "OK".green());
                    println!("    Port {} is open", port);
                },
                Err(e) => {
                    println!("{} {}", "FAIL".red(), e);
                    println!("    Gateway may not be running");
                },
            }

            println!();

            print!("  MCP Loopback      ... ");
            std::io::stdout().flush().ok();

            let mcp = McpServer::new(McpConfig::default());
            let tool_count = mcp.registry().len();
            match mcp.health_check().await {
                Ok(_) => {
                    println!("{}", "OK".green());
                    println!("    Tools registered: {}", tool_count);
                },
                Err(e) => {
                    println!("{} {}", "FAIL".red(), e);
                },
            }

            println!();
            println!("  TCP Port check: {}", port);
            println!("  MCP status: check above");
            println!();
            println!("Complete report: {} zen serve status", "→".blue().italic());

            Ok(())
        },
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
