use rig_agent::AgentBuilder;
use rig_agent::completion::Prompt;
use rig_core::client::{CompletionClient, Nothing};
use rig_core::completion::CompletionModel;
use rig_core::providers::ollama;
use rig_core::streaming::StreamedAssistantContent;
use tokio::sync::mpsc;
use tracing::{info, warn};

use futures_util::StreamExt;

use crate::router::{LlmError, UsedCompletion, blocking_usage_call, used_completion};

#[derive(Debug, Clone)]
pub struct OllamaProvider {
    pub base_url: String,
    pub model: String,
}

impl OllamaProvider {
    pub fn new(base_url: String, model: String) -> Self {
        Self { base_url, model }
    }

    /// Builds the rig Ollama client. Loopback endpoints bypass
    /// HTTP(S)_PROXY env vars: routing 127.0.0.1 through a system proxy
    /// (Privoxy et al) yields 500s and breaks every local completion.
    fn rig_client(&self) -> Result<ollama::Client, LlmError> {
        let http = if is_loopback_url(&self.base_url) {
            Some(
                reqwest::Client::builder()
                    .no_proxy()
                    .build()
                    .map_err(|e| LlmError::Call {
                        reason: format!("Failed to build Ollama http client: {}", e),
                    })?,
            )
        } else {
            None
        };
        match http {
            Some(http) => ollama::Client::builder()
                .api_key(Nothing)
                .base_url(&self.base_url)
                .http_client(http)
                .build(),
            None => ollama::Client::builder()
                .api_key(Nothing)
                .base_url(&self.base_url)
                .build(),
        }
        .map_err(|e| LlmError::Call {
            reason: format!("Failed to create Ollama client: {}", e),
        })
    }

    /// D2b: per-call model override — `options.model` wins over the
    /// provider's configured default (ollama model names are per-request,
    /// so the override needs no client rebuild).
    fn effective_model<'a>(&'a self, options: &'a zen_core::config::ModelOptions) -> &'a str {
        options.model.as_deref().unwrap_or(&self.model)
    }

    pub async fn complete_async(
        &self,
        prompt: &str,
        options: &zen_core::config::ModelOptions,
    ) -> Result<String, LlmError> {
        let client = self.rig_client()?;

        let model = client.completion_model(self.effective_model(options));
        let mut agent_builder = AgentBuilder::new(model);
        if let Some(t) = options.temperature {
            agent_builder = agent_builder.temperature(t);
        }
        if let Some(m) = options.max_tokens {
            agent_builder = agent_builder.max_tokens(m);
        }
        let agent = agent_builder.build();

        let response = agent.prompt(prompt).await.map_err(|e| LlmError::Call {
            reason: format!("Ollama completion failed: {}", e),
        })?;

        info!(
            model = self.effective_model(options),
            response_len = response.len(),
            "OllamaProvider complete"
        );
        Ok(response)
    }

    pub fn complete(
        &self,
        prompt: &str,
        options: &zen_core::config::ModelOptions,
    ) -> Result<String, LlmError> {
        let base_url = self.base_url.clone();
        let model = self.model.clone();
        let prompt = prompt.to_string();
        let options = options.clone();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let provider = OllamaProvider { base_url, model };
            rt.block_on(async {
                tokio::time::timeout(
                    std::time::Duration::from_secs(120),
                    provider.complete_async(&prompt, &options),
                )
                .await
                .map_err(|_| LlmError::Call {
                    reason: "Ollama completion timed out after 120s".into(),
                })?
            })
        })
        .join()
        .map_err(|e| LlmError::Call {
            reason: format!("Ollama thread panic: {:?}", e),
        })?
    }

    /// Usage-bearing sibling of [`Self::complete_async`]: same client
    /// construction (loopback no-proxy included) and options, but the rig
    /// `CompletionResponse` is kept whole so the provider's real token
    /// usage rides out alongside the text.
    pub async fn complete_async_with_usage(
        &self,
        prompt: &str,
        options: &zen_core::config::ModelOptions,
    ) -> Result<UsedCompletion, LlmError> {
        let client = self.rig_client()?;

        let model = client.completion_model(self.effective_model(options));
        let mut request = model.completion_request(prompt);
        if let Some(t) = options.temperature {
            request = request.temperature(t);
        }
        if let Some(m) = options.max_tokens {
            request = request.max_tokens(m);
        }
        let response = request.send().await.map_err(|e| LlmError::Call {
            reason: format!("Ollama completion failed: {}", e),
        })?;

        Ok(used_completion("Ollama", response))
    }

    /// Dedicated-thread wrapper around [`Self::complete_async_with_usage`]
    /// — same 120s timeout and nesting-panic guard as [`Self::complete`].
    pub fn complete_with_usage(
        &self,
        prompt: &str,
        options: &zen_core::config::ModelOptions,
    ) -> Result<UsedCompletion, LlmError> {
        let base_url = self.base_url.clone();
        let model = self.model.clone();
        let prompt = prompt.to_string();
        let options = options.clone();

        blocking_usage_call("Ollama", move || async move {
            let provider = OllamaProvider { base_url, model };
            tokio::time::timeout(
                std::time::Duration::from_secs(120),
                provider.complete_async_with_usage(&prompt, &options),
            )
            .await
            .map_err(|_| LlmError::Call {
                reason: "Ollama completion timed out after 120s".into(),
            })?
        })
    }

    pub async fn complete_streaming(
        &self,
        prompt: &str,
        token_tx: mpsc::UnboundedSender<String>,
        options: &zen_core::config::ModelOptions,
    ) -> Result<(), LlmError> {
        let client = self.rig_client()?;

        let model = client.completion_model(self.effective_model(options));

        let mut request = model.completion_request(prompt.to_string());
        if let Some(t) = options.temperature {
            request = request.temperature(t);
        }
        if let Some(m) = options.max_tokens {
            request = request.max_tokens(m);
        }
        let request = request.build();

        let mut stream = model.stream(request).await.map_err(|e| LlmError::Call {
            reason: format!("Ollama stream failed: {}", e),
        })?;

        let mut full_response = String::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(StreamedAssistantContent::Text(text)) => {
                    let chunk = text.text.clone();
                    full_response.push_str(&chunk);
                    if token_tx.send(chunk).is_err() {
                        break;
                    }
                }
                Ok(StreamedAssistantContent::Final(_)) => break,
                Ok(_) => {
                    // Tool-call / reasoning deltas are not forwarded as text.
                }
                Err(e) => {
                    warn!(error = %e, "Ollama streaming error");
                    break;
                }
            }
        }

        info!(
            model = self.effective_model(options),
            response_len = full_response.len(),
            "OllamaProvider stream complete"
        );
        Ok(())
    }

    /// Probe Ollama's `/api/tags` endpoint (5s timeout, loopback bypasses
    /// proxies).
    ///
    /// The `reqwest::blocking` probe runs on a dedicated thread — the same
    /// pattern as [`Self::complete`]. reqwest's blocking client must never
    /// run inside an async runtime: it panics in debug builds ("Cannot drop
    /// a runtime in a context where blocking is not allowed") and parks a
    /// runtime worker in release. `enforce_sensitivity →
    /// is_local_llm_available` reaches this probe from async callers
    /// (delegate sub-turns, orchestrator turns, scheduler workers, CLI), so
    /// the guard lives in the leaf itself. A fresh thread has no ambient
    /// runtime; a thread panic maps to `false` (fail-safe: Confidential
    /// routing then refuses cloud and surfaces the typed local-unavailable
    /// error instead of panicking the turn).
    pub fn health_check(&self) -> bool {
        let base_url = self.base_url.clone();
        std::thread::spawn(move || {
            let mut b =
                reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(5));
            if is_loopback_url(&base_url) {
                b = b.no_proxy();
            }
            let client = b.build().unwrap_or_default();
            let url = format!("{}/api/tags", base_url.trim_end_matches('/'));
            match client.get(&url).send() {
                Ok(resp) => resp.status().is_success(),
                Err(e) => {
                    warn!(error = %e, "Ollama health check failed");
                    false
                }
            }
        })
        .join()
        .unwrap_or_else(|e| {
            warn!(error = ?e, "Ollama health check thread panicked; local LLM treated as unavailable");
            false
        })
    }
}

pub(crate) fn is_loopback_url(base_url: &str) -> bool {
    let host = url::Url::parse(base_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_default();
    // `host_str` serializes IPv6 hosts with brackets — strip before matching.
    let host = host.trim_matches(|c| c == '[' || c == ']');
    // `0.0.0.0` (unspecified) also never leaves the machine.
    host == "localhost" || host == "::1" || host.starts_with("127.") || host == "0.0.0.0"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_model_prefers_options_override() {
        // D2b: the per-call override wins; absent override keeps the
        // provider's configured default (byte-identical legacy path).
        let provider = OllamaProvider::new(
            "http://127.0.0.1:11434".to_string(),
            "qwen3.6:35b-mlx".to_string(),
        );
        let mut options = zen_core::config::ModelOptions::default();
        assert_eq!(provider.effective_model(&options), "qwen3.6:35b-mlx");
        options.model = Some("llama3.2:2GB".to_string());
        assert_eq!(provider.effective_model(&options), "llama3.2:2GB");
    }
}
