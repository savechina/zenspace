use rig_agent::AgentBuilder;
use rig_core::completion::CompletionRequest;
use rig_core::providers::openai::{OpenAI, OpenAIConfig};
use rig_reqwest::ReqwestClient;
use tokio::sync::mpsc;
use tracing::info;

use crate::providers::effective_model_name;
use crate::providers::ollama::is_loopback_url;
use crate::router::{LlmError, UsedCompletion, blocking_usage_call, used_completion};

#[derive(Debug, Clone)]
pub struct OpenAIProvider {
    pub api_key: String,
    pub model: String,
    pub base_url: String,
}

impl OpenAIProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            api_key,
            model,
            base_url: zen_core::constants::OPENAI_API_URL.into(),
        }
    }

    pub fn new_with_base_url(api_key: String, model: String, base_url: String) -> Self {
        Self {
            api_key,
            model,
            base_url,
        }
    }

    /// rig 0.46: `.chat(name)` is the `/chat/completions` wire — the one
    /// endpoint every dialect serves, direct successor of 0.42's
    /// `completions_api().completion_model()`.
    ///
    /// Loopback endpoints (local vLLM / llama.cpp served on the
    /// openai-compatible wire) bypass HTTP(S)_PROXY env vars — the same
    /// hazard class the Ollama provider closes: routing 127.0.0.1 through a
    /// system proxy (dead Privoxy et al) hijacks every local completion.
    fn rig_client(&self) -> Result<OpenAI, LlmError> {
        let mut config = OpenAIConfig::new(&self.api_key);
        if self.base_url != zen_core::constants::OPENAI_API_URL {
            config = config.with_base_url(&self.base_url);
        }
        if is_loopback_url(&self.base_url) {
            let http =
                reqwest::Client::builder()
                    .no_proxy()
                    .build()
                    .map_err(|e| LlmError::Call {
                        reason: format!("Failed to build OpenAI-compatible http client: {}", e),
                    })?;
            Ok(config.connect(ReqwestClient::from(http)))
        } else {
            Ok(config.client())
        }
    }

    pub async fn complete_async(
        &self,
        prompt: &str,
        options: &zen_core::config::ModelOptions,
    ) -> Result<String, LlmError> {
        let client = self.rig_client()?;

        let model = client.chat(effective_model_name(&self.model, options));
        let mut agent_builder = AgentBuilder::new(model);
        if let Some(t) = options.temperature {
            agent_builder = agent_builder.temperature(t);
        }
        if let Some(m) = options.max_tokens {
            agent_builder = agent_builder.max_tokens(m);
        }
        let agent = agent_builder.build();

        let response = agent.prompt(prompt).await.map_err(|e| LlmError::Call {
            reason: format!("OpenAI completion failed: {}", e),
        })?;

        let text = response.output();
        info!(
            model = effective_model_name(&self.model, options),
            response_len = text.len(),
            "OpenAIProvider complete"
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
        let base_url = self.base_url.clone();
        let prompt = prompt.to_string();
        let options = options.clone();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let provider = OpenAIProvider {
                api_key,
                model,
                base_url,
            };
            rt.block_on(provider.complete_async(&prompt, &options))
        })
        .join()
        .map_err(|e| LlmError::Call {
            reason: format!("OpenAI thread panic: {:?}", e),
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
        let client = self.rig_client()?;

        let model = client.chat(effective_model_name(&self.model, options));
        let mut request = CompletionRequest::new(prompt);
        if let Some(t) = options.temperature {
            request = request.temperature(t);
        }
        if let Some(m) = options.max_tokens {
            request = request.max_tokens(m);
        }
        let response = model.call(request).await.map_err(|e| LlmError::Call {
            reason: format!("OpenAI completion failed: {}", e),
        })?;

        Ok(used_completion("Openai", response))
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
        let base_url = self.base_url.clone();
        let prompt = prompt.to_string();
        let options = options.clone();

        blocking_usage_call("OpenAI", move || async move {
            let provider = OpenAIProvider {
                api_key,
                model,
                base_url,
            };
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
}

#[cfg(test)]
mod loopback_no_proxy_tests {
    use super::*;

    /// W12.21 pin: the no_proxy branch predicate. Every never-leaves-the-
    /// machine host form classifies loopback; remote API hosts (and
    /// unparsable junk, which fails closed to the proxied default path)
    /// do not. Mirrors the Ollama provider's loopback classification.
    #[test]
    fn loopback_predicate_matches_all_local_host_forms() {
        for url in [
            "http://127.0.0.1:8000/v1",
            "http://localhost:1234/v1",
            "http://[::1]:8080/v1",
            "http://0.0.0.0:8000/v1",
            "http://127.9.9.9:8000/v1",
        ] {
            assert!(
                is_loopback_url(url),
                "{url} must classify loopback (no_proxy branch)"
            );
        }
        for url in [
            zen_core::constants::OPENAI_API_URL,
            "https://api.deepseek.com",
            "http://vllm.internal:8000/v1",
            "not a url",
        ] {
            assert!(
                !is_loopback_url(url),
                "{url} must NOT classify loopback (proxied default path)"
            );
        }
    }

    /// W12.21 pin: the loopback builder path constructs the rig client
    /// through the no_proxy reqwest injection (`config.connect(...)`)
    /// instead of the shared proxied client — an Err here would mean the
    /// no_proxy branch fails to build and silently regresses to the
    /// proxy-inheriting default.
    #[test]
    fn loopback_base_url_builds_client_via_no_proxy_path() {
        let provider = OpenAIProvider::new_with_base_url(
            "test-key".to_string(),
            "test-model".to_string(),
            "http://127.0.0.1:9/v1".to_string(),
        );
        provider
            .rig_client()
            .expect("loopback base_url must build the no_proxy rig client");
    }

    /// Byte-identical guard: the remote default path is untouched — the
    /// shared (proxied) client still builds for a non-loopback base_url.
    #[test]
    fn remote_base_url_keeps_default_client_path() {
        let provider = OpenAIProvider::new("test-key".to_string(), "test-model".to_string());
        provider
            .rig_client()
            .expect("remote base_url must keep the shared-client default path");
    }
}
