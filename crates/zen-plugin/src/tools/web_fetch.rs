use std::sync::LazyLock;
use std::time::Duration;

use async_trait::async_trait;
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::{Value, json};

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
    jina_fallback_threshold_chars: usize,
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new()
    }
}

impl WebFetchTool {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("zen-agent/1.0")
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            client,
            jina_fallback_threshold_chars: 500,
        }
    }
}

const DEFAULT_MAX_LINES: usize = 2000;
const MAX_CONTENT_BYTES: usize = 51200;

fn convert_html_to_markdown(html: &str) -> String {
    htmd::convert(html).unwrap_or_else(|_| html.to_string())
}

async fn fetch_jina(client: &reqwest::Client, url: &str) -> Result<String, KernelError> {
    let jina_url = format!("https://r.jina.ai/{}", url);
    let resp = client
        .get(&jina_url)
        .header("X-Return-Format", "markdown")
        .header("Accept", "text/plain")
        .send()
        .await
        .map_err(|e| KernelError::ToolFailed(format!("Jina fallback failed: {}", e)))?;

    resp.text()
        .await
        .map_err(|e| KernelError::ToolFailed(format!("Jina read failed: {}", e)))
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

        let max_lines = args
            .get("max_lines")
            .and_then(|v| v.as_i64())
            .unwrap_or(DEFAULT_MAX_LINES as i64) as usize;

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

        let body = resp
            .text()
            .await
            .map_err(|e| KernelError::ToolFailed(format!("Failed to read response: {}", e)))?;

        let title = extract_title(&body).unwrap_or_default();

        let markdown = if content_type.contains("text/html") {
            convert_html_to_markdown(&body)
        } else {
            body
        };

        let (final_content, used_fallback) = if markdown.len() < self.jina_fallback_threshold_chars
            && content_type.contains("text/html")
        {
            match fetch_jina(&self.client, url).await {
                Ok(jina_content) => (jina_content, true),
                Err(_) => (markdown, false),
            }
        } else {
            (markdown, false)
        };

        let truncated_bytes = final_content.len().min(MAX_CONTENT_BYTES);
        let mut truncated = final_content.len() > MAX_CONTENT_BYTES;
        let content = if truncated {
            // Byte-slicing a String can panic when the boundary splits a
            // multi-byte UTF-8 char; floor_char_boundary lands on a safe edge.
            let end = final_content.floor_char_boundary(truncated_bytes);
            final_content[..end].to_string()
        } else {
            final_content
        };

        let lines: Vec<&str> = content.lines().collect();
        let line_count = lines.len();
        if line_count > max_lines {
            truncated = true;
        }
        let content: String = lines
            .iter()
            .take(max_lines)
            .copied()
            .collect::<Vec<_>>()
            .join("\n");

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
