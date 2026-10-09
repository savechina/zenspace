use rig_agent::AgentBuilder;
use rig_agent::completion::Prompt;
use rig_core::client::CompletionClient;
use rig_core::completion::CompletionModel;
use rig_core::providers::mistral;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::router::{LlmError, UsedCompletion, blocking_usage_call, used_completion};

#[derive(Debug, Clone)]
pub struct MistralProvider {
    pub api_key: String,
    pub model: String,
}

impl MistralProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self { api_key, model }
    }

    pub async fn complete_async(
        &self,
        prompt: &str,
        options: &zen_core::config::ModelOptions,
    ) -> Result<String, LlmError> {
        let client = mistral::Client::new(&self.api_key).map_err(|e| LlmError::Call {
            reason: format!("Failed to create Mistral client: {}", e),
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
            reason: format!("Mistral completion failed: {}", e),
        })?;

        info!(
            model = self.model,
            response_len = response.len(),
            "MistralProvider complete"
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
            let provider = MistralProvider { api_key, model };
            rt.block_on(provider.complete_async(&prompt, &options))
        })
        .join()
        .map_err(|e| LlmError::Call {
            reason: format!("Mistral thread panic: {:?}", e),
        })?
    }

    /// Usage-bearing sibling of [`Self::complete_async`]: same model and
    /// options, but the rig `CompletionResponse` is kept whole so the
    /// provider's real token usage rides out alongside the text.
    pub async fn complete_async_with_usage(
        &self,
        prompt: &str,
        options: &zen_core::config::ModelOptions,
    ) -> Result<UsedCompletion, LlmError> {
        let client = mistral::Client::new(&self.api_key).map_err(|e| LlmError::Call {
            reason: format!("Failed to create Mistral client: {}", e),
        })?;

        let model = client.completion_model(&self.model);
        let mut request = model.completion_request(prompt);
        if let Some(t) = options.temperature {
            request = request.temperature(t);
        }
        if let Some(m) = options.max_tokens {
            request = request.max_tokens(m);
        }
        let response = request.send().await.map_err(|e| LlmError::Call {
            reason: format!("Mistral completion failed: {}", e),
        })?;

        Ok(used_completion("Mistral", response))
    }

    /// Dedicated-thread wrapper around [`Self::complete_async_with_usage`]
    /// (same nesting-panic guard as [`Self::complete`]).
    pub fn complete_with_usage(
        &self,
        prompt: &str,
        options: &zen_core::config::ModelOptions,
    ) -> Result<UsedCompletion, LlmError> {
        let api_key = self.api_key.clone();
        let model = self.model.clone();
        let prompt = prompt.to_string();
        let options = options.clone();

        blocking_usage_call("Mistral", move || async move {
            let provider = MistralProvider { api_key, model };
            provider.complete_async_with_usage(&prompt, &options).await
        })
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
        let url = format!("{}{}", zen_core::constants::MISTRAL_API_URL, "/v1/models");
        match client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
        {
            Ok(resp) => resp.status().is_success(),
            Err(e) => {
                warn!(error = %e, "Mistral health check failed");
                false
            }
        }
    }
}
