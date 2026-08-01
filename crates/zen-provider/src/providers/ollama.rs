use rig::client::{CompletionClient, Nothing};
use rig::providers::ollama;
use rig_agent::AgentBuilder;
use rig_agent::completion::Prompt;
use tokio::sync::mpsc;
use tracing::{info, warn};

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
        let response = self.complete_async(prompt, options).await?;

        let words: Vec<&str> = response.split_whitespace().collect();
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
