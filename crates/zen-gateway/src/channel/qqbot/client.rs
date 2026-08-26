//! QQ official-bot WebSocket gateway client (v2 API).
//!
//! PURPOSE: Full op-code state machine over `wss://{ws_url}` —
//! Hello(10) → Identify(2, token `"QQBot {access_token}"`, intents,
//! shard [0,1]) → READY; heartbeat(1) carrying last seq with ACK(11)
//! tracking; Resume(6) on reconnect; Reconnect(7) and
//! InvalidSession(9) handling with exponential backoff (1s→60s cap,
//! equal-window jittered; reset only after ≥60s of stable uptime so a
//! connect-then-drop crash loop keeps escalating). Dispatch frames
//! (op 0) surface as [`QqWsEvent`].
//!
//! ERRORS: `run` returns Err only for fatal misuse; connection drops
//! retry internally until `shutdown` fires (then Ok(())).

use anyhow::{Context, Result};
use futures_util::SinkExt;
use futures_util::stream::{SplitSink, SplitStream, StreamExt};
use serde::Deserialize;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tracing::{error, info, warn};

/// Backoff floor between reconnect attempts.
const BACKOFF_MIN: Duration = Duration::from_secs(1);
/// Backoff ceiling between reconnect attempts.
const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// A connection must survive this long for its clean drop to reset the
/// reconnect backoff — a connect-then-drop loop keeps escalating.
const MIN_STABLE_UPTIME: Duration = Duration::from_secs(60);
/// Cooldown after InvalidSession(9) before a fresh Identify.
const INVALID_SESSION_COOLDOWN: Duration = Duration::from_secs(3);
/// Heartbeats allowed to go un-ACKed before forcing a reconnect.
const MAX_MISSED_ACKS: u32 = 2;

/// Equal-window jitter on a reconnect pause: lower half kept, upper
/// half randomized from wall-clock nanos (no rand dependency — this
/// only thunders-herd-thins, not cryptographic).
fn jitter(pause: Duration) -> Duration {
    let ms = pause.as_millis().max(2);
    let half = (ms / 2).max(1) as u64;
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::from(d.subsec_nanos()))
        .unwrap_or(0);
    let base = u64::try_from(ms).unwrap_or(u64::MAX) / 2;
    Duration::from_millis(base + seed % half)
}

/// One op-0 dispatch event forwarded to the channel layer.
#[derive(Debug, Clone)]
pub struct QqWsEvent {
    pub kind: QqWsEventKind,
    pub raw: serde_json::Value,
}

/// Event classification derived from the dispatch `t` field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QqWsEventKind {
    Ready { session_id: String },
    GroupAtMessage,
    C2cMessage,
    Other(String),
}

impl QqWsEventKind {
    fn from_dispatch(t: &str, d: &serde_json::Value) -> Self {
        use QqWsEventKind::*;
        match t {
            "READY" => Ready {
                session_id: d
                    .get("session_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            },
            "GROUP_AT_MESSAGE_CREATE" => GroupAtMessage,
            "C2C_MESSAGE_CREATE" => C2cMessage,
            other => Other(other.to_string()),
        }
    }
}

/// Raw WS frame envelope (every op).
#[derive(Debug, Clone, Deserialize)]
pub struct WsFrame {
    pub op: u32,
    pub s: Option<u64>,
    pub t: Option<String>,
    pub d: Option<serde_json::Value>,
}

pub struct QqBotClient {
    auth: Arc<super::auth::QqBotAuth>,
    ws_url: String,
    intents: u32,
}

impl QqBotClient {
    pub fn new(auth: Arc<super::auth::QqBotAuth>, ws_url: String, intents: u32) -> Self {
        Self {
            auth,
            ws_url,
            intents,
        }
    }

    /// Runs connect→identify→operate cycles until `shutdown` fires.
    ///
    /// # Errors
    /// Only an unreachable platform URL (connect_async exhausting the
    /// OS) or a poisoned auth flow aborts; transient drops loop.
    pub async fn run(
        &self,
        events: mpsc::Sender<QqWsEvent>,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        let mut backoff = BACKOFF_MIN;
        let mut resume: Option<ResumeState> = None;
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }
            let connected_at = tokio::time::Instant::now();
            match self.run_once(&events, resume.take(), &mut shutdown).await {
                RunOutcome::Shutdown => return Ok(()),
                RunOutcome::Dropped { resume: next, kind } => {
                    let pause = match kind {
                        DropKind::ReadyDrop => {
                            if connected_at.elapsed() >= MIN_STABLE_UPTIME {
                                backoff = BACKOFF_MIN;
                            }
                            let pause = jitter(backoff);
                            warn!(
                                pause_secs = pause.as_secs(),
                                uptime_secs = connected_at.elapsed().as_secs(),
                                "qq gateway connection dropped; backing off"
                            );
                            pause
                        }
                        DropKind::ConnectFail => {
                            let pause = jitter(backoff);
                            backoff = (backoff * 2).min(BACKOFF_MAX);
                            warn!(
                                pause_secs = pause.as_secs(),
                                "qq gateway connect failed; backing off"
                            );
                            pause
                        }
                        DropKind::InvalidSession => {
                            warn!("qq gateway invalid session; cooling down before fresh identify");
                            INVALID_SESSION_COOLDOWN
                        }
                    };
                    resume = if matches!(kind, DropKind::InvalidSession) {
                        None
                    } else {
                        next
                    };
                    tokio::select! {
                        _ = tokio::time::sleep(pause) => {}
                        _ = shutdown.changed() => return Ok(()),
                    }
                }
            }
        }
    }

    async fn run_once(
        &self,
        events: &mpsc::Sender<QqWsEvent>,
        resume: Option<ResumeState>,
        shutdown: &mut watch::Receiver<bool>,
    ) -> RunOutcome {
        let (ws, _) = match connect_async(&self.ws_url).await {
            Ok(c) => c,
            Err(e) => {
                error!(error = %e, "qq gateway connect failed");
                return RunOutcome::Dropped {
                    resume,
                    kind: DropKind::ConnectFail,
                };
            }
        };
        info!(url = %self.ws_url, "qq gateway connected");
        let (mut sink, mut stream) = ws.split();

        let heartbeat_interval = match hello_interval(&mut stream, shutdown).await {
            Hello::Interval(ms) => Duration::from_millis(ms.unwrap_or(45_000)),
            Hello::Drop => {
                return RunOutcome::Dropped {
                    resume,
                    kind: DropKind::ConnectFail,
                };
            }
            Hello::Shutdown => return RunOutcome::Shutdown,
        };

        let identified = match resume.as_ref() {
            Some(state) if !state.session_id.is_empty() => {
                let token = match self.auth.get_token().await {
                    Ok(t) => t,
                    Err(e) => {
                        error!(error = %e, "qq gateway token fetch failed");
                        return RunOutcome::Dropped {
                            resume,
                            kind: DropKind::ConnectFail,
                        };
                    }
                };
                send_json(
                    &mut sink,
                    serde_json::json!({
                        "op": 6,
                        "d": {"token": format!("QQBot {token}"), "session_id": state.session_id, "seq": state.last_seq}
                    }),
                )
                .await
                .is_ok()
            }
            _ => {
                let token = match self.auth.get_token().await {
                    Ok(t) => t,
                    Err(e) => {
                        error!(error = %e, "qq gateway token fetch failed");
                        return RunOutcome::Dropped {
                            resume: None,
                            kind: DropKind::ConnectFail,
                        };
                    }
                };
                send_json(
                    &mut sink,
                    serde_json::json!({
                        "op": 2,
                        "d": {"token": format!("QQBot {token}"), "intents": self.intents, "shard": [0, 1]}
                    }),
                )
                .await
                .is_ok()
            }
        };
        if !identified {
            return RunOutcome::Dropped {
                resume,
                kind: DropKind::ConnectFail,
            };
        }

        let shared = Arc::new(GatewayShared::new(resume.as_ref().and_then(|r| r.last_seq)));
        let heartbeat_task = tokio::spawn(heartbeat_loop(
            heartbeat_interval,
            Arc::clone(&shared),
            sink,
        ));

        let mut live_session = resume;
        let outcome;
        let mut monitor = tokio::time::interval(heartbeat_interval);
        monitor.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        monitor.tick().await;
        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    outcome = RunOutcome::Shutdown;
                    break;
                }
                _ = monitor.tick() => {
                    if shared.force_reconnect.load(Ordering::Relaxed) {
                        warn!("qq gateway heartbeats un-ACKed; forcing reconnect");
                        outcome = RunOutcome::Dropped {
                            resume: live_session.take(),
                            kind: DropKind::ReadyDrop,
                        };
                        break;
                    }
                }
                frame = stream.next() => {
                    let Some(Ok(msg)) = frame else {
                        outcome = RunOutcome::Dropped {
                            resume: live_session.take(),
                            kind: DropKind::ReadyDrop,
                        };
                        break;
                    };
                    let Ok(text) = msg.to_text() else { continue };
                    let Ok(ws) = serde_json::from_str::<WsFrame>(text) else { continue };
                    match ws.op {
                        0 => {
                            let d = ws.d.clone().unwrap_or(serde_json::Value::Null);
                            let t = ws.t.clone().unwrap_or_default();
                            if let Some(s) = ws.s {
                                *shared.last_seq.lock().unwrap() = Some(s);
                            }
                            if t == "READY"
                                && let Some(sid) = d.get("session_id").and_then(serde_json::Value::as_str)
                            {
                                live_session = Some(ResumeState {
                                    session_id: sid.to_string(),
                                    last_seq: *shared.last_seq.lock().unwrap(),
                                });
                            }
                            let kind = QqWsEventKind::from_dispatch(&t, &d);
                            let send_failed = events
                                .send(QqWsEvent { kind, raw: d })
                                .await
                                .is_err();
                            if send_failed {
                                outcome = RunOutcome::Shutdown;
                                break;
                            }
                        }
                        7 => {
                            warn!("qq gateway server requested reconnect");
                            outcome = RunOutcome::Dropped {
                                resume: live_session.take(),
                                kind: DropKind::ReadyDrop,
                            };
                            break;
                        }
                        9 => {
                            warn!("qq gateway invalid session");
                            outcome = RunOutcome::Dropped {
                                resume: None,
                                kind: DropKind::InvalidSession,
                            };
                            break;
                        }
                        11 => {
                            shared.acks.fetch_add(1, Ordering::Relaxed);
                        }
                        _ => {}
                    }
                }
            }
        }
        heartbeat_task.abort();
        let mut outcome = outcome;
        if let RunOutcome::Dropped {
            resume: Some(state),
            ..
        } = &mut outcome
        {
            state.last_seq = *shared.last_seq.lock().unwrap();
        }
        outcome
    }
}

enum Hello {
    Interval(Option<u64>),
    Drop,
    Shutdown,
}

enum RunOutcome {
    Shutdown,
    Dropped {
        resume: Option<ResumeState>,
        kind: DropKind,
    },
}

enum DropKind {
    ReadyDrop,
    ConnectFail,
    InvalidSession,
}

#[derive(Default, Clone)]
struct ResumeState {
    session_id: String,
    last_seq: Option<u64>,
}

struct GatewayShared {
    last_seq: StdMutex<Option<u64>>,
    acks: AtomicU64,
    beats_sent: AtomicU64,
    force_reconnect: AtomicBool,
}

impl GatewayShared {
    fn new(initial_seq: Option<u64>) -> Self {
        Self {
            last_seq: StdMutex::new(initial_seq),
            acks: AtomicU64::new(0),
            beats_sent: AtomicU64::new(0),
            force_reconnect: AtomicBool::new(false),
        }
    }
}

async fn hello_interval(
    stream: &mut SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    shutdown: &mut watch::Receiver<bool>,
) -> Hello {
    loop {
        tokio::select! {
            _ = shutdown.changed() => return Hello::Shutdown,
            frame = stream.next() => {
                let Some(Ok(msg)) = frame else { return Hello::Drop };
                let Ok(text) = msg.to_text() else { continue };
                let Ok(ws) = serde_json::from_str::<WsFrame>(text) else { continue };
                if ws.op == 10 {
                    let ms = ws
                        .d
                        .as_ref()
                        .and_then(|d| d.get("heartbeat_interval"))
                        .and_then(serde_json::Value::as_u64);
                    return Hello::Interval(ms);
                }
            }
        }
    }
}

#[allow(clippy::type_complexity)]
async fn heartbeat_loop(
    interval: Duration,
    shared: Arc<GatewayShared>,
    mut sink: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;
    loop {
        ticker.tick().await;
        let seq = *shared.last_seq.lock().unwrap();
        let payload = serde_json::json!({"op": 1, "d": seq});
        if sink
            .send(Message::Text(payload.to_string().into()))
            .await
            .is_err()
        {
            return;
        }
        let sent = shared.beats_sent.fetch_add(1, Ordering::Relaxed) + 1;
        let acked = shared.acks.load(Ordering::Relaxed);
        if sent.saturating_sub(acked) > u64::from(MAX_MISSED_ACKS) {
            shared.force_reconnect.store(true, Ordering::Relaxed);
            return;
        }
    }
}

async fn send_json(
    sink: &mut SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
    value: serde_json::Value,
) -> Result<()> {
    sink.send(Message::Text(value.to_string().into()))
        .await
        .with_context(|| format!("send {value}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_frame_parses_hello_with_interval() {
        let raw = r#"{"op":10,"d":{"heartbeat_interval":45000}}"#;
        let f: WsFrame = serde_json::from_str(raw).unwrap();
        assert_eq!(f.op, 10);
        assert_eq!(
            f.d.as_ref()
                .unwrap()
                .get("heartbeat_interval")
                .and_then(|v| v.as_u64()),
            Some(45_000)
        );
    }

    #[test]
    fn ws_frame_parses_dispatch() {
        let raw = r#"{"op":0,"s":42,"t":"GROUP_AT_MESSAGE_CREATE","d":{"id":"M1"}}"#;
        let f: WsFrame = serde_json::from_str(raw).unwrap();
        assert_eq!(f.op, 0);
        assert_eq!(f.s, Some(42));
        assert_eq!(f.t.as_deref(), Some("GROUP_AT_MESSAGE_CREATE"));
    }

    #[test]
    fn kind_maps_known_event_types() {
        let d = serde_json::json!({"session_id": "SID"});
        assert_eq!(
            QqWsEventKind::from_dispatch("READY", &d),
            QqWsEventKind::Ready {
                session_id: "SID".into()
            }
        );
        assert_eq!(
            QqWsEventKind::from_dispatch("GROUP_AT_MESSAGE_CREATE", &serde_json::Value::Null),
            QqWsEventKind::GroupAtMessage
        );
        assert_eq!(
            QqWsEventKind::from_dispatch("C2C_MESSAGE_CREATE", &serde_json::Value::Null),
            QqWsEventKind::C2cMessage
        );
        assert_eq!(
            QqWsEventKind::from_dispatch("FRIEND_ADD", &serde_json::Value::Null),
            QqWsEventKind::Other("FRIEND_ADD".into())
        );
    }
}
