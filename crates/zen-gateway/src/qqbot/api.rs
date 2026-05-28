use anyhow::Result;
use tracing::{error, info};

use super::auth::QqBotAuth;

pub struct QqBotApi {
    auth: QqBotAuth,
    base_url: String,
}

impl QqBotApi {
    pub fn new(auth: QqBotAuth) -> Self {
        Self {
            auth,
            base_url: "https://api.sgroup.qq.com".to_string(),
        }
    }

    pub async fn send_message(&self, channel_id: &str, content: &str) -> Result<()> {
        let token = self.auth.get_token().await?;
        let url = format!(
            "{base}/v2/channels/{channel_id}/messages",
            base = self.base_url
        );

        info!("POST message to channel {channel_id}");

        let resp = reqwest::Client::new()
            .post(&url)
            .header("Authorization", format!("QQBot {token}"))
            .json(&serde_json::json!({
                "content": content,
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let body: serde_json::Value = resp.json().await.unwrap_or_default();
            error!("send_message failed: {body}");
            anyhow::bail!("API error: {body}");
        }

        Ok(())
    }

    #[inline]
    pub fn auth(&self) -> &QqBotAuth {
        &self.auth
    }
}
