use rig::agent::AgentBuilder;
use rig::client::CompletionClient;
use rig::completion::Prompt;
use rig::providers::gemini;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::router::LlmError;

#[derive(Debug, Clone)]
pub struct GeminiProvider {
    pub api_key: String,
    pub model: String,
}

impl GeminiProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self { api_key, model }
    }

    pub async fn complete_async(&self, prompt: &str) -> Result<String, LlmError> {
        let client = gemini::Client::new(&self.api_key).map_err(|e| LlmError::Call {
            reason: format!("Failed to create Gemini client: {}", e),
        })?;

        let model = client.completion_model(&self.model);
        let agent = AgentBuilder::new(model).build();

        let response = agent.prompt(prompt).await.map_err(|e| LlmError::Call {
            reason: format!("Gemini completion failed: {}", e),
        })?;

        info!(
            model = self.model,
            response_len = response.len(),
            "GeminiProvider complete"
        );
        Ok(response)
    }

    pub fn complete(&self, prompt: &str) -> Result<String, LlmError> {
        let api_key = self.api_key.clone();
        let model = self.model.clone();
        let prompt = prompt.to_string();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let provider = GeminiProvider { api_key, model };
            rt.block_on(provider.complete_async(&prompt))
        })
        .join()
        .map_err(|e| LlmError::Call {
            reason: format!("Gemini thread panic: {:?}", e),
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
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models?key={}",
            self.api_key
        );
        match client.get(&url).send() {
            Ok(resp) => resp.status().is_success(),
            Err(e) => {
                warn!(error = %e, "Gemini health check failed");
                false
            }
        }
    }
}
