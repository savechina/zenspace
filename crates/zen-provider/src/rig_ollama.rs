use rig::client::{CompletionClient, Nothing};
use rig::completion::CompletionModel;
use rig::providers::ollama;
use tracing::info;

use crate::router::LlmError;

#[derive(Debug, Clone)]
pub struct RigOllamaProvider {
    pub base_url: String,
    pub model: String,
}

impl RigOllamaProvider {
    pub fn new(base_url: String, model: String) -> Self {
        Self { base_url, model }
    }

    pub fn completion_model(&self) -> Result<impl CompletionModel, LlmError> {
        let client = ollama::Client::builder()
            .api_key(Nothing)
            .base_url(&self.base_url)
            .build()
            .map_err(|e| LlmError::Call {
                reason: format!("Failed to create Ollama client: {}", e),
            })?;

        Ok(client.completion_model(&self.model))
    }

    pub fn health_check(&self) -> bool {
        let client = reqwest::blocking::Client::new();
        let url = format!("{}/api/tags", self.base_url.trim_end_matches('/'));
        match client.get(&url).send() {
            Ok(resp) => resp.status().is_success(),
            Err(e) => {
                info!(error = %e, "Ollama health check failed");
                false
            }
        }
    }
}
