use anyhow::{Context, Result};
use futures_util::stream::StreamExt;
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};
use tokio_tungstenite::connect_async;
use tracing::{error, info};

const QQ_WS_URL: &str = "wss://api.sgroup.qq.com/websocket";
const DEFAULT_HEARTBEAT_SEC: u64 = 30;

#[derive(Debug, Clone, Deserialize)]
pub struct WsFrame {
    op: u32,
    _d: Option<serde_json::Value>,
    t: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReadyPayload {
    #[allow(dead_code)]
    #[serde(rename = "version")]
    _version: u32,
    #[allow(dead_code)]
    #[serde(rename = "session_id")]
    _session_id: String,
}

#[derive(Debug, Clone)]
pub struct QqBotConfig {
    pub token: String,
    pub app_id: String,
    pub heartbeat_sec: u64,
}

impl Default for QqBotConfig {
    fn default() -> Self {
        Self {
            token: String::new(),
            app_id: String::new(),
            heartbeat_sec: DEFAULT_HEARTBEAT_SEC,
        }
    }
}

pub struct QqBotClient {
    config: Arc<QqBotConfig>,
    event_tx: broadcast::Sender<WsFrame>,
    running: Arc<Mutex<bool>>,
}

impl QqBotClient {
    pub fn new(config: QqBotConfig) -> Self {
        let config = Arc::new(config);
        let (event_tx, _) = broadcast::channel::<WsFrame>(256);
        Self {
            config,
            event_tx,
            running: Arc::new(Mutex::new(false)),
        }
    }

    #[inline]
    pub fn subscribe(&self) -> broadcast::Receiver<WsFrame> {
        self.event_tx.subscribe()
    }

    pub async fn connect(&self) -> Result<()> {
        let url = format!("{}?app_id={}", QQ_WS_URL, self.config.app_id);
        info!("connecting to QQ Bot WebSocket: {url}");

        let (ws_stream, _) = connect_async(&url)
            .await
            .with_context(|| "WebSocket connection failed")?;

        let (_, mut read) = ws_stream.split();
        let event_tx = self.event_tx.clone();
        let config = self.config.clone();
        let heartbeat_sec = config.heartbeat_sec;
        let running = self.running.clone();

        {
            let mut l = running.lock().await;
            *l = true;
        }

        let running_stop = running.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(heartbeat_sec)).await;
                let still_running = {
                    let l = running_stop.lock().await;
                    *l
                };
                if !still_running {
                    break;
                }
                info!("QQ Bot heartbeat sent");
            }
        });

        tokio::spawn(async move {
            while let Some(frame) = read.next().await {
                match frame {
                    Ok(msg) => {
                        if let Ok(text) = msg.to_text()
                            && let Ok(ws) = serde_json::from_str::<WsFrame>(text)
                        {
                            if ws.op == 0
                                && ws.t.as_deref() == Some("READY")
                                && let Some(d) = &ws._d
                                && let Ok(ready) = serde_json::from_value::<ReadyPayload>(d.clone())
                            {
                                info!("QQ Bot ready, session={}", ready._session_id);
                            }
                            let _ = event_tx.send(ws);
                        }
                    }
                    Err(e) => {
                        error!("WebSocket read error: {e}");
                        break;
                    }
                }
            }
        });

        Ok(())
    }

    pub async fn start(&self) -> Result<()> {
        self.connect().await
    }

    pub async fn stop(&self) {
        let mut l = self.running.lock().await;
        *l = false;
        info!("QQ Bot client stopped");
    }
}
