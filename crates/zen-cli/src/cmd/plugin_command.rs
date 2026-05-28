use std::path::PathBuf;

use clap::Subcommand;
use colored::Colorize;
use tracing::debug;

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
                    "service" => PluginKind::Service,
                    "provider" => PluginKind::Provider,
                    _ => {
                        return Err(ZenError::Service(format!(
                            "Unknown plugin kind: {}",
                            kind_str
                        )));
                    },
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
        },

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

            println!(
                "{} Plugin '{}' installed successfully",
                "✓".green().bold(),
                plugin_id.cyan().bold()
            );
            println!("  Version: {}", version);
            println!("  Located: {}", target_dir.display());
            Ok(())
        },

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
        },

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
        },

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
        },

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
        },
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
