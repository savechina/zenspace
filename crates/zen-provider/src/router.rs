use crate::providers::{OllamaProvider, OpenAIProvider};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{info, warn};
pub use zen_core::config::{LlmConfig, LlmTaskConfig};
use zen_core::errors::ZenError;
use zen_core::types::Sensitivity;

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

// ---------------------------------------------------------------------------
// Provider enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    OpenAI,
    Anthropic,
    DeepSeek,
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
        let words: Vec<&str> = reply.split_whitespace().collect();
        let mut buf = String::new();
        for word in words {
            buf.push_str(word);
            buf.push(' ');
            let chunk = buf.clone();
            buf.clear();
            if token_tx.send(chunk).is_err() {
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
    EntityExtraction,
    ContradictionDetection,
    Synthesis,
    Dispatch,
    Default_,
}

impl TaskContext {
    #[allow(dead_code)]
    fn from_label(label: &str) -> Self {
        match label {
            "entity_extraction" | "extraction" => TaskContext::EntityExtraction,
            "contradiction_detection" | "contradiction" => TaskContext::ContradictionDetection,
            "synthesis" => TaskContext::Synthesis,
            "dispatch" => TaskContext::Dispatch,
            _ => TaskContext::Default_,
        }
    }

    fn config_key(&self) -> &'static str {
        match self {
            TaskContext::EntityExtraction => "entity_extraction",
            TaskContext::ContradictionDetection => "contradiction_detection",
            TaskContext::Synthesis => "synthesis",
            TaskContext::Dispatch => "dispatch",
            TaskContext::Default_ => "default",
        }
    }

    fn task_config<'a>(&self, cfg: &'a LlmConfig) -> Option<&'a zen_core::config::LlmTaskConfig> {
        match self {
            TaskContext::EntityExtraction => cfg.entity_extraction.as_ref(),
            TaskContext::ContradictionDetection => cfg.contradiction_detection.as_ref(),
            TaskContext::Synthesis => cfg.synthesis.as_ref(),
            TaskContext::Dispatch => cfg.dispatch.as_ref(),
            TaskContext::Default_ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// DefaultRouter — loads routing preferences from AgenticConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DefaultRouter {
    config: zen_core::config::AgenticConfig,
    mock: MockProvider,
    ollama: Option<OllamaProvider>,
    openai: Option<OpenAIProvider>,
}

impl DefaultRouter {
    /// Create from a full [`zen_core::config::AgenticConfig`].
    pub fn from_agentic(agentic: &zen_core::config::AgenticConfig) -> Self {
        let ollama = agentic.providers.get("ollama").map(|p| {
            OllamaProvider::new(
                p.base_url.clone().unwrap_or_else(|| "http://127.0.0.1:11434".into()),
                p.default_model.clone().unwrap_or_else(|| "qwen3-coder".into()),
            )
        });

        let openai = agentic.providers.get("openai").and_then(|p| {
            let key_env = p.api_key_env.as_deref().unwrap_or("OPENAI_API_KEY");
            if let Ok(api_key) = std::env::var(key_env) {
                Some(OpenAIProvider::new(
                    api_key,
                    p.default_model.clone().unwrap_or_else(|| "gpt-4o-mini".into()),
                ))
            } else {
                None
            }
        });

        Self {
            config: agentic.clone(),
            mock: MockProvider::default(),
            ollama,
            openai,
        }
    }

    /// Create from a legacy [`LlmConfig`] for backward compatibility.
    pub fn new(config: LlmConfig) -> Self {
        let ollama = config.entity_extraction.as_ref().and_then(|tc| {
            if tc.provider.as_deref() == Some("ollama") {
                Some(OllamaProvider::new(
                    tc.base_url
                        .clone()
                        .unwrap_or_else(|| "http://127.0.0.1:11434".into()),
                    tc.model.clone().unwrap_or_else(|| "llama3.2".into()),
                ))
            } else {
                None
            }
        });

        let openai = config.dispatch.as_ref().and_then(|tc| {
            if tc.provider.as_deref() == Some("openai") {
                if let Ok(api_key) =
                    std::env::var(tc.api_key_env.as_deref().unwrap_or("OPENAI_API_KEY"))
                {
                    Some(OpenAIProvider::new(
                        api_key,
                        tc.model.clone().unwrap_or_else(|| "gpt-4o-mini".into()),
                    ))
                } else {
                    None
                }
            } else {
                None
            }
        });

        Self {
            config: zen_core::config::AgenticConfig {
                default_provider: config.default_provider.clone(),
                default_model: None,
                providers: std::collections::HashMap::new(),
                agents: std::collections::HashMap::new(),
                features: zen_core::config::FeatureConfig::default(),
                qqbot: None,
                cron: zen_core::config::CronConfig::default(),
                plugin: zen_core::config::PluginConfig::default(),
                feeds: Vec::new(),
                learning: zen_core::config::LearningConfig::default(),
                finance: zen_core::config::FinanceConfig::default(),
            },
            mock: MockProvider::default(),
            ollama,
            openai,
        }
    }

    /// Create a [`DefaultRouter`] configured for a specific provider and model at runtime.
    ///
    /// This is a convenience constructor that builds an [`LlmConfig`], populates
    /// the necessary provider fields, and returns a ready-to-use router.
    /// Does **not** read or write any files.
    pub fn new_for_provider(provider_name: &str, model_name: &str) -> Self {
        let config = match provider_name {
            "ollama" => LlmConfig {
                default_provider: Some("ollama".into()),
                entity_extraction: Some(LlmTaskConfig {
                    provider: Some("ollama".into()),
                    model: Some(model_name.into()),
                    base_url: Some("http://127.0.0.1:11434".into()),
                    api_key_env: None,
                }),
                contradiction_detection: Some(LlmTaskConfig {
                    provider: Some("ollama".into()),
                    model: Some(model_name.into()),
                    base_url: Some("http://127.0.0.1:11434".into()),
                    api_key_env: None,
                }),
                synthesis: Some(LlmTaskConfig {
                    provider: Some("ollama".into()),
                    model: Some(model_name.into()),
                    base_url: Some("http://127.0.0.1:11434".into()),
                    api_key_env: None,
                }),
                dispatch: Some(LlmTaskConfig {
                    provider: Some("ollama".into()),
                    model: Some(model_name.into()),
                    base_url: Some("http://127.0.0.1:11434".into()),
                    api_key_env: None,
                }),
            },
            "openai" => LlmConfig {
                default_provider: Some("openai".into()),
                dispatch: Some(LlmTaskConfig {
                    provider: Some("openai".into()),
                    model: Some(model_name.into()),
                    base_url: None,
                    api_key_env: Some("OPENAI_API_KEY".into()),
                }),
                ..LlmConfig::default()
            },
            _ => LlmConfig {
                default_provider: Some("mock".into()),
                ..LlmConfig::default()
            },
        };

        Self::new(config)
    }

    /// Return the configured default provider name (e.g. "ollama", "openai").
    pub fn default_provider_name(&self) -> &str {
        self.config.default_provider.as_deref().unwrap_or("ollama")
    }

    // -- internal helpers --

    fn resolve_provider(cfg: &zen_core::config::AgenticConfig, task_ctx: TaskContext) -> Option<Provider> {
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
                        Self::resolve_provider(&self.config, TaskContext::EntityExtraction)
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
            },
        }
    }

    fn is_local_llm_available(&self) -> bool {
        if let Some(ref ollama) = self.ollama
            && ollama.health_check()
        {
            return true;
        }
        warn!("is_local_llm_available: no reachable local LLM provider");
        false
    }

    #[allow(dead_code)]
    pub fn health_check(&self) -> bool {
        if let Some(ref ollama) = self.ollama {
            ollama.health_check()
        } else {
            tracing::info!("DefaultRouter health check: no local LLM configured");
            false
        }
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
                },
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
                },
                LlmPreference::Provider(name) => {
                    let p = parse_provider_name(name);
                    info!(
                        "route_with_preferences: agent prefers {}, using {}",
                        name, p
                    );
                    return self.enforce_sensitivity(p, requirements.sensitivity);
                },
                LlmPreference::Any => continue,
            }
        }

        info!("route_with_preferences: no preference matched, falling back to standard route");
        self.route(requirements)
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
        match provider.clone() {
            Provider::Mock => {
                info!(
                    prompt_len = prompt.len(),
                    "DefaultRouter: delegating to MockProvider"
                );
                self.mock.complete("call", prompt)
            },
            Provider::Unknown(ref name) if name == "mock" => {
                info!(
                    prompt_len = prompt.len(),
                    "DefaultRouter: delegating to MockProvider"
                );
                self.mock.complete("call", prompt)
            },
            Provider::Ollama => {
                if let Some(ref ollama) = self.ollama {
                    info!(
                        model = ollama.model,
                        prompt_len = prompt.len(),
                        "DefaultRouter: calling Ollama"
                    );
                    ollama.complete(prompt)
                } else {
                    Err(LlmError::ProviderUnavailable {
                        provider: "ollama".into(),
                        reason: "Ollama provider not configured. Set [llm.entity_extraction] in config.toml".into(),
                    })
                }
            },
            Provider::OpenAI => {
                if let Some(ref openai) = self.openai {
                    info!(
                        model = openai.model,
                        prompt_len = prompt.len(),
                        "DefaultRouter: calling OpenAI"
                    );
                    openai.complete(prompt)
                } else {
                    Err(LlmError::ProviderUnavailable {
                        provider: "openai".into(),
                        reason: "OpenAI provider not configured. Set OPENAI_API_KEY env var and [llm.dispatch] in config.toml".into(),
                    })
                }
            },
            _ => {
                info!(
                    provider = %provider,
                    prompt_len = prompt.len(),
                    "DefaultRouter: call stub (no real HTTP)"
                );
                Ok(format!(
                    "[stub] provider={provider} prompt_len={}",
                    prompt.len()
                ))
            },
        }
    }

    fn call_stream(
        &self,
        provider: Provider,
        prompt: &str,
    ) -> Result<crate::stream::StreamResponse, LlmError> {
        let (stream_resp, token_tx, done_tx) = crate::stream::StreamResponse::new();

        match provider {
            Provider::Mock => {
                let prompt = prompt.to_string();
                info!(prompt_len = prompt.len(), "DefaultRouter: MockProvider stream");
                let mock = self.mock.clone();
                tokio::spawn(async move {
                    let result = mock.complete_streaming("call", &prompt, token_tx).await;
                    let _ = done_tx.send(result.map_err(|e| e.to_string()));
                });
            },
            Provider::Unknown(ref name) if name == "mock" => {
                let prompt = prompt.to_string();
                info!(prompt_len = prompt.len(), "DefaultRouter: MockProvider stream");
                let mock = self.mock.clone();
                tokio::spawn(async move {
                    let result = mock.complete_streaming("call", &prompt, token_tx).await;
                    let _ = done_tx.send(result.map_err(|e| e.to_string()));
                });
            },
            Provider::Ollama => {
                if let Some(ref ollama) = self.ollama {
                    let prompt = prompt.to_string();
                    info!(model = ollama.model, prompt_len = prompt.len(), "DefaultRouter: Ollama stream");
                    let ollama = ollama.clone();
                    tokio::spawn(async move {
                        let result = ollama.complete_streaming(&prompt, token_tx).await;
                        let _ = done_tx.send(result.map_err(|e| e.to_string()));
                    });
                } else {
                    let _ = done_tx.send(Err("Ollama provider not configured".into()));
                }
            },
            Provider::OpenAI => {
                if let Some(ref openai) = self.openai {
                    let prompt = prompt.to_string();
                    info!(model = openai.model, prompt_len = prompt.len(), "DefaultRouter: OpenAI stream");
                    let openai = openai.clone();
                    tokio::spawn(async move {
                        let result = openai.complete_streaming(&prompt, token_tx).await;
                        let _ = done_tx.send(result.map_err(|e| e.to_string()));
                    });
                } else {
                    let _ = done_tx.send(Err("OpenAI provider not configured".into()));
                }
            },
            _ => {
                info!(provider = %provider, prompt_len = prompt.len(), "DefaultRouter: stream stub");
                let reply = format!("[stub stream] provider={provider} prompt_len={}", prompt.len());
                let words: Vec<&str> = reply.split_whitespace().collect();
                let mut buf = String::new();
                for word in words {
                    buf.push_str(word);
                    buf.push(' ');
                    let chunk = buf.clone();
                    buf.clear();
                    if token_tx.send(chunk).is_err() {
                        break;
                    }
                }
                let _ = done_tx.send(Ok(()));
            },
        }

        Ok(stream_resp)
    }

    fn list_providers(&self) -> Vec<(String, String)> {
        let mut result = Vec::new();

        // Named provider definitions
        for (name, provider) in &self.config.providers {
            let model = provider.default_model.clone().unwrap_or_else(|| "default".into());
            result.push((name.clone(), model));
        }

        // Agent task routing references
        for agent in self.config.agents.values() {
            if let Some(ref p) = agent.provider {
                let model = agent.model.clone().unwrap_or_else(|| "default".into());
                if !result.iter().any(|(n, _)| n == p) {
                    result.push((p.clone(), model));
                }
            }
        }

        // Default provider fallback
        if let Some(ref name) = self.config.default_provider
            && !result.iter().any(|(n, _)| n == name)
        {
            result.push((name.clone(), self.config.default_model.clone().unwrap_or_else(|| "default".into())));
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
