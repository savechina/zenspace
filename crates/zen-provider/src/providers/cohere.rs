use rig::agent::AgentBuilder;
use rig::client::CompletionClient;
use rig::completion::Prompt;
use rig::providers::cohere;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::router::LlmError;

#[derive(Debug, Clone)]
pub struct CohereProvider {
    pub api_key: String,
    pub model: String,
}

impl CohereProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self { api_key, model }
    }

    pub async fn complete_async(
        &self,
        prompt: &str,
        options: &zen_core::config::ModelOptions,
    ) -> Result<String, LlmError> {
        let client = cohere::Client::new(&self.api_key).map_err(|e| LlmError::Call {
            reason: format!("Failed to create Cohere client: {}", e),
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
            reason: format!("Cohere completion failed: {}", e),
        })?;

        info!(
            model = self.model,
            response_len = response.len(),
            "CohereProvider complete"
        );
        Ok(response)
    }

    pub fn complete(
        &self,
        prompt: &str,
        options: &zen_core::config::ModelOptions,
    ) -> Result<String, LlmError> {
        let api_key = self.api_key.clone();
        let model = self.model.clone();
        let prompt = prompt.to_string();
        let options = options.clone();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let provider = CohereProvider { api_key, model };
            rt.block_on(provider.complete_async(&prompt, &options))
        })
        .join()
        .map_err(|e| LlmError::Call {
            reason: format!("Cohere thread panic: {:?}", e),
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
        if self.api_key.is_empty() {
            warn!("Cohere health check: API key is empty");
            return false;
        }
        info!("CohereProvider health check (key present)");
        true
    }
}
