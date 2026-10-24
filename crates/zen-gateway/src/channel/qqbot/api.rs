//! QQ official-bot v2 REST send paths.
//!
//! PURPOSE: Passive replies (quoting the inbound `msg_id`, 5min/60min
//! windows) and active messages for group + C2C chats via
//! `POST {api_base}/v2/{groups|users}/{openid}/messages`.
//!
//! ERRORS: Non-2xx responses include status + body so the carrier can
//! detect passive-window expiry and fall back to an active send.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tracing::{error, info};

use super::auth::QqBotAuth;

/// Ceiling for one REST send (token fetch + POST); passive windows are
/// minutes wide — a hung request must never pin a chat worker that long.
const API_TIMEOUT: Duration = Duration::from_secs(30);

/// Char-boundary-safe cap for response bodies carried in error logs
/// (CSO audit: upstream bodies may echo user message content). Mirrors
/// the per-crate `truncate_chars` precedent (server/hosting.rs, adapter.rs).
const ERROR_BODY_MAX_CHARS: usize = 200;

fn truncate_body(s: &str) -> String {
    match s.char_indices().nth(ERROR_BODY_MAX_CHARS) {
        Some((at, _)) => s[..at].to_string(),
        None => s.to_string(),
    }
}

pub struct QqBotApi {
    auth: Arc<QqBotAuth>,
    api_base: String,
    http: reqwest::Client,
}

impl QqBotApi {
    pub fn new(auth: Arc<QqBotAuth>, api_base: String) -> Self {
        Self {
            auth,
            api_base,
            http: reqwest::Client::builder()
                .no_proxy()
                .connect_timeout(Duration::from_secs(10))
                .timeout(API_TIMEOUT)
                .build()
                .expect("static reqwest client config"),
        }
    }

    /// Passive/active reply in a group chat (`group_openid`).
    ///
    /// # Errors
    /// Token fetch failure or non-2xx (status + body in the message).
    pub async fn send_group_message(
        &self,
        group_openid: &str,
        content: &str,
        msg_id: Option<&str>,
        msg_seq: u32,
    ) -> Result<serde_json::Value> {
        self.send("groups", group_openid, content, msg_id, msg_seq)
            .await
    }

    /// Passive/active reply in a C2C chat (`user_openid`).
    ///
    /// # Errors
    /// Token fetch failure or non-2xx (status + body in the message).
    pub async fn send_c2c_message(
        &self,
        user_openid: &str,
        content: &str,
        msg_id: Option<&str>,
        msg_seq: u32,
    ) -> Result<serde_json::Value> {
        self.send("users", user_openid, content, msg_id, msg_seq)
            .await
    }

    async fn send(
        &self,
        kind: &str,
        openid: &str,
        content: &str,
        msg_id: Option<&str>,
        msg_seq: u32,
    ) -> Result<serde_json::Value> {
        let token = self.auth.get_token().await?;
        let url = format!("{}/v2/{kind}/{openid}/messages", self.api_base);
        info!(kind, openid, passive = msg_id.is_some(), "qq api send");
        let mut body = serde_json::json!({"msg_type": 0, "content": content, "msg_seq": msg_seq});
        if let Some(id) = msg_id {
            body["msg_id"] = serde_json::Value::String(id.to_string());
        }
        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("QQBot {token}"))
            .json(&body)
            .send()
            .await
            .with_context(|| format!("send to {url}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            error!(status = %status, body = %truncate_body(&text), "qq api send failed");
            bail!("qq api {status}: {text}");
        }
        serde_json::from_str(&text).with_context(|| format!("parse {text}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_body_caps_at_char_boundary() {
        let long = "a".repeat(500);
        assert_eq!(truncate_body(&long).chars().count(), ERROR_BODY_MAX_CHARS);
        let cjk: String = "中".repeat(300);
        let cut = truncate_body(&cjk);
        assert_eq!(cut.chars().count(), 200);
        assert!(cut.is_char_boundary(cut.len()));
        assert_eq!(truncate_body("ok"), "ok");
        assert_eq!(truncate_body(""), "");
    }
}
