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

    pub async fn complete_async(
        &self,
        prompt: &str,
        options: &zen_core::config::ModelOptions,
    ) -> Result<String, LlmError> {
        let client = ollama::Client::builder()
            .api_key(Nothing)
            .base_url(&self.base_url)
            .build()
            .map_err(|e| LlmError::Call {
                reason: format!("Failed to create Ollama client: {}", e),
            })?;

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
            rt.block_on(provider.complete_async(&prompt, &options))
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
        let client = ollama::Client::builder()
            .api_key(Nothing)
            .base_url(&self.base_url)
            .build()
            .map_err(|e| LlmError::Call {
                reason: format!("Failed to create Ollama client: {}", e),
            })?;

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
        let client = reqwest::blocking::Client::new();
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
