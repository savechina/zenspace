use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, info, warn};

use zen_core::paths::ZenPaths;
use zen_core::tempfile_lifecycle::TempfileDropGuard;

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

    /// Manifest declares a `*.wasm` entry without the required `sha256`
    /// (FR-049a). Deliberate recovery: `zen plugin install` rewrites the
    /// hash automatically, or `zen plugin rehash <id>` after updates.
    #[error(
        "manifest at {path} declares a *.wasm entry without the required sha256 — run `zen plugin install` (auto-writes the hash) or `zen plugin rehash <id>` after deliberate updates"
    )]
    MissingHash { path: String },

    #[error(
        "sha256 mismatch for plugin {id} entry {path}: expected {expected}, got {actual} — run `zen plugin rehash {id}` after deliberate updates"
    )]
    HashMismatch {
        id: String,
        path: String,
        expected: String,
        actual: String,
    },

    #[error("code signature invalid for {path}: {reason}")]
    CodeSignInvalid { path: String, reason: String },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    Tool,
    Hook,
}

/// Namespace prefixes reserved for builtin tools (FR-050, eng review 2026-08-15).
/// Single source of truth — shared by plugin id/name validation (registry),
/// the tool-registration collision guard (`PluginApi::register_tool`), and
/// agent grant matching (the `plugin:*` wildcard excludes reserved namespaces).
pub const RESERVED_NAMESPACE_PREFIXES: &[&str] = &["fs", "web", "system", "plugin", "shell"];

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
    /// Path to the plugin entry file (`.wasm` or `.so`/`.dylib`), relative to the plugin dir.
    #[serde(default)]
    pub entry: Option<String>,
    /// SHA-256 of the entry file. REQUIRED for `*.wasm` entries (FR-049);
    /// absent → rejected at discovery. Optional (warn-only) for native entries.
    #[serde(default)]
    pub sha256: Option<String>,
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

    /// Verify entry-file integrity (FR-043 / FR-049a): SHA-256 against the
    /// manifest's `sha256` field, plus `codesign --verify --strict` for
    /// native `.dylib` entries on macOS. A `*.wasm` entry without a declared
    /// `sha256` is rejected (`MissingHash`); native entries without a hash
    /// only warn.
    pub fn verify_integrity(&self) -> Result<(), PluginRegistryError> {
        let id = &self.manifest.id;

        if let Some(entry) = &self.manifest.entry
            && entry.ends_with(".dylib")
        {
            verify_codesign(&self.dir.join(entry))?;
        }

        if self
            .manifest
            .entry
            .as_deref()
            .is_some_and(|e| e.ends_with(".wasm"))
            && self.manifest.sha256.is_none()
        {
            return Err(PluginRegistryError::MissingHash {
                path: self.dir.join("manifest.toml").display().to_string(),
            });
        }

        let Some(expected) = &self.manifest.sha256 else {
            warn!("plugin {id} has no sha256 in manifest — integrity unverified");
            return Ok(());
        };

        let entry_name =
            self.manifest
                .entry
                .as_ref()
                .ok_or_else(|| PluginRegistryError::InvalidManifest {
                    path: self.dir.join("manifest.toml").display().to_string(),
                    reason: "sha256 declared but manifest has no entry file".to_string(),
                })?;

        let entry_path = self.dir.join(entry_name);
        let actual = compute_sha256(&entry_path)?;
        if actual != *expected {
            return Err(PluginRegistryError::HashMismatch {
                id: id.clone(),
                path: entry_path.display().to_string(),
                expected: expected.clone(),
                actual,
            });
        }

        Ok(())
    }

    /// Recompute the entry file's SHA-256 and write it into this plugin's
    /// manifest.toml (FR-049b/FR-049c — `zen plugin install` auto-write and
    /// `zen plugin rehash`). Other manifest fields are preserved. Returns
    /// the freshly computed hash.
    pub fn rehash_manifest(&self) -> Result<String, PluginRegistryError> {
        let manifest_path = self.dir.join("manifest.toml");

        let entry_name =
            self.manifest
                .entry
                .as_ref()
                .ok_or_else(|| PluginRegistryError::InvalidManifest {
                    path: manifest_path.display().to_string(),
                    reason: "cannot rehash: manifest declares no entry file".to_string(),
                })?;

        let hash = compute_sha256(&self.dir.join(entry_name))?;

        let mut manifest = self.manifest.clone();
        manifest.sha256 = Some(hash.clone());
        write_manifest(&manifest, &manifest_path)?;
        Ok(hash)
    }
}

fn compute_sha256(path: &Path) -> Result<String, PluginRegistryError> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).map_err(PluginRegistryError::Io)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect())
}

fn write_manifest(manifest: &Manifest, manifest_path: &Path) -> Result<(), PluginRegistryError> {
    let mut table =
        toml::Value::try_from(manifest).map_err(|e| PluginRegistryError::InvalidManifest {
            path: manifest_path.display().to_string(),
            reason: format!("failed to serialize manifest: {e}"),
        })?;

    // TOML requires scalar keys before sub-table keys within a table.
    // `config_schema` is the only table-valued field, so re-emit it last
    // to keep round-trips valid when a schema is declared.
    if let toml::Value::Table(map) = &mut table
        && let Some(schema) = map.remove("config_schema")
    {
        map.insert("config_schema".to_string(), schema);
    }

    let content = toml::to_string(&table).map_err(|e| PluginRegistryError::InvalidManifest {
        path: manifest_path.display().to_string(),
        reason: format!("failed to serialize manifest: {e}"),
    })?;
    std::fs::write(manifest_path, content).map_err(PluginRegistryError::Io)?;
    Ok(())
}

/// Plugin id charset validation (FR-050a): ids must match `^[a-z0-9_-]+$`
/// (ASCII lowercase, digits, underscore, hyphen; non-empty). Reserved
/// namespace prefixes ([`RESERVED_NAMESPACE_PREFIXES`]) are a separate
/// validation layer and are NOT checked here.
pub fn is_valid_plugin_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// File name of the persisted plugin state at the plugin dir root (FR-047).
const STATE_FILE: &str = "state.json";

/// Persisted plugin state (FR-047): the disabled-plugin list, stored as
/// `{"disabled": ["<id>", ...]}` in `<plugin_dir>/state.json` — next to the
/// plugin folders, at the root `discover()` scans. The plugin dir is the
/// source of truth for plugin state (Lapce/Obsidian pattern, no DB tables).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginState {
    /// Ids of plugins disabled via `zen plugin disable`.
    #[serde(default)]
    pub disabled: Vec<String>,
}

impl PluginState {
    pub fn is_disabled(&self, id: &str) -> bool {
        self.disabled.iter().any(|d| d.as_str() == id)
    }

    /// Mark `id` disabled (idempotent).
    pub fn disable(&mut self, id: &str) {
        if !self.is_disabled(id) {
            self.disabled.push(id.to_string());
        }
    }

    /// Remove `id` from the disabled list (idempotent).
    pub fn enable(&mut self, id: &str) {
        self.disabled.retain(|d| d.as_str() != id);
    }

    pub fn path(plugin_dir: &Path) -> PathBuf {
        plugin_dir.join(STATE_FILE)
    }

    /// Load persisted state from `<plugin_dir>/state.json`.
    ///
    /// Missing file → empty state. Corrupt or unreadable file → **fail
    /// open**: all plugins enabled, never silently disabled — a loud `warn`
    /// is logged and an audit note appended (FR-047). Never panics.
    pub fn load(plugin_dir: &Path) -> Self {
        let path = Self::path(plugin_dir);
        match std::fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str::<Self>(&content) {
                Ok(state) => state,
                Err(e) => {
                    warn!(
                        "corrupt plugin state at {}: {} — failing open (all plugins enabled)",
                        path.display(),
                        e
                    );
                    audit_state_fail_open(&path, &format!("corrupt state.json: {e}"));
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                warn!(
                    "unreadable plugin state at {}: {} — failing open (all plugins enabled)",
                    path.display(),
                    e
                );
                audit_state_fail_open(&path, &format!("unreadable state.json: {e}"));
                Self::default()
            }
        }
    }

    /// Persist state atomically (FR-040 write-then-rename): serialize to a
    /// temp file in the same directory, then rename over `state.json`. The
    /// drop guard removes the temp file if the rename fails or we unwind.
    pub fn save(&self, plugin_dir: &Path) -> Result<(), PluginRegistryError> {
        std::fs::create_dir_all(plugin_dir).map_err(PluginRegistryError::Io)?;

        let path = Self::path(plugin_dir);
        let tmp_path = plugin_dir.join(format!("{STATE_FILE}.tmp"));

        let content =
            serde_json::to_string_pretty(self).map_err(|e| PluginRegistryError::PluginError {
                id: STATE_FILE.to_string(),
                reason: format!("failed to serialize plugin state: {e}"),
            })?;

        std::fs::write(&tmp_path, content).map_err(PluginRegistryError::Io)?;
        let mut guard = TempfileDropGuard::new(&tmp_path);
        match std::fs::rename(&tmp_path, &path) {
            Ok(()) => {
                guard.disarm();
                Ok(())
            }
            Err(e) => Err(PluginRegistryError::Io(e)),
        }
    }
}

/// Best-effort audit note for `state.json` fail-open events (FR-047),
/// appended to `logs/audit.jsonl` under the global root. Failures are
/// logged at debug and never propagate.
fn audit_state_fail_open(path: &Path, reason: &str) {
    let Ok(paths) = ZenPaths::detect() else {
        debug!("cannot resolve zen paths for plugin state audit note");
        return;
    };

    let entry = serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "event": "plugin_state_fail_open",
        "path": path.display().to_string(),
        "reason": reason,
        "action": "all plugins enabled",
    });
    let log_path = paths.global_root().join("logs").join("audit.jsonl");
    if let Err(e) = zen_core::jsonl::append_jsonl_line(&log_path, &entry) {
        debug!("failed to append plugin state audit note: {}", e);
    }
}

#[cfg(target_os = "macos")]
fn verify_codesign(path: &Path) -> Result<(), PluginRegistryError> {
    let output = std::process::Command::new("codesign")
        .args(["--verify", "--strict"])
        .arg(path)
        .output()
        .map_err(PluginRegistryError::Io)?;
    if !output.status.success() {
        return Err(PluginRegistryError::CodeSignInvalid {
            path: path.display().to_string(),
            reason: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn verify_codesign(_path: &Path) -> Result<(), PluginRegistryError> {
    Ok(())
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

        let state = PluginState::load(&self.plugin_dir);

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
                Ok(mut entry) => {
                    let id = entry.manifest.id.clone();

                    // FR-050a: invalid ids are never registered.
                    if !is_valid_plugin_id(&id) {
                        let err = PluginRegistryError::InvalidManifest {
                            path: manifest_path.display().to_string(),
                            reason: format!(
                                "invalid plugin id '{id}': ids must match ^[a-z0-9_-]+$"
                            ),
                        };
                        warn!("Rejected plugin at {}: {}", path.display(), err);
                        continue;
                    }

                    // FR-047: disabled plugins are skipped at discovery.
                    if state.is_disabled(&id) {
                        debug!("Skipping disabled plugin: {id}");
                        continue;
                    }

                    if let Err(e) = entry.verify_integrity() {
                        warn!("Plugin {} failed integrity verification: {}", id, e);
                        entry.lifecycle = Lifecycle::Failed;
                        self.plugins.insert(id.clone(), entry);
                        continue;
                    }
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
