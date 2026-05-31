use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, info, warn};

use zen_core::paths::ZenPaths;

#[derive(Debug, Error)]
pub enum PluginRegistryError {
    #[error("plugin not found: {id}")]
    NotFound { id: String },

    #[error("plugin already registered: {id}")]
    AlreadyRegistered { id: String },

    #[error("failed to read manifest: {path}: {reason}")]
    ManifestLoad { path: String, reason: String },

    #[error("invalid manifest at {path}: {reason}")]
    InvalidManifest { path: String, reason: String },

    #[error("plugin error: {id}: {reason}")]
    PluginError { id: String, reason: String },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    Tool,
    Hook,
    Service,
    Provider,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(rename = "type")]
    pub kind: PluginKind,
    pub permissions: Vec<String>,
    #[serde(default)]
    pub config_schema: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lifecycle {
    Built,
    Starting,
    Running,
    Stopping,
    Stopped,
    Failed,
}

#[derive(Debug, Clone)]
pub struct PluginEntry {
    pub manifest: Manifest,
    pub lifecycle: Lifecycle,
    pub enabled: bool,
    pub dir: PathBuf,
}

impl PluginEntry {
    pub fn new(manifest: Manifest, dir: PathBuf) -> Self {
        Self {
            manifest,
            lifecycle: Lifecycle::Built,
            enabled: true,
            dir,
        }
    }

    pub fn from_manifest_path(manifest_path: &Path) -> Result<Self, PluginRegistryError> {
        let content = std::fs::read_to_string(manifest_path).map_err(|e| {
            PluginRegistryError::ManifestLoad {
                path: manifest_path.display().to_string(),
                reason: e.to_string(),
            }
        })?;

        let manifest: Manifest =
            toml::from_str(&content).map_err(|e| PluginRegistryError::InvalidManifest {
                path: manifest_path.display().to_string(),
                reason: e.to_string(),
            })?;

        let dir = manifest_path
            .parent()
            .expect("manifest must have parent")
            .to_path_buf();

        Ok(Self::new(manifest, dir))
    }
}

pub struct PluginRegistry {
    plugins: HashMap<String, PluginEntry>,
    plugin_dir: PathBuf,
}

impl PluginRegistry {
    pub fn new() -> Self {
        let plugin_dir = ZenPaths::detect()
            .map(|p| p.global_root().join("plugins"))
            .unwrap_or_else(|_| {
                home::home_dir()
                    .map(|h| h.join(".zen").join("plugins"))
                    .unwrap_or_else(|| PathBuf::from(".zen/plugins"))
            });

        Self {
            plugins: HashMap::new(),
            plugin_dir,
        }
    }

    pub fn with_plugin_dir(plugin_dir: PathBuf) -> Self {
        Self {
            plugins: HashMap::new(),
            plugin_dir,
        }
    }

    pub fn plugin_dir(&self) -> &PathBuf {
        &self.plugin_dir
    }

    pub fn discover(&mut self) -> Result<usize, PluginRegistryError> {
        if !self.plugin_dir.exists() {
            std::fs::create_dir_all(&self.plugin_dir).map_err(PluginRegistryError::Io)?;
            info!("Created plugin directory: {}", self.plugin_dir.display());
            return Ok(0);
        }

        let mut count = 0;
        for entry in std::fs::read_dir(&self.plugin_dir).map_err(PluginRegistryError::Io)? {
            let entry = entry.map_err(PluginRegistryError::Io)?;
            let path = entry.path();

            if !path.is_dir() {
                continue;
            }

            let manifest_path = path.join("manifest.toml");
            if !manifest_path.exists() {
                warn!(
                    "Plugin directory {} has no manifest.toml, skipping",
                    path.display()
                );
                continue;
            }

            match PluginEntry::from_manifest_path(&manifest_path) {
                Ok(entry) => {
                    let id = entry.manifest.id.clone();
                    self.plugins.insert(id.clone(), entry);
                    count += 1;
                    debug!(
                        "Discovered plugin: {} v{}",
                        id, self.plugins[&id].manifest.version
                    );
                }
                Err(e) => {
                    warn!("Failed to load plugin at {}: {}", path.display(), e);
                }
            }
        }

        info!(
            "Discovered {} plugins from {}",
            count,
            self.plugin_dir.display()
        );
        Ok(count)
    }

    pub fn register(&mut self, entry: PluginEntry) -> Result<(), PluginRegistryError> {
        let id = entry.manifest.id.clone();
        if self.plugins.contains_key(&id) {
            return Err(PluginRegistryError::AlreadyRegistered { id });
        }
        self.plugins.insert(id, entry);
        Ok(())
    }

    pub fn unregister(&mut self, id: &str) -> Result<PluginEntry, PluginRegistryError> {
        self.plugins
            .remove(id)
            .ok_or_else(|| PluginRegistryError::NotFound { id: id.to_string() })
    }

    pub fn get(&self, id: &str) -> Option<&PluginEntry> {
        self.plugins.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut PluginEntry> {
        self.plugins.get_mut(id)
    }

    pub fn list(&self) -> impl Iterator<Item = &PluginEntry> {
        self.plugins.values()
    }

    pub fn list_by_kind(&self, kind: &PluginKind) -> impl Iterator<Item = &PluginEntry> {
        self.plugins
            .values()
            .filter(move |e| e.manifest.kind == *kind)
    }

    pub fn list_enabled(&self) -> impl Iterator<Item = &PluginEntry> {
        self.plugins.values().filter(|e| e.enabled)
    }

    pub fn list_running(&self) -> impl Iterator<Item = &PluginEntry> {
        self.plugins
            .values()
            .filter(|e| e.lifecycle == Lifecycle::Running)
    }

    pub fn enable(&mut self, id: &str) -> Result<(), PluginRegistryError> {
        let entry = self
            .plugins
            .get_mut(id)
            .ok_or_else(|| PluginRegistryError::NotFound { id: id.to_string() })?;
        entry.enabled = true;
        info!("Plugin enabled: {}", id);
        Ok(())
    }

    pub fn disable(&mut self, id: &str) -> Result<(), PluginRegistryError> {
        let entry = self
            .plugins
            .get_mut(id)
            .ok_or_else(|| PluginRegistryError::NotFound { id: id.to_string() })?;

        if entry.lifecycle == Lifecycle::Running {
            entry.lifecycle = Lifecycle::Stopping;
        }
        entry.enabled = false;
        info!("Plugin disabled: {}", id);
        Ok(())
    }

    pub fn set_lifecycle(
        &mut self,
        id: &str,
        lifecycle: Lifecycle,
    ) -> Result<(), PluginRegistryError> {
        let entry = self
            .plugins
            .get_mut(id)
            .ok_or_else(|| PluginRegistryError::NotFound { id: id.to_string() })?;
        entry.lifecycle = lifecycle;
        Ok(())
    }

    pub fn start(&mut self, id: &str) -> Result<(), PluginRegistryError> {
        let entry = self
            .plugins
            .get_mut(id)
            .ok_or_else(|| PluginRegistryError::NotFound { id: id.to_string() })?;

        if !entry.enabled {
            return Err(PluginRegistryError::PluginError {
                id: id.to_string(),
                reason: "plugin is disabled".to_string(),
            });
        }

        entry.lifecycle = Lifecycle::Starting;
        entry.lifecycle = Lifecycle::Running;
        info!("Plugin started: {}", id);
        Ok(())
    }

    pub fn stop(&mut self, id: &str) -> Result<(), PluginRegistryError> {
        let entry = self
            .plugins
            .get_mut(id)
            .ok_or_else(|| PluginRegistryError::NotFound { id: id.to_string() })?;

        entry.lifecycle = Lifecycle::Stopping;
        entry.lifecycle = Lifecycle::Stopped;
        info!("Plugin stopped: {}", id);
        Ok(())
    }

    pub fn count(&self) -> usize {
        self.plugins.len()
    }

    pub fn ids(&self) -> Vec<&str> {
        self.plugins.keys().map(|k| k.as_str()).collect()
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}
