use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::sync::Mutex;
use tracing::info;

const TOKEN_REFRESH_BUFFER: i64 = 3600;

struct TokenState {
    access_token: String,
    expires_at: DateTime<Utc>,
}

pub struct QqBotAuth {
    app_id: String,
    token: Mutex<Option<TokenState>>,
}

impl QqBotAuth {
    pub fn new(app_id: String) -> Self {
        Self {
            app_id,
            token: Mutex::new(None),
        }
    }

    pub async fn get_token(&self) -> Result<String> {
        let need_refresh = {
            let guard = self.token.lock().unwrap();
            match &*guard {
                Some(s) => {
                    let now = Utc::now();
                    now + chrono::Duration::seconds(TOKEN_REFRESH_BUFFER) >= s.expires_at
                }
                None => true,
            }
        };

        if need_refresh {
            self.refresh_token().await?;
        }

        let guard = self.token.lock().unwrap();
        guard
            .as_ref()
            .map(|s| s.access_token.clone())
            .context("no token available")
    }

    pub async fn refresh_token(&self) -> Result<()> {
        info!("refreshing QQ Bot access token");

        let resp = reqwest::Client::new()
            .post("https://api.sgroup.qq.com/users/me")
            .json(&serde_json::json!({
                "appid": self.app_id,
            }))
            .send()
            .await?;

        let json: serde_json::Value = resp.json().await?;
        let access_token = json["access_token"]
            .as_str()
            .context("missing access_token in response")?
            .to_string();

        let expires_in = json["expires_in"].as_i64().unwrap_or(7200);
        let expires_at = Utc::now() + chrono::Duration::seconds(expires_in);

        let mut guard = self.token.lock().unwrap();
        *guard = Some(TokenState {
            access_token,
            expires_at,
        });

        Ok(())
    }

    #[inline]
    pub fn app_id(&self) -> &str {
        &self.app_id
    }
}
