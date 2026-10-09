use rig_agent::AgentBuilder;
use rig_agent::completion::Prompt;
use rig_core::client::CompletionClient;
use rig_core::completion::CompletionModel;
use rig_core::providers::anthropic;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::cache::{CacheSegments, CacheUsage, CachedCompletion, PromptCacheControl};
use crate::router::{LlmError, UsedCompletion, blocking_usage_call, used_completion};

#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    pub api_key: String,
    pub model: String,
    pub base_url: String,
}

/// Cache breakpoints ride the LAST doc block, the summary block and the
/// whitelist block (cumulative prefix checkpoints — OpenKB's 3-marker
/// layout). The system block alone is never marked.
pub const CACHE_BREAKPOINTS_PER_PROMPT: usize = 3;

/// Wall-clock ceiling for one cached `/v1/messages` request.
///
/// `reqwest::Client::new()` has **no** default timeout, so without this a
/// stalled connection hangs the calling worker forever. Matches the 120s
/// bound the Ollama path already uses; this is a single non-streaming
/// request with a bounded prompt, not a long stream.
pub const CACHED_COMPLETE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

impl AnthropicProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            api_key,
            model,
            base_url: zen_core::constants::ANTHROPIC_BASE_URL.into(),
        }
    }

    pub fn new_with_base_url(api_key: String, model: String, base_url: String) -> Self {
        Self {
            api_key,
            model,
            base_url,
        }
    }

    pub async fn complete_async(
        &self,
        prompt: &str,
        options: &zen_core::config::ModelOptions,
    ) -> Result<String, LlmError> {
        let mut builder = anthropic::Client::builder().api_key(&self.api_key);
        if self.base_url != zen_core::constants::ANTHROPIC_BASE_URL {
            builder = builder.base_url(&self.base_url);
        }
        let client = builder.build().map_err(|e| LlmError::Call {
            reason: format!("Failed to create Anthropic client: {}", e),
        })?;

        let model = client.completion_model(&self.model);
        let mut agent_builder = AgentBuilder::new(model);
        let max_tokens = options.max_tokens.unwrap_or(4096);
        agent_builder = agent_builder.max_tokens(max_tokens);
        if let Some(t) = options.temperature {
            agent_builder = agent_builder.temperature(t);
        }
        let agent = agent_builder.build();

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
            let provider = AnthropicProvider {
                api_key,
                model,
                base_url,
            };
            rt.block_on(provider.complete_async(&prompt, &options))
        })
        .join()
        .map_err(|e| LlmError::Call {
            reason: format!("Anthropic thread panic: {:?}", e),
        })?
    }

    /// Usage-bearing sibling of [`Self::complete_async`]: same model,
    /// base-url handling and the 4096 max-token default, but the rig
    /// `CompletionResponse` is kept whole so the provider's real token
    /// usage rides out alongside the text.
    pub async fn complete_async_with_usage(
        &self,
        prompt: &str,
        options: &zen_core::config::ModelOptions,
    ) -> Result<UsedCompletion, LlmError> {
        let mut builder = anthropic::Client::builder().api_key(&self.api_key);
        if self.base_url != zen_core::constants::ANTHROPIC_BASE_URL {
            builder = builder.base_url(&self.base_url);
        }
        let client = builder.build().map_err(|e| LlmError::Call {
            reason: format!("Failed to create Anthropic client: {}", e),
        })?;

        let model = client.completion_model(&self.model);
        let mut request = model.completion_request(prompt);
        request = request.max_tokens(options.max_tokens.unwrap_or(4096));
        if let Some(t) = options.temperature {
            request = request.temperature(t);
        }
        let response = request.send().await.map_err(|e| LlmError::Call {
            reason: format!("Anthropic completion failed: {}", e),
        })?;

        Ok(used_completion("Anthropic", response))
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

        blocking_usage_call("Anthropic", move || async move {
            let provider = AnthropicProvider {
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

    /// Cache-aware completion over the raw `/v1/messages` endpoint:
    /// segments become `system` blocks with `cache_control` breakpoints,
    /// `task` is the user message. Bypasses rig-core, which does not expose
    /// `cache_control` (compile-hygiene ③).
    pub async fn complete_cached(
        &self,
        segments: &CacheSegments,
        task: &str,
        options: &zen_core::config::ModelOptions,
    ) -> Result<CachedCompletion, LlmError> {
        let body = build_cached_request_body(&self.model, segments, task, options);
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(CACHED_COMPLETE_TIMEOUT)
            .build()
            .map_err(|e| LlmError::Call {
                reason: format!("Anthropic cached client build failed: {}", e),
            })?;
        let response = client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Call {
                reason: format!("Anthropic cached completion failed: {}", e),
            })?;

        let status = response.status();
        let payload: serde_json::Value = response.json().await.map_err(|e| LlmError::Call {
            reason: format!("Anthropic cached completion decode failed: {}", e),
        })?;
        if !status.is_success() {
            return Err(LlmError::Call {
                reason: format!("Anthropic cached completion status {status}: {payload}"),
            });
        }

        let usage = parse_cache_usage(&payload);
        let text = payload["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        info!(
            model = self.model,
            cache_creation = ?usage.cache_creation_input_tokens,
            cache_read = ?usage.cache_read_input_tokens,
            "AnthropicProvider cached complete"
        );
        Ok(CachedCompletion { text, usage })
    }
}

/// Marker trait impl — the only provider with cache support today.
impl PromptCacheControl for AnthropicProvider {
    fn supports_prompt_cache(&self) -> bool {
        true
    }
}

/// Build the `/v1/messages` request body: segmented `system` blocks with
/// `cache_control` on the last doc, summary and whitelist blocks (pure —
/// unit-testable without network).
pub fn build_cached_request_body(
    model: &str,
    segments: &CacheSegments,
    task: &str,
    options: &zen_core::config::ModelOptions,
) -> serde_json::Value {
    let last_doc = segments
        .segments
        .iter()
        .rposition(|s| s.kind == crate::cache::SegmentKind::Doc);
    let summary_idx = segments
        .segments
        .iter()
        .position(|s| s.kind == crate::cache::SegmentKind::Summary);
    let whitelist_idx = segments
        .segments
        .iter()
        .position(|s| s.kind == crate::cache::SegmentKind::Whitelist);
    let marked = [last_doc, summary_idx, whitelist_idx]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    debug_assert_eq!(
        marked.len(),
        CACHE_BREAKPOINTS_PER_PROMPT,
        "a CacheSegments built by `build` always carries one Doc, Summary \
         and Whitelist block, so all three breakpoints must land"
    );

    let system: Vec<serde_json::Value> = segments
        .segments
        .iter()
        .enumerate()
        .map(|(i, segment)| {
            let mut block = serde_json::json!({ "type": "text", "text": segment.text });
            if marked.contains(&i) {
                block["cache_control"] = serde_json::json!({ "type": "ephemeral" });
            }
            block
        })
        .collect();

    let mut body = serde_json::json!({
        "model": model,
        "max_tokens": options.max_tokens.unwrap_or(4096),
        "system": system,
        "messages": [{ "role": "user", "content": task }],
    });
    if let Some(t) = options.temperature {
        body["temperature"] = serde_json::json!(t);
    }
    body
}

/// Extract the cache-usage echo from a `/v1/messages` response; absent
/// fields stay `None` (older gateways, non-Anthropic-compatible proxies).
fn parse_cache_usage(payload: &serde_json::Value) -> CacheUsage {
    let usage = &payload["usage"];
    CacheUsage {
        input_tokens: usage["input_tokens"].as_u64(),
        output_tokens: usage["output_tokens"].as_u64(),
        cache_creation_input_tokens: usage["cache_creation_input_tokens"].as_u64(),
        cache_read_input_tokens: usage["cache_read_input_tokens"].as_u64(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::SegmentKind;

    fn segments() -> CacheSegments {
        CacheSegments::build(
            "system instructions",
            &[String::from("doc one"), String::from("doc two")],
            "summary so far",
            &["Alpha".to_string(), "Beta".to_string()],
        )
    }

    #[test]
    fn supports_prompt_cache_is_true() {
        let provider = AnthropicProvider::new("key".into(), "claude-x".into());
        assert!(PromptCacheControl::supports_prompt_cache(&provider));
    }

    #[test]
    fn cache_markers_land_on_last_doc_summary_whitelist_only() {
        let body = build_cached_request_body(
            "claude-x",
            &segments(),
            "compile this",
            &zen_core::config::ModelOptions::default(),
        );

        let system = body["system"].as_array().expect("system blocks");
        assert_eq!(system.len(), 5, "system + 2 docs + summary + whitelist");
        let marked: Vec<usize> = system
            .iter()
            .enumerate()
            .filter(|(_, b)| !b["cache_control"].is_null())
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            marked,
            vec![2, 3, 4],
            "breakpoints: last doc (2), summary (3), whitelist (4)"
        );
        assert_eq!(system[2]["text"], "doc two");
        assert_eq!(body["system"][4]["cache_control"]["type"], "ephemeral");
        assert_eq!(CACHE_BREAKPOINTS_PER_PROMPT, marked.len());
    }

    #[test]
    fn request_body_carries_model_task_and_options() {
        let options = zen_core::config::ModelOptions {
            max_tokens: Some(2048),
            temperature: Some(0.3),
            ..Default::default()
        };
        let body = build_cached_request_body("claude-x", &segments(), "compile this", &options);
        assert_eq!(body["model"], "claude-x");
        assert_eq!(body["max_tokens"], 2048);
        assert_eq!(body["temperature"], 0.3);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "compile this");
    }

    #[test]
    fn non_anthropic_providers_have_no_cache_support() {
        // Providers without a `PromptCacheControl` impl cannot even be
        // asked — the compile-time absence of the method IS the no-op
        // guarantee (Q-A). The fallback path a caller takes for them is
        // the flat prompt, which carries no markers.
        let flat = segments().to_single_prompt();
        assert!(!flat.contains("cache_control"));
        assert!(
            segments()
                .segments
                .iter()
                .any(|s| s.kind == SegmentKind::Whitelist)
        );
    }

    #[test]
    fn usage_parse_tolerates_missing_fields() {
        let payload = serde_json::json!({
            "content": [{ "type": "text", "text": "hi" }],
            "usage": { "input_tokens": 100, "cache_read_input_tokens": 80 }
        });
        let usage = parse_cache_usage(&payload);
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.cache_read_input_tokens, Some(80));
        assert_eq!(usage.output_tokens, None);
        assert_eq!(usage.cache_creation_input_tokens, None);
    }
}
