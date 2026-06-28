use rig_core::OneOrMany;
use rig_core::completion::{
    AssistantContent, CompletionError, CompletionModel, CompletionRequest, CompletionResponse,
    GetTokenUsage, Usage,
};
use rig_core::message::Message;
use rig_core::streaming::{RawStreamingChoice, StreamingCompletionResponse};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use zen_provider::LlmRouter;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MockResponse {
    _text: String,
}

impl GetTokenUsage for MockResponse {
    fn token_usage(&self) -> Option<Usage> {
        None
    }
}

#[derive(Clone)]
pub struct ZenCompletionModel {
    router: Arc<zen_provider::DefaultRouter>,
    provider: zen_provider::Provider,
    model_name: String,
}

impl ZenCompletionModel {
    pub fn new(router: zen_provider::DefaultRouter, provider_name: &str) -> Self {
        let provider = parse_provider_name(provider_name);
        Self {
            router: Arc::new(router),
            provider,
            model_name: provider_name.to_string(),
        }
    }

    pub fn provider_name(&self) -> &str {
        &self.model_name
    }
}

impl CompletionModel for ZenCompletionModel {
    type Response = MockResponse;
    type StreamingResponse = MockResponse;
    type Client = ();

    fn make(_: &(), model: impl Into<String>) -> Self {
        let model_str = model.into();
        let config = zen_provider::LlmConfig::default();
        let router = zen_provider::DefaultRouter::new(config);
        let provider = parse_provider_name(&model_str);

        Self {
            router: Arc::new(router),
            provider,
            model_name: model_str,
        }
    }

    async fn completion(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        let prompt = extract_last_user_prompt(&request);
        let prompt_len = prompt.len();

        tracing::debug!(
            provider = %self.model_name,
            prompt_len,
            "completion_model: starting LLM completion"
        );

        let response_text = self
            .router
            .call(self.provider.clone(), &prompt)
            .map_err(|e| {
                let err_msg = format!("zen-provider call failed: {e}");
                tracing::error!(provider = %self.model_name, error = %err_msg, "completion_model: LLM completion failed");
                CompletionError::ProviderError(err_msg)
            })?;

        tracing::info!(
            provider = %self.model_name,
            response_len = response_text.len(),
            "completion_model: LLM completion succeeded"
        );

        Ok(CompletionResponse {
            choice: OneOrMany::one(AssistantContent::text(response_text.clone())),
            usage: Usage::new(),
            raw_response: MockResponse {
                _text: response_text,
            },
            message_id: None,
        })
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        let prompt = extract_last_user_prompt(&request);
        let prompt_len = prompt.len();

        tracing::debug!(
            provider = %self.model_name,
            prompt_len,
            "completion_model: starting LLM stream"
        );

        let stream_resp = self
            .router
            .call_stream(self.provider.clone(), &prompt)
            .map_err(|e| {
                let err_msg = format!("zen-provider call_stream failed: {e}");
                tracing::error!(provider = %self.model_name, error = %err_msg, "completion_model: LLM stream setup failed");
                CompletionError::ProviderError(err_msg)
            })?;

        tracing::info!(provider = %self.model_name, "completion_model: LLM stream initialized");

        let state = (
            stream_resp.token_rx,
            stream_resp.done_rx,
            String::new(),
            0usize,
            self.model_name.clone(),
        );

        #[allow(clippy::type_complexity)]
        let boxed: std::pin::Pin<
            Box<
                dyn futures::Stream<
                        Item = Result<RawStreamingChoice<Self::StreamingResponse>, CompletionError>,
                    > + Send,
            >,
        > = Box::pin(futures::stream::unfold(
            state,
            |(mut token_rx, mut done_rx, mut collected, mut token_count, provider_name)| async move {
                match token_rx.recv().await {
                    Some(token) => {
                        collected.push_str(&token);
                        token_count += 1;
                        Some((
                            Ok(RawStreamingChoice::Message(token.clone())),
                            (token_rx, done_rx, collected, token_count, provider_name),
                        ))
                    }
                    None => {
                        let result = done_rx.try_recv().unwrap_or(Ok(()));
                        let text = std::mem::take(&mut collected);
                        match result {
                            Ok(()) => {
                                tracing::info!(
                                    provider = %provider_name,
                                    token_count,
                                    response_len = text.len(),
                                    "completion_model: LLM stream completed"
                                );
                                Some((
                                    Ok(RawStreamingChoice::FinalResponse(MockResponse {
                                        _text: text,
                                    })),
                                    (token_rx, done_rx, collected, token_count, provider_name),
                                ))
                            }
                            Err(e) => {
                                tracing::warn!(
                                    provider = %provider_name,
                                    token_count,
                                    response_len = text.len(),
                                    error = %e,
                                    "completion_model: LLM stream ended with error"
                                );
                                Some((
                                    Err(CompletionError::ProviderError(format!(
                                        "streaming error: {e}"
                                    ))),
                                    (token_rx, done_rx, collected, token_count, provider_name),
                                ))
                            }
                        }
                    }
                }
            },
        ));

        Ok(StreamingCompletionResponse::stream(boxed))
    }
}

fn parse_provider_name(name: &str) -> zen_provider::Provider {
    match name.to_lowercase().as_str() {
        "openai" | "oa" => zen_provider::Provider::OpenAI,
        "anthropic" | "an" => zen_provider::Provider::Anthropic,
        "deepseek" | "ds" | "deep_seek" => zen_provider::Provider::DeepSeek,
        "qq" | "qqbot" | "qq_bot" => zen_provider::Provider::QQBot,
        "ollama" | "local" | "ollama-local" => zen_provider::Provider::Ollama,
        "mock" => zen_provider::Provider::Unknown("mock".into()),
        other => zen_provider::Provider::Unknown(other.into()),
    }
}

fn extract_last_user_prompt(request: &CompletionRequest) -> String {
    let user_prompt = {
        let msgs: Vec<_> = request.chat_history.iter().collect();
        let mut found: Option<String> = None;
        for msg in msgs.into_iter().rev() {
            if let Message::User { content } = msg {
                let ucs: Vec<_> = content.iter().collect();
                for uc in ucs.into_iter().rev() {
                    if let rig_core::message::UserContent::Text(t) = uc {
                        found = Some(t.text.clone());
                        break;
                    }
                }
                if found.is_some() {
                    break;
                }
            }
        }
        found
    };

    let preamble = request.preamble.as_deref().unwrap_or("");

    match (preamble.is_empty(), user_prompt) {
        (true, Some(prompt)) => prompt,
        (false, Some(prompt)) => format!("{preamble}\n\n{prompt}"),
        (false, None) => preamble.to_string(),
        (true, None) => "Hello".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zen_core::config::LlmConfig;

    fn mock_router() -> zen_provider::DefaultRouter {
        let config = LlmConfig {
            default_provider: Some("mock".to_string()),
            ..Default::default()
        };
        zen_provider::DefaultRouter::new(config)
    }

    #[tokio::test]
    async fn zen_completion_model_returns_mock_response() {
        let model = ZenCompletionModel::new(mock_router(), "mock");

        let request = CompletionRequest {
            model: None,
            preamble: None,
            chat_history: OneOrMany::one(Message::user("Say hello")),
            documents: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            tool_choice: None,
            additional_params: None,
            output_schema: None,
        };

        let response = model.completion(request).await.expect("completion");

        let text = match response.choice.first() {
            AssistantContent::Text(t) => t.text.clone(),
            _ => panic!("expected text response, got non-text"),
        };
        assert!(text.contains("mock"), "expected mock response, got: {text}");
        assert_eq!(response.usage.input_tokens, 0);
        assert_eq!(response.usage.output_tokens, 0);
    }

    #[test]
    fn extract_prompt_prepends_preamble() {
        let request = CompletionRequest {
            model: None,
            preamble: Some("You are a test assistant.".to_string()),
            chat_history: OneOrMany::one(Message::user("Hello")),
            documents: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            tool_choice: None,
            additional_params: None,
            output_schema: None,
        };

        let prompt = extract_last_user_prompt(&request);
        assert!(prompt.starts_with("You are a test assistant."));
        assert!(prompt.contains("Hello"));
    }

    #[test]
    fn extract_prompt_no_preamble_just_user() {
        let request = CompletionRequest {
            model: None,
            preamble: None,
            chat_history: OneOrMany::one(Message::user("Hello")),
            documents: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            tool_choice: None,
            additional_params: None,
            output_schema: None,
        };

        let prompt = extract_last_user_prompt(&request);
        assert_eq!(prompt, "Hello");
    }
}
