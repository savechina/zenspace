use rig_agent::AgentBuilder;
use rig_core::completion::CompletionRequest;
use rig_core::providers::cohere::{Cohere, CohereConfig};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::router::{LlmError, UsedCompletion, blocking_usage_call, used_completion};

#[derive(Debug, Clone)]
pub struct CohereProvider {
    pub api_key: String,
    pub model: String,
}

impl CohereProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self { api_key, model }
    }

    /// rig 0.46: `.completion(name)` is the Cohere chat wire — the direct
    /// successor of 0.42's `completion_model()`.
    fn rig_client(&self) -> Cohere {
        CohereConfig::new(&self.api_key).client()
    }

    pub async fn complete_async(
        &self,
        prompt: &str,
        options: &zen_core::config::ModelOptions,
    ) -> Result<String, LlmError> {
        let client = self.rig_client();

        let model = client.completion(&self.model);
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

        let text = response.output();
        info!(
            model = self.model,
            response_len = text.len(),
            "CohereProvider complete"
        );
        Ok(text)
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

    /// Usage-bearing sibling of [`Self::complete_async`]: same model and
    /// options, but the rig `CompletionResponse` is kept whole so the
    /// provider's real token usage rides out alongside the text.
    pub async fn complete_async_with_usage(
        &self,
        prompt: &str,
        options: &zen_core::config::ModelOptions,
    ) -> Result<UsedCompletion, LlmError> {
        let client = self.rig_client();

        let model = client.completion(&self.model);
        let mut request = CompletionRequest::new(prompt);
        if let Some(t) = options.temperature {
            request = request.temperature(t);
        }
        if let Some(m) = options.max_tokens {
            request = request.max_tokens(m);
        }
        let response = model.call(request).await.map_err(|e| LlmError::Call {
            reason: format!("Cohere completion failed: {}", e),
        })?;

        Ok(used_completion("Cohere", response))
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

        blocking_usage_call("Cohere", move || async move {
            let provider = CohereProvider { api_key, model };
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
        if self.api_key.is_empty() {
            warn!("Cohere health check: API key is empty");
            return false;
        }
        info!("CohereProvider health check (key present)");
        true
    }
}
