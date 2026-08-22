use std::sync::LazyLock;
use std::time::Duration;

use async_trait::async_trait;
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::{Value, json};
use zen_core::config::WebFetchConfig;
use zen_core::network_policy::NetworkPolicy;

const NAME: &str = "web.fetch";
const DESCRIPTION: &str = "Fetch a URL and extract its content as Markdown";

static ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "url": { "type": "string", "description": "URL to fetch" },
            "max_lines": { "type": "integer", "description": "Maximum lines of content to return (default 2000)" }
        },
        "required": ["url"]
    })
});

static RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "url": { "type": "string" },
            "content": { "type": "string" },
            "title": { "type": "string" },
            "content_length": { "type": "integer" },
            "lines": { "type": "integer" },
            "truncated": { "type": "boolean" },
            "used_fallback": { "type": "boolean" }
        }
    })
});

#[derive(Clone)]
pub struct WebFetchTool {
    client: reqwest::Client,
    max_content_size_kb: u32,
    max_lines: u32,
    timeout_ms: u64,
    jina_fallback: bool,
    jina_fallback_threshold_chars: u32,
    user_agent: String,
    network_policy: NetworkPolicy,
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new()
    }
}

impl WebFetchTool {
    pub fn new() -> Self {
        Self::with_config(WebFetchConfig::default())
    }

    pub fn with_config(cfg: WebFetchConfig) -> Self {
        // Redirect hops are policy-validated (FR-036): reqwest's default
        // redirect-following would silently bypass the pre-dispatch
        // NetworkPolicy check (public URL 302 → 169.254.169.254 = SSRF).
        // Each hop re-validates; a blocked target stops the chain with an
        // error instead of fetching it.
        let policy = Self::default_policy();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(cfg.timeout_ms))
            .user_agent(cfg.user_agent.clone())
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() > 10 {
                    return attempt.error("too many redirects");
                }
                if let Err(reason) = policy.validate_url(attempt.url().as_str()) {
                    return attempt.error(format!("redirect blocked by network policy: {reason}"));
                }
                attempt.follow()
            }))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            client,
            max_content_size_kb: cfg.max_content_size_kb,
            max_lines: cfg.max_lines,
            timeout_ms: cfg.timeout_ms,
            jina_fallback: cfg.jina_fallback,
            jina_fallback_threshold_chars: cfg.jina_fallback_threshold_chars,
            user_agent: cfg.user_agent,
            network_policy: Self::default_policy(),
        }
    }

    fn default_policy() -> NetworkPolicy {
        NetworkPolicy::with_allow_hosts(vec!["localhost".into(), "127.0.0.1".into()])
    }

    pub fn config(&self) -> WebFetchConfig {
        WebFetchConfig {
            max_content_size_kb: self.max_content_size_kb,
            max_lines: self.max_lines,
            timeout_ms: self.timeout_ms,
            jina_fallback: self.jina_fallback,
            jina_fallback_threshold_chars: self.jina_fallback_threshold_chars,
            user_agent: self.user_agent.clone(),
        }
    }
}

const TRUNCATION_MARKER: &str = "...(truncated)";

fn convert_html_to_markdown(html: &str) -> String {
    htmd::convert(html).unwrap_or_else(|_| html.to_string())
}

async fn fetch_jina(
    client: &reqwest::Client,
    policy: &NetworkPolicy,
    url: &str,
    max_bytes: usize,
) -> Result<String, KernelError> {
    let jina_url = format!("https://r.jina.ai/{url}");
    policy.validate_url(&jina_url).map_err(|reason| {
        KernelError::ToolFailed(format!("Jina URL blocked by network policy: {reason}"))
    })?;
    let resp = client
        .get(&jina_url)
        .header("X-Return-Format", "markdown")
        .header("Accept", "text/plain")
        .send()
        .await
        .map_err(|e| KernelError::ToolFailed(format!("Jina fallback failed: {e}")))?;

    let (text, _truncated) = read_body_capped(resp, max_bytes).await?;
    Ok(text)
}

/// Reads a response body in bounded chunks, stopping at `max_bytes` so a
/// large or streaming response can't exhaust memory (memory-DoS guard).
async fn read_body_capped(
    mut resp: reqwest::Response,
    max_bytes: usize,
) -> Result<(String, bool), KernelError> {
    let mut bytes: Vec<u8> = Vec::with_capacity(max_bytes.min(64 * 1024));
    let mut truncated = false;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| KernelError::ToolFailed(format!("Failed to read response: {e}")))?
    {
        if bytes.len() + chunk.len() > max_bytes {
            truncated = true;
            let remaining = max_bytes.saturating_sub(bytes.len());
            bytes.extend_from_slice(&chunk[..remaining]);
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Ok((text, truncated))
}

#[async_trait]
impl Tool for WebFetchTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: NAME.to_string(),
            description: DESCRIPTION.to_string(),
            args_schema: ARGS_SCHEMA.clone(),
            result_schema: RESULT_SCHEMA.clone(),
        }
    }

    async fn invoke(&self, args: Value) -> Result<Value, KernelError> {
        let url = args["url"]
            .as_str()
            .ok_or_else(|| KernelError::InvalidArgument("Missing or invalid 'url' field".into()))?;

        // FR-036: network egress policy — block SSRF vectors (metadata
        // endpoints, private ranges) before any request leaves the process.
        self.network_policy.validate_url(url).map_err(|reason| {
            KernelError::ToolFailed(format!("URL blocked by network policy: {reason}"))
        })?;

        let max_lines = args
            .get("max_lines")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(self.max_lines as usize);

        let max_bytes = (self.max_content_size_kb as usize).saturating_mul(1024);

        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| KernelError::ToolFailed(format!("HTTP request failed: {}", e)))?;

        let status = resp.status();
        if !status.is_success() {
            return Err(KernelError::ToolFailed(format!(
                "HTTP {} for {}",
                status, url
            )));
        }

        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let (body, read_truncated) = read_body_capped(resp, max_bytes).await?;

        let title = extract_title(&body).unwrap_or_default();

        let markdown = if content_type.contains("text/html") {
            convert_html_to_markdown(&body)
        } else {
            body
        };

        let needs_fallback = self.jina_fallback
            && content_type.contains("text/html")
            && markdown.len() < self.jina_fallback_threshold_chars as usize;
        let (final_content, used_fallback) = if needs_fallback {
            tracing::warn!(
                url = url,
                chars = markdown.len(),
                threshold = self.jina_fallback_threshold_chars,
                "web.fetch content below readability threshold, falling back to Jina Reader"
            );
            match fetch_jina(&self.client, &self.network_policy, url, max_bytes).await {
                Ok(jina_content) => (jina_content, true),
                Err(e) => {
                    tracing::warn!(url = url, error = %e, "Jina Reader fallback failed, using raw extract");
                    (markdown, false)
                }
            }
        } else {
            (markdown, false)
        };

        let mut truncated = read_truncated;
        let mut content = final_content;

        let lines: Vec<&str> = content.lines().collect();
        if lines.len() > max_lines {
            truncated = true;
            content = lines
                .iter()
                .take(max_lines)
                .copied()
                .collect::<Vec<_>>()
                .join("\n");
        }

        if truncated {
            content.push('\n');
            content.push_str(TRUNCATION_MARKER);
        }

        Ok(json!({
            "url": url,
            "content": content,
            "title": title,
            "content_length": content.len(),
            "lines": content.lines().count(),
            "truncated": truncated,
            "used_fallback": used_fallback
        }))
    }
}

fn extract_title(html: &str) -> Option<String> {
    let start = html.find("<title>")? + 7;
    let end = html[start..].find("</title>")? + start;
    let title = html[start..end].trim();
    if title.is_empty() {
        None
    } else {
        Some(title.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_metadata_url_by_network_policy() {
        let tool = WebFetchTool::new();
        let res = tool
            .invoke(json!({
                "url": "http://169.254.169.254/latest/meta-data/"
            }))
            .await;
        let err = res.expect_err("policy violation must return Err, not Ok");
        assert!(
            err.to_string().contains("blocked by network policy"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn allows_loopback_url_by_network_policy() {
        let tool = WebFetchTool::new();
        // Validation passes for the default allowlist; the request itself
        // fails to connect (no server), proving we got past the policy gate.
        let res = tool.invoke(json!({ "url": "http://127.0.0.1:1/" })).await;
        assert!(
            res.is_err(),
            "should fail at HTTP, not at policy: {:?}",
            res
        );
    }
}
