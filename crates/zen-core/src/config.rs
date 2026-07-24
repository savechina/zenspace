use include_dir::{Dir, include_dir};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
#[cfg(not(test))]
use std::sync::OnceLock;

use crate::errors::{ConfigError, ZenError};
use crate::paths::ZenPaths;

// ---------------------------------------------------------------------------
// Embedded config directory (T026)
// ---------------------------------------------------------------------------

static CONFIGS: Dir = include_dir!("$CARGO_MANIFEST_DIR/../../config");

// ---------------------------------------------------------------------------
// Global config cache (parse once per process)
// ---------------------------------------------------------------------------

#[cfg(not(test))]
static CONFIG_CACHE: OnceLock<ZenConfig> = OnceLock::new();

// ---------------------------------------------------------------------------
// Config structs — Provider/Agent separation (FR-002)
// ---------------------------------------------------------------------------

/// Root configuration for the Agentic module.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ZenConfig {
    /// Default provider name (references a key in `providers`).
    #[serde(default)]
    pub default_provider: Option<String>,
    /// Default model to use when no task-specific model is set.
    #[serde(default)]
    pub default_model: Option<String>,
    /// Named provider definitions — connection settings defined once.
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    /// Agent task routing — which provider/model per task.
    #[serde(default)]
    pub agents: HashMap<String, AgentConfig>,
    #[serde(default)]
    pub features: FeatureConfig,
    #[serde(default)]
    pub channels: ChannelsConfig,
    #[serde(default)]
    pub cron: CronConfig,
    #[serde(default)]
    pub plugin: PluginConfig,
    #[serde(default)]
    pub feeds: Vec<FeedConfig>,
    #[serde(default)]
    pub tui: TuiConfig,
    #[serde(default)]
    pub history: HistoryConfig,
    /// Embedding provider selection (standalone from chat providers).
    #[serde(default)]
    pub embeddings: EmbeddingsConfig,
}

/// IM channel configuration — supports multiple platforms.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ChannelsConfig {
    #[serde(default)]
    pub qqbot: Option<QqBotChannelConfig>,
    #[serde(default)]
    pub whatsapp: Option<WhatsAppChannelConfig>,
    #[serde(default)]
    pub telegram: Option<TelegramChannelConfig>,
}

/// QQ Bot channel configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct QqBotChannelConfig {
    pub app_id: String,
    pub client_secret: String,
    #[serde(default)]
    pub allowed_users: Vec<String>,
    #[serde(default)]
    pub home_channel: Option<String>,
}

/// WhatsApp channel configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct WhatsAppChannelConfig {
    pub phone_number_id: String,
    pub access_token: String,
    #[serde(default)]
    pub allowed_users: Vec<String>,
}

/// Telegram channel configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct TelegramChannelConfig {
    pub bot_token: String,
    #[serde(default)]
    pub allowed_users: Vec<String>,
    #[serde(default)]
    pub home_chat_id: Option<String>,
}

/// Provider definition — connection settings defined once, referenced by name.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ProviderConfig {
    /// Provider type: "ollama", "openai", "anthropic", "deepseek", "mock".
    #[serde(rename = "type", default)]
    pub provider_type: Option<String>,
    /// Base URL for the provider API.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Secret reference for API key (FR-061c).
    ///
    /// TOML formats:
    /// - `api_key = { keychain: "zen-openai-api-key" }`
    /// - `api_key = { env: "ZEN_OPENAI_API_KEY" }`
    #[serde(default)]
    pub api_key: Option<crate::secrets::SecretRef>,
    /// Legacy env var name for backward compatibility (deprecated).
    #[serde(rename = "env_key", default)]
    pub api_key_env: Option<String>,
    /// Default model for this provider.
    #[serde(default)]
    pub default_model: Option<String>,
    /// Model used for embeddings (separate from chat default_model).
    ///
    /// When set, the embedding router will use this model instead of
    /// `default_model`. This is important because many providers use
    /// different models for chat vs embeddings (e.g., Ollama uses
    /// `qwen3-embedding`, DashScope uses `text-embedding-v3`).
    /// When unset, the embedding router falls back to a provider-specific
    /// default (see `DefaultEmbeddingRouter::from_config`).
    #[serde(default)]
    pub embedding_model: Option<String>,
    /// API wire protocol: "completions" (default) or "responses".
    #[serde(rename = "wire_api", default)]
    pub wire_api: Option<String>,
    /// Per-model catalog — model entries with parameters and variants.
    ///
    /// When present, `default_model` selects a key into this map.
    /// When absent, `default_model` is used directly as the API model name
    /// (backward compatible).
    #[serde(default)]
    pub models: HashMap<String, ModelEntry>,
}

/// Fallback step for sequential fallback chain.
#[derive(Debug, Clone, Deserialize)]
pub struct FallbackStep {
    /// Provider name (must match a key in `providers`).
    pub provider: String,
    /// Model override (optional, falls back to provider's default_model).
    #[serde(default)]
    pub model: Option<String>,
    /// Timeout for this step in seconds (optional).
    #[serde(default)]
    pub timeout_secs: Option<u32>,
    /// Variant name for this fallback step's model.
    #[serde(default)]
    pub variant: Option<String>,
}

/// Retry policy for transient errors.
#[derive(Debug, Clone, Deserialize)]
pub struct RetryPolicy {
    /// Maximum retry attempts for transient errors (429, 5xx, timeout).
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// Timeout per attempt in seconds.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u32,
}

fn default_max_retries() -> u32 {
    3
}
fn default_timeout_secs() -> u32 {
    30
}

/// Agent task routing — references a provider by name with optional model override.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct AgentConfig {
    /// Provider name (must match a key in `providers`).
    #[serde(default)]
    pub provider: Option<String>,
    /// Model override for this task (falls back to provider's default_model).
    #[serde(default)]
    pub model: Option<String>,
    /// Sequential fallback chain (tried in order if primary fails).
    #[serde(default)]
    pub fallbacks: Vec<FallbackStep>,
    /// Retry policy for transient errors (optional).
    #[serde(default)]
    pub retry_policy: Option<RetryPolicy>,
    /// Data sensitivity level (optional, enforces local-only if Private/Confidential).
    #[serde(default)]
    pub sensitivity: Option<crate::types::Sensitivity>,
    /// Variant name for the selected model (e.g. "high", "low").
    #[serde(default)]
    pub variant: Option<String>,
    /// Override model temperature (inherits from model catalog if None).
    #[serde(default)]
    pub temperature: Option<f64>,
    /// Override max tokens (inherits from model catalog if None).
    #[serde(default)]
    pub max_tokens: Option<u64>,
}

/// Feature flags.
#[derive(Debug, Clone, Deserialize)]
pub struct FeatureConfig {
    #[serde(default)]
    pub multi_agent: Option<bool>,
    #[serde(default)]
    pub auto_research: Option<bool>,
}

impl Default for FeatureConfig {
    fn default() -> Self {
        Self {
            multi_agent: Some(true),
            auto_research: Some(true),
        }
    }
}

/// Legacy LLM routing config — kept for backward compatibility during migration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub default_provider: Option<String>,
    pub notion_extraction: Option<LlmTaskConfig>,
    pub contradiction_detection: Option<LlmTaskConfig>,
    pub synthesis: Option<LlmTaskConfig>,
    pub dispatch: Option<LlmTaskConfig>,
}

/// Legacy per-task LLM routing entry — kept for backward compatibility.
#[derive(Debug, Clone, Deserialize)]
pub struct LlmTaskConfig {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
}

/// Agent LLM preference for provider selection.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LlmPreference {
    Any,
    LocalOnly,
    CloudOnly,
    Provider(String),
}

impl std::fmt::Display for LlmPreference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmPreference::Any => write!(f, "any"),
            LlmPreference::LocalOnly => write!(f, "local-only"),
            LlmPreference::CloudOnly => write!(f, "cloud-only"),
            LlmPreference::Provider(name) => write!(f, "{name}"),
        }
    }
}

/// Cron scheduling config.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CronConfig {
    pub consolidation_time: Option<String>,
    pub timezone: Option<String>,
    pub subconscious_interval_minutes: Option<u32>,
    pub dream_start_hour: Option<u32>,
    pub dream_end_hour: Option<u32>,
    pub wisdom_synthesis_schedule: Option<String>,
    pub fresh_eyes_mode: Option<bool>,
    /// Maximum cumulative LLM cost (USD) per worker per month.
    /// If a worker's cumulative cost exceeds this cap, it skips execution
    /// until the next monthly reset. Default: 10.0 (sane for personal use).
    pub llm_cost_cap_usd: Option<f64>,
}

/// Plugin system config.
///
/// TOML layout:
/// ```toml
/// [plugin]
/// base_path = "~/.zen/plugins"
///
/// [plugin.finance]
/// enabled = true
/// base_currency = "CNY"
/// ```
///
/// Known fields (`base_path`, `wasm_cache_path`) are deserialized directly.
/// All other `[plugin.{id}]` sections are collected into `plugins` via `#[serde(flatten)]`,
/// where each value is parsed into a [`PluginEntry`] (extracting `enabled`,
/// everything else into `config`).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PluginConfig {
    pub base_path: Option<String>,
    pub wasm_cache_path: Option<String>,
    /// Per-plugin configuration keyed by plugin ID.
    /// Collected via `#[serde(flatten)]` — any `[plugin.{id}]` section
    /// that isn't a known field lands here.
    #[serde(flatten)]
    pub plugins: HashMap<String, PluginEntry>,
}

/// Individual plugin instance configuration.
///
/// Deserialized from a TOML table with `enabled` extracted as a first-class
/// field, and all remaining keys folded into `config` as a JSON object.
#[derive(Debug, Clone)]
pub struct PluginEntry {
    /// Whether this plugin is enabled
    pub enabled: bool,
    /// Plugin-specific configuration (flexible schema)
    pub config: serde_json::Value,
}

impl Default for PluginEntry {
    fn default() -> Self {
        Self {
            enabled: true,
            config: serde_json::Value::Null,
        }
    }
}

impl<'de> Deserialize<'de> for PluginEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        /// Helper struct: `enabled` is extracted, everything else is flattened
        /// into the `rest` map, then folded into `config`.
        #[derive(Deserialize)]
        struct PluginEntryHelper {
            #[serde(default = "default_true")]
            enabled: bool,
            #[serde(flatten)]
            rest: HashMap<String, serde_json::Value>,
        }

        let helper = PluginEntryHelper::deserialize(deserializer)?;
        Ok(PluginEntry {
            enabled: helper.enabled,
            config: serde_json::Value::Object(helper.rest.into_iter().collect()),
        })
    }
}

fn default_true() -> bool {
    true
}

/// Multi-model catalog entry — defines a model variant within a provider.
///
/// Each entry specifies the API model name, default generation parameters,
/// and named variants for different inference configurations.
///
/// ```toml
/// [providers.openai.models.gpt-4o]
/// model = "gpt-4o"
/// options = { temperature = 0.7, max_tokens = 4096 }
///
/// [providers.openai.models.gpt-4o.variants.high]
/// reasoning_effort = "high"
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct ModelEntry {
    pub model: String,
    #[serde(default)]
    pub options: Option<ModelOptions>,
    #[serde(default)]
    pub variants: HashMap<String, VariantConfig>,
}

/// Default generation parameters for a model.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ModelOptions {
    pub temperature: Option<f64>,
    pub max_tokens: Option<u64>,
    pub reasoning_effort: Option<String>,
    pub top_p: Option<f64>,
}

/// Named variant override for a model — same model, different params.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct VariantConfig {
    pub reasoning_effort: Option<String>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u64>,
}

/// RSS/Atom feed source config.
#[derive(Debug, Clone, Deserialize)]
pub struct FeedConfig {
    pub name: String,
    pub url: String,
    pub poll_interval_minutes: Option<u32>,
}

/// Auto-research / learning config.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LearningConfig {
    pub auto_research: Option<bool>,
    pub interval: Option<String>,
}

/// Finance aggregation config.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FinanceConfig {
    pub base_currency: Option<String>,
    pub disclaimer_acknowledged: Option<bool>,
    pub tracked_categories: Option<Vec<String>>,
}

/// TUI presentation config. Holds visual settings for the terminal UI.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct TuiConfig {
    /// Theme name: "zen", "classic", "catppuccin", "deep-ocean", "cyber-purple", "eink".
    pub theme: Option<String>,
}

/// Global command history config (history.jsonl).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct HistoryConfig {
    pub max_bytes: Option<u32>,
}

/// Embedding provider selection — standalone config for the embedding pipeline.
///
/// Controls which provider and model to use for vector embedding generation,
/// independently from chat provider configs.
///
/// # Modes
///
/// - `provider = "local"` — run embeddings locally:
///   - `local_provider = "fastembed"` → ONNX inference via fastembed crate
///   - `local_provider = "ollama"` → Ollama API (must be running)
/// - `provider = "cloud"` — use a remote API (references a key in `[providers]`):
///   - `api_provider = "aliyun"` → uses that provider's `embedding_model`
///   - `api_provider = "openai"` → uses that provider's `embedding_model`
///
/// # Examples
///
/// ```toml
/// [embeddings]
/// provider = "local"
/// local_provider = "fastembed"
/// model = "BGESmallENV15"
/// ```
///
/// ```toml
/// [embeddings]
/// provider = "cloud"
/// api_provider = "aliyun"
/// model = "text-embedding-v4"
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct EmbeddingsConfig {
    /// "local" (fastembed or Ollama) or "cloud" (OpenAI-compatible API).
    pub provider: Option<String>,
    /// For cloud mode: which named provider from `[providers]` to use.
    pub api_provider: Option<String>,
    /// Model name:
    ///   - cloud: API model name (e.g., "text-embedding-v4")
    ///   - local + fastembed: EmbeddingModel variant (e.g., "BGESmallENV15")
    ///   - local + ollama: Ollama model name (e.g., "nomic-embed-text")
    pub model: Option<String>,
    /// For local mode: "fastembed" or "ollama".
    pub local_provider: Option<String>,
    /// HuggingFace mirror endpoint for fastembed model downloads.
    /// Used in China where huggingface.co is blocked (set to "https://hf-mirror.com").
    pub hf_endpoint: Option<String>,
}

// ---------------------------------------------------------------------------
// Default values
// ---------------------------------------------------------------------------

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            default_provider: Some("ollama".into()),
            notion_extraction: Some(LlmTaskConfig::default()),
            contradiction_detection: Some(LlmTaskConfig::default()),
            synthesis: Some(LlmTaskConfig::default()),
            dispatch: Some(LlmTaskConfig::default()),
        }
    }
}

impl Default for LlmTaskConfig {
    fn default() -> Self {
        Self {
            provider: Some("ollama".into()),
            model: Some("qwen3.6:35b-mlx".into()),
            base_url: Some("http://127.0.0.1:11434".into()),
            api_key_env: None,
        }
    }
}

impl Default for CronConfig {
    fn default() -> Self {
        Self {
            consolidation_time: Some("02:00".into()),
            timezone: Some("Asia/Shanghai".into()),
            subconscious_interval_minutes: Some(5),
            dream_start_hour: Some(2),
            dream_end_hour: Some(4),
            wisdom_synthesis_schedule: Some("0 0 2 * * 7".into()),
            fresh_eyes_mode: Some(false),
            llm_cost_cap_usd: Some(10.0),
        }
    }
}

impl Default for PluginConfig {
    fn default() -> Self {
        let home = home::home_dir()
            .map(|h| h.display().to_string())
            .unwrap_or_default();
        Self {
            base_path: Some(format!("{home}/.zen/plugins")),
            wasm_cache_path: Some(format!("{home}/.zen/plugins/cache")),
            plugins: HashMap::new(),
        }
    }
}

impl PluginConfig {
    /// Retrieve a typed plugin configuration.
    ///
    /// Deserializes the plugin's `config` JSON blob into the requested type `T`.
    /// Returns `ConfigError::MissingPlugin` if the plugin ID is not found,
    /// or `ConfigError::PluginParseError` if deserialization fails.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let finance: FinanceConfig = config.plugin.get_typed("finance")?;
    /// ```
    pub fn get_typed<T: serde::de::DeserializeOwned>(
        &self,
        id: &str,
    ) -> Result<T, crate::errors::ConfigError> {
        let instance = self
            .plugins
            .get(id)
            .ok_or_else(|| crate::errors::ConfigError::MissingPlugin { id: id.into() })?;
        serde_json::from_value(instance.config.clone()).map_err(|e| {
            crate::errors::ConfigError::PluginParseError {
                id: id.into(),
                reason: e.to_string(),
            }
        })
    }
}

impl Default for LearningConfig {
    fn default() -> Self {
        Self {
            auto_research: Some(true),
            interval: Some("daily".into()),
        }
    }
}

impl Default for FinanceConfig {
    fn default() -> Self {
        Self {
            base_currency: Some("CNY".into()),
            disclaimer_acknowledged: Some(false),
            tracked_categories: Some(Vec::new()),
        }
    }
}

// ---------------------------------------------------------------------------
// Config loading (T023) — Priority: env → workspace → global → embedded
// ---------------------------------------------------------------------------

/// Load AgenticConfig with the full priority chain (cached):
/// 1. Environment variables (`ZEN_*`)
/// 2. Workspace `.zen/config.toml` (upward search from cwd)
/// 3. Global `~/.zen/config.toml`
/// 4. Embedded `config/config.toml`
/// 5. Rust `Default` impl
///
/// Keychain resolution (FR-061 `SecretRef`) is deferred to zen-auth (T033-T034).
///
/// **Caching**: Config is parsed once per process and cached globally.
/// Subsequent calls return a reference to the cached config.
/// In test mode, caching is disabled to allow environment variable changes.
pub fn load_config() -> Result<&'static ZenConfig, ZenError> {
    #[cfg(test)]
    {
        // In test mode, always load fresh config to allow env var changes
        dotenvy::dotenv().ok();
        let paths = ZenPaths::detect().map_err(ZenError::Path)?;
        let embedded = load_embedded_config()?;
        let global = load_file_config(paths.global_root().join("config.toml")).unwrap_or_default();
        let workspace = match paths.workspace_root() {
            Some(w) => load_file_config(w.join("config.toml")).unwrap_or_default(),
            None => ZenConfig::default(),
        };
        let merged = merge_configs(embedded, global)?;
        let merged = merge_configs(merged, workspace)?;
        let config = apply_env_overrides(merged);
        // Leak the config to get a 'static reference (acceptable for tests)
        Ok(Box::leak(Box::new(config)))
    }

    #[cfg(not(test))]
    {
        if let Some(config) = CONFIG_CACHE.get() {
            return Ok(config);
        }

        dotenvy::dotenv().ok();

        let paths = ZenPaths::detect().map_err(ZenError::Path)?;

        // 1. Embedded defaults (T026)
        let embedded = load_embedded_config()?;

        // 2. Global config from ~/.zen/config.toml (T025)
        let global = load_file_config(paths.global_root().join("config.toml")).unwrap_or_default();

        // 3. Workspace config from .zen/config.toml (T025, upward search via ZenPaths)
        let workspace = match paths.workspace_root() {
            Some(w) => load_file_config(w.join("config.toml")).unwrap_or_default(),
            None => ZenConfig::default(),
        };

        // 4. Merge: embedded ← global ← workspace
        let merged = merge_configs(embedded, global)?;
        let merged = merge_configs(merged, workspace)?;

        // 5. Environment overrides take highest priority
        let config = apply_env_overrides(merged);

        CONFIG_CACHE.set(config).map_err(|_| {
            ZenError::Config(ConfigError::ParseError {
                path: "global".to_string(),
                reason: "Config already initialized".to_string(),
            })
        })?;

        CONFIG_CACHE.get().ok_or_else(|| {
            ZenError::Config(ConfigError::ParseError {
                path: "global".to_string(),
                reason: "Config initialization failed".to_string(),
            })
        })
    }
}

fn load_file_config(path: PathBuf) -> Result<ZenConfig, ZenError> {
    if !path.exists() {
        return Err(ZenError::Config(ConfigError::MissingFile {
            path: path.display().to_string(),
        }));
    }

    let contents = std::fs::read_to_string(&path).map_err(|e| {
        ZenError::Config(ConfigError::ParseError {
            path: path.display().to_string(),
            reason: e.to_string(),
        })
    })?;

    let config: ZenConfig = toml::from_str(&contents).map_err(|e| {
        ZenError::Config(ConfigError::ParseError {
            path: path.display().to_string(),
            reason: e.to_string(),
        })
    })?;

    Ok(config)
}

pub fn load_embedded_config() -> Result<ZenConfig, ZenError> {
    let config_file = CONFIGS.get_file("config.toml").ok_or_else(|| {
        ZenError::Config(ConfigError::MissingFile {
            path: "embedded://config.toml".into(),
        })
    })?;

    let contents = config_file.contents_utf8().ok_or_else(|| {
        ZenError::Config(ConfigError::ParseError {
            path: "embedded://config.toml".into(),
            reason: "invalid UTF-8".into(),
        })
    })?;

    let config: ZenConfig = toml::from_str(contents).map_err(|e| {
        ZenError::Config(ConfigError::ParseError {
            path: "embedded://config.toml".into(),
            reason: e.to_string(),
        })
    })?;

    Ok(config)
}

// ---------------------------------------------------------------------------
// Config inheritance / merge logic (T025)
// ---------------------------------------------------------------------------

fn merge_configs(base: ZenConfig, override_cfg: ZenConfig) -> Result<ZenConfig, ZenError> {
    Ok(ZenConfig {
        default_provider: str_merge(base.default_provider, override_cfg.default_provider),
        default_model: str_merge(base.default_model, override_cfg.default_model),
        providers: merge_providers(base.providers, override_cfg.providers),
        agents: merge_agents(base.agents, override_cfg.agents),
        features: merge_features(base.features, override_cfg.features),
        channels: merge_channels(base.channels, override_cfg.channels),
        cron: merge_cron(base.cron, override_cfg.cron),
        plugin: merge_plugin(base.plugin, override_cfg.plugin),
        feeds: merge_feeds(base.feeds, override_cfg.feeds),
        tui: merge_tui(base.tui, override_cfg.tui),
        history: merge_history(base.history, override_cfg.history),
        embeddings: merge_embeddings(base.embeddings, override_cfg.embeddings),
    })
}

fn merge_providers(
    base: HashMap<String, ProviderConfig>,
    ov: HashMap<String, ProviderConfig>,
) -> HashMap<String, ProviderConfig> {
    let mut merged = base;
    for (k, v) in ov {
        merged
            .entry(k)
            .and_modify(|existing| {
                existing.provider_type = v.provider_type.clone().or(existing.provider_type.clone());
                existing.base_url = v.base_url.clone().or(existing.base_url.clone());
                existing.api_key = v.api_key.clone().or(existing.api_key.clone());
                existing.api_key_env = v.api_key_env.clone().or(existing.api_key_env.clone());
                existing.default_model = v.default_model.clone().or(existing.default_model.clone());
                existing.embedding_model = v.embedding_model.clone().or(existing.embedding_model.clone());
                existing.wire_api = v.wire_api.clone().or(existing.wire_api.clone());
                existing.models = merge_models(existing.models.clone(), v.models.clone());
            })
            .or_insert(v);
    }
    merged
}

fn merge_models(
    base: HashMap<String, ModelEntry>,
    ov: HashMap<String, ModelEntry>,
) -> HashMap<String, ModelEntry> {
    let mut merged = base;
    for (k, v) in ov {
        merged.entry(k).or_insert(v);
    }
    merged
}

fn merge_agents(
    base: HashMap<String, AgentConfig>,
    ov: HashMap<String, AgentConfig>,
) -> HashMap<String, AgentConfig> {
    let mut merged = base;
    for (k, v) in ov {
        merged.entry(k).or_insert(v);
    }
    merged
}

fn merge_features(base: FeatureConfig, ov: FeatureConfig) -> FeatureConfig {
    FeatureConfig {
        multi_agent: ov.multi_agent.or(base.multi_agent),
        auto_research: ov.auto_research.or(base.auto_research),
    }
}

fn merge_channels(base: ChannelsConfig, ov: ChannelsConfig) -> ChannelsConfig {
    ChannelsConfig {
        qqbot: merge_option(base.qqbot, ov.qqbot),
        whatsapp: merge_option(base.whatsapp, ov.whatsapp),
        telegram: merge_option(base.telegram, ov.telegram),
    }
}

fn merge_option<T>(base: Option<T>, ov: Option<T>) -> Option<T> {
    ov.or(base)
}

fn merge_cron(base: CronConfig, ov: CronConfig) -> CronConfig {
    CronConfig {
        consolidation_time: str_merge(base.consolidation_time, ov.consolidation_time),
        timezone: str_merge(base.timezone, ov.timezone),
        subconscious_interval_minutes: ov
            .subconscious_interval_minutes
            .or(base.subconscious_interval_minutes),
        dream_start_hour: ov.dream_start_hour.or(base.dream_start_hour),
        dream_end_hour: ov.dream_end_hour.or(base.dream_end_hour),
        wisdom_synthesis_schedule: str_merge(
            base.wisdom_synthesis_schedule,
            ov.wisdom_synthesis_schedule,
        ),
        fresh_eyes_mode: ov.fresh_eyes_mode.or(base.fresh_eyes_mode),
        llm_cost_cap_usd: ov.llm_cost_cap_usd.or(base.llm_cost_cap_usd),
    }
}

fn merge_plugin(base: PluginConfig, ov: PluginConfig) -> PluginConfig {
    let mut plugins = base.plugins;
    for (key, ov_entry) in ov.plugins {
        plugins
            .entry(key)
            .and_modify(|base_entry| {
                // enabled from override wins
                base_entry.enabled = ov_entry.enabled;
                // JSON field-level deep merge on config objects
                if let (Some(base_obj), Some(ov_obj)) = (
                    base_entry.config.as_object_mut(),
                    ov_entry.config.as_object(),
                ) {
                    for (k, v) in ov_obj {
                        base_obj.insert(k.clone(), v.clone());
                    }
                }
            })
            .or_insert(ov_entry);
    }
    PluginConfig {
        base_path: str_merge(base.base_path, ov.base_path),
        wasm_cache_path: str_merge(base.wasm_cache_path, ov.wasm_cache_path),
        plugins,
    }
}

fn merge_feeds(mut base: Vec<FeedConfig>, ov: Vec<FeedConfig>) -> Vec<FeedConfig> {
    if !ov.is_empty() {
        base.extend(ov);
    }
    base
}

fn merge_tui(base: TuiConfig, ov: TuiConfig) -> TuiConfig {
    TuiConfig {
        theme: str_merge(base.theme, ov.theme),
    }
}

fn merge_history(base: HistoryConfig, ov: HistoryConfig) -> HistoryConfig {
    HistoryConfig {
        max_bytes: ov.max_bytes.or(base.max_bytes),
    }
}

fn merge_embeddings(base: EmbeddingsConfig, ov: EmbeddingsConfig) -> EmbeddingsConfig {
    EmbeddingsConfig {
        provider: str_merge(base.provider, ov.provider),
        api_provider: str_merge(base.api_provider, ov.api_provider),
        model: str_merge(base.model, ov.model),
        local_provider: str_merge(base.local_provider, ov.local_provider),
        hf_endpoint: str_merge(base.hf_endpoint, ov.hf_endpoint),
    }
}

fn str_merge(base: Option<String>, ov: Option<String>) -> Option<String> {
    ov.or(base)
}

// ---------------------------------------------------------------------------
// Environment variable overrides (T023 — dotenvy + ZEN_* env vars)
// ---------------------------------------------------------------------------

fn apply_env_overrides(mut config: ZenConfig) -> ZenConfig {
    if let Some(v) = env_str("ZEN_DEFAULT_PROVIDER") {
        config.default_provider = Some(v);
    }
    if let Some(v) = env_str("ZEN_DEFAULT_MODEL") {
        config.default_model = Some(v);
    }
    apply_agent_env(&mut config.agents);
    apply_cron_env(&mut config.cron);
    apply_plugin_env(&mut config.plugin);
    apply_channels_env(&mut config.channels);
    apply_history_env(&mut config.history);
    apply_embeddings_env(&mut config.embeddings);
    config
}

fn apply_agent_env(agents: &mut HashMap<String, AgentConfig>) {
    // Per-task env overrides: ZEN_AGENT_{TASK}_PROVIDER, ZEN_AGENT_{TASK}_MODEL
    for task in [
        "notion_extraction",
        "contradiction_detection",
        "synthesis",
        "dispatch",
    ] {
        let provider_key = format!("ZEN_AGENT_{}_PROVIDER", task.to_uppercase());
        let model_key = format!("ZEN_AGENT_{}_MODEL", task.to_uppercase());
        if let Some(v) = env_str(&provider_key) {
            agents.entry(task.into()).or_default().provider = Some(v);
        }
        if let Some(v) = env_str(&model_key) {
            agents.entry(task.into()).or_default().model = Some(v);
        }
    }
}

fn apply_cron_env(cron: &mut CronConfig) {
    if let Some(v) = env_str("ZEN_CRON_CONSOLIDATION_TIME") {
        cron.consolidation_time = Some(v);
    }
    if let Some(v) = env_str("ZEN_CRON_TIMEZONE") {
        cron.timezone = Some(v);
    }
    if let Some(v) = env_u32("ZEN_CRON_SUBCONSCIOUS_INTERVAL_MINUTES") {
        cron.subconscious_interval_minutes = Some(v);
    }
    if let Some(v) = env_str("ZEN_CRON_WISDOM_SYNTHESIS") {
        cron.wisdom_synthesis_schedule = Some(v);
    }
}

fn apply_plugin_env(plugin: &mut PluginConfig) {
    if let Some(v) = env_str("ZEN_PLUGIN_BASE_PATH") {
        plugin.base_path = Some(v);
    }
    if let Some(v) = env_str("ZEN_PLUGIN_WASM_CACHE_PATH") {
        plugin.wasm_cache_path = Some(v);
    }
    // Plugin fields via env: write directly into plugins[id].config JSON
    env_plugin_field(plugin, "learning", |obj| {
        if let Some(v) = env_bool("ZEN_LEARNING_AUTO_RESEARCH") {
            obj.insert("auto_research".into(), serde_json::Value::Bool(v));
        }
        if let Some(v) = env_str("ZEN_LEARNING_INTERVAL") {
            obj.insert("interval".into(), serde_json::Value::String(v));
        }
    });
    env_plugin_field(plugin, "finance", |obj| {
        if let Some(v) = env_str("ZEN_FINANCE_BASE_CURRENCY") {
            obj.insert("base_currency".into(), serde_json::Value::String(v));
        }
        if let Some(v) = env_bool("ZEN_FINANCE_DISCLAIMER_ACKNOWLEDGED") {
            obj.insert("disclaimer_acknowledged".into(), serde_json::Value::Bool(v));
        }
    });
}

/// Helper: access (or create) a plugin's config JSON object for env var injection.
fn env_plugin_field(
    plugin: &mut PluginConfig,
    id: &str,
    f: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) {
    let entry = plugin.plugins.entry(id.into()).or_default();
    if entry.config.is_null() {
        entry.config = serde_json::Value::Object(serde_json::Map::new());
    }
    if let Some(obj) = entry.config.as_object_mut() {
        f(obj);
    }
}

fn apply_history_env(history: &mut HistoryConfig) {
    if let Some(v) = env_u32("ZEN_HISTORY_MAX_BYTES") {
        history.max_bytes = Some(v);
    }
}

fn apply_embeddings_env(emb: &mut EmbeddingsConfig) {
    if let Some(v) = env_str("ZEN_EMBEDDINGS_PROVIDER") {
        emb.provider = Some(v);
    }
    if let Some(v) = env_str("ZEN_EMBEDDINGS_API_PROVIDER") {
        emb.api_provider = Some(v);
    }
    if let Some(v) = env_str("ZEN_EMBEDDINGS_MODEL") {
        emb.model = Some(v);
    }
    if let Some(v) = env_str("ZEN_EMBEDDINGS_LOCAL_PROVIDER") {
        emb.local_provider = Some(v);
    }
    if let Some(v) = env_str("ZEN_EMBEDDINGS_HF_ENDPOINT") {
        emb.hf_endpoint = Some(v);
    }
}

fn apply_channels_env(channels: &mut ChannelsConfig) {
    // QQ Bot env overrides
    let app_id = env_str("ZEN_QQBOT_APP_ID");
    let client_secret = env_str("ZEN_QQBOT_CLIENT_SECRET");
    if app_id.is_some() || client_secret.is_some() {
        if channels.qqbot.is_none() {
            channels.qqbot = Some(QqBotChannelConfig {
                app_id: String::new(),
                client_secret: String::new(),
                allowed_users: Vec::new(),
                home_channel: None,
            });
        }
        let q = channels.qqbot.as_mut().unwrap();
        if let Some(v) = app_id {
            q.app_id = v;
        }
        if let Some(v) = client_secret {
            q.client_secret = v;
        }
    }

    // WhatsApp env overrides
    let phone_id = env_str("ZEN_WHATSAPP_PHONE_ID");
    let access_token = env_str("ZEN_WHATSAPP_ACCESS_TOKEN");
    if phone_id.is_some() || access_token.is_some() {
        if channels.whatsapp.is_none() {
            channels.whatsapp = Some(WhatsAppChannelConfig {
                phone_number_id: String::new(),
                access_token: String::new(),
                allowed_users: Vec::new(),
            });
        }
        let w = channels.whatsapp.as_mut().unwrap();
        if let Some(v) = phone_id {
            w.phone_number_id = v;
        }
        if let Some(v) = access_token {
            w.access_token = v;
        }
    }

    // Telegram env overrides
    let bot_token = env_str("ZEN_TELEGRAM_BOT_TOKEN");
    if let Some(v) = bot_token {
        if let Some(ref mut tg) = channels.telegram {
            tg.bot_token = v;
        } else {
            channels.telegram = Some(TelegramChannelConfig {
                bot_token: v,
                allowed_users: Vec::new(),
                home_chat_id: None,
            });
        }
    }
}

fn env_str(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn env_u32(key: &str) -> Option<u32> {
    std::env::var(key).ok().and_then(|v| v.parse().ok())
}

fn env_bool(key: &str) -> Option<bool> {
    std::env::var(key)
        .ok()
        .map(|v| matches!(v.to_lowercase().as_str(), "true" | "1" | "yes"))
}

// ---------------------------------------------------------------------------
// Convenience helpers
// ---------------------------------------------------------------------------

impl ZenConfig {
    /// Resolve theme from TUI section.
    pub fn tui_theme(&self) -> Option<&str> {
        self.tui.theme.as_deref()
    }
}

/// Get the default LLM provider string.
pub fn default_llm_provider(config: &ZenConfig) -> &str {
    config.default_provider.as_deref().unwrap_or("ollama")
}

/// Get the default model string.
pub fn default_model(config: &ZenConfig) -> &str {
    config.default_model.as_deref().unwrap_or("qwen3-coder")
}

/// Get a provider definition by name.
pub fn get_provider<'a>(config: &'a ZenConfig, name: &str) -> Option<&'a ProviderConfig> {
    config.providers.get(name)
}

/// Get an agent task config by name.
pub fn get_agent_task<'a>(config: &'a ZenConfig, name: &str) -> Option<&'a AgentConfig> {
    config.agents.get(name)
}

/// Resolve the effective provider for a task, falling back to default.
pub fn resolve_task_provider<'a>(config: &'a ZenConfig, task: &str) -> &'a str {
    config
        .agents
        .get(task)
        .and_then(|a| a.provider.as_deref())
        .or(config.default_provider.as_deref())
        .unwrap_or("ollama")
}

/// Resolve the effective model for a task, falling back through provider default → global default.
pub fn resolve_task_model<'a>(config: &'a ZenConfig, task: &str) -> &'a str {
    let provider_name = resolve_task_provider(config, task);
    config
        .agents
        .get(task)
        .and_then(|a| a.model.as_deref())
        .or_else(|| {
            config
                .providers
                .get(provider_name)
                .and_then(|p| p.default_model.as_deref())
        })
        .or(config.default_model.as_deref())
        .unwrap_or("qwen3-coder")
}

/// Get the consolidation cron schedule string (HH:MM).
pub fn consolidation_time(config: &ZenConfig) -> &str {
    config.cron.consolidation_time.as_deref().unwrap_or("02:00")
}

/// Generate a cron expression for the daily-log worker from [`CronConfig`].
///
/// Uses `subconscious_interval_minutes` to produce `"0 */N * * * *"`, falling
/// back to `"0 */5 * * * *"` when the field is unset or invalid.
impl CronConfig {
    /// Generate a cron expression for the daily-log worker.
    pub fn daily_log_schedule(&self) -> Option<String> {
        self.subconscious_interval_minutes
            .map(|mins| format!("0 */{mins} * * * *"))
    }

    /// Generate a cron expression for the dream (nightly consolidation) worker.
    /// Produces `"0 0 {start}-{end} * * *"` from start and end hours, or `None` if invalid.
    pub fn night_dream_schedule(&self) -> Option<String> {
        let start = self.dream_start_hour?;
        if !(1..24).contains(&start) {
            return None;
        }
        let end = self.dream_end_hour?;
        if end <= start || end > 24 {
            return None;
        }
        Some(format!("0 0 {start}-{end} * * *"))
    }
}

/// Generate the default daily-log schedule expression.
///
/// This is the fallback used when no config-driven value is available.
pub fn default_daily_log_schedule() -> &'static str {
    "0 */5 * * * *"
}

/// Generate the default night-dream schedule expression.
pub fn default_night_dream_schedule() -> &'static str {
    "0 0 2-4 * * *"
}

pub fn default_wisdom_synthesis_schedule() -> &'static str {
    "0 0 2 * * 7"
}

// ---------------------------------------------------------------------------
/// Persist model selection to workspace config file.
/// Falls back to global `~/.zen/config.toml` if no workspace is detected.
/// Existing lines for these keys are replaced; new keys are appended.
pub fn save_model_selection(provider: &str, model: &str) -> Result<(), ZenError> {
    let paths = ZenPaths::detect().map_err(ZenError::Path)?;
    let config_dir = paths
        .workspace_root()
        .unwrap_or_else(|| paths.global_root())
        .clone();
    std::fs::create_dir_all(&config_dir).ok();
    let config_path = config_dir.join("config.toml");

    let existing = std::fs::read_to_string(&config_path).unwrap_or_default();

    let mut output = String::new();
    let mut seen_provider = false;
    let mut seen_model = false;

    for line in existing.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("default_provider") && !seen_provider {
            output.push_str(&format!("default_provider = \"{provider}\"\n"));
            seen_provider = true;
        } else if trimmed.starts_with("default_model") && !seen_model {
            output.push_str(&format!("default_model = \"{model}\"\n"));
            seen_model = true;
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }

    if !seen_provider {
        output.push_str(&format!("default_provider = \"{provider}\"\n"));
    }
    if !seen_model {
        output.push_str(&format!("default_model = \"{model}\"\n"));
    }

    std::fs::write(&config_path, output).map_err(|e| {
        ZenError::Config(ConfigError::ParseError {
            path: config_path.display().to_string(),
            reason: e.to_string(),
        })
    })
}
