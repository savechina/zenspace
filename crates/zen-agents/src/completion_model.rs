use rig_core::completion::message::Origin;
use rig_core::completion::{AssistantContent, CompletionRequest, CompletionResponse, Usage};
use rig_core::error::ProviderError;
use rig_core::message::Message;
use std::sync::Arc;
use zen_provider::LlmRouter;

/// Budget for the FIRST token only: local models (e.g. 30B+ on Ollama)
/// spend minutes on cold model load + long-prompt prefill before any
/// token exists — the inter-token budget must not govern that window.
/// W7.1 (D22/F2 FINAL, owner directives ×2): THE primary timeout —
/// code-tunable only; no config key, no `[llm]` table, no
/// `ZEN_LLM_REQUEST_TIMEOUT_SECS` ever lands (W7.2 grep-proves absence).
const STREAM_FIRST_TOKEN_TIMEOUT_SECS: u64 = 600;

/// W7.1: the inter-token budget is a DERIVATION, not a second constant —
/// `max(30, first-token/5)`. Defaults preserve exactly 600/120; a host
/// overriding the first-token env shrinks the inter-token default with it.
/// The pre-existing `ZEN_STREAM_INACTIVITY_TIMEOUT_SECS` env still beats
/// this default (override surface unchanged, not deprecated).
pub fn inactivity_budget_default_secs() -> u64 {
    (first_token_budget_secs() / 5).max(30)
}

/// Env override keys (parsed once per process):
/// `ZEN_STREAM_FIRST_TOKEN_TIMEOUT_SECS`, `ZEN_STREAM_INACTIVITY_TIMEOUT_SECS`.
fn env_budget(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// W6.5: the first-token budget as a plain seconds value — the local
/// synthesis timeout derives from it (min(300, first-token)).
pub fn first_token_budget_secs() -> u64 {
    env_budget(
        "ZEN_STREAM_FIRST_TOKEN_TIMEOUT_SECS",
        STREAM_FIRST_TOKEN_TIMEOUT_SECS,
    )
}

/// The timeout applying to the next `token_rx.recv()` poll: the first
/// token gets the long cold-start budget, every later token the short
/// inactivity budget.
fn stream_budget(
    is_first_token: bool,
    first_token_secs: u64,
    inactivity_secs: u64,
) -> std::time::Duration {
    std::time::Duration::from_secs(if is_first_token {
        first_token_secs
    } else {
        inactivity_secs
    })
}

fn stream_budget_for_next(token_count: usize) -> std::time::Duration {
    static BUDGETS: std::sync::OnceLock<(u64, u64)> = std::sync::OnceLock::new();
    let (first, inactivity) = *BUDGETS.get_or_init(|| {
        let first = env_budget(
            "ZEN_STREAM_FIRST_TOKEN_TIMEOUT_SECS",
            STREAM_FIRST_TOKEN_TIMEOUT_SECS,
        );
        (
            first,
            env_budget("ZEN_STREAM_INACTIVITY_TIMEOUT_SECS", (first / 5).max(30)),
        )
    });
    stream_budget(token_count == 0, first, inactivity)
}

/// W7.1: the non-streaming call bound — the first-token constant applied
/// to `completion()` (previously unbounded). Deriving the inter-token
/// default and bounding the sync call both hang off the ONE primary
/// timeout.
pub fn nonstream_call_bound_secs() -> u64 {
    first_token_budget_secs()
}

/// Conservative token estimate for a STREAMED response body: ~4 chars/token.
///
/// rig-core 0.42 replaced the 0.41 `GetTokenUsage` trait with the concrete
/// `Usage` struct carried by `StreamFinal`/`CompletionResponse`. The
/// non-stream path now feeds real provider usage into `Usage` (see
/// `completion`); the stream path has no provider echo yet, so the same
/// len/4 heuristic that fed the old `ZenCompletionResponse::token_usage`
/// feeds `Usage.output_tokens` (T096 budgets read these totals).
fn estimate_tokens(text: &str) -> Usage {
    let n = text.len() as u64 / 4; // conservative ~4 chars/token
    Usage::new()
        .input_tokens(0)
        .output_tokens(n)
        .total_tokens(n)
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

    /// T225: construct from an ALREADY-ROUTED [`zen_provider::Provider`].
    ///
    /// The orchestrator resolves the generation provider through
    /// sensitivity-aware routing (`route_with_preferences` over the session
    /// sensitivity policy + the agent's `LlmPreference`) before building the
    /// model, so the routed provider can differ from the configured default.
    /// Taking the enum directly avoids re-parsing a name back into a provider.
    pub fn from_provider(
        router: zen_provider::DefaultRouter,
        provider: zen_provider::Provider,
    ) -> Self {
        let model_name = match &provider {
            zen_provider::Provider::Unknown(name) => name.clone(),
            other => other.to_string(),
        };
        Self {
            router: Arc::new(router),
            provider,
            model_name,
        }
    }

    pub fn provider_name(&self) -> &str {
        &self.model_name
    }
}

impl ZenCompletionModel {
    /// rig 0.46: the `CompletionModel` trait no longer exists — models are
    /// `Model<W>` values and zen's router-backed model was never fed into
    /// rig's `AgentBuilder` (the orchestrator drives the raw `AgentRun`
    /// state machine with synthetic `ModelTurn`s). The 0.42 trait methods
    /// become inherent async fns; errors are `ProviderError` (the 0.46
    /// successor of `CompletionError`).
    // rig 0.46's ProviderError is a large enum (its own Model::call returns
    // it by value); these inherent methods mirror that upstream shape.
    #[allow(clippy::result_large_err)]
    pub async fn completion(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        let prompt = extract_last_user_prompt(&request);
        let prompt_len = prompt.len();

        tracing::debug!(
            provider = %self.model_name,
            prompt_len,
            "completion_model: starting LLM completion"
        );

        // W7.1: the non-streaming call acquires the primary-timeout bound.
        // The router call is synchronous (dedicated-thread completion), so
        // it moves onto the blocking pool; on timeout the wrapper future
        // cancels first (bound is best-effort — the thread result is
        // dropped when it eventually finishes, mirroring the gateway
        // watchdog semantics).
        let router = self.router.clone();
        let provider = self.provider.clone();
        let prompt = prompt.to_string();
        let bound = std::time::Duration::from_secs(nonstream_call_bound_secs());
        let completion = match tokio::time::timeout(bound, async move {
            tokio::task::spawn_blocking(move || router.call_with_usage(provider, &prompt))
                .await
                .map_err(|e| format!("router completion join: {e}"))?
                .map_err(|e| format!("zen-provider call failed: {e}"))
        })
        .await
        {
            Ok(inner) => inner.map_err(|e| {
                tracing::error!(provider = %self.model_name, error = %e, "completion_model: LLM completion failed");
                ProviderError::request(e)
            })?,
            Err(_) => {
                let err_msg = format!(
                    "non-streaming completion timed out after {bound:?} \
                     (cold model load or long prefill; tune \
                     ZEN_STREAM_FIRST_TOKEN_TIMEOUT_SECS)"
                );
                tracing::error!(provider = %self.model_name, error = %err_msg, "completion_model: LLM completion timed out");
                return Err(ProviderError::request(err_msg));
            }
        };
        let response_text = completion.text;

        tracing::info!(
            provider = %self.model_name,
            response_len = response_text.len(),
            usage_reported = completion.usage.is_some(),
            "completion_model: LLM completion succeeded"
        );

        // Real provider usage when the sync surface echoed it (the rig
        // Usage totals feed AgentRun/executor token accounting); the old
        // empty Usage only when the provider stayed silent.
        let usage = match completion.usage {
            Some(u) => Usage::new()
                .input_tokens(u.input_tokens)
                .output_tokens(u.output_tokens)
                .total_tokens(u.input_tokens + u.output_tokens),
            None => Usage::new(),
        };

        Ok(CompletionResponse::new(
            vec![AssistantContent::text(response_text.clone())],
            usage,
            Origin::new("zen-native", "zen", self.model_name.clone()),
            serde_json::Value::Null,
        ))
    }

    #[allow(clippy::result_large_err)]
    pub async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<ZenStreamHandle, ProviderError> {
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
                ProviderError::request(err_msg)
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
            Box<dyn futures::Stream<Item = Result<ZenStreamItem, ProviderError>> + Send>,
        > = Box::pin(futures::stream::unfold(
            state,
            |(mut token_rx, mut done_rx, mut collected, mut token_count, provider_name)| async move {
                match tokio::time::timeout(stream_budget_for_next(token_count), token_rx.recv())
                    .await
                {
                    Ok(Some(token)) => {
                        collected.push_str(&token);
                        token_count += 1;
                        Some((
                            Ok(ZenStreamItem::Delta(token.clone())),
                            (token_rx, done_rx, collected, token_count, provider_name),
                        ))
                    }
                    Ok(None) => {
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
                                    Ok(ZenStreamItem::Final {
                                        provider: provider_name.clone(),
                                        usage: estimate_tokens(&text),
                                    }),
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
                                    Err(ProviderError::request(format!("streaming error: {e}"))),
                                    (token_rx, done_rx, collected, token_count, provider_name),
                                ))
                            }
                        }
                    }
                    Err(_) => {
                        tracing::error!(
                            provider = %provider_name,
                            token_count,
                            response_len = collected.len(),
                            "completion_model: LLM stream timed out waiting for tokens"
                        );
                        let reason = if token_count == 0 {
                            "streaming timed out before the first token \
                             (cold model load or long prefill; tune \
                             ZEN_STREAM_FIRST_TOKEN_TIMEOUT_SECS)"
                        } else {
                            "streaming timed out: no tokens received"
                        };
                        Some((
                            Err(ProviderError::request(reason)),
                            (token_rx, done_rx, collected, token_count, provider_name),
                        ))
                    }
                }
            },
        ));

        Ok(ZenStreamHandle {
            model: self.model_name.clone(),
            inner: boxed,
        })
    }
}

/// One item of zen's own stream carrier: a text delta, or the terminal
/// record carrying the estimated usage.
#[derive(Debug, Clone)]
pub enum ZenStreamItem {
    Delta(String),
    Final { provider: String, usage: Usage },
}

/// zen-owned stream handle replacing rig 0.42's
/// `StreamingCompletionResponse` (deleted in 0.46). zen never fed its
/// router-backed stream into rig's own fold — this carries the same
/// unfold through the consumers (zen_agent.rs stream fold, orchestrator
/// stream path) with no rig coupling.
pub struct ZenStreamHandle {
    pub model: String,
    pub inner:
        std::pin::Pin<Box<dyn futures::Stream<Item = Result<ZenStreamItem, ProviderError>> + Send>>,
}

impl ZenStreamHandle {
    /// Consume the terminal item, returning `(full_text, usage)` — the
    /// delta-collecting fold every consumer previously hand-rolled over
    /// `RawStreamingChoice`.
    #[allow(clippy::result_large_err)] // mirrors upstream ProviderError shape
    pub async fn collect(mut self) -> Result<(String, Usage), ProviderError> {
        use futures::StreamExt;
        let mut collected = String::new();
        let mut usage = Usage::new();
        while let Some(item) = self.inner.next().await {
            match item? {
                ZenStreamItem::Delta(token) => collected.push_str(&token),
                ZenStreamItem::Final { usage: u, .. } => {
                    usage = u;
                    break;
                }
            }
        }
        Ok((collected, usage))
    }
}

pub(crate) fn parse_provider_name(name: &str) -> zen_provider::Provider {
    match name.to_lowercase().as_str() {
        "openai" | "oa" => zen_provider::Provider::OpenAI,
        "anthropic" | "an" => zen_provider::Provider::Anthropic,
        "deepseek" | "ds" | "deep_seek" => zen_provider::Provider::DeepSeek,
        "qq" | "qqbot" | "qq_bot" => zen_provider::Provider::QQBot,
        "ollama" | "local" | "ollama-local" => zen_provider::Provider::Ollama,
        "mock" => zen_provider::Provider::Mock,
        other => zen_provider::Provider::Unknown(other.into()),
    }
}

fn extract_last_user_prompt(request: &CompletionRequest) -> String {
    let user_prompt = {
        let mut found: Option<String> = None;
        for msg in request.chat_history.iter().rev() {
            if let Message::User { content } = msg.as_ref() {
                for uc in content.iter().rev() {
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

    // rig 0.46 dropped `preamble` — the system prompt rides as the leading
    // System message of the history (the shape zen's request builder emits).
    let preamble = request.chat_history.iter().find_map(|m| match m.as_ref() {
        Message::System { content } => Some(content.as_str()),
        _ => None,
    });

    let preamble_owned = preamble.unwrap_or("").to_string();
    let preamble: &str = &preamble_owned;

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

    // W7.1: defaults are exactly 600/120 — the inter-token budget is a
    // derivation, no second constant exists.
    #[test]
    fn w71_default_budgets_are_exactly_600_120() {
        assert_eq!(first_token_budget_secs(), 600, "default primary timeout");
        assert_eq!(
            inactivity_budget_default_secs(),
            120,
            "derived inter-token default"
        );
    }

    // W7.1: the inter-token budget DERIVES (first=240 => 48; first=100 =>
    // floor 30 kicks in) — shrinking the first-token env shrinks the
    // inter-token default with it.
    #[test]
    fn w71_inter_token_budget_derives_from_first_token() {
        // SAFETY: nextest runs each test in its own process; no other
        // thread reads these envs concurrently (value is valid UTF-8).
        unsafe { std::env::set_var("ZEN_STREAM_FIRST_TOKEN_TIMEOUT_SECS", "240") };
        assert_eq!(inactivity_budget_default_secs(), 48);
        unsafe { std::env::set_var("ZEN_STREAM_FIRST_TOKEN_TIMEOUT_SECS", "100") };
        assert_eq!(inactivity_budget_default_secs(), 30);
        unsafe { std::env::remove_var("ZEN_STREAM_FIRST_TOKEN_TIMEOUT_SECS") };
    }

    // W7.1: the non-streaming call acquires the primary-timeout bound —
    // a provider that accepts TCP but never answers fails fast at the
    // budget instead of hanging on the provider's own (240s+) timeout.
    #[test]
    fn w71_nonstream_call_is_bounded_by_the_primary_timeout() {
        // SAFETY: process-per-test (nextest); valid UTF-8 value.
        unsafe { std::env::set_var("ZEN_STREAM_FIRST_TOKEN_TIMEOUT_SECS", "2") };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            // Accept and hold connections open forever — a deterministic
            // HTTP hang without writing a byte.
            for stream in listener.incoming().flatten() {
                // Leak the accepted socket — dropping it would close the
                // connection (RST) and turn the deterministic hang into a
                // fast error, voiding the bound proof.
                std::mem::forget(stream);
            }
        });
        let router = zen_provider::DefaultRouter::from_agentic(&zen_core::config::ZenConfig {
            default_provider: Some("ollama".to_string()),
            providers: std::collections::HashMap::from([(
                "ollama".to_string(),
                zen_core::config::ProviderConfig {
                    provider_type: Some("ollama".to_string()),
                    base_url: Some(format!("http://127.0.0.1:{port}")),
                    default_model: Some("test-model".to_string()),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        });
        let model = ZenCompletionModel::from_provider(
            router,
            zen_provider::Provider::Unknown("ollama".into()),
        );
        let request = CompletionRequest::from(vec![Message::user("Say hello")]);
        // Explicit runtime with a bounded shutdown: the leaked blocking
        // task (the provider's own 120s wrapper) must not gate teardown —
        // `shutdown_timeout` abandons it, the tokio-test runtime would
        // wait for it and stretch the test to 120s.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("test runtime");
        let started = std::time::Instant::now();
        let result = rt.block_on(model.completion(request));
        let elapsed = started.elapsed();
        // Shuttle build: the stub runtime has no blocking pool to leak.
        #[cfg(not(feature = "shuttle"))]
        rt.shutdown_timeout(std::time::Duration::from_millis(200));
        unsafe { std::env::remove_var("ZEN_STREAM_FIRST_TOKEN_TIMEOUT_SECS") };
        let err = result.expect_err("the hanging provider must time out");
        let msg = err.to_string();
        assert!(
            msg.contains("non-streaming completion timed out"),
            "the failure must be the primary-timeout bound, not the              provider's own: {msg}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(30),
            "the bound must fire at ~2s, not the provider's own 120s: {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn zen_completion_model_returns_mock_response() {
        let model = ZenCompletionModel::new(mock_router(), "mock");

        let request = CompletionRequest::from(vec![Message::user("Say hello")]);

        let response = model.completion(request).await.expect("completion");

        let text = match response.choice.first() {
            Some(AssistantContent::Text(t)) => t.text.clone(),
            _ => panic!("expected text response, got non-text"),
        };
        assert!(text.contains("mock"), "expected mock response, got: {text}");
        // The mock provider never reports usage — DL-1 semantics: counters
        // stay None (reported-zero would be Some(0)).
        assert_eq!(response.usage.input_tokens, None);
        assert_eq!(response.usage.output_tokens, None);
    }

    #[test]
    fn extract_prompt_prepends_preamble() {
        let request = CompletionRequest::from(vec![
            Message::system("You are a test assistant."),
            Message::user("Hello"),
        ]);

        let prompt = extract_last_user_prompt(&request);
        assert!(prompt.starts_with("You are a test assistant."));
        assert!(prompt.contains("Hello"));
    }

    #[test]
    fn extract_prompt_no_preamble_just_user() {
        let request = CompletionRequest::from(vec![Message::user("Hello")]);

        let prompt = extract_last_user_prompt(&request);
        assert_eq!(prompt, "Hello");
    }

    #[test]
    fn stream_budget_first_token_uses_cold_start_budget() {
        assert_eq!(
            stream_budget(true, 600, 120),
            std::time::Duration::from_secs(600)
        );
    }

    #[test]
    fn stream_budget_subsequent_tokens_use_inactivity_budget() {
        assert_eq!(
            stream_budget(false, 600, 120),
            std::time::Duration::from_secs(120)
        );
    }

    #[test]
    fn env_budget_falls_back_to_default_on_missing_or_malformed() {
        // Not setting the env var (other tests may run in parallel and
        // mutating the process env would race): only the fallback path
        // is asserted here.
        assert_eq!(env_budget("ZEN_TEST_DEFINITELY_UNSET_ENV_KEY", 42), 42);
    }
}
