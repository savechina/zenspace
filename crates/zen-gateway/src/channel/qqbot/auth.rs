//! QQ official-bot OAuth2 access-token management (v2 API).
//!
//! PURPOSE: Keeps one cached `app_access_token` alive for the WS
//! gateway and REST send paths; refreshes 60s before expiry via
//! `POST {token_url}` with `{appId, clientSecret}` (official
//! `bots.qq.com/app/getAppAccessToken`; TTL 7200s).
//!
//! ERRORS: HTTP failure, non-JSON body, or missing `access_token` —
//! callers treat as transient (next call retries).

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::sync::Mutex;
use tracing::info;

/// Refresh this many seconds before `expires_at` (official 60s overlap
/// window keeps the old token valid during rollover).
const REFRESH_BUFFER_SECS: i64 = 60;

struct TokenState {
    access_token: String,
    expires_at: DateTime<Utc>,
}

pub struct QqBotAuth {
    app_id: String,
    client_secret: String,
    token_url: String,
    http: reqwest::Client,
    token: Mutex<Option<TokenState>>,
}

impl QqBotAuth {
    pub fn new(app_id: String, client_secret: String, token_url: String) -> Self {
        Self {
            app_id,
            client_secret,
            token_url,
            http: reqwest::Client::builder()
                .no_proxy()
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("static reqwest client config"),
            token: Mutex::new(None),
        }
    }

    /// Returns a valid access token, refreshing first when inside the
    /// 60s pre-expiry window.
    ///
    /// # Errors
    /// Refresh failure (network or malformed response) with context.
    pub async fn get_token(&self) -> Result<String> {
        let need_refresh = {
            let guard = self.token.lock().unwrap();
            match guard.as_ref() {
                Some(s) => {
                    Utc::now() + chrono::Duration::seconds(REFRESH_BUFFER_SECS) >= s.expires_at
                }
                None => true,
            }
        };
        if need_refresh {
            self.refresh_token().await?;
        }
        self.token
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.access_token.clone())
            .context("token absent after successful refresh")
    }

    /// Fetches and caches a fresh token.
    ///
    /// # Errors
    /// HTTP failure, non-JSON body, missing `access_token`, or
    /// unparseable `expires_in` (official API returns a STRING — both
    /// string and number forms accepted).
    pub async fn refresh_token(&self) -> Result<()> {
        info!("refreshing QQ bot access token");
        let resp = self
            .http
            .post(&self.token_url)
            .json(&serde_json::json!({
                "appId": self.app_id,
                "clientSecret": self.client_secret,
            }))
            .send()
            .await
            .context("qq token request failed")?;
        let json: serde_json::Value = resp
            .error_for_status()
            .context("qq token endpoint returned error status")?
            .json()
            .await
            .context("qq token response not JSON")?;
        let access_token = json
            .get("access_token")
            .and_then(serde_json::Value::as_str)
            .context("qq token response missing access_token")?
            .to_string();
        let expires_in = match json.get("expires_in") {
            Some(serde_json::Value::String(s)) => s
                .parse::<i64>()
                .context("qq token expires_in string not numeric")?,
            Some(serde_json::Value::Number(n)) => n
                .as_i64()
                .context("qq token expires_in number out of range")?,
            _ => 7200,
        };
        *self.token.lock().unwrap() = Some(TokenState {
            access_token,
            expires_at: Utc::now() + chrono::Duration::seconds(expires_in),
        });
        Ok(())
    }
}
