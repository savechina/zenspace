use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{info, warn};
pub use zen_core::config::LlmConfig;
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

    /// Return a list of `provider_name -> model` pairs currently configured.
    fn list_providers(&self) -> Vec<(String, String)>;
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

#[derive(Debug)]
pub struct DefaultRouter {
    config: LlmConfig,
    mock: MockProvider,
}

impl DefaultRouter {
    pub fn new(config: LlmConfig) -> Self {
        Self {
            config,
            mock: MockProvider::default(),
        }
    }

    /// Create from an already-loaded [`zen_core::config::AgenticConfig`].
    pub fn from_agentic(agentic: &zen_core::config::AgenticConfig) -> Self {
        Self::new(agentic.llm.clone())
    }

    // -- internal helpers --

    fn resolve_provider(cfg: &LlmConfig, task_ctx: TaskContext) -> Option<Provider> {
        // 1. Task-specific config
        if let Some(tc) = task_ctx.task_config(cfg)
            && let Some(ref name) = tc.provider
        {
            let p = parse_provider_name(name);
            info!(
                task = ?task_ctx,
                provider = %p,
                model = tc.model.as_deref().unwrap_or("default"),
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
        let name = provider.to_string();
        if let Some(tc) = self.config.entity_extraction.as_ref() {
            if let Some(ref ep) = tc.provider
                && *ep == name
            {
                if let Some(ref bu) = tc.base_url
                    && bu.starts_with("http://localhost")
                {
                    return true;
                }
            }
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
                    Ok(provider)
                } else {
                    // Attempt fallback to local (ollama)
                    if let Some(local) =
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
                }
            },
        }
    }

    #[allow(dead_code)]
    pub fn health_check(&self) -> bool {
        tracing::info!("DefaultRouter health check stub (assume local LLM available)");
        true
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
        match &provider {
            Provider::Mock => {
                info!(
                    prompt_len = prompt.len(),
                    "DefaultRouter: delegating to MockProvider"
                );
                self.mock.complete("call", prompt)
            },
            Provider::Unknown(name) if name == "mock" => {
                info!(
                    prompt_len = prompt.len(),
                    "DefaultRouter: delegating to MockProvider"
                );
                self.mock.complete("call", prompt)
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

    fn list_providers(&self) -> Vec<(String, String)> {
        let mut result = Vec::new();

        if let Some(ref name) = self.config.default_provider
            && !result.iter().any(|(n, _)| n == name)
        {
            result.push((name.clone(), "default".into()));
        }

        for (_task_name, opt) in [
            ("entity_extraction", &self.config.entity_extraction),
            (
                "contradiction_detection",
                &self.config.contradiction_detection,
            ),
            ("synthesis", &self.config.synthesis),
            ("dispatch", &self.config.dispatch),
        ] {
            if let Some(tc) = opt {
                if let Some(ref p) = tc.provider {
                    let model = tc.model.as_deref().unwrap_or("default").to_string();
                    if !result.iter().any(|(n, _)| n == p) {
                        result.push((p.clone(), model));
                    }
                }
            }
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
