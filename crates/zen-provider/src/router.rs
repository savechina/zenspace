use crate::providers::{
    AnthropicProvider, CohereProvider, GeminiProvider, MistralProvider, OllamaProvider,
    OpenAIProvider,
};
use rig_compose::reliability::RetryClass;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{info, warn};
pub use zen_core::config::{LlmConfig, ProviderConfig, ZenConfig};
use zen_core::errors::ZenError;
use zen_core::secrets::SecretRef;
use zen_core::types::Sensitivity;

// ---------------------------------------------------------------------------
// API Key Resolution (FR-061c)
// ---------------------------------------------------------------------------

/// Resolve API key from ProviderConfig using Keychain-first fallback chain.
///
/// Resolution order:
/// 1. Keychain (default naming: `zen-{provider}-api-key`) → env fallback via SecretResolver
/// 2. `api_key` (SecretRef) — explicit Keychain or env from config
/// 3. `api_key_env` (legacy) — direct env var name
/// 4. Default env var: `{PROVIDER}_API_KEY`
pub(crate) fn resolve_api_key(p: &ProviderConfig, provider_name: &str) -> Option<String> {
    // 1. Try Keychain-first with default naming (Keychain → env fallback)
    let kc_name = format!("zen-{provider_name}-api-key");
    let default_env = SecretRef::legacy_env_var(provider_name);
    if let Ok(key) = zen_auth::SecretResolver::new(&kc_name, &default_env).resolve() {
        info!(
            provider = provider_name,
            "resolved API key via Keychain resolver"
        );
        return Some(key);
    }

    // 2. Try explicitly configured SecretRef
    if let Some(ref secret_ref) = p.api_key {
        match zen_auth::resolve_secret_ref(secret_ref) {
            Ok(key) => {
                info!(provider = provider_name, source = %secret_ref, "resolved API key via SecretRef");
                return Some(key);
            }
            Err(e) => {
                tracing::debug!(provider = provider_name, secret_ref = %secret_ref, error = %e, "SecretRef not found, falling back");
            }
        }
    }

    // 3. Legacy api_key_env or default {PROVIDER}_API_KEY
    let env_name = p.api_key_env.as_deref().unwrap_or(&default_env);
    std::env::var(env_name).ok()
}

// ---------------------------------------------------------------------------
// LlmError
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum LlmError {
    #[error("provider unavailable: {provider} — {reason}")]
    ProviderUnavailable { provider: String, reason: String },

    #[error("routing failed: {reason}")]
    Routing { reason: String },

    #[error("call failed: {reason}")]
    Call { reason: String },
}

impl LlmError {
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            LlmError::ProviderUnavailable { .. } | LlmError::Call { .. }
        )
    }
}

pub trait LlmRetryClassifier: Send + Sync {
    fn classify(&self, error: &LlmError) -> RetryClass;
}

pub struct DefaultLlmRetryClassifier;

impl LlmRetryClassifier for DefaultLlmRetryClassifier {
    fn classify(&self, error: &LlmError) -> RetryClass {
        match error {
            LlmError::ProviderUnavailable { .. } | LlmError::Call { .. } => RetryClass::Transient,
            LlmError::Routing { .. } => RetryClass::Permanent,
        }
    }
}

// ---------------------------------------------------------------------------
// Provider enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    OpenAI,
    Anthropic,
    DeepSeek,
    Aliyun,
    Mistral,
    Groq,
    Moonshot,
    XAI,
    Perplexity,
    Gemini,
    QQBot,
    Ollama,
    #[serde(rename = "mock")]
    Mock,
    Unknown(String),
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Provider::OpenAI => write!(f, "openai"),
            Provider::Anthropic => write!(f, "anthropic"),
            Provider::DeepSeek => write!(f, "deepseek"),
            Provider::Aliyun => write!(f, "aliyun"),
            Provider::Mistral => write!(f, "mistral"),
            Provider::Groq => write!(f, "groq"),
            Provider::Moonshot => write!(f, "moonshot"),
            Provider::XAI => write!(f, "xai"),
            Provider::Perplexity => write!(f, "perplexity"),
            Provider::Gemini => write!(f, "gemini"),
            Provider::QQBot => write!(f, "qqbot"),
            Provider::Ollama => write!(f, "ollama"),
            Provider::Mock => write!(f, "mock"),
            Provider::Unknown(name) => write!(f, "{name}"),
        }
    }
}

// ---------------------------------------------------------------------------
// MockProvider
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct MockProvider {
    pub response: String,
}

impl Default for MockProvider {
    fn default() -> Self {
        Self {
            response: "mock completion response".into(),
        }
    }
}

impl MockProvider {
    pub fn complete(&self, task: &str, prompt: &str) -> Result<String, LlmError> {
        let reply = format!(
            "[mock] task={task} prompt_len={} reply={}",
            prompt.len(),
            self.response
        );
        info!(
            task,
            prompt_len = prompt.len(),
            "MockProvider complete (stub)"
        );
        Ok(reply)
    }

    pub async fn complete_streaming(
        &self,
        task: &str,
        prompt: &str,
        token_tx: mpsc::UnboundedSender<String>,
    ) -> Result<(), LlmError> {
        let reply = format!(
            "[mock] task={task} prompt_len={} reply={}",
            prompt.len(),
            self.response
        );
        for chunk in reply.split_inclusive([' ', '\n']) {
            if token_tx.send(chunk.to_string()).is_err() {
                break;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// TaskRequirements
// ---------------------------------------------------------------------------

pub struct TaskRequirements {
    pub max_tokens: Option<u32>,
    pub sensitivity: Sensitivity,
    pub preferred_model: Option<String>,
    pub budget_limit: Option<f64>,
}

// ---------------------------------------------------------------------------
// LlmRouter trait
// ---------------------------------------------------------------------------

/// Abstract interface for selecting and calling an LLM provider.
pub trait LlmRouter: std::fmt::Debug + Send + Sync {
    /// Select the best provider for the given task requirements.
    fn route(&self, requirements: &TaskRequirements) -> Result<Provider, LlmError>;

    /// Send a completion request to the given provider.
    ///
    /// This is a **stub** — no actual HTTP calls are made.
    fn call(&self, provider: Provider, prompt: &str) -> Result<String, LlmError>;

    /// Send a streaming completion request. Returns [`crate::stream::StreamResponse`].
    fn call_stream(
        &self,
        provider: Provider,
        prompt: &str,
    ) -> Result<crate::stream::StreamResponse, LlmError>;

    /// Return a list of `provider_name -> model` pairs currently configured.
    fn list_providers(&self) -> Vec<(String, String)>;

    /// Check whether a local LLM provider is available and reachable.
    ///
    /// Returns `true` if at least one local provider (e.g. Ollama) is configured
    /// and responds to a health check. Returns `false` by default for routers
    /// that do not support local execution.
    fn is_local_llm_available(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// LlmRouterExt — convenience wrapper (matches spec task/request/sensitivity API)
// ---------------------------------------------------------------------------

/// Extension trait so callers that already have `(task, request, sensitivity)`
/// can still use the router without manually constructing [`TaskRequirements`].
pub trait LlmRouterExt: LlmRouter {
    /// Convenience wrapper matching the original `complete(task, request, sensitivity)` API.
    fn complete(
        &self,
        task: &str,
        request: &str,
        sensitivity: Sensitivity,
    ) -> Result<String, ZenError> {
        let requirements = TaskRequirements {
            max_tokens: None,
            sensitivity,
            preferred_model: None,
            budget_limit: None,
        };

        let provider = self.route(&requirements).map_err(|e| {
            warn!(task, error = %e, "LlmRouter route failed");
            ZenError::Agentic(
                zen_core::errors::AgenticError::LlmRoutingFailed {
                    provider: "unknown".into(),
                    reason: e.to_string(),
                },
                zen_core::errors::ErrorCategory::SystemError,
            )
        })?;

        self.call(provider.clone(), request).map_err(|e| {
            warn!(
                task,
                provider = %provider,
                error = %e,
                "LlmRouter call failed"
            );
            match &e {
                LlmError::ProviderUnavailable { provider, reason } => ZenError::Agentic(
                    zen_core::errors::AgenticError::LlmProviderUnavailable {
                        provider: provider.clone(),
                        reason: reason.clone(),
                    },
                    zen_core::errors::ErrorCategory::SystemError,
                ),
                LlmError::Routing { reason } => ZenError::Agentic(
                    zen_core::errors::AgenticError::LlmRoutingFailed {
                        provider: provider.to_string(),
                        reason: reason.clone(),
                    },
                    zen_core::errors::ErrorCategory::SystemError,
                ),
                LlmError::Call { reason } => ZenError::Agentic(
                    zen_core::errors::AgenticError::LlmProviderUnavailable {
                        provider: provider.to_string(),
                        reason: format!("call failed: {reason}"),
                    },
                    zen_core::errors::ErrorCategory::SystemError,
                ),
            }
        })
    }
}

impl<T: LlmRouter> LlmRouterExt for T {}

// ---------------------------------------------------------------------------
// Routing context — maps task names to config sections
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
enum TaskContext {
    NotionExtraction,
    ContradictionDetection,
    Synthesis,
    Dispatch,
    Default_,
}

impl TaskContext {
    #[allow(dead_code)]
    fn from_label(label: &str) -> Self {
        match label {
            "notion_extraction" | "extraction" => TaskContext::NotionExtraction,
            "contradiction_detection" | "contradiction" => TaskContext::ContradictionDetection,
            "synthesis" => TaskContext::Synthesis,
            "dispatch" => TaskContext::Dispatch,
            _ => TaskContext::Default_,
        }
    }

    fn config_key(&self) -> &'static str {
        match self {
            TaskContext::NotionExtraction => "notion_extraction",
            TaskContext::ContradictionDetection => "contradiction_detection",
            TaskContext::Synthesis => "synthesis",
            TaskContext::Dispatch => "dispatch",
            TaskContext::Default_ => "default",
        }
    }

    #[allow(dead_code)]
    fn task_config<'a>(&self, cfg: &'a LlmConfig) -> Option<&'a zen_core::config::LlmTaskConfig> {
        match self {
            TaskContext::NotionExtraction => cfg.notion_extraction.as_ref(),
            TaskContext::ContradictionDetection => cfg.contradiction_detection.as_ref(),
            TaskContext::Synthesis => cfg.synthesis.as_ref(),
            TaskContext::Dispatch => cfg.dispatch.as_ref(),
            TaskContext::Default_ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// ProviderInstance — dynamic provider wrapper for registry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum ProviderInstance {
    Anthropic(AnthropicProvider),
    AnthropicCompatible(AnthropicProvider),
    Cohere(CohereProvider),
    Gemini(GeminiProvider),
    Mistral(MistralProvider),
    Ollama(OllamaProvider),
    OpenAICompatible(OpenAIProvider),
    Mock(MockProvider),
}

impl ProviderInstance {
    pub fn complete(
        &self,
        prompt: &str,
        options: &zen_core::config::ModelOptions,
    ) -> Result<String, LlmError> {
        match self {
            ProviderInstance::Anthropic(p) => p.complete(prompt, options),
            ProviderInstance::AnthropicCompatible(p) => p.complete(prompt, options),
            ProviderInstance::Cohere(p) => p.complete(prompt, options),
            ProviderInstance::Gemini(p) => p.complete(prompt, options),
            ProviderInstance::Mistral(p) => p.complete(prompt, options),
            ProviderInstance::Ollama(p) => p.complete(prompt, options),
            ProviderInstance::OpenAICompatible(p) => p.complete(prompt, options),
            ProviderInstance::Mock(p) => p.complete("call", prompt),
        }
    }

    pub async fn complete_streaming(
        &self,
        prompt: &str,
        token_tx: mpsc::UnboundedSender<String>,
        options: &zen_core::config::ModelOptions,
    ) -> Result<(), LlmError> {
        match self {
            ProviderInstance::Anthropic(p) => p.complete_streaming(prompt, token_tx, options).await,
            ProviderInstance::AnthropicCompatible(p) => {
                p.complete_streaming(prompt, token_tx, options).await
            }
            ProviderInstance::Cohere(p) => p.complete_streaming(prompt, token_tx, options).await,
            ProviderInstance::Gemini(p) => p.complete_streaming(prompt, token_tx, options).await,
            ProviderInstance::Mistral(p) => p.complete_streaming(prompt, token_tx, options).await,
            ProviderInstance::Ollama(p) => p.complete_streaming(prompt, token_tx, options).await,
            ProviderInstance::OpenAICompatible(p) => {
                p.complete_streaming(prompt, token_tx, options).await
            }
            ProviderInstance::Mock(p) => p.complete_streaming("call", prompt, token_tx).await,
        }
    }

    pub fn model_name(&self) -> &str {
        match self {
            ProviderInstance::Anthropic(p) => &p.model,
            ProviderInstance::AnthropicCompatible(p) => &p.model,
            ProviderInstance::Cohere(p) => &p.model,
            ProviderInstance::Gemini(p) => &p.model,
            ProviderInstance::Mistral(p) => &p.model,
            ProviderInstance::Ollama(p) => &p.model,
            ProviderInstance::OpenAICompatible(p) => &p.model,
            ProviderInstance::Mock(_) => "mock",
        }
    }

    pub fn base_url(&self) -> Option<&str> {
        match self {
            ProviderInstance::Anthropic(_) => None,
            ProviderInstance::AnthropicCompatible(p) => Some(&p.base_url),
            ProviderInstance::Cohere(_) => None,
            ProviderInstance::Gemini(_) => None,
            ProviderInstance::Mistral(_) => None,
            ProviderInstance::Ollama(p) => Some(&p.base_url),
            ProviderInstance::OpenAICompatible(p) => Some(&p.base_url),
            ProviderInstance::Mock(_) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// DefaultRouter — loads routing preferences from AgenticConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DefaultRouter {
    config: zen_core::config::ZenConfig,
    mock: MockProvider,
    providers: std::collections::HashMap<String, ProviderInstance>,
    current_variant: Option<String>,
}

impl DefaultRouter {
    /// Create from a full [`zen_core::config::AgenticConfig`].
    pub fn from_agentic(agentic: &zen_core::config::ZenConfig) -> Self {
        let mut providers = std::collections::HashMap::new();

        for (name, cfg) in &agentic.providers {
            let instance = Self::create_provider_instance(name, cfg);
            if let Some(p) = instance {
                providers.insert(name.clone(), p);
            }
        }

        Self {
            config: agentic.clone(),
            mock: MockProvider::default(),
            providers,
            current_variant: None,
        }
    }

    fn create_provider_instance(
        name: &str,
        cfg: &zen_core::config::ProviderConfig,
    ) -> Option<ProviderInstance> {
        // Resolve API key at construction time. SecretResolver::resolve() checks
        // env vars FIRST (fast, non-blocking) before falling back to macOS Keychain,
        // so this does NOT block for users with standard env-var-based config.
        // Prevents silent provider creation failure (the previous `None` key caused
        // every cloud provider match arm to short-circuit via `?`, meaning the
        // provider was never inserted into the map at all — producing misleading
        // "Provider 'X' not configured" errors at route() time).
        let api_key = resolve_api_key(cfg, name);
        Self::create_provider_instance_with_key(name, cfg, api_key)
    }

    /// Creates a provider instance with a pre-resolved (possibly None) API key.
    /// This is the inner implementation — the public-facing [`create_provider_instance`]
    /// passes `None` to defer resolution, but callers that already have a key
    /// can bypass resolution.
    fn create_provider_instance_with_key(
        _name: &str,
        cfg: &zen_core::config::ProviderConfig,
        api_key: Option<String>,
    ) -> Option<ProviderInstance> {
        let provider_type = cfg.provider_type.as_deref().unwrap_or("openai-compatible");

        match provider_type {
            "ollama" => {
                let base_url = cfg
                    .base_url
                    .clone()
                    .unwrap_or_else(|| zen_core::constants::OLLAMA_BASE_URL.into());
                let model = cfg
                    .default_model
                    .clone()
                    .unwrap_or_else(|| zen_core::constants::OLLAMA_DEFAULT_MODEL.into());
                Some(ProviderInstance::Ollama(OllamaProvider::new(
                    base_url, model,
                )))
            }
            "mock" => Some(ProviderInstance::Mock(MockProvider::default())),
            "anthropic" => {
                let api_key = api_key.clone()?;
                let model = cfg
                    .default_model
                    .clone()
                    .unwrap_or_else(|| zen_core::constants::ANTHROPIC_DEFAULT_MODEL.into());
                Some(ProviderInstance::Anthropic(AnthropicProvider::new(
                    api_key, model,
                )))
            }
            "anthropic-compatible" => {
                let api_key = api_key.clone()?;
                let base_url = cfg.base_url.clone()?;
                let model = cfg
                    .default_model
                    .clone()
                    .unwrap_or_else(|| zen_core::constants::ANTHROPIC_DEFAULT_MODEL.into());
                Some(ProviderInstance::AnthropicCompatible(
                    AnthropicProvider::new_with_base_url(api_key, model, base_url),
                ))
            }
            "cohere" => {
                let api_key = api_key.clone()?;
                let model = cfg
                    .default_model
                    .clone()
                    .unwrap_or_else(|| zen_core::constants::COHERE_DEFAULT_MODEL.into());
                Some(ProviderInstance::Cohere(CohereProvider::new(
                    api_key, model,
                )))
            }
            "gemini" => {
                let api_key = api_key.clone()?;
                let model = cfg
                    .default_model
                    .clone()
                    .unwrap_or_else(|| zen_core::constants::GEMINI_DEFAULT_MODEL.into());
                Some(ProviderInstance::Gemini(GeminiProvider::new(
                    api_key, model,
                )))
            }
            "mistral" => {
                let api_key = api_key.clone()?;
                let model = cfg
                    .default_model
                    .clone()
                    .unwrap_or_else(|| zen_core::constants::MISTRAL_DEFAULT_MODEL.into());
                Some(ProviderInstance::Mistral(MistralProvider::new(
                    api_key, model,
                )))
            }
            "openai" => {
                let api_key = api_key.clone()?;
                let base_url = cfg
                    .base_url
                    .clone()
                    .unwrap_or_else(|| zen_core::constants::OPENAI_BASE_URL.into());
                let model = cfg
                    .default_model
                    .clone()
                    .unwrap_or_else(|| zen_core::constants::OPENAI_DEFAULT_MODEL.into());
                Some(ProviderInstance::OpenAICompatible(
                    OpenAIProvider::new_with_base_url(api_key, model, base_url),
                ))
            }
            "openai-compatible" => {
                let api_key = api_key.clone()?;
                let base_url = cfg.base_url.clone()?;
                let model = cfg
                    .default_model
                    .clone()
                    .unwrap_or_else(|| zen_core::constants::OPENAI_DEFAULT_MODEL.into());
                Some(ProviderInstance::OpenAICompatible(
                    OpenAIProvider::new_with_base_url(api_key, model, base_url),
                ))
            }
            unknown => {
                warn!("Unknown provider type '{unknown}', treating as openai-compatible");
                let api_key = api_key.clone()?;
                let base_url = cfg.base_url.clone()?;
                let model = cfg
                    .default_model
                    .clone()
                    .unwrap_or_else(|| zen_core::constants::OPENAI_DEFAULT_MODEL.into());
                Some(ProviderInstance::OpenAICompatible(
                    OpenAIProvider::new_with_base_url(api_key, model, base_url),
                ))
            }
        }
    }

    /// Create from a legacy [`LlmConfig`] for backward compatibility.
    pub fn new(config: LlmConfig) -> Self {
        let mut providers = std::collections::HashMap::new();

        if let Some(tc) = config.notion_extraction.as_ref()
            && tc.provider.as_deref() == Some("ollama")
        {
            let base_url = tc
                .base_url
                .clone()
                .unwrap_or_else(|| "http://127.0.0.1:11434".into());
            let model = tc.model.clone().unwrap_or_else(|| "llama3.2".into());
            providers.insert(
                "ollama".into(),
                ProviderInstance::Ollama(OllamaProvider::new(base_url, model)),
            );
        }

        if let Some(tc) = config.dispatch.as_ref()
            && let Some(ref provider_name) = tc.provider
        {
            if provider_name == "ollama" {
                // Ollama handled above in notion_extraction
            } else if provider_name != "mock" {
                // Cloud provider: resolve API key and base_url from config
                let default_env = format!("{}_API_KEY", provider_name.to_uppercase());
                let env_var: &str = match provider_name.as_str() {
                    "aliyun" => "DASHSCOPE_API_KEY",
                    _ => tc.api_key_env.as_deref().unwrap_or(&default_env),
                };

                if let Ok(api_key) = std::env::var(env_var)
                    && let Some(ref base_url) = tc.base_url
                {
                    let model = tc
                        .model
                        .clone()
                        .unwrap_or_else(|| zen_core::constants::OPENAI_DEFAULT_MODEL.into());
                    providers.insert(
                        provider_name.clone(),
                        ProviderInstance::OpenAICompatible(OpenAIProvider::new_with_base_url(
                            api_key,
                            model,
                            base_url.clone(),
                        )),
                    );
                }
            }
        }

        Self {
            config: zen_core::config::ZenConfig {
                default_provider: config.default_provider.clone(),
                default_model: None,
                providers: std::collections::HashMap::new(),
                agents: std::collections::HashMap::new(),
                features: zen_core::config::FeatureConfig::default(),
                channels: zen_core::config::ChannelsConfig::default(),
                cron: zen_core::config::CronConfig::default(),
                plugin: zen_core::config::PluginConfig::default(),
                feeds: Vec::new(),
                tui: zen_core::config::TuiConfig::default(),
                history: zen_core::config::HistoryConfig::default(),
                embeddings: zen_core::config::EmbeddingsConfig::default(),
                web_fetch: zen_core::config::WebFetchConfig::default(),
                web_search: zen_core::config::WebSearchConfig::default(),
                mcp_servers: Vec::new(),
            },
            mock: MockProvider::default(),
            providers,
            current_variant: None,
        }
    }

    /// Create a [`DefaultRouter`] configured for a specific provider and model at runtime.
    ///
    /// Accepts an explicit [`ZenConfig`] (from `load_config()`) to preserve the full
    /// 5-layer config merge chain (env → workspace → global → embedded). Overrides
    /// the provider's `default_model` and sets it as the `default_provider`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = zen_core::config::load_config()?;
    /// let router = DefaultRouter::from_config_override(config, "openai", "gpt-4o");
    /// ```
    pub fn from_config_override(config: &ZenConfig, provider_name: &str, model_name: &str) -> Self {
        let mut providers = config.providers.clone();
        if let Some(cfg) = providers.get_mut(provider_name) {
            cfg.default_model = Some(model_name.into());
        } else {
            providers.insert(
                provider_name.into(),
                ProviderConfig {
                    // Default to "openai-compatible" for custom providers.
                    // This matches the fallback in create_provider_instance_with_key,
                    // ensuring unknown provider names get a valid protocol type
                    // instead of silently failing the match block.
                    provider_type: Some("openai-compatible".into()),
                    default_model: Some(model_name.into()),
                    ..Default::default()
                },
            );
        }

        let mut overridden = config.clone();
        overridden.default_provider = Some(provider_name.into());
        overridden.providers = providers;

        Self::from_agentic(&overridden)
    }

    /// Return the configured default provider name (e.g. "ollama", "openai").
    pub fn default_provider_name(&self) -> &str {
        self.config.default_provider.as_deref().unwrap_or("ollama")
    }

    /// Resolve effective [`ModelOptions`] for a provider by looking up its
    /// model catalog entry and optionally applying a variant.
    /// Returns `None` if the provider has no model catalog or the selected
    /// model has no options.
    fn resolve_effective_options(
        &self,
        provider_name: &str,
    ) -> Option<zen_core::config::ModelOptions> {
        let provider = self.config.providers.get(provider_name)?;
        let model_name = provider.default_model.as_deref()?;
        let entry = provider.models.get(model_name)?;
        let mut base = entry.options.clone().unwrap_or_default();

        // Merge variant params if one is selected
        if let Some(ref variant_name) = self.current_variant
            && let Some(variant) = entry.variants.get(variant_name)
        {
            if let Some(t) = variant.temperature {
                base.temperature = Some(t);
            }
            if let Some(m) = variant.max_tokens {
                base.max_tokens = Some(m);
            }
        }

        Some(base)
    }

    // -- internal helpers --

    fn resolve_provider(
        cfg: &zen_core::config::ZenConfig,
        task_ctx: TaskContext,
    ) -> Option<Provider> {
        let task_name = task_ctx.config_key();

        // 1. Agent task config
        if let Some(agent) = cfg.agents.get(task_name)
            && let Some(ref name) = agent.provider
        {
            let p = parse_provider_name(name);
            info!(
                task = ?task_ctx,
                provider = %p,
                model = agent.model.as_deref().unwrap_or("default"),
                "DefaultRouter: resolved task-specific provider"
            );
            return Some(p);
        }

        // 2. Fallback to default_provider
        if let Some(ref name) = cfg.default_provider {
            let p = parse_provider_name(name);
            info!(
                provider = %p,
                "DefaultRouter: fell back to default_provider"
            );
            return Some(p);
        }

        warn!("DefaultRouter: no provider configured for task");
        None
    }

    fn is_local(&self, provider: &Provider) -> bool {
        if let Some(p) = self.config.providers.get("ollama")
            && let Some(ref bu) = p.base_url
            && (bu.starts_with("http://localhost") || bu.starts_with("http://127.0.0.1"))
        {
            return true;
        }
        matches!(provider, Provider::Ollama)
    }

    fn enforce_sensitivity(
        &self,
        provider: Provider,
        sensitivity: Sensitivity,
    ) -> Result<Provider, LlmError> {
        match sensitivity {
            Sensitivity::Public => Ok(provider),
            Sensitivity::Private | Sensitivity::Confidential => {
                if self.is_local(&provider) {
                    // Quick path: the routed provider itself is local.
                    // Verify it is actually reachable.
                    if self.is_local_llm_available() {
                        return Ok(provider);
                    }
                }

                // Attempt fallback to a local provider (ollama) if available + reachable.
                if self.is_local_llm_available()
                    && let Some(local) =
                        Self::resolve_provider(&self.config, TaskContext::NotionExtraction)
                    && self.is_local(&local)
                {
                    warn!(
                        requested = %provider,
                        fallback = %local,
                        "DefaultRouter: private/confidential data routed away from cloud provider"
                    );
                    return Ok(local);
                }

                Err(LlmError::ProviderUnavailable {
                    provider: provider.to_string(),
                    reason: format!(
                        "Local LLM unavailable. Start Ollama or configure a local provider. {sensitivity} data cannot be routed to cloud."
                    ),
                })
            }
        }
    }

    fn is_local_llm_available(&self) -> bool {
        if let Some(instance) = self.providers.get("ollama")
            && let ProviderInstance::Ollama(ollama) = instance
            && ollama.health_check()
        {
            return true;
        }
        warn!("is_local_llm_available: no reachable local LLM provider");
        false
    }

    #[allow(dead_code)]
    pub fn health_check(&self) -> bool {
        if let Some(instance) = self.providers.get("ollama")
            && let ProviderInstance::Ollama(ollama) = instance
        {
            return ollama.health_check();
        }
        tracing::info!("DefaultRouter health check: no local LLM configured");
        false
    }

    /// Route based on agent LLM preferences, falling back to standard routing.
    ///
    /// Preference order:
    /// 1. `LocalOnly` → Ollama (if available), else error
    /// 2. `CloudOnly` → configured cloud provider
    /// 3. `Provider(name)` → specific provider
    /// 4. `Any` → fall through to standard `route()`
    pub fn route_with_preferences(
        &self,
        requirements: &TaskRequirements,
        llm_preferences: &[zen_core::config::LlmPreference],
    ) -> Result<Provider, LlmError> {
        use zen_core::config::LlmPreference;

        for pref in llm_preferences {
            match pref {
                LlmPreference::LocalOnly => {
                    if self.is_local_llm_available() {
                        info!("route_with_preferences: agent requires local-only, using Ollama");
                        return Ok(Provider::Ollama);
                    }
                    warn!(
                        "route_with_preferences: agent requires local-only but no local LLM available"
                    );
                }
                LlmPreference::CloudOnly => {
                    if let Some(ref name) = self.config.default_provider {
                        let p = parse_provider_name(name);
                        if !self.is_local(&p) {
                            info!(
                                "route_with_preferences: agent requires cloud-only, using {}",
                                p
                            );
                            return self.enforce_sensitivity(p, requirements.sensitivity);
                        }
                    }
                }
                LlmPreference::Provider(name) => {
                    let p = parse_provider_name(name);
                    info!(
                        "route_with_preferences: agent prefers {}, using {}",
                        name, p
                    );
                    return self.enforce_sensitivity(p, requirements.sensitivity);
                }
                LlmPreference::Any => continue,
            }
        }

        info!("route_with_preferences: no preference matched, falling back to standard route");
        self.route(requirements)
    }

    pub fn build_fallback_chain(&self, agent_name: &str) -> Vec<(Provider, Option<String>)> {
        let mut chain = vec![];

        let agent_config = self.config.agents.get(agent_name);

        if let Some(agent) = agent_config
            && let Some(ref provider_name) = agent.provider
        {
            chain.push((parse_provider_name(provider_name), agent.model.clone()));
        }

        if let Some(agent) = agent_config {
            for step in &agent.fallbacks {
                chain.push((parse_provider_name(&step.provider), step.model.clone()));
            }
        }

        if chain.is_empty()
            && let Some(ref name) = self.config.default_provider
        {
            chain.push((parse_provider_name(name), self.config.default_model.clone()));
        }

        chain.push((Provider::Mock, None));

        chain
    }
}

impl LlmRouter for DefaultRouter {
    fn route(&self, requirements: &TaskRequirements) -> Result<Provider, LlmError> {
        // For route(), we need a task label; since TaskRequirements doesn't carry one,
        // we use Default_ and fall through to default_provider.
        let task_ctx = TaskContext::Default_;
        let provider = Self::resolve_provider(&self.config, task_ctx).ok_or_else(|| {
            LlmError::ProviderUnavailable {
                provider: "none".into(),
                reason: "No provider configured; set [agentic.llm] in config.toml".into(),
            }
        })?;
        self.enforce_sensitivity(provider, requirements.sensitivity)
    }

    fn call(&self, provider: Provider, prompt: &str) -> Result<String, LlmError> {
        let provider_name = match &provider {
            Provider::Unknown(name) => name.clone(),
            _ => provider.to_string(),
        };

        if provider_name == "mock" {
            info!(
                prompt_len = prompt.len(),
                "DefaultRouter: delegating to MockProvider"
            );
            return self.mock.complete("call", prompt);
        }

        let options = self
            .resolve_effective_options(&provider_name)
            .unwrap_or_default();

        if let Some(instance) = self.providers.get(&provider_name) {
            info!(
                provider = provider_name,
                model = instance.model_name(),
                base_url = instance
                    .base_url()
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
                prompt_len = prompt.len(),
                "DefaultRouter: calling provider"
            );
            instance.complete(prompt, &options)
        } else {
            Err(LlmError::ProviderUnavailable {
                provider: provider_name.clone(),
                reason: format!(
                    "Provider '{}' not configured. Add to ~/.zen/config.toml: [providers.{}]",
                    provider_name, provider_name
                ),
            })
        }
    }

    fn call_stream(
        &self,
        provider: Provider,
        prompt: &str,
    ) -> Result<crate::stream::StreamResponse, LlmError> {
        let (stream_resp, token_tx, done_tx) = crate::stream::StreamResponse::new();

        let provider_name = match &provider {
            Provider::Unknown(name) => name.clone(),
            _ => provider.to_string(),
        };

        if provider_name == "mock" {
            let prompt = prompt.to_string();
            info!(
                prompt_len = prompt.len(),
                "DefaultRouter: MockProvider stream"
            );
            let mock = self.mock.clone();
            tokio::spawn(async move {
                let result = mock.complete_streaming("call", &prompt, token_tx).await;
                if let Err(e) = done_tx.send(result.map_err(|e| e.to_string())) {
                    warn!("mock provider done channel closed: {:?}", e);
                }
            });
            return Ok(stream_resp);
        }

        if let Some(instance) = self.providers.get(&provider_name) {
            let prompt = prompt.to_string();
            let options = self
                .resolve_effective_options(&provider_name)
                .unwrap_or_default();
            info!(
                provider = provider_name,
                model = instance.model_name(),
                prompt_len = prompt.len(),
                "DefaultRouter: provider stream"
            );
            let instance = instance.clone();
            tokio::spawn(async move {
                let result = instance
                    .complete_streaming(&prompt, token_tx, &options)
                    .await;
                if let Err(e) = done_tx.send(result.map_err(|e| e.to_string())) {
                    warn!("provider done channel closed: {:?}", e);
                }
            });
        } else {
            if let Err(e) =
                done_tx.send(Err(format!("Provider '{}' not configured", provider_name)))
            {
                warn!("provider error done channel closed: {:?}", e);
            }
        }

        Ok(stream_resp)
    }

    fn list_providers(&self) -> Vec<(String, String)> {
        let mut result = Vec::new();

        for (name, instance) in &self.providers {
            result.push((name.clone(), instance.model_name().to_string()));
        }

        if result.is_empty()
            && let Some(ref name) = self.config.default_provider
        {
            result.push((
                name.clone(),
                self.config
                    .default_model
                    .clone()
                    .unwrap_or_else(|| zen_core::constants::OPENAI_DEFAULT_MODEL.into()),
            ));
        }

        result
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_provider_name(name: &str) -> Provider {
    match name.to_lowercase().as_str() {
        "openai" | "oa" => Provider::OpenAI,
        "anthropic" | "an" => Provider::Anthropic,
        "deepseek" | "ds" | "deep_seek" => Provider::DeepSeek,
        "aliyun" | "qwen" | "alibaba" => Provider::Aliyun,
        "mistral" | "mi" => Provider::Mistral,
        "groq" => Provider::Groq,
        "moonshot" | "ms" => Provider::Moonshot,
        "xai" => Provider::XAI,
        "perplexity" => Provider::Perplexity,
        "gemini" | "ge" => Provider::Gemini,
        "qq" | "qqbot" | "qq_bot" => Provider::QQBot,
        "ollama" | "local" | "ollama-local" => Provider::Ollama,
        "mock" => Provider::Unknown("mock".into()),
        other => Provider::Unknown(other.into()),
    }
}

/// Standalone helper — calls `router.is_local_llm_available()`.
///
/// Use this from session orchestration code that needs to check local
/// LLM availability without holding a concrete `DefaultRouter` reference.
pub fn is_local_llm_available(router: &dyn LlmRouter) -> bool {
    router.is_local_llm_available()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_streaming_preserves_newlines_as_tokens() {
        let mock = MockProvider {
            response: "line one\nline two\nline three".into(),
        };
        let (tx, mut rx) = mpsc::unbounded_channel();
        mock.complete_streaming("task", "prompt", tx).await.unwrap();
        rx.close();
        let mut tokens = Vec::new();
        while let Some(t) = rx.recv().await {
            tokens.push(t);
        }
        let joined: String = tokens.iter().cloned().collect();
        assert!(
            joined.contains('\n'),
            "tokens should contain newlines, got {:?}",
            tokens
        );
        assert!(
            tokens.len() > 1,
            "should produce multiple tokens, got {}",
            tokens.len()
        );
    }

    #[tokio::test]
    async fn mock_streaming_tokens_preserve_word_boundaries() {
        let mock = MockProvider::default();
        let (tx, mut rx) = mpsc::unbounded_channel();
        mock.complete_streaming("task", "prompt", tx).await.unwrap();
        rx.close();
        let mut tokens = Vec::new();
        while let Some(t) = rx.recv().await {
            tokens.push(t);
        }
        assert!(
            tokens.len() > 1,
            "default mock should produce multiple word-level tokens"
        );
    }
}
