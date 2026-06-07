use include_dir::{Dir, include_dir};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
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

static CONFIG_CACHE: OnceLock<AgenticConfig> = OnceLock::new();

// ---------------------------------------------------------------------------
// Config structs — Provider/Agent separation (FR-002)
// ---------------------------------------------------------------------------

/// Root configuration for the Agentic module.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct AgenticConfig {
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
    pub learning: LearningConfig,
    #[serde(default)]
    pub finance: FinanceConfig,
    #[serde(default)]
    pub tui: TuiConfig,
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
    /// API wire protocol: "completions" (default) or "responses".
    #[serde(rename = "wire_api", default)]
    pub wire_api: Option<String>,
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
    pub entity_extraction: Option<LlmTaskConfig>,
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
}

/// Plugin system config.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PluginConfig {
    pub scan_dir: Option<String>,
    pub wasm_cache_dir: Option<String>,
    /// Per-plugin configuration keyed by plugin ID
    pub plugins: HashMap<String, PluginInstance>,
}

/// Individual plugin instance configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct PluginInstance {
    /// Whether this plugin is enabled
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Plugin-specific configuration (flexible schema)
    #[serde(default)]
    pub config: serde_json::Value,
}

impl Default for PluginInstance {
    fn default() -> Self {
        Self {
            enabled: true,
            config: serde_json::Value::Null,
        }
    }
}

fn default_true() -> bool {
    true
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
pub struct TuiConfig {
    /// Theme name: "zen", "classic", "catppuccin", "deep-ocean", "cyber-purple", "eink".
    pub theme: Option<String>,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self { theme: None }
    }
}

// ---------------------------------------------------------------------------
// Default values
// ---------------------------------------------------------------------------

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            default_provider: Some("ollama".into()),
            entity_extraction: Some(LlmTaskConfig::default()),
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
        }
    }
}

impl Default for PluginConfig {
    fn default() -> Self {
        let home = home::home_dir()
            .map(|h| h.display().to_string())
            .unwrap_or_default();
        Self {
            scan_dir: Some(format!("{home}/.zen/plugins")),
            wasm_cache_dir: Some(format!("{home}/.zen/plugins/cache")),
            plugins: HashMap::new(),
        }
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
pub fn load_config() -> Result<&'static AgenticConfig, ZenError> {
    #[cfg(test)]
    {
        // In test mode, always load fresh config to allow env var changes
        dotenvy::dotenv().ok();
        let paths = ZenPaths::detect().map_err(ZenError::Path)?;
        let embedded = load_embedded_config()?;
        let global = load_file_config(paths.global_root().join("config.toml")).unwrap_or_default();
        let workspace = match paths.workspace_root() {
            Some(w) => load_file_config(w.join("config.toml")).unwrap_or_default(),
            None => AgenticConfig::default(),
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
            None => AgenticConfig::default(),
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

fn load_file_config(path: PathBuf) -> Result<AgenticConfig, ZenError> {
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

    let config: AgenticConfig = toml::from_str(&contents).map_err(|e| {
        ZenError::Config(ConfigError::ParseError {
            path: path.display().to_string(),
            reason: e.to_string(),
        })
    })?;

    Ok(config)
}

pub fn load_embedded_config() -> Result<AgenticConfig, ZenError> {
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

    let config: AgenticConfig = toml::from_str(contents).map_err(|e| {
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

fn merge_configs(
    base: AgenticConfig,
    override_cfg: AgenticConfig,
) -> Result<AgenticConfig, ZenError> {
    Ok(AgenticConfig {
        default_provider: str_merge(base.default_provider, override_cfg.default_provider),
        default_model: str_merge(base.default_model, override_cfg.default_model),
        providers: merge_providers(base.providers, override_cfg.providers),
        agents: merge_agents(base.agents, override_cfg.agents),
        features: merge_features(base.features, override_cfg.features),
        channels: merge_channels(base.channels, override_cfg.channels),
        cron: merge_cron(base.cron, override_cfg.cron),
        plugin: merge_plugin(base.plugin, override_cfg.plugin),
        feeds: merge_feeds(base.feeds, override_cfg.feeds),
        learning: merge_learning(base.learning, override_cfg.learning),
        finance: merge_finance(base.finance, override_cfg.finance),
        tui: merge_tui(base.tui, override_cfg.tui),
    })
}

fn merge_providers(
    base: HashMap<String, ProviderConfig>,
    ov: HashMap<String, ProviderConfig>,
) -> HashMap<String, ProviderConfig> {
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
    }
}

fn merge_plugin(base: PluginConfig, ov: PluginConfig) -> PluginConfig {
    let mut plugins = base.plugins;
    plugins.extend(ov.plugins);
    PluginConfig {
        scan_dir: str_merge(base.scan_dir, ov.scan_dir),
        wasm_cache_dir: str_merge(base.wasm_cache_dir, ov.wasm_cache_dir),
        plugins,
    }
}

fn merge_feeds(mut base: Vec<FeedConfig>, ov: Vec<FeedConfig>) -> Vec<FeedConfig> {
    if !ov.is_empty() {
        base.extend(ov);
    }
    base
}

fn merge_learning(base: LearningConfig, ov: LearningConfig) -> LearningConfig {
    LearningConfig {
        auto_research: ov.auto_research.or(base.auto_research),
        interval: str_merge(base.interval, ov.interval),
    }
}

fn merge_tui(base: TuiConfig, ov: TuiConfig) -> TuiConfig {
    TuiConfig {
        theme: str_merge(base.theme, ov.theme),
    }
}

fn merge_finance(base: FinanceConfig, ov: FinanceConfig) -> FinanceConfig {
    FinanceConfig {
        base_currency: str_merge(base.base_currency, ov.base_currency),
        disclaimer_acknowledged: ov.disclaimer_acknowledged.or(base.disclaimer_acknowledged),
        tracked_categories: match (base.tracked_categories, ov.tracked_categories) {
            (None, None) => None,
            (Some(b), None) => Some(b),
            (None, Some(o)) => Some(o),
            (Some(mut b), Some(mut o)) => {
                b.append(&mut o);
                Some(b)
            }
        },
    }
}

fn str_merge(base: Option<String>, ov: Option<String>) -> Option<String> {
    ov.or(base)
}

// ---------------------------------------------------------------------------
// Environment variable overrides (T023 — dotenvy + ZEN_* env vars)
// ---------------------------------------------------------------------------

fn apply_env_overrides(mut config: AgenticConfig) -> AgenticConfig {
    if let Some(v) = env_str("ZEN_DEFAULT_PROVIDER") {
        config.default_provider = Some(v);
    }
    if let Some(v) = env_str("ZEN_DEFAULT_MODEL") {
        config.default_model = Some(v);
    }
    apply_agent_env(&mut config.agents);
    apply_cron_env(&mut config.cron);
    apply_plugin_env(&mut config.plugin);
    apply_learning_env(&mut config.learning);
    apply_finance_env(&mut config.finance);
    apply_channels_env(&mut config.channels);
    config
}

fn apply_agent_env(agents: &mut HashMap<String, AgentConfig>) {
    // Per-task env overrides: ZEN_AGENT_{TASK}_PROVIDER, ZEN_AGENT_{TASK}_MODEL
    for task in [
        "entity_extraction",
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
}

fn apply_plugin_env(plugin: &mut PluginConfig) {
    if let Some(v) = env_str("ZEN_PLUGIN_SCAN_DIR") {
        plugin.scan_dir = Some(v);
    }
    if let Some(v) = env_str("ZEN_PLUGIN_WASM_CACHE_DIR") {
        plugin.wasm_cache_dir = Some(v);
    }
}

fn apply_learning_env(learning: &mut LearningConfig) {
    if let Some(v) = env_bool("ZEN_LEARNING_AUTO_RESEARCH") {
        learning.auto_research = Some(v);
    }
    if let Some(v) = env_str("ZEN_LEARNING_INTERVAL") {
        learning.interval = Some(v);
    }
}

fn apply_finance_env(finance: &mut FinanceConfig) {
    if let Some(v) = env_str("ZEN_FINANCE_BASE_CURRENCY") {
        finance.base_currency = Some(v);
    }
    if let Some(v) = env_bool("ZEN_FINANCE_DISCLAIMER_ACKNOWLEDGED") {
        finance.disclaimer_acknowledged = Some(v);
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
        if channels.telegram.is_none() {
            channels.telegram = Some(TelegramChannelConfig {
                bot_token: v,
                allowed_users: Vec::new(),
                home_chat_id: None,
            });
        } else {
            channels.telegram.as_mut().unwrap().bot_token = v;
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

impl AgenticConfig {
    /// Resolve theme from TUI section.
    pub fn tui_theme(&self) -> Option<&str> {
        self.tui.theme.as_deref()
    }
}

/// Get the default LLM provider string.
pub fn default_llm_provider(config: &AgenticConfig) -> &str {
    config.default_provider.as_deref().unwrap_or("ollama")
}

/// Get the default model string.
pub fn default_model(config: &AgenticConfig) -> &str {
    config.default_model.as_deref().unwrap_or("qwen3-coder")
}

/// Get a provider definition by name.
pub fn get_provider<'a>(config: &'a AgenticConfig, name: &str) -> Option<&'a ProviderConfig> {
    config.providers.get(name)
}

/// Get an agent task config by name.
pub fn get_agent_task<'a>(config: &'a AgenticConfig, name: &str) -> Option<&'a AgentConfig> {
    config.agents.get(name)
}

/// Resolve the effective provider for a task, falling back to default.
pub fn resolve_task_provider<'a>(config: &'a AgenticConfig, task: &str) -> &'a str {
    config
        .agents
        .get(task)
        .and_then(|a| a.provider.as_deref())
        .or(config.default_provider.as_deref())
        .unwrap_or("ollama")
}

/// Resolve the effective model for a task, falling back through provider default → global default.
pub fn resolve_task_model<'a>(config: &'a AgenticConfig, task: &str) -> &'a str {
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
pub fn consolidation_time(config: &AgenticConfig) -> &str {
    config.cron.consolidation_time.as_deref().unwrap_or("02:00")
}
