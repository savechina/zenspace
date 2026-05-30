use include_dir::{Dir, include_dir};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

use crate::errors::{ConfigError, ZenError};
use crate::paths::ZenPaths;

// ---------------------------------------------------------------------------
// Embedded config directory (T026)
// ---------------------------------------------------------------------------

static CONFIGS: Dir = include_dir!("$CARGO_MANIFEST_DIR/../../config");

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
    pub agents: HashMap<String, AgentTaskConfig>,
    #[serde(default)]
    pub features: FeatureConfig,
    #[serde(default)]
    pub qqbot: Option<QqBotConfig>,
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
}

/// Provider definition — connection settings defined once, referenced by name.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ProviderConfig {
    /// Provider type: "ollama", "openai", "anthropic", "deepseek", "mock".
    #[serde(default)]
    pub r#type: Option<String>,
    /// Base URL for the provider API.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Environment variable name for the API key.
    #[serde(rename = "env_key", default)]
    pub api_key_env: Option<String>,
    /// Default model for this provider.
    #[serde(default)]
    pub default_model: Option<String>,
    /// API wire protocol: "completions" (default) or "responses".
    #[serde(rename = "wire_api", default)]
    pub wire_api: Option<String>,
}

/// Agent task routing — references a provider by name with optional model override.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct AgentTaskConfig {
    /// Provider name (must match a key in `providers`).
    #[serde(default)]
    pub provider: Option<String>,
    /// Model override for this task (falls back to provider's default_model).
    #[serde(default)]
    pub model: Option<String>,
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
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LlmPreference {
    Any,
    LocalOnly,
    CloudOnly,
    Provider(String),
}

/// QQ Bot integration config.
#[derive(Debug, Clone, Deserialize)]
pub struct QqBotConfig {
    pub app_id: Option<String>,
    pub token_env: Option<String>,
    pub secret_env: Option<String>,
    pub groups: Option<Vec<String>>,
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

/// Load AgenticConfig with the full priority chain:
/// 1. Environment variables (`ZEN_*`)
/// 2. Workspace `.zen/config.toml` (upward search from cwd)
/// 3. Global `~/.zen/config.toml`
/// 4. Embedded `config/config.toml`
/// 5. Rust `Default` impl
///
/// Keychain resolution (FR-061 `SecretRef`) is deferred to zen-auth (T033-T034).
pub fn load_config() -> Result<AgenticConfig, ZenError> {
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
    Ok(apply_env_overrides(merged))
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

fn load_embedded_config() -> Result<AgenticConfig, ZenError> {
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
        qqbot: merge_option(base.qqbot, override_cfg.qqbot),
        cron: merge_cron(base.cron, override_cfg.cron),
        plugin: merge_plugin(base.plugin, override_cfg.plugin),
        feeds: merge_feeds(base.feeds, override_cfg.feeds),
        learning: merge_learning(base.learning, override_cfg.learning),
        finance: merge_finance(base.finance, override_cfg.finance),
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
    base: HashMap<String, AgentTaskConfig>,
    ov: HashMap<String, AgentTaskConfig>,
) -> HashMap<String, AgentTaskConfig> {
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
    PluginConfig {
        scan_dir: str_merge(base.scan_dir, ov.scan_dir),
        wasm_cache_dir: str_merge(base.wasm_cache_dir, ov.wasm_cache_dir),
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
            },
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
    apply_qqbot_env(&mut config.qqbot);
    config
}

fn apply_agent_env(agents: &mut HashMap<String, AgentTaskConfig>) {
    // Per-task env overrides: ZEN_AGENT_{TASK}_PROVIDER, ZEN_AGENT_{TASK}_MODEL
    for task in ["entity_extraction", "contradiction_detection", "synthesis", "dispatch"] {
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

fn apply_qqbot_env(qqbot: &mut Option<QqBotConfig>) {
    let app_id = env_str("ZEN_QQBOT_APP_ID");
    let token_env = env_str("ZEN_QQBOT_TOKEN_ENV");
    let secret_env = env_str("ZEN_QQBOT_SECRET_ENV");
    if app_id.is_some() || token_env.is_some() || secret_env.is_some() {
        if qqbot.is_none() {
            *qqbot = Some(QqBotConfig {
                app_id: None,
                token_env: None,
                secret_env: None,
                groups: None,
            });
        }
        let q = qqbot.as_mut().unwrap();
        if let Some(v) = app_id {
            q.app_id = Some(v);
        }
        if let Some(v) = token_env {
            q.token_env = Some(v);
        }
        if let Some(v) = secret_env {
            q.secret_env = Some(v);
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
pub fn get_agent_task<'a>(config: &'a AgenticConfig, name: &str) -> Option<&'a AgentTaskConfig> {
    config.agents.get(name)
}

/// Resolve the effective provider for a task, falling back to default.
pub fn resolve_task_provider<'a>(config: &'a AgenticConfig, task: &str) -> &'a str {
    config.agents.get(task)
        .and_then(|a| a.provider.as_deref())
        .or(config.default_provider.as_deref())
        .unwrap_or("ollama")
}

/// Resolve the effective model for a task, falling back through provider default → global default.
pub fn resolve_task_model<'a>(config: &'a AgenticConfig, task: &str) -> &'a str {
    let provider_name = resolve_task_provider(config, task);
    config.agents.get(task)
        .and_then(|a| a.model.as_deref())
        .or_else(|| config.providers.get(provider_name).and_then(|p| p.default_model.as_deref()))
        .or(config.default_model.as_deref())
        .unwrap_or("qwen3-coder")
}

/// Get the consolidation cron schedule string (HH:MM).
pub fn consolidation_time(config: &AgenticConfig) -> &str {
    config.cron.consolidation_time.as_deref().unwrap_or("02:00")
}
