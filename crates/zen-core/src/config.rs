use include_dir::{Dir, include_dir};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
#[cfg(not(test))]
use std::sync::RwLock;

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
static CONFIG_CACHE: RwLock<Option<ZenConfig>> = RwLock::new(None);

/// Drop the cached config so the next [`load_config`] re-reads files + env.
///
/// Production code never calls this (parse-once-per-process stands); it
/// exists for integration tests, which link the normally-compiled crate
/// (where `#[cfg(test)]` is off and the cache is live) and must observe
/// mid-process env changes such as `ZEN_SKILLS_AUTO_ROUTE=0`.
#[cfg(not(test))]
pub fn invalidate_config_cache() {
    if let Ok(mut guard) = CONFIG_CACHE.write() {
        // Forget (leak) rather than drop: outstanding `&'static` borrows from
        // earlier `load_config` calls must stay valid. Invalidations happen a
        // handful of times per test process — bounded, deliberate leakage.
        let old = guard.take();
        std::mem::forget(old);
    }
}

// ---------------------------------------------------------------------------
// Config structs — Provider/Agent separation (FR-002)
// ---------------------------------------------------------------------------

/// Root configuration for the Agentic module.
///
/// Deserialization is manual (see the `impl Deserialize` below) so the
/// `[agents]` table can carry both the task-routing map and the FR-046
/// `tools` overlay array; absent fields default via [`Default`].
#[derive(Debug, Clone, Default)]
pub struct ZenConfig {
    /// Default provider name (references a key in `providers`).
    pub default_provider: Option<String>,
    /// Default model to use when no task-specific model is set.
    pub default_model: Option<String>,
    /// Named provider definitions — connection settings defined once.
    pub providers: HashMap<String, ProviderConfig>,
    /// Agent task routing — which provider/model per task.
    pub agents: HashMap<String, AgentConfig>,
    /// Agent tool-grant overlay (FR-046, TOML `[agents] tools = [...]`).
    ///
    /// Additive on top of the builtin per-agent grant map. Entries are
    /// exact tool names, `prefix.*` wildcards, or the special `plugin:*`
    /// pattern (every plugin-registered tool). Empty (the default) leaves
    /// the builtin grant set unchanged.
    pub agents_tools: Vec<String>,
    pub features: FeatureConfig,
    pub channels: ChannelsConfig,
    pub cron: CronConfig,
    pub plugin: PluginConfig,
    pub feeds: Vec<FeedConfig>,
    pub tui: TuiConfig,
    pub history: HistoryConfig,
    /// Embedding provider selection (standalone from chat providers).
    pub embeddings: EmbeddingsConfig,
    pub web_fetch: WebFetchConfig,
    pub web_search: WebSearchConfig,
    pub mcp_servers: Vec<McpServerConfig>,
    /// Sandbox hardening (`[sandbox.*]`).
    pub sandbox: SandboxConfig,
    /// Agentic module sections (`[agentic.*]`, 005-agentic-loop).
    pub agentic: AgenticConfig,
    /// Skill auto-routing (`[skills.*]`, 005-agentic-loop T076).
    pub skills: SkillsConfig,
}

/// Sandbox hardening config — `[sandbox.*]` sections (T091).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct SandboxConfig {
    /// WASM sandbox permission policy (`[sandbox.wasm]`).
    #[serde(default)]
    pub wasm: WasmSandboxConfig,
    /// OS-layer network access for sandboxed child processes
    /// (`[sandbox] network_access`, FR-028 D6). Default false = deny.
    #[serde(default)]
    pub network_access: bool,
}

/// Manual [`Deserialize`] for [`ZenConfig`] (FR-046): the `[agents]` table
/// carries both the task-routing map (`[agents.<task>]` tables, parsed as
/// before) and the `tools` overlay array, which is lifted into
/// [`ZenConfig::agents_tools`] instead of being rejected as a task entry.
/// Field defaults match the per-field `#[serde(default)]`s the derived
/// impl used to apply.
impl<'de> Deserialize<'de> for ZenConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct ZenConfigShadow {
            default_provider: Option<String>,
            default_model: Option<String>,
            providers: HashMap<String, ProviderConfig>,
            agents: AgentsSectionShadow,
            features: FeatureConfig,
            channels: ChannelsConfig,
            cron: CronConfig,
            plugin: PluginConfig,
            feeds: Vec<FeedConfig>,
            tui: TuiConfig,
            history: HistoryConfig,
            embeddings: EmbeddingsConfig,
            web_fetch: WebFetchConfig,
            web_search: WebSearchConfig,
            mcp_servers: Vec<McpServerConfig>,
            sandbox: SandboxConfig,
            agentic: AgenticConfig,
            skills: SkillsConfig,
        }

        let shadow = ZenConfigShadow::deserialize(deserializer)?;
        Ok(ZenConfig {
            default_provider: shadow.default_provider,
            default_model: shadow.default_model,
            providers: shadow.providers,
            agents: shadow.agents.tasks,
            agents_tools: shadow.agents.tools,
            features: shadow.features,
            channels: shadow.channels,
            cron: shadow.cron,
            plugin: shadow.plugin,
            feeds: shadow.feeds,
            tui: shadow.tui,
            history: shadow.history,
            embeddings: shadow.embeddings,
            web_fetch: shadow.web_fetch,
            web_search: shadow.web_search,
            mcp_servers: shadow.mcp_servers,
            sandbox: shadow.sandbox,
            agentic: shadow.agentic,
            skills: shadow.skills,
        })
    }
}

/// `[agents]` section deserialization helper (FR-046): extracts the `tools`
/// overlay array; every other key deserializes into the task-routing map.
#[derive(Default)]
struct AgentsSectionShadow {
    tasks: HashMap<String, AgentConfig>,
    tools: Vec<String>,
}

impl<'de> Deserialize<'de> for AgentsSectionShadow {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)]
            tools: Vec<String>,
            #[serde(flatten)]
            tasks: HashMap<String, AgentConfig>,
        }

        let raw = Raw::deserialize(deserializer)?;
        Ok(Self {
            tasks: raw.tasks,
            tools: raw.tools,
        })
    }
}

/// WASM sandbox permission policy (T091, FR-029).
///
/// Every flag defaults to `false` (deny-all), matching the pre-config
/// behavior: a plugin whose manifest declares a permission only loads
/// when the policy grants it.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct WasmSandboxConfig {
    #[serde(default)]
    pub allow_filesystem_read: bool,
    #[serde(default)]
    pub allow_filesystem_write: bool,
    #[serde(default)]
    pub allow_network: bool,
    #[serde(default)]
    pub allow_system: bool,
}

/// IM channel configuration — supports multiple platforms.
///
/// T183: `whatsapp`/`telegram` were removed — no channel implementation ever
/// consumed them (phantom config). `[channels.whatsapp]` / `[channels.telegram]`
/// sections in config.toml are inertly ignored (no `deny_unknown_fields`).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ChannelsConfig {
    #[serde(default)]
    pub qqbot: Option<QqBotChannelConfig>,
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
    /// Morning-brief outbox drain tick, seconds (T101).
    ///
    /// Functionality: cadence at which the qqbot carrier sweeps
    /// `logs/outbox/morning-brief-*.json` with an active send.
    /// User impact: lower values deliver the 9am brief sooner after a
    /// daemon restart; higher values reduce QQ API churn.
    /// Default: absent → 300s.
    /// Values: clamped to 60..=3600 at the CLI mapping layer.
    /// Interaction: independent of the WS gateway/intent knobs.
    #[serde(default)]
    pub outbox_drain_interval_secs: Option<u64>,
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
    /// USD price per 1M input tokens for this provider's default model.
    ///
    /// Scope logic (Constitution XV):
    /// - Functionality: enables real LLM cost accounting for the scheduler's
    ///   per-worker monthly cost cap (`[cron] llm_cost_cap_usd`); consumed by
    ///   `DefaultRouter::complete_metered` via `ModelMetadata` pricing.
    /// - User impact: when set, worker LLM calls accumulate real USD cost and
    ///   the cap can trip; when absent on a cloud provider, cost meters as
    ///   0.0 with a warning (the cap cannot trip for that provider).
    /// - Default: `None` (pricing unknown — never assumed).
    /// - Interaction: local providers (`type = "ollama"`) always cost 0.0
    ///   regardless of these fields; `output_cost_per_million` is the pair.
    #[serde(default)]
    pub input_cost_per_million: Option<f64>,
    /// USD price per 1M output tokens for this provider's default model.
    ///
    /// Scope logic: mirror of `input_cost_per_million` (same functionality,
    /// user impact, default, and interaction); both fields are independent —
    /// setting only one meters the other at 0.0.
    #[serde(default)]
    pub output_cost_per_million: Option<f64>,
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
    /// In-app (TUI-hosted) scheduler kill switch. When true, the TUI
    /// spawns the learning-core worker subset for its lifetime; when
    /// false, learning runs only under an explicit `zen serve start`.
    /// Default: true.
    pub tui_scheduler: Option<bool>,
}

/// Agentic module sections — `[agentic.*]` (005-agentic-loop).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct AgenticConfig {
    /// Knowledge-processing loop — TOML `[agentic.loop]`.
    /// (`loop` is a Rust keyword, hence the `loop_cfg` field name.)
    #[serde(rename = "loop")]
    pub loop_cfg: LoopConfig,
    /// Per-turn tool dispatch loop — TOML `[agentic.tool_loop]` (T050).
    pub tool_loop: ToolLoopConfig,
    /// Agent quality-gate tuning — TOML `[agentic.review]` (T092).
    pub review: ReviewConfig,
    /// Delegate sub-agent tuning — TOML `[agentic.delegate]` (006).
    pub delegate: DelegateConfig,
    /// Orchestrator tool surface — TOML `[agentic.orchestrator]` (T378).
    pub orchestrator: OrchestratorConfig,
    /// Intent classification tuning — TOML `[agentic.intent]` (T168).
    pub intent: IntentConfig,
    /// Binary-classifier thresholds — TOML `[agentic.classifiers]` (T171).
    pub classifiers: ClassifierConfig,
    /// Decision-audit content policy — TOML `[agentic.audit]` (T173/T175).
    pub audit: AuditConfig,
    /// Retention sweep gating — TOML `[agentic.retention]` (review D2).
    pub retention: RetentionConfig,
}

/// Knowledge-processing loop configuration (005-agentic-loop, T001).
///
/// Scope logic (Constitution XV):
/// - Functionality: gates `ZenLoopWorker` cron registration and merge/distill tuning.
/// - User impact: `enabled = false` makes the loop manual-only (`zen wiki loop run` still works).
/// - Default: enabled=true, 5-min cron, merge threshold 0.82, pure-duplicate 0.98,
///   max_attempts 3, min_free_bytes 100 MiB, host_sources empty (FR-033 off),
///   hypothesis_refinement true, reverify_older_than_days 7,
///   raw_graph_routing true, cas_commit true, host_stage_timeout_secs 60.
/// - Interaction: `ZEN_LOOP_*` env vars override any config layer (5th layer).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct LoopConfig {
    /// Worker fires on cron when true; absent → enabled (contract cli.md).
    pub enabled: Option<bool>,
    /// 6-field cron expression. Default `"0 */5 * * * *"`.
    pub interval: Option<String>,
    /// Trigram-Jaccard similarity threshold for wiki merge clustering (FR-016).
    pub merge_threshold: Option<f64>,
    /// Similarity at/above which two pages short-circuit as pure duplicates. Default 0.98.
    pub merge_pure_duplicate: Option<f64>,
    /// Provider model reference for LLM-assisted merges (mem0 ADD/UPDATE discipline).
    pub merge_llm_model: Option<String>,
    /// Per-note retry attempts before quarantine (FR-010). Default 3.
    pub max_attempts: Option<u32>,
    /// Pre-cycle free-space guard in bytes; below → cycle Aborted (FR-005 guard).
    pub min_free_bytes: Option<u64>,
    /// Skip file extensions for ingest (FR-012). Default empty = no skip.
    /// Example: `skip_extensions = ["tmp", "log", "bak"]`
    pub skip_extensions: Option<Vec<String>>,
    /// Host directories governed by FR-033 (default empty = feature off).
    pub host_sources: Vec<HostSourceConfig>,
    /// Run FR-028 hypothesis refinement (queue build + periodic reverify)
    /// in the cycle's Stage 5c. Default true.
    pub hypothesis_refinement: Option<bool>,
    /// Re-verify hypotheses older than this many days (FR-028 Compounding
    /// Synthesis). Default 7.
    pub reverify_older_than_days: Option<u64>,
    /// Route `raw/` non-note sources through the FR-030 GraphRouter each
    /// cycle (Code/Paper tracks join entities without wiki compile).
    /// Default true.
    pub raw_graph_routing: Option<bool>,
    /// Commit distill wiki writes via FR-032 OCC/CAS (VersionSnapshot +
    /// commit_conditional) instead of unconditional commit. Default true.
    pub cas_commit: Option<bool>,
    /// Per-host-source staging timeout in seconds. A source whose directory
    /// scan exceeds this is skipped with a 30-minute backoff (macOS TCC
    /// denial can hang `opendir` indefinitely). Default 60.
    pub host_stage_timeout_secs: Option<u64>,
    /// Per-cycle step budget (FR-032 LoopBudget): notes processed per cycle;
    /// the remainder is deferred to `vault/archive/pending/` and re-queued next
    /// cycle. Default 10 — SC-004 requires 100+ notes/hour and the default
    /// 5-minute interval gives 12 cycles/hour, so fewer than 9 steps/cycle
    /// cannot meet it (the previous fixed 5 capped throughput at 60/hour).
    pub max_steps: Option<u32>,
    /// Per-cycle token budget (FR-032 LoopBudget). Default 8000.
    pub max_tokens: Option<u32>,
    /// Whole-file ingest size ceiling in bytes (T158). Files above this are
    /// stat-and-skipped (never read into memory) and quarantined, so a
    /// multi-GB `*.txt` in a watched host dir cannot OOM the daemon.
    /// Default 64 MiB; clamped to 1 MiB..=1 GiB.
    pub max_ingest_bytes: Option<u64>,
    /// Louvain resolution γ for community detection (T141). γ < 1 favors
    /// larger communities, γ > 1 favors smaller ones. Default 1.0; clamped
    /// to 0.1..=5.0 so a typo can neither merge everything nor atomize it.
    pub community_resolution: Option<f64>,
    /// Minimum community size for the summarization surface (T141 part B):
    /// communities at or below this size are skipped entirely (small pairs
    /// are noise and would flood the vault). Default 3; clamped to 2..=50.
    pub community_min_size: Option<u32>,
}

impl LoopConfig {
    pub fn enabled_or_default(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    pub fn interval_or_default(&self) -> &str {
        self.interval.as_deref().unwrap_or("0 */5 * * * *")
    }

    pub fn merge_threshold_or_default(&self) -> f64 {
        self.merge_threshold.unwrap_or(0.82)
    }

    pub fn merge_pure_duplicate_or_default(&self) -> f64 {
        self.merge_pure_duplicate.unwrap_or(0.98)
    }

    pub fn max_attempts_or_default(&self) -> u32 {
        self.max_attempts.unwrap_or(3)
    }

    /// Per-cycle note budget. Default 10 so the default 5-minute interval
    /// yields ≥100 notes/hour (SC-004); clamped to 1..=100 so a typo cannot
    /// disable the loop's budget or make a single cycle unbounded.
    pub fn max_steps_or_default(&self) -> u32 {
        self.max_steps.unwrap_or(10).clamp(1, 100)
    }

    /// Per-cycle token budget. Default 8000; clamped to 1..=1_000_000.
    pub fn max_tokens_or_default(&self) -> u32 {
        self.max_tokens.unwrap_or(8_000).clamp(1, 1_000_000)
    }

    /// Whole-file ingest size ceiling in bytes (T158). Default 64 MiB;
    /// clamped to 1 MiB..=1 GiB so a typo can neither disable the guard
    /// (0/tiny) nor make it unbounded (huge).
    pub fn max_ingest_bytes_or_default(&self) -> u64 {
        self.max_ingest_bytes
            .unwrap_or(64 * 1024 * 1024)
            .clamp(1024 * 1024, 1024 * 1024 * 1024)
    }

    /// Louvain resolution γ (T141). Default 1.0; clamped to 0.1..=5.0 so a
    /// typo can neither merge the whole graph into one community nor atomize
    /// it into singletons.
    pub fn community_resolution_or_default(&self) -> f64 {
        self.community_resolution.unwrap_or(1.0).clamp(0.1, 5.0)
    }

    /// Minimum community size for the summarization surface (T141 part B).
    /// Default 3; clamped to 2..=50 so a typo can neither write every pair
    /// as a page nor suppress all pages.
    pub fn community_min_size_or_default(&self) -> u32 {
        self.community_min_size.unwrap_or(3).clamp(2, 50)
    }

    pub fn min_free_bytes_or_default(&self) -> u64 {
        self.min_free_bytes.unwrap_or(100 * 1024 * 1024)
    }

    pub fn skip_extensions_or_default(&self) -> Vec<String> {
        self.skip_extensions.clone().unwrap_or_default()
    }

    pub fn hypothesis_refinement_or_default(&self) -> bool {
        self.hypothesis_refinement.unwrap_or(true)
    }

    pub fn reverify_older_than_days_or_default(&self) -> u64 {
        self.reverify_older_than_days.unwrap_or(7)
    }

    pub fn raw_graph_routing_or_default(&self) -> bool {
        self.raw_graph_routing.unwrap_or(true)
    }

    pub fn cas_commit_or_default(&self) -> bool {
        self.cas_commit.unwrap_or(true)
    }

    /// Per-source staging timeout in seconds. Default 60 (covers the
    /// worst-case macOS TCC stall; healthy directories finish in <1s).
    pub fn host_stage_timeout_secs_or_default(&self) -> u64 {
        self.host_stage_timeout_secs.unwrap_or(60)
    }

    pub fn is_extension_skipped(&self, ext: &str) -> bool {
        let lower = ext.to_ascii_lowercase();
        self.skip_extensions_or_default()
            .iter()
            .any(|e| e.to_ascii_lowercase() == lower)
    }
}

/// Clamp bounds for `[agentic.tool_loop] max_rounds` (contract tool-loop.json).
const TOOL_MAX_ROUNDS_MIN: u8 = 1;
const TOOL_MAX_ROUNDS_MAX: u8 = 16;
const TOOL_MAX_ROUNDS_DEFAULT: u8 = 8;

/// Per-turn tool dispatch loop configuration (005-agentic-loop, T050) —
/// TOML `[agentic.tool_loop]`.
///
/// Scope logic (Constitution XV):
/// - Functionality: caps the orchestrator's tool dispatch rounds per user turn
///   (`orchestrator::execute_stream`/`execute`); replaces the former const
///   `MAX_TOOL_ROUNDS = 4`. Token budget and turn watchdog remain in force.
/// - User impact: higher values let the agent chain more tool calls per turn
///   (multi-step web.search + fs.* sequences); lower values cut per-turn
///   latency and cost at the price of truncated tool chains.
/// - Default: `max_rounds = 8`, clamped to `1..=16` at load — both TOML and
///   env-sourced values are bounded.
/// - Interaction: env `ZEN_TOOL_MAX_ROUNDS` (5th layer) overrides any config
///   file layer; the clamp applies after the 5-layer merge.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ToolLoopConfig {
    /// Max tool dispatch rounds per user turn (absent → 8, clamped 1..=16).
    pub max_rounds: Option<u8>,
}

impl ToolLoopConfig {
    /// Effective per-turn tool dispatch round cap: config value clamped to
    /// `1..=16`, or the default 8 when absent.
    pub fn max_rounds_or_default(&self) -> u8 {
        self.max_rounds
            .unwrap_or(TOOL_MAX_ROUNDS_DEFAULT)
            .clamp(TOOL_MAX_ROUNDS_MIN, TOOL_MAX_ROUNDS_MAX)
    }
}

/// Bounds for the quality-gate retry budgets (T092).
const REVIEW_MAX_MOMUS_DEFAULT: u8 = 2;
const REVIEW_MAX_MOMUS_MAX: u8 = 5;
const REVIEW_MAX_HERMES_DEFAULT: u8 = 1;
const REVIEW_MAX_HERMES_MAX: u8 = 5;

/// Agent quality-gate configuration (005-agentic-loop, T092) —
/// TOML `[agentic.review]`.
///
/// Scope logic (Constitution XV):
/// - Functionality: overrides the heuristic `QualityPipeline` budgets
///   (`max_momus_retries`, `max_hermes_revisions`) and gates the LLM
///   semantic-review stage for HIGH blast-radius tasks.
/// - User impact: higher budgets trade latency for stricter gates;
///   `llm_review_high_blast = false` restores the pure-heuristic fast
///   path for every task (no LLM cost in review).
/// - Default: retries 2, revisions 1, LLM stage on — matches the
///   hardcoded pipeline behavior, so absent config changes nothing.
/// - Interaction: env `ZEN_REVIEW_MAX_MOMUS_RETRIES` /
///   `ZEN_REVIEW_MAX_HERMES_REVISIONS` / `ZEN_REVIEW_LLM_HIGH_BLAST`
///   (5th layer) override file layers; clamps apply after the merge.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ReviewConfig {
    /// Momus gate retries (absent → 2, clamped 0..=5).
    pub max_momus_retries: Option<u8>,
    /// Hermes revision rounds (absent → 1, clamped 0..=5).
    pub max_hermes_revisions: Option<u8>,
    /// LLM semantic-review stage for HIGH blast-radius tasks
    /// (absent → true).
    pub llm_review_high_blast: Option<bool>,
    /// T170: calibrated escalation threshold for the de-anchored judge
    /// cascade. Absent (the default, and the only state until labels exist)
    /// means the local judge phase is skipped entirely and the frontier
    /// reviewer runs as it did before T170. Setting it activates the cascade:
    /// a local verdict at or above this confidence is final and the frontier
    /// call is skipped. Valid range 0.0..=1.0; out-of-range is ignored.
    pub escalate_threshold: Option<f32>,
}

impl ReviewConfig {
    /// Effective Momus retry budget: config value clamped to `0..=5`,
    /// or the default 2 when absent.
    pub fn max_momus_retries_or_default(&self) -> u8 {
        self.max_momus_retries
            .unwrap_or(REVIEW_MAX_MOMUS_DEFAULT)
            .min(REVIEW_MAX_MOMUS_MAX)
    }

    /// Effective Hermes revision budget: config value clamped to `0..=5`,
    /// or the default 1 when absent.
    pub fn max_hermes_revisions_or_default(&self) -> u8 {
        self.max_hermes_revisions
            .unwrap_or(REVIEW_MAX_HERMES_DEFAULT)
            .min(REVIEW_MAX_HERMES_MAX)
    }

    /// Whether the LLM semantic-review stage runs for HIGH blast-radius
    /// tasks (default true).
    pub fn llm_review_high_blast_or_default(&self) -> bool {
        self.llm_review_high_blast.unwrap_or(true)
    }

    /// The configured escalation threshold, or `None` when unset/out-of-range.
    ///
    /// `None` is load-bearing: it means "no calibrated operating point", so
    /// the local judge phase does not run and the pre-T170 frontier-only path
    /// is preserved exactly.
    pub fn escalate_threshold(&self) -> Option<f32> {
        match self.escalate_threshold {
            Some(value) if (0.0..=1.0).contains(&value) => Some(value),
            Some(value) => {
                tracing::warn!(
                    value,
                    "[agentic.review] escalate_threshold outside 0.0..=1.0; ignoring (local judge phase stays off)"
                );
                None
            }
            None => None,
        }
    }
}

/// Delegate sub-agent configuration — TOML `[agentic.delegate]` (006).
///
/// Scope logic (Constitution XV):
/// - Functionality: gates the `delegate.task` tool — the model-driven
///   sub-agent delegation path (real LLM sub-turns, depth-1 by grants).
/// - User impact: `enabled = false` removes the tool from every agent's
///   reachable set, so turns never delegate (status quo ante 006).
/// - Default: enabled=true, timeout_secs=300 (clamped 30..=1800).
/// - Interaction: the kill-switch only gates tool registration; sub-agent
///   depth-1 exclusion and the token-budget gate apply independently.
const DELEGATE_TIMEOUT_DEFAULT: u64 = 300;
const DELEGATE_TIMEOUT_MIN: u64 = 30;
const DELEGATE_TIMEOUT_MAX: u64 = 1800;
const DELEGATE_DEPTH_DEFAULT: u32 = 1;
const DELEGATE_DEPTH_MIN: u32 = 1;
const DELEGATE_DEPTH_MAX: u32 = 3;
const DELEGATE_CONCURRENT_DEFAULT: u32 = 4;
const DELEGATE_CONCURRENT_MIN: u32 = 1;
const DELEGATE_CONCURRENT_MAX: u32 = 8;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct DelegateConfig {
    /// Delegate tool availability (absent → true).
    pub enabled: Option<bool>,
    /// Wall-clock budget per delegated sub-turn in seconds
    /// (absent → 300, clamped 30..=1800).
    pub timeout_secs: Option<u64>,
    /// Delegation chain depth cap (absent → 1, clamped 1..=3).
    /// 1 = orchestrator-only delegation (006 depth-1 behavior);
    /// the 001 A.1 hierarchy target is 3 (L0→L1→L2).
    pub max_depth: Option<u32>,
    /// Parallel fan-out width for multi-task delegation
    /// (absent → 4, clamped 1..=8).
    pub max_concurrent: Option<u32>,
}

impl DelegateConfig {
    /// Whether `delegate.task` is registered at all (default true).
    pub fn enabled_or_default(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    /// Effective per-delegation wall-clock budget (default 300, clamped).
    pub fn timeout_or_default(&self) -> u64 {
        self.timeout_secs
            .unwrap_or(DELEGATE_TIMEOUT_DEFAULT)
            .clamp(DELEGATE_TIMEOUT_MIN, DELEGATE_TIMEOUT_MAX)
    }

    /// Effective delegation depth cap (default 1, clamped 1..=3).
    pub fn max_depth_or_default(&self) -> u32 {
        self.max_depth
            .unwrap_or(DELEGATE_DEPTH_DEFAULT)
            .clamp(DELEGATE_DEPTH_MIN, DELEGATE_DEPTH_MAX)
    }

    /// Effective parallel fan-out width (default 4, clamped 1..=8).
    pub fn max_concurrent_or_default(&self) -> u32 {
        self.max_concurrent
            .unwrap_or(DELEGATE_CONCURRENT_DEFAULT)
            .clamp(DELEGATE_CONCURRENT_MIN, DELEGATE_CONCURRENT_MAX)
    }
}

/// Orchestrator tool surface — TOML `[agentic.orchestrator]` (T378).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct OrchestratorConfig {
    /// `full` (default) keeps Sisyphus's direct tool grants;
    /// `delegation-only` strips every direct tool so all work happens
    /// in scoped sub-agents via delegate.task / plan.execute
    /// (001 A.8 ultra surface). Unknown values fall back to `full`.
    pub surface: Option<String>,
}

pub const ORCHESTRATOR_SURFACE_DELEGATION_ONLY: &str = "delegation-only";
pub const ORCHESTRATOR_SURFACE_FULL: &str = "full";

impl OrchestratorConfig {
    /// Effective surface string; invalid values warn and degrade to
    /// `full` (a typo must never strip the orchestrator's tools).
    pub fn surface_or_default(&self) -> &'static str {
        match self.surface.as_deref() {
            Some(ORCHESTRATOR_SURFACE_DELEGATION_ONLY) => ORCHESTRATOR_SURFACE_DELEGATION_ONLY,
            Some(ORCHESTRATOR_SURFACE_FULL) | None => ORCHESTRATOR_SURFACE_FULL,
            Some(other) => {
                tracing::warn!(
                    surface = other,
                    "invalid [agentic.orchestrator] surface; falling back to full"
                );
                ORCHESTRATOR_SURFACE_FULL
            }
        }
    }

    /// True when the orchestrator must run delegation-only.
    pub fn delegation_only(&self) -> bool {
        self.surface_or_default() == ORCHESTRATOR_SURFACE_DELEGATION_ONLY
    }
}

/// Intent classification configuration — TOML `[agentic.intent]` (T168/T169).
///
/// Scope logic (Constitution XV):
/// - Functionality: `shadow_embedding` gates the L1 observation at the intent
///   decision point (the embedding router runs after the production decision
///   and records a `loop.decision` audit line; it never affects routing).
///   `l1_threshold` is the calibrated operating point that lets the L1 rung
///   *gate* the decision (T169) — the ladder tries L1 first and only falls
///   through to the LLM when L1's confidence is below it.
/// - User impact: shadow mode starts accumulating calibration data (L1 vs
///   production disagreements are the highest-information samples for T173);
///   setting `l1_threshold` activates the fast path that avoids the LLM
///   classification call on confidently-L1 turns.
/// - Default: `shadow_embedding = false` (per-turn cost unchanged until
///   opt-in) and `l1_threshold = absent`. **An absent threshold means the
///   L1 rung never gates** — the ladder behaves exactly as it did before
///   T169. This is deliberate (V13-A.3): no τ is ever invented here; it is
///   supplied by calibration (T173's harness writes
///   `<ZEN_HOME>/logs/decision-audit/thresholds.json`) or explicitly by the
///   user via this key.
/// - Interaction: `ZEN_INTENT_SHADOW_EMBEDDING` / `ZEN_INTENT_L1_THRESHOLD`
///   env vars override any config layer (5th layer). The config value wins
///   over the calibration artefact when both are present, so a user can pin
///   or override a calibrated operating point.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct IntentConfig {
    /// Run the L1 embedding rung in shadow (observation-only) after each
    /// intent classification. Default false.
    pub shadow_embedding: Option<bool>,
    /// Calibrated confidence threshold for the L1 gate. Absent ⇒ the L1 rung
    /// never gates and the pre-T169 behaviour is preserved exactly. Valid
    /// range 0.0..=1.0; out-of-range values warn and are ignored.
    pub l1_threshold: Option<f32>,
}

impl IntentConfig {
    /// Effective shadow-embedding flag (default false).
    pub fn shadow_embedding_or_default(&self) -> bool {
        self.shadow_embedding.unwrap_or(false)
    }

    /// The configured L1 gate, or `None` when unset/out-of-range.
    ///
    /// `None` is the load-bearing signal: it means "no calibrated operating
    /// point", so every L1 gate stays closed.
    pub fn l1_threshold(&self) -> Option<f32> {
        match self.l1_threshold {
            Some(value) if (0.0..=1.0).contains(&value) => Some(value),
            Some(value) => {
                tracing::warn!(
                    value,
                    "[agentic.intent] l1_threshold outside 0.0..=1.0; ignoring (L1 gate stays closed)"
                );
                None
            }
            None => None,
        }
    }
}

/// Binary-classifier thresholds — TOML `[agentic.classifiers]` (T171).
///
/// Scope logic (Constitution XV):
/// - Functionality: calibrated operating points for the two T171 binary
///   classifiers — "is this user turn correcting prior output?" and "does
///   this response cite this note?" — which supersede T161's substring and
///   fingerprint heuristics once a threshold exists.
/// - User impact: setting either activates the local classifier for that
///   decision; a confident verdict then drives FR-034 reward bookkeeping
///   instead of the substring rule.
/// - Default: both absent. **Absent means the T161 heuristic stays
///   authoritative** — no threshold is invented (V13-A.3); values come from
///   calibration (T173's `logs/decision-audit/thresholds.json`) or this
///   explicit override. Valid range 0.0..=1.0; out-of-range is ignored.
/// - Interaction: `ZEN_CLASSIFIER_CORRECTION_THRESHOLD` /
///   `ZEN_CLASSIFIER_CITATION_THRESHOLD` override any config layer; the
///   config value wins over the calibration artefact.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ClassifierConfig {
    /// Gate for the is-this-a-correction classifier. Absent ⇒ T161's
    /// CORRECTION_MARKERS leading-clause rule decides.
    pub correction_threshold: Option<f32>,
    /// Gate for the does-this-cite classifier. Absent ⇒ T161's body-fingerprint
    /// containment rule decides.
    pub citation_threshold: Option<f32>,
}

impl ClassifierConfig {
    /// The configured correction gate, or `None` when unset/out-of-range.
    pub fn correction_threshold(&self) -> Option<f32> {
        validated_threshold(self.correction_threshold, "correction_threshold")
    }

    /// The configured citation gate, or `None` when unset/out-of-range.
    pub fn citation_threshold(&self) -> Option<f32> {
        validated_threshold(self.citation_threshold, "citation_threshold")
    }
}

/// A classifier threshold is usable only inside `0.0..=1.0`; anything else
/// warns and leaves the gate closed rather than silently mis-gating.
fn validated_threshold(value: Option<f32>, key: &str) -> Option<f32> {
    match value {
        Some(value) if (0.0..=1.0).contains(&value) => Some(value),
        Some(value) => {
            tracing::warn!(key, value, "threshold outside 0.0..=1.0; gate stays closed");
            None
        }
        None => None,
    }
}

/// Upper bound on a decision excerpt, regardless of configuration.
pub const AUDIT_EXCERPT_MAX: usize = 500;

/// Decision-audit content policy — TOML `[agentic.audit]`.
///
/// Scope logic (Constitution XV):
/// - Functionality: controls whether the decision audit lines carry a bounded
///   excerpt of the user's input. Without one, a recorded decision cannot be
///   adjudicated by a human, so `labels.jsonl` can never be filled and the
///   calibrated thresholds can never open.
/// - User impact: **enabling this writes (bounded) user input into a local log
///   file** (`<ZEN_HOME>/logs/audit.jsonl`). That is a privacy decision, so it
///   is off unless the user turns it on. When on, the excerpt is clamped to
///   [`AUDIT_EXCERPT_MAX`] characters, truncated on a character boundary, and
///   has its newlines flattened to spaces so it can never forge a second
///   JSONL line.
/// - Default: 0 = off. Nothing about the input is written unless explicitly
///   configured.
/// - Interaction: `ZEN_AUDIT_DECISION_EXCERPT_CHARS` overrides any config
///   layer; values above the cap are clamped, not trusted.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct AuditConfig {
    /// Characters of user input to record on a decision audit line. 0 = off.
    pub decision_excerpt_chars: Option<usize>,
}

impl AuditConfig {
    /// Effective excerpt length: 0 (off) unless configured, clamped to
    /// `0..=AUDIT_EXCERPT_MAX`.
    pub fn decision_excerpt_chars_or_default(&self) -> usize {
        self.decision_excerpt_chars
            .unwrap_or(0)
            .min(AUDIT_EXCERPT_MAX)
    }

    /// Build the single-line, char-boundary-truncated excerpt of `input`, or
    /// `None` when the feature is off (0).
    ///
    /// Newlines and carriage returns are replaced with spaces so one excerpt
    /// can never forge a second JSONL line; truncation happens on a character
    /// boundary so a UTF-8 codepoint is never split. The result is at most
    /// [`AUDIT_EXCERPT_MAX`] characters — the configured value is clamped,
    /// never trusted.
    pub fn excerpt(&self, input: &str) -> Option<String> {
        let limit = self.decision_excerpt_chars_or_default();
        if limit == 0 {
            return None;
        }
        let flattened: String = input
            .chars()
            .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
            .collect();
        let excerpt: String = flattened.chars().take(limit).collect();
        Some(excerpt.trim().to_string())
    }
}

/// Retention sweep configuration — TOML `[agentic.retention]` (review D2).
///
/// Scope logic (Constitution XV):
/// - Functionality: gates the daily `RetentionWorker` sweep that bounds the
///   append-only filesystem homes (JSONL log rotation, age-based deletes,
///   keep-newest caps; quarantine is report-only). The policy values
///   themselves are code-defined defaults — no per-directory overrides.
/// - User impact: `enabled = false` stops every retention sweep (the homes
///   grow unbounded again); `dry_run = true` computes and audits the full
///   sweep report without mutating anything (safe preview).
/// - Default: enabled=true, dry_run=false.
/// - Interaction: env `ZEN_RETENTION_ENABLED` / `ZEN_RETENTION_DRY_RUN`
///   (5th layer) override any config file layer.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct RetentionConfig {
    /// Daily retention sweep fires when true (absent → true).
    pub enabled: Option<bool>,
    /// Compute + audit the sweep without mutating anything (absent → false).
    pub dry_run: Option<bool>,
}

impl RetentionConfig {
    pub fn enabled_or_default(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    pub fn dry_run_or_default(&self) -> bool {
        self.dry_run.unwrap_or(false)
    }
}

/// Skill sections — TOML `[skills.*]` (005-agentic-loop, T076).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct SkillsConfig {
    /// Pi-style trigger-hit auto-routing — TOML `[skills.auto_route]`.
    pub auto_route: SkillsAutoRouteConfig,
}

/// Skill hit auto-routing configuration (005-agentic-loop, T076) —
/// TOML `[skills.auto_route]`.
///
/// Scope logic (Constitution XV):
/// - Functionality: gates `SkillHitRouter` globally — when enabled, the
///   orchestrator matches the user query against skill triggers before
///   `route()` and injects at most one matching skill prompt into the M1
///   context (threshold 0.72, max_hits 1, contract skill-hit.json).
/// - User impact: `enabled = false` disables all automatic skill prompt
///   injection; skills remain manually runnable (`zen skill run`).
/// - Default: `enabled = true` (absent section → enabled).
/// - Interaction: a skill's own frontmatter `auto_route: false` opts that
///   skill out independently (per-skill overrides the global switch); env
///   `ZEN_SKILLS_AUTO_ROUTE` (5th layer) overrides any config file layer.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct SkillsAutoRouteConfig {
    /// Global auto-route switch (absent → enabled, contract skill-hit.json).
    pub enabled: Option<bool>,
}

impl SkillsAutoRouteConfig {
    /// Effective global switch: config value, or enabled when absent.
    pub fn enabled_or_default(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
}

/// One governed host directory (FR-033, T043).
///
/// TOML layout (inside `[agentic.loop]`):
/// ```toml
/// [[agentic.loop.host_sources]]
/// host_path   = "~/Documents/Work"
/// para_target = "areas"          # projects|areas|resources|archive
/// m_tier      = "M3"             # M3|M4|M5
/// worker_type = "doc"            # code|doc
/// raw_policy  = "copy"           # index-only|copy
/// sensitivity = "Private"        # Private|Internal
/// allow_cloud = false            # cloud LLM extraction opt-in
/// ```
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct HostSourceConfig {
    /// Absolute or `~`-expanded host directory path.
    pub host_path: String,
    /// PARA bucket the source classifies into.
    pub para_target: Option<String>,
    /// DESIGN memory tier this source feeds (M3/M4/M5).
    pub m_tier: Option<String>,
    /// `code` (deterministic, index-only) or `doc` (LLM semantic, copy).
    pub worker_type: Option<String>,
    /// `index-only` (code track) or `copy` (doc track → `vault/raw/{host_hash}/`).
    pub raw_policy: Option<String>,
    /// `Private` (default for Personal/Work) or `Internal`.
    pub sensitivity: Option<String>,
    /// Cloud LLM extraction opt-in; false (default) forces local models for Private.
    pub allow_cloud: Option<bool>,
}

// ---------------------------------------------------------------------------
// Host governance resolution (FR-033, T043)
// ---------------------------------------------------------------------------

/// Normalized worker kind for a host source (FR-033 `worker_type: code|doc`).
///
/// Scope logic (Constitution XV):
/// - Functionality: forces the Router lane — `Code` = deterministic slug
///   extraction, index-only provenance, no vault copy; `Doc` = agentic
///   semantic extraction with raw preservation copy.
/// - User impact: absent `worker_type` → `None` → the GraphRouter classifies
///   per-file by content heuristics (existing `resolve_track` behavior).
/// - Default: `None` (auto-classify).
/// - Interaction: invalid TOML values are rejected by
///   [`HostSourceConfig::resolve`] — the loop worker warns and skips the
///   whole source (never silently reinterprets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostWorkerKind {
    /// Deterministic track (no LLM, index-only).
    Code,
    /// Agentic semantic track (raw copy + limited products).
    Doc,
}

impl HostWorkerKind {
    /// Parse a TOML `worker_type` value; `None` on anything but `code`/`doc`.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "code" => Some(Self::Code),
            "doc" => Some(Self::Doc),
            _ => None,
        }
    }

    /// Canonical string form (used in frontmatter tags and audit events).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Doc => "doc",
        }
    }
}

/// Raw-preservation policy for a host source (FR-033 `raw_policy`).
///
/// Scope logic (Constitution XV):
/// - Functionality: decides whether host files are copied into
///   `vault/raw/{host_hash}/` (read-only preservation with provenance
///   frontmatter) or only indexed in place.
/// - User impact: `index-only` keeps GB-scale repos out of the vault;
///   `copy` preserves document originals for full-text search/reindex.
/// - Default: `index-only` when `worker_type = code`, `copy` otherwise
///   (including auto-classified sources).
/// - Interaction: the code track NEVER copies regardless of this policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostRawPolicy {
    /// Provenance pages only; no file copies (code track default).
    IndexOnly,
    /// Copy originals to `vault/raw/{host_hash}/` (doc track default).
    Copy,
}

impl HostRawPolicy {
    /// Parse a TOML `raw_policy` value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "index-only" => Some(Self::IndexOnly),
            "copy" => Some(Self::Copy),
            _ => None,
        }
    }

    /// Canonical string form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IndexOnly => "index-only",
            Self::Copy => "copy",
        }
    }
}

/// Fully-resolved host source: validated config + `~` expansion + stable
/// staging hash. Built once per cycle by the loop worker via
/// [`HostSourceConfig::resolve`]; invalid sources never produce a context.
#[derive(Debug, Clone)]
pub struct HostSourceContext {
    /// `~`/`$HOME`-expanded host directory.
    pub host_path: PathBuf,
    /// First 8 hex chars of SHA-256 over the expanded host path — stable
    /// key for `_incoming/{host_hash}/` staging and `raw/{host_hash}/` copies.
    pub host_hash: String,
    /// `None` = auto-classify per file in the GraphRouter.
    pub worker_type: Option<HostWorkerKind>,
    /// Resolved preservation policy (defaults by worker kind).
    pub raw_policy: HostRawPolicy,
    /// Data classification feeding sensitivity routing (default Private).
    pub sensitivity: crate::types::Sensitivity,
    /// Cloud LLM extraction opt-in (default false → local-only routing).
    pub allow_cloud: bool,
    /// PARA bucket for code-track provenance pages; None = no page.
    pub para_target: Option<String>,
    /// DESIGN memory tier tag (M3/M4/M5), stamped into frontmatter/metadata.
    pub m_tier: Option<String>,
    /// Workspace identity stamped into frontmatter for cross-workspace filtering.
    pub workspace_id: Option<String>,
}

impl HostSourceContext {
    /// True when files from this source are preserved under
    /// `vault/raw/{host_hash}/` (doc track with copy policy).
    pub fn preserves_raw(&self) -> bool {
        self.raw_policy == HostRawPolicy::Copy && self.worker_type != Some(HostWorkerKind::Code)
    }
}

/// Expand `~` and `$HOME` in a configured host path.
///
/// # Parameters
/// - `raw` — the literal `host_path` string from config.
///
/// # Returns
/// Expanded absolute path (best-effort: an empty `$HOME` leaves the literal).
pub fn expand_home_path(raw: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    if raw == "~" {
        return PathBuf::from(home);
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return PathBuf::from(format!("{home}/{rest}"));
    }
    PathBuf::from(raw.replace("$HOME", &home))
}

/// Stable staging key for a host directory: first 8 hex chars of
/// SHA-256 over the expanded path string. Collisions across the 12-entry
/// planning reference are practically impossible; the full path always
/// travels alongside in provenance frontmatter.
///
/// # Parameters
/// - `host_path` — the expanded host directory.
///
/// # Returns
/// 8 lowercase hex characters.
pub fn host_dir_hash(host_path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(host_path.to_string_lossy().as_bytes());
    digest[..4].iter().map(|b| format!("{b:02x}")).collect()
}

use std::path::Path;

impl HostSourceConfig {
    /// Validate and resolve this source into a [`HostSourceContext`].
    ///
    /// # Parameters
    /// - `workspace_id` — workspace identity for frontmatter stamping
    ///   (usually `ZenPaths::workspace_root()` as a string).
    ///
    /// # Returns
    /// The resolved context with `~` expansion and staging hash applied.
    ///
    /// # Errors
    /// `Err(reason)` when any field is invalid — the caller (loop worker)
    /// logs a warning and skips the source (fail-closed per FR-033):
    /// empty `host_path`, `worker_type` ∉ {code, doc}, `raw_policy` ∉
    /// {index-only, copy}, `sensitivity` ∉ {Private, Internal, Public},
    /// or `para_target` ∉ {projects, areas, resources, archive}.
    pub fn resolve(&self, workspace_id: Option<&str>) -> Result<HostSourceContext, String> {
        if self.host_path.trim().is_empty() {
            return Err("host_path is empty".to_string());
        }
        let worker_type = match self.worker_type.as_deref() {
            None => None,
            Some(v) => Some(
                HostWorkerKind::parse(v)
                    .ok_or_else(|| format!("invalid worker_type `{v}` (expected code|doc)"))?,
            ),
        };
        let raw_policy = match self.raw_policy.as_deref() {
            None => match worker_type {
                Some(HostWorkerKind::Code) => HostRawPolicy::IndexOnly,
                _ => HostRawPolicy::Copy,
            },
            Some(v) => HostRawPolicy::parse(v)
                .ok_or_else(|| format!("invalid raw_policy `{v}` (expected index-only|copy)"))?,
        };
        let sensitivity = match self.sensitivity.as_deref() {
            None => crate::types::Sensitivity::Private,
            Some(v) => match v {
                "Private" => crate::types::Sensitivity::Private,
                // FR-033 "Internal" (company-internal) maps onto the
                // Sensitivity taxonomy's local-only Private tier — both are
                // withheld from cloud routing by `enforce_sensitivity`.
                "Internal" => crate::types::Sensitivity::Private,
                "Public" => crate::types::Sensitivity::Public,
                other => {
                    return Err(format!(
                        "invalid sensitivity `{other}` (expected Private|Internal|Public)"
                    ));
                }
            },
        };
        if let Some(target) = self.para_target.as_deref()
            && !matches!(target, "projects" | "areas" | "resources" | "archive")
        {
            return Err(format!(
                "invalid para_target `{target}` (expected projects|areas|resources|archive)"
            ));
        }
        let host_path = expand_home_path(&self.host_path);
        Ok(HostSourceContext {
            host_hash: host_dir_hash(&host_path),
            host_path,
            worker_type,
            raw_policy,
            sensitivity,
            allow_cloud: self.allow_cloud.unwrap_or(false),
            para_target: self.para_target.clone(),
            m_tier: self.m_tier.clone(),
            workspace_id: workspace_id.map(|s| s.to_string()),
        })
    }
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

fn default_web_fetch_max_size() -> u32 {
    50
}

fn default_web_fetch_max_lines() -> u32 {
    2000
}

fn default_web_fetch_timeout() -> u64 {
    10000
}

fn default_jina_threshold() -> u32 {
    500
}

fn default_web_fetch_user_agent() -> String {
    "zen-agent/1.0".to_string()
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
    /// Knowledge-base search behaviour for interactive chat context injection
    /// (`[tui] knowledge_search`, T054). See [`KnowledgeSearchMode`].
    pub knowledge_search: KnowledgeSearchMode,
}

/// Knowledge-base search mode for interactive TUI chat (`[tui] knowledge_search`).
///
/// - `fast` (default): cap the search tier at FTS5 — no embeddings, graph, or
///   LLM synthesis — and apply a per-directory timeout, keeping
///   Enter → LLM dispatch snappy (input-display-plan.md P0).
/// - `full`: use the tier selected by `TierSelector` (previous behaviour).
/// - `off`: skip knowledge-base search entirely (direct file lookup only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KnowledgeSearchMode {
    #[default]
    Fast,
    Full,
    Off,
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
/// # cache_dir = "~/.cache/fastembed"   # global share across projects
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
    /// Cache directory for fastembed model downloads.
    /// Default: `./.fastembed_cache` (project-local).
    /// Recommended: `~/.cache/fastembed/` or `~/.zen/.cache/fastembed/` for global sharing.
    /// Can also be set via `ZEN_EMBEDDINGS_CACHE_DIR` env var.
    pub cache_dir: Option<String>,
}

/// Web fetch tool configuration — controls content extraction limits and fallback behavior.
///
/// `Default` is implemented by hand to match the `#[serde(default = "...")]`
/// helpers; `#[derive(Default)]` would yield zeroed fields (e.g. `timeout_ms = 0`).
#[derive(Debug, Clone, Deserialize)]
pub struct WebFetchConfig {
    /// Maximum content size in KB to fetch and process.
    #[serde(default = "default_web_fetch_max_size")]
    pub max_content_size_kb: u32,
    /// Maximum number of lines to extract from fetched content.
    #[serde(default = "default_web_fetch_max_lines")]
    pub max_lines: u32,
    /// HTTP request timeout in milliseconds.
    #[serde(default = "default_web_fetch_timeout")]
    pub timeout_ms: u64,
    /// Enable Jina Reader API fallback for JS-rendered pages.
    #[serde(default = "default_true")]
    pub jina_fallback: bool,
    /// Character threshold below which Jina fallback is used.
    #[serde(default = "default_jina_threshold")]
    pub jina_fallback_threshold_chars: u32,
    /// User-Agent header for direct HTTP fetches.
    #[serde(default = "default_web_fetch_user_agent")]
    pub user_agent: String,
}

impl Default for WebFetchConfig {
    fn default() -> Self {
        Self {
            max_content_size_kb: default_web_fetch_max_size(),
            max_lines: default_web_fetch_max_lines(),
            timeout_ms: default_web_fetch_timeout(),
            jina_fallback: default_true(),
            jina_fallback_threshold_chars: default_jina_threshold(),
            user_agent: default_web_fetch_user_agent(),
        }
    }
}

/// Web search tool configuration — provider selection and API keys.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct WebSearchConfig {
    /// Explicit provider override: "duckduckgo" | "brave" | "tavily".
    #[serde(default)]
    pub default_provider: Option<String>,
    /// Brave Search API key (falls back to `BRAVE_SEARCH_API_KEY` env).
    #[serde(default)]
    pub api_key_brave: Option<String>,
    /// Tavily API key (falls back to `TAVILY_API_KEY` env).
    #[serde(default)]
    pub api_key_tavily: Option<String>,
}

/// Persistent trust store for MCP server trust decisions.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpTrustStore {
    #[serde(default)]
    pub trusted_servers: HashMap<String, bool>,
}

impl McpTrustStore {
    pub fn load(paths: &ZenPaths) -> Result<Self, ZenError> {
        let path = paths.global_root().join("mcp_trust.json");
        match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str(&contents) {
                Ok(store) => Ok(store),
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "MCP trust store corrupted, starting fresh"
                    );
                    Ok(Self::default())
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn is_trusted(&self, server_name: &str) -> bool {
        *self.trusted_servers.get(server_name).unwrap_or(&false)
    }

    pub fn set_trusted(&mut self, server_name: &str, trusted: bool) {
        self.trusted_servers
            .insert(server_name.to_string(), trusted);
    }

    pub fn save(&self, paths: &ZenPaths) -> Result<(), ZenError> {
        let path = paths.global_root().join("mcp_trust.json");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct McpServerConfig {
    pub name: String,
    pub transport: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub auto_refresh: bool,
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
            tui_scheduler: Some(true),
        }
    }
}

impl Default for PluginConfig {
    fn default() -> Self {
        let zen_root = crate::paths::user_root();
        let root_str = zen_root.display().to_string();
        Self {
            base_path: Some(format!("{root_str}/plugins")),
            wasm_cache_path: Some(format!("{root_str}/plugins/cache")),
            plugins: HashMap::new(),
        }
    }
}

impl PluginConfig {
    /// Resolve `base_path` to a concrete directory, expanding a leading `~`
    /// against the user's home directory. Unresolvable home → the raw path.
    pub fn resolved_base_path(&self) -> Option<PathBuf> {
        self.base_path.as_ref().map(|p| {
            if let Some(rest) = p.strip_prefix('~') {
                home::home_dir()
                    .map(|h| h.join(rest.trim_start_matches('/')))
                    .unwrap_or_else(|| PathBuf::from(p))
            } else {
                PathBuf::from(p)
            }
        })
    }

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
// Config loading (T023) — Priority: env → global → embedded → Default
// ---------------------------------------------------------------------------

/// Load AgenticConfig with the full priority chain (cached):
/// 1. Rust `Default` impl
/// 2. Embedded `config/config.toml`
/// 3. Global `~/.zen/config.toml`
/// 4. Environment variables (`ZEN_*`)
///
/// Path Spec v2 (T18/D8-rev1): workspace config.toml is IGNORED — config
/// is global-only. `.zen/` survives as project context marker + sandbox
/// allowlist only.
///
/// Keychain resolution (FR-061 `SecretRef`) is deferred to zen-auth (T033-T034).
///
/// **Caching**: Config is parsed once per process and cached globally.
/// Subsequent calls return a reference to the cached config.
/// In test mode, caching is disabled to allow environment variable changes.
pub fn load_config() -> Result<&'static ZenConfig, ZenError> {
    #[cfg(test)]
    {
        dotenvy::dotenv().ok();
        let paths = ZenPaths::detect().map_err(ZenError::Path)?;
        let embedded = load_embedded_config()?;
        let global = load_file_config(paths.global_root().join("config.toml")).unwrap_or_default();
        // Path Spec v2 (T18/D8-rev1): 4-layer merge only.
        // Workspace config.toml is IGNORED — config is global-only.
        let merged = merge_configs(embedded, global)?;
        let config = apply_env_overrides(merged);
        Ok(Box::leak(Box::new(config)))
    }

    #[cfg(not(test))]
    {
        if let Ok(guard) = CONFIG_CACHE.read()
            && let Some(config) = guard.as_ref()
        {
            // SAFETY: the cached value lives for the process lifetime and is
            // only replaced by `invalidate_config_cache` (test support), which
            // never runs concurrently with production reads.
            let ptr: *const ZenConfig = config;
            return Ok(unsafe { &*ptr });
        }

        dotenvy::dotenv().ok();

        let paths = ZenPaths::detect().map_err(ZenError::Path)?;

        // 1. Embedded defaults (T026)
        let embedded = load_embedded_config()?;

        // 2. Global config from ~/.zen/config.toml (T025)
        let global = load_file_config(paths.global_root().join("config.toml")).unwrap_or_default();

        // 3. Path Spec v2 (T18/D8-rev1): 4-layer merge only.
        // Workspace config.toml is IGNORED — config is global-only.

        // 4. Merge: embedded ← global
        let merged = merge_configs(embedded, global)?;

        // 5. Environment overrides take highest priority
        let config = apply_env_overrides(merged);

        let mut guard = CONFIG_CACHE.write().map_err(|_| {
            ZenError::Config(ConfigError::ParseError {
                path: "global".to_string(),
                reason: "Config initialization failed".to_string(),
            })
        })?;
        *guard = Some(config);
        let ptr: *const ZenConfig = guard.as_ref().expect("value just stored");
        // SAFETY: the value is owned by the process-lifetime static and is
        // never dropped (`invalidate_config_cache` forgets instead of
        // dropping); the guard is released before returning.
        let static_ref: &'static ZenConfig = unsafe { &*ptr };
        drop(guard);
        Ok(static_ref)
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
        agents_tools: merge_agents_tools(base.agents_tools, override_cfg.agents_tools),
        features: merge_features(base.features, override_cfg.features),
        channels: merge_channels(base.channels, override_cfg.channels),
        cron: merge_cron(base.cron, override_cfg.cron),
        plugin: merge_plugin(base.plugin, override_cfg.plugin),
        feeds: merge_feeds(base.feeds, override_cfg.feeds),
        tui: merge_tui(base.tui, override_cfg.tui),
        history: merge_history(base.history, override_cfg.history),
        embeddings: merge_embeddings(base.embeddings, override_cfg.embeddings),
        web_fetch: merge_web_fetch(base.web_fetch, override_cfg.web_fetch),
        web_search: merge_web_search(base.web_search, override_cfg.web_search),
        mcp_servers: merge_mcp_servers(base.mcp_servers, override_cfg.mcp_servers),
        sandbox: merge_sandbox(base.sandbox, override_cfg.sandbox),
        agentic: merge_agentic(base.agentic, override_cfg.agentic),
        skills: merge_skills(base.skills, override_cfg.skills),
    })
}

fn merge_agentic(base: AgenticConfig, ov: AgenticConfig) -> AgenticConfig {
    AgenticConfig {
        loop_cfg: merge_loop(base.loop_cfg, ov.loop_cfg),
        tool_loop: merge_tool_loop(base.tool_loop, ov.tool_loop),
        review: merge_review(base.review, ov.review),
        delegate: merge_delegate(base.delegate, ov.delegate),
        orchestrator: OrchestratorConfig {
            surface: ov.orchestrator.surface.or(base.orchestrator.surface),
        },
        intent: IntentConfig {
            shadow_embedding: ov.intent.shadow_embedding.or(base.intent.shadow_embedding),
            l1_threshold: ov.intent.l1_threshold.or(base.intent.l1_threshold),
        },
        classifiers: ClassifierConfig {
            correction_threshold: ov
                .classifiers
                .correction_threshold
                .or(base.classifiers.correction_threshold),
            citation_threshold: ov
                .classifiers
                .citation_threshold
                .or(base.classifiers.citation_threshold),
        },
        audit: AuditConfig {
            decision_excerpt_chars: ov
                .audit
                .decision_excerpt_chars
                .or(base.audit.decision_excerpt_chars),
        },
        retention: RetentionConfig {
            enabled: ov.retention.enabled.or(base.retention.enabled),
            dry_run: ov.retention.dry_run.or(base.retention.dry_run),
        },
    }
}

fn merge_delegate(base: DelegateConfig, ov: DelegateConfig) -> DelegateConfig {
    DelegateConfig {
        enabled: ov.enabled.or(base.enabled),
        timeout_secs: ov.timeout_secs.or(base.timeout_secs),
        max_depth: ov.max_depth.or(base.max_depth),
        max_concurrent: ov.max_concurrent.or(base.max_concurrent),
    }
}

fn merge_tool_loop(base: ToolLoopConfig, ov: ToolLoopConfig) -> ToolLoopConfig {
    ToolLoopConfig {
        max_rounds: ov.max_rounds.or(base.max_rounds),
    }
}

fn merge_review(base: ReviewConfig, ov: ReviewConfig) -> ReviewConfig {
    ReviewConfig {
        max_momus_retries: ov.max_momus_retries.or(base.max_momus_retries),
        max_hermes_revisions: ov.max_hermes_revisions.or(base.max_hermes_revisions),
        llm_review_high_blast: ov.llm_review_high_blast.or(base.llm_review_high_blast),
        escalate_threshold: ov.escalate_threshold.or(base.escalate_threshold),
    }
}

fn merge_skills(base: SkillsConfig, ov: SkillsConfig) -> SkillsConfig {
    SkillsConfig {
        auto_route: SkillsAutoRouteConfig {
            enabled: ov.auto_route.enabled.or(base.auto_route.enabled),
        },
    }
}

fn merge_loop(base: LoopConfig, ov: LoopConfig) -> LoopConfig {
    LoopConfig {
        enabled: ov.enabled.or(base.enabled),
        interval: str_merge(base.interval, ov.interval),
        merge_threshold: ov.merge_threshold.or(base.merge_threshold),
        merge_pure_duplicate: ov.merge_pure_duplicate.or(base.merge_pure_duplicate),
        merge_llm_model: str_merge(base.merge_llm_model, ov.merge_llm_model),
        max_attempts: ov.max_attempts.or(base.max_attempts),
        min_free_bytes: ov.min_free_bytes.or(base.min_free_bytes),
        skip_extensions: ov.skip_extensions.or(base.skip_extensions),
        host_sources: if ov.host_sources.is_empty() {
            base.host_sources
        } else {
            ov.host_sources
        },
        hypothesis_refinement: ov.hypothesis_refinement.or(base.hypothesis_refinement),
        reverify_older_than_days: ov
            .reverify_older_than_days
            .or(base.reverify_older_than_days),
        raw_graph_routing: ov.raw_graph_routing.or(base.raw_graph_routing),
        cas_commit: ov.cas_commit.or(base.cas_commit),
        host_stage_timeout_secs: ov.host_stage_timeout_secs.or(base.host_stage_timeout_secs),
        max_steps: ov.max_steps.or(base.max_steps),
        max_tokens: ov.max_tokens.or(base.max_tokens),
        max_ingest_bytes: ov.max_ingest_bytes.or(base.max_ingest_bytes),
        community_resolution: ov.community_resolution.or(base.community_resolution),
        community_min_size: ov.community_min_size.or(base.community_min_size),
    }
}

/// Grants accumulate across config layers: any layer enabling a WASM
/// permission keeps it enabled (absent sections parse as all-false).
fn merge_sandbox(base: SandboxConfig, ov: SandboxConfig) -> SandboxConfig {
    SandboxConfig {
        wasm: WasmSandboxConfig {
            allow_filesystem_read: base.wasm.allow_filesystem_read || ov.wasm.allow_filesystem_read,
            allow_filesystem_write: base.wasm.allow_filesystem_write
                || ov.wasm.allow_filesystem_write,
            allow_network: base.wasm.allow_network || ov.wasm.allow_network,
            allow_system: base.wasm.allow_system || ov.wasm.allow_system,
        },
        network_access: base.network_access || ov.network_access,
    }
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
                existing.embedding_model = v
                    .embedding_model
                    .clone()
                    .or(existing.embedding_model.clone());
                existing.wire_api = v.wire_api.clone().or(existing.wire_api.clone());
                existing.models = merge_models(existing.models.clone(), v.models.clone());
                existing.input_cost_per_million =
                    v.input_cost_per_million.or(existing.input_cost_per_million);
                existing.output_cost_per_million = v
                    .output_cost_per_million
                    .or(existing.output_cost_per_million);
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

/// FR-046: tool-grant overlays accumulate across config layers — a grant
/// from a lower layer stays in force, later layers append entries not
/// already present, and absent overlays stay empty (builtin set unchanged).
fn merge_agents_tools(mut base: Vec<String>, ov: Vec<String>) -> Vec<String> {
    for entry in ov {
        if !base.contains(&entry) {
            base.push(entry);
        }
    }
    base
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
        tui_scheduler: ov.tui_scheduler.or(base.tui_scheduler),
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
        // Non-default override wins; otherwise keep the base layer's value.
        knowledge_search: if ov.knowledge_search != KnowledgeSearchMode::default() {
            ov.knowledge_search
        } else {
            base.knowledge_search
        },
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
        cache_dir: str_merge(base.cache_dir, ov.cache_dir),
    }
}

fn merge_web_fetch(base: WebFetchConfig, override_cfg: WebFetchConfig) -> WebFetchConfig {
    WebFetchConfig {
        max_content_size_kb: override_cfg.max_content_size_kb,
        max_lines: override_cfg.max_lines,
        timeout_ms: override_cfg.timeout_ms,
        jina_fallback: override_cfg.jina_fallback,
        jina_fallback_threshold_chars: override_cfg.jina_fallback_threshold_chars,
        user_agent: if override_cfg.user_agent != default_web_fetch_user_agent() {
            override_cfg.user_agent
        } else {
            base.user_agent
        },
    }
}

fn merge_web_search(base: WebSearchConfig, override_cfg: WebSearchConfig) -> WebSearchConfig {
    WebSearchConfig {
        default_provider: override_cfg
            .default_provider
            .clone()
            .or_else(|| base.default_provider.clone()),
        api_key_brave: override_cfg
            .api_key_brave
            .clone()
            .or_else(|| base.api_key_brave.clone()),
        api_key_tavily: override_cfg
            .api_key_tavily
            .clone()
            .or_else(|| base.api_key_tavily.clone()),
    }
}

fn merge_mcp_servers(
    base: Vec<McpServerConfig>,
    override_cfg: Vec<McpServerConfig>,
) -> Vec<McpServerConfig> {
    let mut merged: Vec<McpServerConfig> = base;
    for server in override_cfg {
        if let Some(existing) = merged.iter_mut().find(|s| s.name == server.name) {
            existing.transport = server.transport;
            existing.command = server.command.or(existing.command.clone());
            existing.args = server.args.or(existing.args.clone());
            existing.env = server.env.or(existing.env.clone());
            existing.url = server.url.or(existing.url.clone());
            existing.headers = server.headers.or(existing.headers.clone());
            existing.enabled = server.enabled;
            existing.auto_refresh = server.auto_refresh;
        } else {
            merged.push(server);
        }
    }
    merged
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
    apply_loop_env(&mut config.agentic.loop_cfg);
    apply_tool_loop_env(&mut config.agentic.tool_loop);
    apply_review_env(&mut config.agentic.review);
    apply_delegate_env(&mut config.agentic.delegate);
    apply_orchestrator_env(&mut config.agentic.orchestrator);
    apply_intent_env(&mut config.agentic.intent);
    apply_classifier_env(&mut config.agentic.classifiers);
    apply_audit_env(&mut config.agentic.audit);
    apply_retention_env(&mut config.agentic.retention);
    apply_skills_env(&mut config.skills.auto_route);
    config
}

fn apply_retention_env(cfg: &mut RetentionConfig) {
    if let Some(v) = env_bool("ZEN_RETENTION_ENABLED") {
        cfg.enabled = Some(v);
    }
    if let Some(v) = env_bool("ZEN_RETENTION_DRY_RUN") {
        cfg.dry_run = Some(v);
    }
}

fn apply_tool_loop_env(cfg: &mut ToolLoopConfig) {
    if let Some(v) = env_str("ZEN_TOOL_MAX_ROUNDS")
        && let Ok(n) = v.parse::<u8>()
    {
        cfg.max_rounds = Some(n.clamp(TOOL_MAX_ROUNDS_MIN, TOOL_MAX_ROUNDS_MAX));
    }
}

fn apply_review_env(cfg: &mut ReviewConfig) {
    if let Some(v) = env_str("ZEN_REVIEW_MAX_MOMUS_RETRIES")
        && let Ok(n) = v.parse::<u8>()
    {
        cfg.max_momus_retries = Some(n.min(REVIEW_MAX_MOMUS_MAX));
    }
    if let Some(v) = env_str("ZEN_REVIEW_MAX_HERMES_REVISIONS")
        && let Ok(n) = v.parse::<u8>()
    {
        cfg.max_hermes_revisions = Some(n.min(REVIEW_MAX_HERMES_MAX));
    }
    if let Some(v) = env_bool("ZEN_REVIEW_LLM_HIGH_BLAST") {
        cfg.llm_review_high_blast = Some(v);
    }
    if let Some(v) = env_str("ZEN_REVIEW_ESCALATE_THRESHOLD") {
        match v.trim().parse::<f32>() {
            Ok(parsed) => cfg.escalate_threshold = Some(parsed),
            Err(_) => tracing::warn!(
                value = %v,
                "ZEN_REVIEW_ESCALATE_THRESHOLD is not a number; ignoring (local judge phase stays off)"
            ),
        }
    }
}

fn apply_delegate_env(cfg: &mut DelegateConfig) {
    if let Some(v) = env_bool("ZEN_DELEGATE_ENABLED") {
        cfg.enabled = Some(v);
    }
    if let Some(v) = env_str("ZEN_DELEGATE_TIMEOUT_SECS")
        && let Ok(n) = v.parse::<u64>()
    {
        cfg.timeout_secs = Some(n);
    }
    if let Some(v) = env_str("ZEN_DELEGATE_MAX_DEPTH")
        && let Ok(n) = v.parse::<u32>()
    {
        cfg.max_depth = Some(n);
    }
    if let Some(v) = env_str("ZEN_DELEGATE_MAX_CONCURRENT")
        && let Ok(n) = v.parse::<u32>()
    {
        cfg.max_concurrent = Some(n);
    }
}

fn apply_orchestrator_env(cfg: &mut OrchestratorConfig) {
    if let Some(v) = env_str("ZEN_ORCHESTRATOR_SURFACE") {
        cfg.surface = Some(v);
    }
}

fn apply_audit_env(cfg: &mut AuditConfig) {
    if let Some(v) = env_str("ZEN_AUDIT_DECISION_EXCERPT_CHARS") {
        match v.trim().parse::<usize>() {
            Ok(parsed) => cfg.decision_excerpt_chars = Some(parsed),
            Err(_) => tracing::warn!(
                value = %v,
                "ZEN_AUDIT_DECISION_EXCERPT_CHARS is not a number; ignoring (excerpt stays off)"
            ),
        }
    }
}

fn apply_classifier_env(cfg: &mut ClassifierConfig) {
    if let Some(v) = env_str("ZEN_CLASSIFIER_CORRECTION_THRESHOLD") {
        match v.trim().parse::<f32>() {
            Ok(parsed) => cfg.correction_threshold = Some(parsed),
            Err(_) => tracing::warn!(
                value = %v,
                "ZEN_CLASSIFIER_CORRECTION_THRESHOLD is not a number; ignoring (heuristic decides)"
            ),
        }
    }
    if let Some(v) = env_str("ZEN_CLASSIFIER_CITATION_THRESHOLD") {
        match v.trim().parse::<f32>() {
            Ok(parsed) => cfg.citation_threshold = Some(parsed),
            Err(_) => tracing::warn!(
                value = %v,
                "ZEN_CLASSIFIER_CITATION_THRESHOLD is not a number; ignoring (heuristic decides)"
            ),
        }
    }
}

fn apply_intent_env(cfg: &mut IntentConfig) {
    if let Some(v) = env_bool("ZEN_INTENT_SHADOW_EMBEDDING") {
        cfg.shadow_embedding = Some(v);
    }
    if let Some(v) = env_str("ZEN_INTENT_L1_THRESHOLD") {
        match v.trim().parse::<f32>() {
            Ok(parsed) => cfg.l1_threshold = Some(parsed),
            Err(_) => tracing::warn!(
                value = %v,
                "ZEN_INTENT_L1_THRESHOLD is not a number; ignoring (L1 gate stays closed)"
            ),
        }
    }
}

fn apply_skills_env(cfg: &mut SkillsAutoRouteConfig) {
    if let Some(v) = env_bool("ZEN_SKILLS_AUTO_ROUTE") {
        cfg.enabled = Some(v);
    }
}

fn apply_loop_env(cfg: &mut LoopConfig) {
    if let Some(v) = env_bool("ZEN_LOOP_ENABLED") {
        cfg.enabled = Some(v);
    }
    if let Some(v) = env_str("ZEN_LOOP_INTERVAL") {
        cfg.interval = Some(v);
    }
    if let Some(v) = env_str("ZEN_LOOP_MERGE_THRESHOLD")
        && let Ok(f) = v.parse()
    {
        cfg.merge_threshold = Some(f);
    }
    if let Some(v) = env_u32("ZEN_LOOP_MAX_ATTEMPTS") {
        cfg.max_attempts = Some(v);
    }
    if let Some(v) = env_str("ZEN_LOOP_MIN_FREE_BYTES")
        && let Ok(n) = v.parse()
    {
        cfg.min_free_bytes = Some(n);
    }
    if let Some(v) = env_str("ZEN_LOOP_SKIP_EXTENSIONS") {
        let list = v
            .split(',')
            .map(|s| s.trim().trim_start_matches('.').to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();
        if !list.is_empty() {
            cfg.skip_extensions = Some(list);
        }
    }
    if let Some(v) = env_str("ZEN_LOOP_HOST_STAGE_TIMEOUT_SECS")
        && let Ok(n) = v.parse::<u64>()
        && n > 0
    {
        cfg.host_stage_timeout_secs = Some(n);
    }
    if let Some(v) = env_u32("ZEN_LOOP_MAX_STEPS")
        && v > 0
    {
        cfg.max_steps = Some(v);
    }
    if let Some(v) = env_u32("ZEN_LOOP_MAX_TOKENS")
        && v > 0
    {
        cfg.max_tokens = Some(v);
    }
    if let Some(v) = env_str("ZEN_LOOP_MAX_INGEST_BYTES")
        && let Ok(n) = v.parse::<u64>()
        && n > 0
    {
        cfg.max_ingest_bytes = Some(n);
    }
    if let Some(v) = env_str("ZEN_LOOP_COMMUNITY_RESOLUTION")
        && let Ok(f) = v.parse::<f64>()
        && f > 0.0
    {
        cfg.community_resolution = Some(f);
    }
    if let Some(v) = env_u32("ZEN_LOOP_COMMUNITY_MIN_SIZE")
        && v > 0
    {
        cfg.community_min_size = Some(v);
    }
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
    if let Some(v) = env_bool("ZEN_TUI_SCHEDULER") {
        cron.tui_scheduler = Some(v);
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
    if let Some(v) = env_str("ZEN_EMBEDDINGS_CACHE_DIR") {
        emb.cache_dir = Some(v);
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
                outbox_drain_interval_secs: None,
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
    /// Resolve the IANA timezone cron schedules are evaluated in (E11).
    ///
    /// Scope logic:
    /// - Functionality: interprets wall-clock fields in worker cron
    ///   expressions ("9am", "2-4h") against this zone instead of Utc
    /// - User impact: `CronConfig::default()` ships `Asia/Shanghai`, so the
    ///   historical Utc-only behavior is restored to what the defaults
    ///   always claimed; `ZEN_CRON_TIMEZONE` overrides per standard 5-layer config
    /// - Default: Utc when unset or unparsable — a typo must never silently
    ///   shift every schedule; the scheduler logs a warn on fallback
    /// - Interaction: only affects schedule matching; worker `ctx.now` stays
    ///   an absolute Utc instant
    pub fn timezone_or_default(&self) -> chrono_tz::Tz {
        match self.timezone.as_deref().map(str::parse) {
            Some(Ok(tz)) => tz,
            other => {
                if other.is_some() {
                    tracing::warn!(
                        timezone = ?self.timezone,
                        "cron: unparsable timezone, falling back to Utc"
                    );
                }
                chrono_tz::UTC
            }
        }
    }

    /// Generate a cron expression for the daily-log worker.
    pub fn daily_log_schedule(&self) -> Option<String> {
        self.subconscious_interval_minutes
            .map(|mins| format!("0 */{mins} * * * *"))
    }

    /// In-app (TUI-hosted) scheduler gate.
    ///
    /// Scope logic:
    /// - Functionality: lets the TUI spawn the learning-core worker
    ///   subset for the app's lifetime, no daemon required
    /// - User impact: false disables all background learning outside an
    ///   explicit `zen serve start`
    /// - Default: true (background learning while the app is in use)
    /// - Interaction: independent of the daemon's own scheduler gate
    ///   (`ZEN_SERVE_NO_SCHEDULER`); coexistence arbitration is the
    ///   TUI's job (health/status `scheduler` probe)
    pub fn tui_scheduler_or_default(&self) -> bool {
        self.tui_scheduler.unwrap_or(true)
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
///
/// This is the fallback used when no config-driven value is available.
/// Fires once at 2:00 AM: a `2-4` hour range would run the consolidation
/// cycle three times per night (2:00/3:00/4:00), tripling LLM calls and
/// duplicate memory writes.
pub fn default_night_dream_schedule() -> &'static str {
    "0 0 2 * * *"
}

pub fn default_wisdom_synthesis_schedule() -> &'static str {
    "0 0 2 * * 7"
}

// ---------------------------------------------------------------------------
/// Persist model selection to global config file (`~/.zen/config.toml`).
/// Path Spec v2 (T18): always writes to global root — workspace config.toml
/// is ignored. Existing lines for these keys are replaced; new keys are appended.
pub fn save_model_selection(provider: &str, model: &str) -> Result<(), ZenError> {
    let paths = ZenPaths::detect().map_err(ZenError::Path)?;
    let config_dir = paths.global_root().clone();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wasm_sandbox_config_absent_section_defaults_deny_all() {
        let config: ZenConfig = toml::from_str("").unwrap();
        assert!(!config.sandbox.wasm.allow_filesystem_read);
        assert!(!config.sandbox.wasm.allow_filesystem_write);
        assert!(!config.sandbox.wasm.allow_network);
        assert!(!config.sandbox.wasm.allow_system);
    }

    #[test]
    fn cron_timezone_or_default_parses_and_falls_back() {
        let mut cfg = CronConfig::default();
        assert_eq!(cfg.timezone_or_default(), chrono_tz::Asia::Shanghai);
        cfg.timezone = Some("Not/AZone".into());
        assert_eq!(cfg.timezone_or_default(), chrono_tz::UTC);
        cfg.timezone = None;
        assert_eq!(cfg.timezone_or_default(), chrono_tz::UTC);
    }

    #[test]
    fn delegate_config_defaults_clamps_and_merges() {
        let cfg = DelegateConfig::default();
        assert!(cfg.enabled_or_default());
        assert_eq!(cfg.timeout_or_default(), 300);

        let clamped = DelegateConfig {
            enabled: Some(false),
            timeout_secs: Some(5),
            max_depth: Some(9),
            max_concurrent: Some(99),
        };
        assert!(!clamped.enabled_or_default());
        assert_eq!(clamped.timeout_or_default(), 30);
        assert_eq!(clamped.max_depth_or_default(), 3);
        assert_eq!(clamped.max_concurrent_or_default(), 8);

        let deeper = DelegateConfig {
            max_depth: Some(3),
            ..DelegateConfig::default()
        };
        assert_eq!(deeper.max_depth_or_default(), 3);

        let ultra = OrchestratorConfig {
            surface: Some(ORCHESTRATOR_SURFACE_DELEGATION_ONLY.to_string()),
        };
        assert!(ultra.delegation_only());
        assert!(!OrchestratorConfig::default().delegation_only());
        let typo = OrchestratorConfig {
            surface: Some("delegation_only!".to_string()),
        };
        assert!(!typo.delegation_only(), "invalid surface degrades to full");
        let merged_orch = merge_agentic(
            AgenticConfig::default(),
            AgenticConfig {
                orchestrator: ultra,
                ..AgenticConfig::default()
            },
        );
        assert!(merged_orch.orchestrator.delegation_only());

        let merged = merge_delegate(DelegateConfig::default(), clamped.clone());
        assert!(!merged.enabled_or_default());
        assert_eq!(merged.timeout_or_default(), 30);
        let base_wins = merge_delegate(clamped, DelegateConfig::default());
        assert!(!base_wins.enabled_or_default());
    }

    #[test]
    fn wasm_sandbox_config_parses_present_section() {
        let config: ZenConfig = toml::from_str(
            r#"
[sandbox.wasm]
allow_network = true
allow_system = true
"#,
        )
        .unwrap();
        assert!(config.sandbox.wasm.allow_network);
        assert!(config.sandbox.wasm.allow_system);
        assert!(!config.sandbox.wasm.allow_filesystem_read);
        assert!(!config.sandbox.wasm.allow_filesystem_write);
    }

    #[test]
    fn wasm_sandbox_config_partial_section_defaults_unset_keys() {
        let config: ZenConfig =
            toml::from_str("[sandbox.wasm]\nallow_filesystem_read = true\n").unwrap();
        assert!(config.sandbox.wasm.allow_filesystem_read);
        assert!(!config.sandbox.wasm.allow_filesystem_write);
        assert!(!config.sandbox.wasm.allow_network);
        assert!(!config.sandbox.wasm.allow_system);
    }

    #[test]
    fn merge_sandbox_accumulates_grants_across_layers() {
        let base: ZenConfig = toml::from_str("[sandbox.wasm]\nallow_network = true\n").unwrap();
        let ov: ZenConfig = toml::from_str("[sandbox.wasm]\nallow_system = true\n").unwrap();
        let merged = merge_configs(base, ov).unwrap();
        assert!(merged.sandbox.wasm.allow_network);
        assert!(merged.sandbox.wasm.allow_system);
    }

    #[test]
    fn agents_tools_absent_section_defaults_empty() {
        let config: ZenConfig = toml::from_str("").unwrap();
        assert!(config.agents_tools.is_empty());
    }

    #[test]
    fn agents_tools_parses_overlay_alongside_task_entries() {
        let config: ZenConfig = toml::from_str(
            r#"
[agents]
tools = ["plugin:*", "fs.*"]

[agents.synthesis]
provider = "ollama"

[agents.Sisyphus]
provider = "anthropic"
"#,
        )
        .unwrap();
        assert_eq!(config.agents_tools, vec!["plugin:*", "fs.*"]);
        assert_eq!(
            config
                .agents
                .get("synthesis")
                .and_then(|a| a.provider.as_deref()),
            Some("ollama")
        );
        assert_eq!(
            config
                .agents
                .get("Sisyphus")
                .and_then(|a| a.provider.as_deref()),
            Some("anthropic")
        );
    }

    #[test]
    fn merge_agents_tools_accumulates_across_layers() {
        let base: ZenConfig = toml::from_str("[agents]\ntools = [\"fs.*\"]\n").unwrap();
        let ov: ZenConfig = toml::from_str("[agents]\ntools = [\"plugin:*\", \"fs.*\"]\n").unwrap();
        let merged = merge_configs(base, ov).unwrap();
        assert_eq!(merged.agents_tools, vec!["fs.*", "plugin:*"]);
    }

    #[test]
    fn tool_loop_absent_section_defaults_to_max_rounds_8() {
        let config: ZenConfig = toml::from_str("").unwrap();
        assert_eq!(config.agentic.tool_loop.max_rounds, None);
        assert_eq!(config.agentic.tool_loop.max_rounds_or_default(), 8);
    }

    #[test]
    fn tool_loop_parses_present_section() {
        let config: ZenConfig = toml::from_str("[agentic.tool_loop]\nmax_rounds = 12\n").unwrap();
        assert_eq!(config.agentic.tool_loop.max_rounds, Some(12));
        assert_eq!(config.agentic.tool_loop.max_rounds_or_default(), 12);
    }

    #[test]
    fn tool_loop_max_rounds_clamped_to_1_16() {
        for (raw, expected) in [(0, 1), (1, 1), (16, 16), (99, 16), (200, 16)] {
            let toml_str = format!("[agentic.tool_loop]\nmax_rounds = {raw}\n");
            let config: ZenConfig = toml::from_str(&toml_str).unwrap();
            assert_eq!(
                config.agentic.tool_loop.max_rounds_or_default(),
                expected,
                "raw: {raw}"
            );
        }
    }

    #[test]
    fn merge_tool_loop_override_layer_wins_absent_keeps_base() {
        let parse = |s: &str| -> ZenConfig { toml::from_str(s).unwrap() };
        let merged = merge_configs(
            parse("[agentic.tool_loop]\nmax_rounds = 4\n"),
            parse("[agentic.tool_loop]\nmax_rounds = 12\n"),
        )
        .unwrap();
        assert_eq!(merged.agentic.tool_loop.max_rounds_or_default(), 12);

        let merged_absent =
            merge_configs(parse("[agentic.tool_loop]\nmax_rounds = 4\n"), parse("")).unwrap();
        assert_eq!(merged_absent.agentic.tool_loop.max_rounds_or_default(), 4);
    }

    #[test]
    fn tool_loop_env_override_respected_and_clamped() {
        // SAFETY: test-only env mutation; ZEN_TOOL_MAX_ROUNDS is read by no
        // sibling test in this binary and is removed at the end of the test.
        unsafe { std::env::set_var("ZEN_TOOL_MAX_ROUNDS", "3") };
        let cfg = apply_env_overrides(ZenConfig::default());
        assert_eq!(cfg.agentic.tool_loop.max_rounds_or_default(), 3);

        unsafe { std::env::set_var("ZEN_TOOL_MAX_ROUNDS", "99") };
        let cfg = apply_env_overrides(ZenConfig::default());
        assert_eq!(cfg.agentic.tool_loop.max_rounds_or_default(), 16);

        unsafe { std::env::remove_var("ZEN_TOOL_MAX_ROUNDS") };
    }

    #[test]
    fn intent_shadow_embedding_defaults_off_and_env_overrides() {
        // Default: shadow observation off — per-turn cost unchanged until opt-in.
        assert!(
            !ZenConfig::default()
                .agentic
                .intent
                .shadow_embedding_or_default()
        );

        // TOML layer: [agentic.intent] shadow_embedding = true.
        let config: ZenConfig =
            toml::from_str("[agentic.intent]\nshadow_embedding = true\n").unwrap();
        assert!(config.agentic.intent.shadow_embedding_or_default());

        // 5th layer: ZEN_INTENT_SHADOW_EMBEDDING env override.
        // SAFETY: test-only env mutation; read by no sibling test in this
        // binary and removed at the end of the test.
        unsafe { std::env::set_var("ZEN_INTENT_SHADOW_EMBEDDING", "1") };
        let cfg = apply_env_overrides(ZenConfig::default());
        assert!(cfg.agentic.intent.shadow_embedding_or_default());

        unsafe { std::env::set_var("ZEN_INTENT_SHADOW_EMBEDDING", "false") };
        let cfg = apply_env_overrides(ZenConfig::default());
        assert!(!cfg.agentic.intent.shadow_embedding_or_default());

        unsafe { std::env::remove_var("ZEN_INTENT_SHADOW_EMBEDDING") };
    }

    #[test]
    fn loop_max_ingest_bytes_clamped_to_1mib_1gib() {
        for (raw, expected) in [
            (0, 1024 * 1024),
            (1024, 1024 * 1024),
            (1024 * 1024, 1024 * 1024),
            (64 * 1024 * 1024, 64 * 1024 * 1024),
            (1024 * 1024 * 1024, 1024 * 1024 * 1024),
            (8u64 * 1024 * 1024 * 1024, 1024 * 1024 * 1024),
        ] {
            let toml_str = format!("[agentic.loop]\nmax_ingest_bytes = {raw}\n");
            let config: ZenConfig = toml::from_str(&toml_str).unwrap();
            assert_eq!(
                config.agentic.loop_cfg.max_ingest_bytes_or_default(),
                expected,
                "raw: {raw}"
            );
        }
    }

    #[test]
    fn loop_max_ingest_bytes_env_override_respected_and_clamped() {
        // SAFETY: test-only env mutation; ZEN_LOOP_MAX_INGEST_BYTES is read by
        // no sibling test in this binary and is removed at the end of the test.
        unsafe { std::env::set_var("ZEN_LOOP_MAX_INGEST_BYTES", "2097152") };
        let cfg = apply_env_overrides(ZenConfig::default());
        assert_eq!(
            cfg.agentic.loop_cfg.max_ingest_bytes_or_default(),
            2 * 1024 * 1024
        );

        unsafe { std::env::set_var("ZEN_LOOP_MAX_INGEST_BYTES", "999999999999") };
        let cfg = apply_env_overrides(ZenConfig::default());
        assert_eq!(
            cfg.agentic.loop_cfg.max_ingest_bytes_or_default(),
            1024 * 1024 * 1024
        );

        unsafe { std::env::remove_var("ZEN_LOOP_MAX_INGEST_BYTES") };
    }

    #[test]
    fn loop_community_resolution_clamped_to_01_50() {
        for (raw, expected) in [
            (0.0, 0.1),
            (0.05, 0.1),
            (0.1, 0.1),
            (1.0, 1.0),
            (5.0, 5.0),
            (10.0, 5.0),
        ] {
            let toml_str = format!("[agentic.loop]\ncommunity_resolution = {raw}\n");
            let config: ZenConfig = toml::from_str(&toml_str).unwrap();
            assert_eq!(
                config.agentic.loop_cfg.community_resolution_or_default(),
                expected,
                "raw: {raw}"
            );
        }
    }

    #[test]
    fn loop_community_min_size_clamped_to_2_50() {
        for (raw, expected) in [(0, 2), (1, 2), (2, 2), (3, 3), (50, 50), (100, 50)] {
            let toml_str = format!("[agentic.loop]\ncommunity_min_size = {raw}\n");
            let config: ZenConfig = toml::from_str(&toml_str).unwrap();
            assert_eq!(
                config.agentic.loop_cfg.community_min_size_or_default(),
                expected,
                "raw: {raw}"
            );
        }
    }

    #[test]
    fn loop_community_env_overrides_respected_and_clamped() {
        // SAFETY: test-only env mutation; both vars are read by no sibling
        // test in this binary and are removed at the end of the test.
        unsafe { std::env::set_var("ZEN_LOOP_COMMUNITY_RESOLUTION", "2.5") };
        let cfg = apply_env_overrides(ZenConfig::default());
        assert_eq!(cfg.agentic.loop_cfg.community_resolution_or_default(), 2.5);

        unsafe { std::env::set_var("ZEN_LOOP_COMMUNITY_RESOLUTION", "99") };
        let cfg = apply_env_overrides(ZenConfig::default());
        assert_eq!(cfg.agentic.loop_cfg.community_resolution_or_default(), 5.0);

        unsafe { std::env::set_var("ZEN_LOOP_COMMUNITY_MIN_SIZE", "7") };
        let cfg = apply_env_overrides(ZenConfig::default());
        assert_eq!(cfg.agentic.loop_cfg.community_min_size_or_default(), 7);

        unsafe { std::env::set_var("ZEN_LOOP_COMMUNITY_MIN_SIZE", "999") };
        let cfg = apply_env_overrides(ZenConfig::default());
        assert_eq!(cfg.agentic.loop_cfg.community_min_size_or_default(), 50);

        unsafe { std::env::remove_var("ZEN_LOOP_COMMUNITY_RESOLUTION") };
        unsafe { std::env::remove_var("ZEN_LOOP_COMMUNITY_MIN_SIZE") };
    }

    #[test]
    fn review_config_defaults_merge_and_env_override() {
        let parse = |s: &str| -> ZenConfig { toml::from_str(s).unwrap() };
        let absent = parse("");
        assert_eq!(absent.agentic.review.max_momus_retries_or_default(), 2);
        assert_eq!(absent.agentic.review.max_hermes_revisions_or_default(), 1);
        assert!(absent.agentic.review.llm_review_high_blast_or_default());

        let merged = merge_configs(
            parse("[agentic.review]\nmax_momus_retries = 4\n"),
            parse("[agentic.review]\nllm_review_high_blast = false\n"),
        )
        .unwrap();
        assert_eq!(merged.agentic.review.max_momus_retries_or_default(), 4);
        assert_eq!(merged.agentic.review.max_hermes_revisions_or_default(), 1);
        assert!(!merged.agentic.review.llm_review_high_blast_or_default());

        // SAFETY: test-only env mutation; ZEN_REVIEW_* are read by no
        // sibling test in this binary and are removed at the end.
        unsafe { std::env::set_var("ZEN_REVIEW_MAX_MOMUS_RETRIES", "99") };
        unsafe { std::env::set_var("ZEN_REVIEW_LLM_HIGH_BLAST", "false") };
        let cfg = apply_env_overrides(ZenConfig::default());
        assert_eq!(cfg.agentic.review.max_momus_retries_or_default(), 5);
        assert!(!cfg.agentic.review.llm_review_high_blast_or_default());
        unsafe { std::env::remove_var("ZEN_REVIEW_MAX_MOMUS_RETRIES") };
        unsafe { std::env::remove_var("ZEN_REVIEW_LLM_HIGH_BLAST") };
    }

    #[test]
    fn embedded_config_ships_tool_loop_default_8() {
        let config = load_embedded_config().unwrap();
        assert_eq!(config.agentic.tool_loop.max_rounds, Some(8));
        assert_eq!(config.agentic.tool_loop.max_rounds_or_default(), 8);
    }

    #[test]
    fn skills_auto_route_absent_section_defaults_enabled() {
        let config: ZenConfig = toml::from_str("").unwrap();
        assert_eq!(config.skills.auto_route.enabled, None);
        assert!(config.skills.auto_route.enabled_or_default());
    }

    #[test]
    fn skills_auto_route_parses_present_section() {
        let config: ZenConfig = toml::from_str("[skills.auto_route]\nenabled = false\n").unwrap();
        assert_eq!(config.skills.auto_route.enabled, Some(false));
        assert!(!config.skills.auto_route.enabled_or_default());
    }

    #[test]
    fn merge_skills_auto_route_override_layer_wins_absent_keeps_base() {
        let parse = |s: &str| -> ZenConfig { toml::from_str(s).unwrap() };
        let merged = merge_configs(
            parse("[skills.auto_route]\nenabled = true\n"),
            parse("[skills.auto_route]\nenabled = false\n"),
        )
        .unwrap();
        assert!(!merged.skills.auto_route.enabled_or_default());

        let merged_absent =
            merge_configs(parse("[skills.auto_route]\nenabled = false\n"), parse("")).unwrap();
        assert!(!merged_absent.skills.auto_route.enabled_or_default());
    }

    #[test]
    fn skills_auto_route_env_override_respected() {
        // SAFETY: test-only env mutation; ZEN_SKILLS_AUTO_ROUTE is read by no
        // sibling test in this binary and is removed at the end of the test.
        unsafe { std::env::set_var("ZEN_SKILLS_AUTO_ROUTE", "0") };
        let cfg = apply_env_overrides(ZenConfig::default());
        assert!(!cfg.skills.auto_route.enabled_or_default());

        unsafe { std::env::set_var("ZEN_SKILLS_AUTO_ROUTE", "true") };
        let cfg = apply_env_overrides(ZenConfig::default());
        assert!(cfg.skills.auto_route.enabled_or_default());

        unsafe { std::env::remove_var("ZEN_SKILLS_AUTO_ROUTE") };
    }

    #[test]
    fn resolved_base_path_expands_tilde() {
        let plugin = PluginConfig {
            base_path: Some("~/.zen/plugins".into()),
            ..PluginConfig::default()
        };
        let path = plugin.resolved_base_path().unwrap();
        assert_ne!(path, PathBuf::from("~/.zen/plugins"));
        assert!(path.ends_with(".zen/plugins"), "got: {}", path.display());
    }

    #[test]
    fn resolved_base_path_keeps_absolute_path() {
        let plugin = PluginConfig {
            base_path: Some("/opt/zen/plugins".into()),
            ..PluginConfig::default()
        };
        assert_eq!(
            plugin.resolved_base_path(),
            Some(PathBuf::from("/opt/zen/plugins"))
        );
    }
}

#[cfg(test)]
mod tui_config_tests {
    use super::{KnowledgeSearchMode, ZenConfig};

    #[test]
    fn knowledge_search_defaults_to_fast() {
        let cfg: ZenConfig = toml::from_str("").expect("empty config");
        assert_eq!(cfg.tui.knowledge_search, KnowledgeSearchMode::Fast);
    }

    #[test]
    fn knowledge_search_parses_all_modes() {
        for (raw, expected) in [
            ("fast", KnowledgeSearchMode::Fast),
            ("full", KnowledgeSearchMode::Full),
            ("off", KnowledgeSearchMode::Off),
        ] {
            let toml_str = format!("[tui]\nknowledge_search = \"{raw}\"");
            let cfg: ZenConfig = toml::from_str(&toml_str).expect("parse mode");
            assert_eq!(cfg.tui.knowledge_search, expected, "mode: {raw}");
        }
    }
}

#[cfg(test)]
mod host_source_tests {
    use super::*;

    fn source(raw: &str) -> HostSourceConfig {
        toml::from_str(raw).expect("parse host source")
    }

    #[test]
    fn resolve_defaults_are_private_local_doc() {
        let hs = source("host_path = \"~/Documents/Work\"");
        let ctx = hs.resolve(Some("ws")).unwrap();
        assert_eq!(ctx.worker_type, None);
        assert_eq!(ctx.raw_policy, HostRawPolicy::Copy);
        assert_eq!(ctx.sensitivity, crate::types::Sensitivity::Private);
        assert!(!ctx.allow_cloud);
        assert!(ctx.preserves_raw());
        assert!(ctx.host_path.to_string_lossy().ends_with("Documents/Work"));
        assert_eq!(ctx.workspace_id.as_deref(), Some("ws"));
    }

    #[test]
    fn resolve_code_source_defaults_index_only() {
        let hs = source("host_path = \"~/CodeRepo/ownspace\"\nworker_type = \"code\"");
        let ctx = hs.resolve(None).unwrap();
        assert_eq!(ctx.raw_policy, HostRawPolicy::IndexOnly);
        assert!(!ctx.preserves_raw());
    }

    #[test]
    fn resolve_rejects_invalid_fields() {
        assert!(source("host_path = \"\"").resolve(None).is_err());
        assert!(
            source("host_path = \"/tmp\"\nworker_type = \"bogus\"")
                .resolve(None)
                .is_err()
        );
        assert!(
            source("host_path = \"/tmp\"\nraw_policy = \"mirror\"")
                .resolve(None)
                .is_err()
        );
        assert!(
            source("host_path = \"/tmp\"\nsensitivity = \"Secret\"")
                .resolve(None)
                .is_err()
        );
        assert!(
            source("host_path = \"/tmp\"\npara_target = \"stuff\"")
                .resolve(None)
                .is_err()
        );
    }

    #[test]
    fn resolve_internal_sensitivity_and_allow_cloud() {
        let hs =
            source("host_path = \"/tmp/docs\"\nsensitivity = \"Internal\"\nallow_cloud = true");
        let ctx = hs.resolve(None).unwrap();
        // "Internal" maps onto the taxonomy's local-only Private tier.
        assert_eq!(ctx.sensitivity, crate::types::Sensitivity::Private);
        assert!(ctx.allow_cloud);
    }

    #[test]
    fn host_dir_hash_is_stable_eight_hex() {
        let a = host_dir_hash(&PathBuf::from("/home/u/Documents/Work"));
        let b = host_dir_hash(&PathBuf::from("/home/u/Documents/Work"));
        assert_eq!(a, b);
        assert_eq!(a.len(), 8);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, host_dir_hash(&PathBuf::from("/home/u/Documents/Other")));
    }

    #[test]
    fn expand_home_path_forms() {
        let home = std::env::var("HOME").unwrap_or_default();
        assert_eq!(
            expand_home_path("~/Docs"),
            PathBuf::from(format!("{home}/Docs"))
        );
        assert_eq!(expand_home_path("~"), PathBuf::from(home.clone()));
        assert_eq!(
            expand_home_path("$HOME/Work"),
            PathBuf::from(format!("{home}/Work"))
        );
        assert_eq!(expand_home_path("/abs/path"), PathBuf::from("/abs/path"));
    }

    #[test]
    fn audit_excerpt_off_by_default() {
        let cfg = AuditConfig::default();
        assert_eq!(cfg.decision_excerpt_chars_or_default(), 0);
        assert!(cfg.excerpt("anything at all").is_none());
    }

    #[test]
    fn audit_excerpt_clamp_rejects_above_500() {
        let cfg = AuditConfig {
            decision_excerpt_chars: Some(10_000),
        };
        assert_eq!(cfg.decision_excerpt_chars_or_default(), AUDIT_EXCERPT_MAX);
        let excerpt = cfg.excerpt("x".repeat(10_000).as_str()).expect("excerpt");
        assert_eq!(excerpt.chars().count(), AUDIT_EXCERPT_MAX);
    }

    #[test]
    fn audit_excerpt_truncates_on_char_boundary() {
        // 600 CJK chars = 1800 UTF-8 bytes; a byte-wise cut would split a
        // codepoint. The excerpt must be exactly the clamp, all valid chars.
        let cfg = AuditConfig {
            decision_excerpt_chars: Some(AUDIT_EXCERPT_MAX),
        };
        let input = "你".repeat(600);
        let excerpt = cfg.excerpt(&input).expect("excerpt");
        assert_eq!(excerpt.chars().count(), AUDIT_EXCERPT_MAX);
        assert!(excerpt.chars().all(|c| c == '你'));
    }

    #[test]
    fn audit_excerpt_flattens_newlines() {
        let cfg = AuditConfig {
            decision_excerpt_chars: Some(100),
        };
        let excerpt = cfg
            .excerpt("line one\nline two\r\nline three")
            .expect("excerpt");
        assert!(!excerpt.contains('\n'));
        assert!(!excerpt.contains('\r'));
        assert_eq!(excerpt, "line one line two  line three");
    }

    #[test]
    fn audit_env_override_respected_and_clamped() {
        // SAFETY: test-only env mutation; ZEN_AUDIT_DECISION_EXCERPT_CHARS is
        // read by no sibling test in this binary and is removed at the end.
        unsafe { std::env::set_var("ZEN_AUDIT_DECISION_EXCERPT_CHARS", "200") };
        let cfg = apply_env_overrides(ZenConfig::default());
        assert_eq!(cfg.agentic.audit.decision_excerpt_chars_or_default(), 200);

        unsafe { std::env::set_var("ZEN_AUDIT_DECISION_EXCERPT_CHARS", "9999") };
        let cfg = apply_env_overrides(ZenConfig::default());
        assert_eq!(
            cfg.agentic.audit.decision_excerpt_chars_or_default(),
            AUDIT_EXCERPT_MAX,
            "an env value above the cap is clamped, not trusted"
        );

        unsafe { std::env::set_var("ZEN_AUDIT_DECISION_EXCERPT_CHARS", "not-a-number") };
        let cfg = apply_env_overrides(ZenConfig::default());
        assert_eq!(
            cfg.agentic.audit.decision_excerpt_chars_or_default(),
            0,
            "an unparsable env value leaves the excerpt off"
        );

        unsafe { std::env::remove_var("ZEN_AUDIT_DECISION_EXCERPT_CHARS") };
    }
}
