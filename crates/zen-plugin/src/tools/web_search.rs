use std::sync::LazyLock;
use std::time::Duration;

use async_trait::async_trait;
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::{Value, json};
use tracing::warn;
use zen_core::network_policy::NetworkPolicy;

use crate::retry::retry_with_backoff;

const NAME: &str = "web.search";
const DESCRIPTION: &str =
    "Search the web using DuckDuckGo (default) or Brave/Tavily when API keys are configured";

static ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string", "description": "Search query" },
            "max_results": { "type": "integer", "description": "Maximum results to return (default 5)" }
        },
        "required": ["query"]
    })
});

static RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string" },
            "results": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "title": { "type": "string" },
                        "url": { "type": "string" },
                        "snippet": { "type": "string" }
                    }
                }
            },
            "count": { "type": "integer" },
            "dropped_results": { "type": "integer" },
            "provider": { "type": "string" },
            "message": { "type": "string" }
        }
    })
});

#[derive(Clone)]
pub struct WebSearchTool {
    client: reqwest::Client,
    network_policy: NetworkPolicy,
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

impl WebSearchTool {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent("zen-agent/1.0")
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            client,
            network_policy: NetworkPolicy::with_allow_hosts(vec![
                "localhost".into(),
                "127.0.0.1".into(),
            ]),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

/// Parsed provider response plus the number of malformed items dropped.
#[derive(Debug, Default)]
struct SearchOutcome {
    results: Vec<SearchResult>,
    dropped: usize,
}

/// Provider failure classified for retry/fallback decisions.
#[derive(Debug)]
enum ProviderError {
    /// Transient (429/5xx/network): safe to retry, then fall back.
    Retryable(String),
    /// Permanent (auth/config/parse/policy): retrying won't help.
    Fatal(String),
}

impl ProviderError {
    fn retryable(&self) -> bool {
        matches!(self, ProviderError::Retryable(_))
    }

    fn into_message(self) -> String {
        match self {
            ProviderError::Retryable(m) | ProviderError::Fatal(m) => m,
        }
    }
}

const DEFAULT_MAX_RESULTS: usize = 5;
const MAX_RESULTS_LIMIT: usize = 50;

fn url_encode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(byte as char);
            }
            b' ' => result.push('+'),
            _ => result.push_str(&format!("%{:02X}", byte)),
        }
    }
    result
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// Extracts and decodes the `uddg=` target parameter from a DuckDuckGo
/// redirect URL. Returns the real target URL, or `None` if `uddg=` is absent.
fn extract_uddg(href: &str) -> Option<String> {
    let key = "uddg=";
    let idx = href.find(key)?;
    let after = &href[idx + key.len()..];
    let encoded = after.split('&').next()?;
    if encoded.is_empty() {
        return None;
    }
    Some(percent_decode(encoded))
}

async fn check_provider_status(resp: &reqwest::Response) -> Result<(), ProviderError> {
    use reqwest::StatusCode;
    match resp.status() {
        s if s.is_success() => Ok(()),
        StatusCode::TOO_MANY_REQUESTS => {
            let retry_after = parse_retry_after(
                resp.headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok()),
            );
            Err(ProviderError::Retryable(format!(
                "search provider rate limit reached (429): retry after {retry_after}s"
            )))
        }
        s if s.is_server_error() => Err(ProviderError::Retryable(format!(
            "search provider server error ({s})"
        ))),
        s => Err(ProviderError::Fatal(format!(
            "search provider returned {s}"
        ))),
    }
}

fn parse_retry_after(value: Option<&str>) -> u64 {
    value.and_then(|v| v.parse::<u64>().ok()).unwrap_or(60)
}

async fn search_brave(
    client: &reqwest::Client,
    policy: &NetworkPolicy,
    query: &str,
    max: usize,
    api_key: &str,
) -> Result<SearchOutcome, ProviderError> {
    let url = format!(
        "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
        url_encode(query),
        max
    );
    policy
        .validate_url(&url)
        .map_err(|e| ProviderError::Fatal(e.to_string()))?;
    let resp = client
        .get(&url)
        .header("X-Subscription-Token", api_key)
        .send()
        .await
        .map_err(|e| ProviderError::Retryable(e.to_string()))?;

    check_provider_status(&resp).await?;

    let body: Value = resp
        .json()
        .await
        .map_err(|e| ProviderError::Fatal(e.to_string()))?;
    let mut outcome = SearchOutcome::default();
    if let Some(arr) = body
        .get("web")
        .and_then(|w| w.get("results"))
        .and_then(|r| r.as_array())
    {
        for item in arr.iter().take(max) {
            match (
                item.get("title").and_then(|v| v.as_str()),
                item.get("url").and_then(|v| v.as_str()),
            ) {
                (Some(title), Some(url)) => outcome.results.push(SearchResult {
                    title: title.to_string(),
                    url: url.to_string(),
                    snippet: item
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string(),
                }),
                _ => outcome.dropped += 1,
            }
        }
    }

    Ok(outcome)
}

async fn search_tavily(
    client: &reqwest::Client,
    policy: &NetworkPolicy,
    query: &str,
    max: usize,
    api_key: &str,
) -> Result<SearchOutcome, ProviderError> {
    let body = json!({
        "query": query,
        "max_results": max,
        "include_answer": false
    });

    let endpoint = "https://api.tavily.com/search";
    policy
        .validate_url(endpoint)
        .map_err(|e| ProviderError::Fatal(e.to_string()))?;
    let resp = client
        .post(endpoint)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&body)
        .send()
        .await
        .map_err(|e| ProviderError::Retryable(e.to_string()))?;

    check_provider_status(&resp).await?;

    let resp_json: Value = resp
        .json()
        .await
        .map_err(|e| ProviderError::Fatal(e.to_string()))?;
    let mut outcome = SearchOutcome::default();
    if let Some(arr) = resp_json.get("results").and_then(|r| r.as_array()) {
        for item in arr.iter().take(max) {
            match (
                item.get("title").and_then(|v| v.as_str()),
                item.get("url").and_then(|v| v.as_str()),
            ) {
                (Some(title), Some(url)) => outcome.results.push(SearchResult {
                    title: title.to_string(),
                    url: url.to_string(),
                    snippet: item
                        .get("content")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string(),
                }),
                _ => outcome.dropped += 1,
            }
        }
    }

    Ok(outcome)
}

async fn search_ddg(
    client: &reqwest::Client,
    policy: &NetworkPolicy,
    query: &str,
    max: usize,
) -> Result<SearchOutcome, ProviderError> {
    let url = format!("https://html.duckduckgo.com/html/?q={}", url_encode(query));
    policy
        .validate_url(&url)
        .map_err(|e| ProviderError::Fatal(e.to_string()))?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| ProviderError::Retryable(e.to_string()))?;

    check_provider_status(&resp).await?;

    let html = resp
        .text()
        .await
        .map_err(|e| ProviderError::Fatal(e.to_string()))?;
    let document = scraper::Html::parse_document(&html);
    let result_selector =
        scraper::Selector::parse(".result").map_err(|e| ProviderError::Fatal(e.to_string()))?;

    let title_selector = scraper::Selector::parse(".result__title a")
        .map_err(|e| ProviderError::Fatal(e.to_string()))?;
    let snippet_selector = scraper::Selector::parse(".result__snippet")
        .map_err(|e| ProviderError::Fatal(e.to_string()))?;

    let mut results = Vec::new();
    for result_el in document.select(&result_selector).take(max) {
        let title = result_el
            .select(&title_selector)
            .next()
            .map(|el| el.text().collect::<String>())
            .unwrap_or_default()
            .trim()
            .to_string();

        let url = result_el
            .select(&title_selector)
            .next()
            .and_then(|el| el.value().attr("href"))
            .map(|h| extract_uddg(h).unwrap_or_else(|| h.to_string()))
            .unwrap_or_default();

        let snippet = result_el
            .select(&snippet_selector)
            .next()
            .map(|el| el.text().collect::<String>())
            .unwrap_or_default()
            .trim()
            .to_string();

        if !title.is_empty() && !url.is_empty() {
            results.push(SearchResult {
                title,
                url,
                snippet,
            });
        }
    }

    Ok(SearchOutcome {
        results,
        dropped: 0,
    })
}

#[async_trait]
impl Tool for WebSearchTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: NAME.to_string(),
            description: DESCRIPTION.to_string(),
            args_schema: ARGS_SCHEMA.clone(),
            result_schema: RESULT_SCHEMA.clone(),
        }
    }

    async fn invoke(&self, args: Value) -> Result<Value, KernelError> {
        let query = args["query"].as_str().ok_or_else(|| {
            KernelError::InvalidArgument("Missing or invalid 'query' field".into())
        })?;

        let max = args
            .get("max_results")
            .and_then(|v| v.as_i64())
            .unwrap_or(DEFAULT_MAX_RESULTS as i64)
            .clamp(1, MAX_RESULTS_LIMIT as i64) as usize;

        let config = zen_core::config::load_config().ok();
        let configured = config.as_ref().map(|c| &c.web_search);
        let override_provider = configured
            .and_then(|c| c.default_provider.as_deref())
            .unwrap_or("");
        let brave_key = configured
            .and_then(|c| c.api_key_brave.as_deref())
            .map(|k| k.to_string())
            .filter(|k| !k.is_empty())
            .or_else(|| std::env::var("BRAVE_SEARCH_API_KEY").ok());
        let tavily_key = configured
            .and_then(|c| c.api_key_tavily.as_deref())
            .map(|k| k.to_string())
            .filter(|k| !k.is_empty())
            .or_else(|| std::env::var("TAVILY_API_KEY").ok());

        let (outcome, provider) = match override_provider {
            "brave" => {
                let key = brave_key.as_deref().ok_or_else(|| {
                    KernelError::ToolFailed("API key not configured for provider 'brave'".into())
                })?;
                let o = retry_with_backoff(3, ProviderError::retryable, || {
                    search_brave(&self.client, &self.network_policy, query, max, key)
                })
                .await
                .map_err(|e| KernelError::ToolFailed(e.into_message()))?;
                (o, "brave")
            }
            "tavily" => {
                let key = tavily_key.as_deref().ok_or_else(|| {
                    KernelError::ToolFailed("API key not configured for provider 'tavily'".into())
                })?;
                let o = retry_with_backoff(3, ProviderError::retryable, || {
                    search_tavily(&self.client, &self.network_policy, query, max, key)
                })
                .await
                .map_err(|e| KernelError::ToolFailed(e.into_message()))?;
                (o, "tavily")
            }
            _ => {
                let brave_try = brave_key.as_deref().map(|key| {
                    retry_with_backoff(3, ProviderError::retryable, || {
                        search_brave(&self.client, &self.network_policy, query, max, key)
                    })
                });
                let tavily_try = tavily_key.as_deref().map(|key| {
                    retry_with_backoff(3, ProviderError::retryable, || {
                        search_tavily(&self.client, &self.network_policy, query, max, key)
                    })
                });

                let mut selected: Option<(&str, SearchOutcome)> = None;
                if let Some(try_brave) = brave_try
                    && let Ok(o) = try_brave.await
                {
                    selected = Some(("brave", o));
                }
                if selected.is_none()
                    && let Some(try_tavily) = tavily_try
                    && let Ok(o) = try_tavily.await
                {
                    selected = Some(("tavily", o));
                }
                if selected.is_none() {
                    let o = retry_with_backoff(3, ProviderError::retryable, || {
                        search_ddg(&self.client, &self.network_policy, query, max)
                    })
                    .await
                    .map_err(|e| KernelError::ToolFailed(e.into_message()))?;
                    selected = Some(("duckduckgo", o));
                }

                let (p, o) = selected.expect("fallback always selects a provider");
                (o, p)
            }
        };

        let results = outcome.results;
        let dropped = outcome.dropped;
        if dropped > 0 {
            warn!(provider, dropped, "dropped malformed search results");
        }

        let count = results.len();
        let results_json: Vec<Value> = results
            .iter()
            .map(|r| json!({ "title": r.title, "url": r.url, "snippet": r.snippet }))
            .collect();

        let message = if count == 0 {
            Some(format!(
                "No results found for query '{query}'; try broadening or rephrasing the query"
            ))
        } else {
            None
        };

        Ok(json!({
            "query": query,
            "results": results_json,
            "count": count,
            "dropped_results": dropped,
            "provider": provider,
            "message": message
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_retry_after() {
        assert_eq!(parse_retry_after(Some("30")), 30);
        assert_eq!(parse_retry_after(Some("abc")), 60);
        assert_eq!(parse_retry_after(None), 60);
    }

    #[test]
    fn test_percent_decode() {
        assert_eq!(percent_decode("hello"), "hello");
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(
            percent_decode("https%3A%2F%2Fexample.com%2Fpath"),
            "https://example.com/path"
        );
        assert_eq!(percent_decode("%E4%B8%AD"), "中");
        assert_eq!(percent_decode("trailing%2"), "trailing%2");
    }

    #[test]
    fn test_extract_uddg() {
        assert_eq!(
            extract_uddg("//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fp&rut=abc"),
            Some("https://example.com/p".to_string())
        );
        assert_eq!(
            extract_uddg("/l/?uddg=https%3A%2F%2Ffoo.bar&a=1"),
            Some("https://foo.bar".to_string())
        );
        assert_eq!(extract_uddg("https://example.com/no-redirect"), None);
        assert_eq!(extract_uddg("//duckduckgo.com/l/?uddg=&rut=x"), None);
    }

    #[test]
    fn test_max_results_clamp() {
        let clamp = |v: i64| v.clamp(1, MAX_RESULTS_LIMIT as i64) as usize;
        assert_eq!(clamp(0), 1);
        assert_eq!(clamp(5), 5);
        assert_eq!(clamp(50), 50);
        assert_eq!(clamp(1000), 50);
        assert_eq!(clamp(-3), 1);
    }

    #[test]
    fn provider_endpoints_pass_default_policy() {
        let policy = NetworkPolicy::with_allow_hosts(vec!["localhost".into(), "127.0.0.1".into()]);
        for url in [
            "https://api.search.brave.com/res/v1/web/search?q=x&count=5",
            "https://api.tavily.com/search",
            "https://html.duckduckgo.com/html/?q=x",
        ] {
            assert!(policy.validate_url(url).is_ok(), "{url}");
        }
    }
}
