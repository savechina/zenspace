use rig::client::CompletionClient;
use rig::completion::CompletionModel;
use rig::providers::openai;
use tracing::info;

use crate::router::LlmError;

#[derive(Debug, Clone)]
pub struct RigOpenAIProvider {
    pub api_key: String,
    pub model: String,
}

impl RigOpenAIProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self { api_key, model }
    }

    pub fn completion_model(&self) -> Result<impl CompletionModel, LlmError> {
        let client = openai::Client::new(&self.api_key).map_err(|e| LlmError::Call {
            reason: format!("Failed to create OpenAI client: {}", e),
        })?;
        Ok(client.completion_model(&self.model))
    }

    pub fn health_check(&self) -> bool {
        let client = reqwest::blocking::Client::new();
        let url = "https://api.openai.com/v1/models";
        match client
            .get(url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
        {
            Ok(resp) => resp.status().is_success(),
            Err(e) => {
                info!(error = %e, "OpenAI health check failed");
                false
            }
        }
    }
}
