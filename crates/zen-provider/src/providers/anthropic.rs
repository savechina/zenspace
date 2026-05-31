use rig::agent::AgentBuilder;
use rig::client::CompletionClient;
use rig::completion::Prompt;
use rig::providers::anthropic;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::router::LlmError;

#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    pub api_key: String,
    pub model: String,
    pub base_url: String,
}

impl AnthropicProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            api_key,
            model,
            base_url: "https://api.anthropic.com".into(),
        }
    }

    pub fn new_with_base_url(api_key: String, model: String, base_url: String) -> Self {
        Self {
            api_key,
            model,
            base_url,
        }
    }

    pub async fn complete_async(&self, prompt: &str) -> Result<String, LlmError> {
        let mut builder = anthropic::Client::builder().api_key(&self.api_key);
        if self.base_url != "https://api.anthropic.com" {
            builder = builder.base_url(&self.base_url);
        }
        let client = builder.build().map_err(|e| LlmError::Call {
            reason: format!("Failed to create Anthropic client: {}", e),
        })?;

        let model = client.completion_model(&self.model);
        let agent = AgentBuilder::new(model).build();

        let response = agent.prompt(prompt).await.map_err(|e| LlmError::Call {
            reason: format!("Anthropic completion failed: {}", e),
        })?;

        info!(
            model = self.model,
            response_len = response.len(),
            "AnthropicProvider complete"
        );
        Ok(response)
    }

    pub fn complete(&self, prompt: &str) -> Result<String, LlmError> {
        let api_key = self.api_key.clone();
        let model = self.model.clone();
        let base_url = self.base_url.clone();
        let prompt = prompt.to_string();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let provider = AnthropicProvider {
                api_key,
                model,
                base_url,
            };
            rt.block_on(provider.complete_async(&prompt))
        })
        .join()
        .map_err(|e| LlmError::Call {
            reason: format!("Anthropic thread panic: {:?}", e),
        })?
    }

    pub async fn complete_streaming(
        &self,
        prompt: &str,
        token_tx: mpsc::UnboundedSender<String>,
    ) -> Result<(), LlmError> {
        let response = self.complete_async(prompt).await?;

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
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        match client
            .get(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .send()
        {
            Ok(resp) => {
                // Anthropic returns 405 for GET on messages — that means the endpoint is alive
                resp.status() == 405 || resp.status().is_success()
            }
            Err(e) => {
                warn!(error = %e, "Anthropic health check failed");
                false
            }
        }
    }
}
