use std::path::PathBuf;

use clap::Subcommand;
use colored::Colorize;
use tracing::{debug, warn};

use zen_core::errors::ZenError;
use zen_plugin::{Lifecycle, PluginEntry, PluginKind, PluginRegistry};

#[derive(Subcommand)]
pub enum PluginCommands {
    /// List installed plugins
    List {
        #[arg(short, long)]
        kind: Option<String>,

        #[arg(short, long)]
        enabled: bool,
    },
    /// Install a plugin from a directory
    Install {
        /// Path to plugin directory (containing manifest.toml)
        path: PathBuf,
    },
    /// Remove a plugin
    Remove {
        /// Plugin ID to remove
        id: String,
    },
    /// Enable a plugin
    Enable {
        /// Plugin ID to enable
        id: String,
    },
    /// Disable a plugin
    Disable {
        /// Plugin ID to disable
        id: String,
    },
    /// Discover and load plugins from plugin directory
    Discover,
    /// List all registered agent tools
    Tools,
    /// Manage MCP server trust (which servers the agent may connect to)
    Mcp {
        #[command(subcommand)]
        operation: McpCommands,
    },
}

#[derive(Subcommand)]
pub enum McpCommands {
    /// List configured MCP servers and their trust status
    List,
    /// Trust an MCP server by name
    Trust {
        /// MCP server name (must exist in config mcp_servers)
        name: String,
        /// Skip the interactive confirmation prompt (FR-018)
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Revoke trust from an MCP server by name
    Untrust {
        /// MCP server name
        name: String,
    },
    /// Force a reconnect (connectivity smoke test) to an MCP server (D4)
    Reconnect {
        /// MCP server name (must exist in config mcp_servers)
        name: String,
    },
}

pub fn execute_command(operation: &PluginCommands) -> Result<(), ZenError> {
    let mut registry = PluginRegistry::new();

    match operation {
        PluginCommands::List { kind, enabled } => {
            debug!(
                "listing {} plugins from {}",
                if *enabled { "enabled" } else { "all" },
                registry.plugin_dir().display()
            );

            registry
                .discover()
                .map_err(|e| ZenError::Service(format!("Failed to discover plugins: {}", e)))?;

            let entries: Vec<_> = if let Some(kind_str) = kind {
                let kind = match kind_str.as_str() {
                    "tool" => PluginKind::Tool,
                    "hook" => PluginKind::Hook,
                    _ => {
                        warn!("Unknown plugin kind: {}, defaulting to tool", kind_str);
                        PluginKind::Tool
                    }
                };
                if *enabled {
                    registry
                        .list_by_kind(&kind)
                        .filter(|e| e.enabled)
                        .cloned()
                        .collect()
                } else {
                    registry.list_by_kind(&kind).cloned().collect()
                }
            } else if *enabled {
                registry.list_enabled().cloned().collect()
            } else {
                registry.list().cloned().collect()
            };

            if entries.is_empty() {
                println!("{} No plugins installed", "⛔".red());
                println!("  Plugin directory: {}", registry.plugin_dir().display());
                return Ok(());
            }

            println!("{}", "Plugins".bold());
            for entry in &entries {
                let status = match entry.lifecycle {
                    Lifecycle::Running => "🟢".to_string(),
                    Lifecycle::Stopped => "⚫".to_string(),
                    Lifecycle::Failed => "🔴".to_string(),
                    _ => "⚪".to_string(),
                };
                let enabled_str = if entry.enabled {
                    "enabled".green()
                } else {
                    "disabled".red()
                };
                let kind_str = format!("{:?}", entry.manifest.kind).to_lowercase();

                println!(
                    "  {} {} {} v{} [{}] ({})",
                    status,
                    entry.manifest.id.bold(),
                    entry.manifest.name.dimmed(),
                    entry.manifest.version,
                    kind_str.cyan(),
                    enabled_str
                );
            }
            println!(
                "\n  Total: {} plugins",
                entries.len().to_string().green().bold()
            );
            Ok(())
        }

        PluginCommands::Install { path } => {
            let manifest_path = path.join("manifest.toml");
            if !manifest_path.exists() {
                println!(
                    "{} No manifest.toml found at: {}",
                    "✗".red().bold(),
                    path.display()
                );
                return Ok(());
            }

            let entry = PluginEntry::from_manifest_path(&manifest_path)
                .map_err(|e| ZenError::Service(format!("Failed to load manifest: {}", e)))?;

            let plugin_id = entry.manifest.id.clone();
            let version = entry.manifest.version.clone();
            registry
                .register(entry)
                .map_err(|e| ZenError::Service(format!("Plugin registration failed: {}", e)))?;

            let target_dir = registry.plugin_dir().join(&plugin_id);
            if target_dir.exists() {
                println!(
                    "{} Plugin '{}' is already installed at: {}",
                    "⚠️".yellow(),
                    plugin_id,
                    target_dir.display()
                );
                return Ok(());
            }

            std::fs::create_dir_all(target_dir.parent().unwrap_or(&target_dir)).ok();
            if let Err(e) = copy_dir_all(path, &target_dir) {
                registry.unregister(&plugin_id).ok();
                return Err(ZenError::Service(format!("Failed to copy plugin: {}", e)));
            }

            // FR-043: verify integrity of the installed copy before declaring success.
            let installed_manifest_path = target_dir.join("manifest.toml");
            let installed_entry = match PluginEntry::from_manifest_path(&installed_manifest_path) {
                Ok(entry) => entry,
                Err(e) => {
                    registry.unregister(&plugin_id).ok();
                    std::fs::remove_dir_all(&target_dir).ok();
                    return Err(ZenError::Service(format!(
                        "Failed to load installed manifest: {}",
                        e
                    )));
                }
            };
            if let Err(e) = installed_entry.verify_integrity() {
                registry.unregister(&plugin_id).ok();
                std::fs::remove_dir_all(&target_dir).ok();
                return Err(ZenError::Service(format!(
                    "Plugin '{}' failed integrity verification: {}",
                    plugin_id, e
                )));
            }

            println!(
                "{} Plugin '{}' installed successfully",
                "✓".green().bold(),
                plugin_id.cyan().bold()
            );
            println!("  Version: {}", version);
            println!("  Located: {}", target_dir.display());
            Ok(())
        }

        PluginCommands::Remove { id } => {
            debug!("removing plugin: {}", id);

            let plugin_dir = registry.plugin_dir().join(id);
            if !plugin_dir.exists() {
                println!(
                    "{} Plugin '{}' not found at: {}",
                    "✗".red().bold(),
                    id,
                    plugin_dir.display()
                );
                return Ok(());
            }

            registry
                .unregister(id)
                .map_err(|e| ZenError::Service(format!("Plugin not registered: {}", e)))
                .ok();

            std::fs::remove_dir_all(&plugin_dir).map_err(|e| {
                ZenError::Service(format!("Failed to remove plugin directory: {}", e))
            })?;

            println!(
                "{} Plugin '{}' removed",
                "✓".green().bold(),
                id.cyan().bold()
            );
            Ok(())
        }

        PluginCommands::Enable { id } => {
            debug!("enabling plugin: {}", id);

            registry
                .discover()
                .map_err(|e| ZenError::Service(format!("Failed to discover plugins: {}", e)))?;

            registry
                .enable(id)
                .map_err(|e| ZenError::Service(format!("Failed to enable plugin: {}", e)))?;

            println!(
                "{} Plugin '{}' enabled",
                "✓".green().bold(),
                id.cyan().bold()
            );
            Ok(())
        }

        PluginCommands::Disable { id } => {
            debug!("disabling plugin: {}", id);

            registry
                .discover()
                .map_err(|e| ZenError::Service(format!("Failed to discover plugins: {}", e)))?;

            registry
                .disable(id)
                .map_err(|e| ZenError::Service(format!("Failed to disable plugin: {}", e)))?;

            println!(
                "{} Plugin '{}' disabled",
                "✓".green().bold(),
                id.cyan().bold()
            );
            Ok(())
        }

        PluginCommands::Discover => {
            let count = registry
                .discover()
                .map_err(|e| ZenError::Service(format!("Plugin discovery failed: {}", e)))?;

            println!(
                "{} Discovered {} plugins from {}",
                "✓".green().bold(),
                count.to_string().green().bold(),
                registry.plugin_dir().display()
            );

            for entry in registry.list() {
                println!(
                    "  {} {} v{} [{}]",
                    entry.manifest.id.cyan(),
                    entry.manifest.name.dimmed(),
                    entry.manifest.version,
                    format!("{:?}", entry.manifest.kind).to_lowercase()
                );
            }
            Ok(())
        }

        PluginCommands::Tools => {
            let wiring = zen_agents::wiring::ZenWiring::new();
            println!(
                "{} Registered Agent Tools ({} total)\n",
                "─".dimmed(),
                wiring.tools.len()
            );
            let mut names: Vec<&String> = wiring.tool_sensitivity.keys().collect();
            names.sort();
            for name in names {
                let sensitivity = &wiring.tool_sensitivity[name];
                let sens_label = match sensitivity {
                    zen_core::types::Sensitivity::Public => "PUBLIC".green(),
                    zen_core::types::Sensitivity::Private => "PRIVATE".yellow(),
                    zen_core::types::Sensitivity::Confidential => "CONFIDENTIAL".red(),
                };
                if wiring.tools.get(name).is_ok() {
                    let source = if name.contains('.')
                        && !name.starts_with("fs.")
                        && !name.starts_with("web.")
                        && !name.starts_with("system.")
                        && !name.starts_with("plugin.")
                        && !name.starts_with("shell.")
                    {
                        "plugin"
                    } else {
                        "builtin"
                    };
                    println!(
                        "  {} [{}] [{}] {}",
                        name.cyan(),
                        sens_label,
                        source,
                        "✓".green()
                    );
                }
            }
            Ok(())
        }

        PluginCommands::Mcp { operation } => execute_mcp_command(operation),
    }
}

fn execute_mcp_command(operation: &McpCommands) -> Result<(), ZenError> {
    let paths = zen_core::paths::ZenPaths::detect()
        .map_err(|e| ZenError::Service(format!("Failed to detect zen paths: {}", e)))?;
    let config = zen_core::config::load_config()
        .map_err(|e| ZenError::Service(format!("Failed to load config: {}", e)))?;
    let mut trust_store = zen_core::config::McpTrustStore::load(&paths)
        .map_err(|e| ZenError::Service(format!("Failed to load MCP trust store: {}", e)))?;

    match operation {
        McpCommands::List => {
            if config.mcp_servers.is_empty() {
                println!(
                    "{} No MCP servers configured. Add a [mcp_servers] section to config.toml.",
                    "⛔".red()
                );
                return Ok(());
            }
            println!("{}", "MCP Servers".bold());
            for server in &config.mcp_servers {
                let trusted = trust_store.is_trusted(&server.name);
                let trust_label = if trusted {
                    "trusted".green()
                } else {
                    "untrusted".red()
                };
                let enabled_label = if server.enabled {
                    "enabled".green()
                } else {
                    "disabled".yellow()
                };
                let transport = &server.transport;
                let command = server.command.as_deref().unwrap_or("-");
                println!(
                    "  {} {} [{}] [{}] (transport: {}, cmd: {})",
                    if trusted { "🔓" } else { "🔒" },
                    server.name.cyan().bold(),
                    trust_label,
                    enabled_label,
                    transport.dimmed(),
                    command.dimmed()
                );
            }
            println!(
                "\n  Trust a server with: {}",
                "zen plugin mcp trust <name>".green()
            );
            Ok(())
        }

        McpCommands::Trust { name, yes } => {
            let server = match config.mcp_servers.iter().find(|s| s.name == *name) {
                Some(s) => s,
                None => {
                    println!(
                        "{} MCP server '{}' not found in config mcp_servers.",
                        "✗".red().bold(),
                        name.cyan()
                    );
                    println!(
                        "  Configured servers: {}",
                        config
                            .mcp_servers
                            .iter()
                            .map(|s| s.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    return Ok(());
                }
            };

            // FR-018: show the subprocess that will be launched and require
            // confirmation unless --yes was passed.
            if !*yes {
                let command = server.command.as_deref().unwrap_or("(none)");
                let args = server.args.clone().unwrap_or_default().join(" ");
                println!(
                    "{} Server '{}' will launch subprocess '{} {}'.",
                    "⚠️".yellow(),
                    name.cyan().bold(),
                    command,
                    args
                );
                print!("Trust this server? [y/N] ");
                use std::io::{BufRead, Write};
                let _ = std::io::stdout().flush();
                let mut line = String::new();
                if std::io::stdin().lock().read_line(&mut line).is_err() {
                    println!("{} trust cancelled (no input)", "✗".red());
                    return Ok(());
                }
                let confirmed = matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes");
                if !confirmed {
                    println!("{} trust cancelled", "✗".red());
                    return Ok(());
                }
            }

            trust_store.set_trusted(name, true);
            trust_store
                .save(&paths)
                .map_err(|e| ZenError::Service(format!("Failed to save trust store: {}", e)))?;
            println!(
                "{} MCP server '{}' trusted — its tools will be registered for agent use.",
                "✓".green().bold(),
                name.cyan().bold()
            );
            Ok(())
        }

        McpCommands::Untrust { name } => {
            trust_store.set_trusted(name, false);
            trust_store
                .save(&paths)
                .map_err(|e| ZenError::Service(format!("Failed to save trust store: {}", e)))?;
            println!(
                "{} MCP server '{}' untrusted — its tools will be skipped.",
                "✓".green().bold(),
                name.cyan().bold()
            );
            Ok(())
        }

        McpCommands::Reconnect { name } => {
            let server = match config.mcp_servers.iter().find(|s| s.name == *name) {
                Some(s) => s,
                None => {
                    println!(
                        "{} MCP server '{}' not found in config mcp_servers.",
                        "✗".red().bold(),
                        name.cyan()
                    );
                    println!(
                        "  Configured servers: {}",
                        config
                            .mcp_servers
                            .iter()
                            .map(|s| s.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    return Ok(());
                }
            };

            // Warn if untrusted but don't block — the user explicitly
            // asked to reconnect, so this doubles as a connectivity test.
            if !trust_store.is_trusted(name) {
                println!(
                    "{} Server '{}' is untrusted — run `zen plugin mcp trust {}` first to register its tools.",
                    "⚠️".yellow(),
                    name.cyan().bold(),
                    name
                );
            }

            println!(
                "{} Reconnecting to MCP server '{}' (transport: {})…",
                "→".cyan(),
                name.cyan().bold(),
                server.transport.dimmed()
            );

            let server_clone = server.clone();
            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(
                    zen_plugin::tools::mcp_client::reconnect_mcp_server(&server_clone),
                )
            });

            match result {
                Ok(count) => {
                    println!(
                        "{} MCP server '{}' reachable — {} tool(s) discovered.",
                        "✓".green().bold(),
                        name.cyan().bold(),
                        count.to_string().green().bold()
                    );
                    println!(
                        "  {} Tools will be registered on next agent start.",
                        "ℹ".dimmed()
                    );
                    Ok(())
                }
                Err(e) => {
                    println!(
                        "{} Reconnect failed for '{}': {}",
                        "✗".red().bold(),
                        name.cyan().bold(),
                        e
                    );
                    Ok(())
                }
            }
        }
    }
}

fn copy_dir_all(src: &PathBuf, dst: &PathBuf) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}
