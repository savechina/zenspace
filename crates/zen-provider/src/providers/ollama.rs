use rig::client::{CompletionClient, Nothing};
use rig::completion::CompletionModel;
use rig::providers::ollama;
use rig::streaming::StreamedAssistantContent;
use rig_agent::AgentBuilder;
use rig_agent::completion::Prompt;
use tokio::sync::mpsc;
use tracing::{info, warn};

use futures_util::StreamExt;

use crate::router::LlmError;

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

    pub async fn complete_async(
        &self,
        prompt: &str,
        options: &zen_core::config::ModelOptions,
    ) -> Result<String, LlmError> {
        let client = self.rig_client()?;

        let model = client.completion_model(&self.model);
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
            model = self.model,
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

    pub async fn complete_streaming(
        &self,
        prompt: &str,
        token_tx: mpsc::UnboundedSender<String>,
        options: &zen_core::config::ModelOptions,
    ) -> Result<(), LlmError> {
        let client = self.rig_client()?;

        let model = client.completion_model(&self.model);

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
            model = self.model,
            response_len = full_response.len(),
            "OllamaProvider stream complete"
        );
        Ok(())
    }

    pub fn health_check(&self) -> bool {
        let mut b = reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(5));
        if is_loopback_url(&self.base_url) {
            b = b.no_proxy();
        }
        let client = b.build().unwrap_or_default();
        let url = format!("{}/api/tags", self.base_url.trim_end_matches('/'));
        match client.get(&url).send() {
            Ok(resp) => resp.status().is_success(),
            Err(e) => {
                warn!(error = %e, "Ollama health check failed");
                false
            }
        }
    }
}

fn is_loopback_url(base_url: &str) -> bool {
    let host = url::Url::parse(base_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_default();
    // `host_str` serializes IPv6 hosts with brackets — strip before matching.
    let host = host.trim_matches(|c| c == '[' || c == ']');
    // `0.0.0.0` (unspecified) also never leaves the machine.
    host == "localhost" || host == "::1" || host.starts_with("127.") || host == "0.0.0.0"
}
