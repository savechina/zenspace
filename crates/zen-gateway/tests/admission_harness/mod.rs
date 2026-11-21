//! Shared headless harness for the 007 admission e2e suites
//! (`admission_observe.rs`, `admission_auto.rs`).
//!
//! PURPOSE: one fully wired daemon + mock QQ backend, mirroring
//! `qqbot_channel.rs`, exposing the decision ledger over the same `state.db`
//! the seam writes. Scripted executors make "turn executed" observable.
//! USAGE: `spawn_stack(executor, accept_mode).await` then `stack.mock.inject(..)`.
//! EXPECTED: deterministic; daemons drain cleanly on `shutdown`.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use axum::{
    Json, Router,
    extract::{Path, State, WebSocketUpgrade},
    response::IntoResponse,
    routing::{get, post},
};
use serde_json::{Value, json};
use zen_core::types::SessionContext;
use zen_gateway::channel::qqbot::QqBotAdapterOptions;
use zen_gateway::server::hosting::TurnExecutor;
use zen_gateway::{GatewayDaemonConfig, GatewayService};
use zen_repo::{AdmissionRepo, DecisionRow, DispositionFilter, SqliteClient};

pub const ALLOWED_MEMBER: &str = "memberOK";
pub const DENIED_MEMBER: &str = "memberNO";
pub const ALLOWED_C2C_USER: &str = "userOK";
const TOKEN: &str = "TEST_TOKEN";

fn next_carrier_port() -> u16 {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let pid_offset = (std::process::id() % 256) as u16;
    24_000 + pid_offset + NEXT.fetch_add(1, Ordering::SeqCst) as u16
}

/// Echo executor (optionally slow on a marker) — proves the standard
/// pipeline carried the user's text; the call counter proves execution.
pub struct EchoExec {
    pub calls: Arc<AtomicUsize>,
    pub slow_contains: Option<(&'static str, Duration)>,
}

#[async_trait::async_trait]
impl TurnExecutor for EchoExec {
    async fn execute_stream(
        &self,
        _session: &mut SessionContext,
        prompt: &str,
        callback: &mut (dyn FnMut(String) + Send),
    ) -> anyhow::Result<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some((marker, delay)) = &self.slow_contains
            && prompt.contains(marker)
        {
            tokio::time::sleep(*delay).await;
        }
        callback(format!("echo:{prompt}"));
        Ok(format!("echo:{prompt}"))
    }
}

/// Failing executor — honest failure reporting / ticket `failed` state.
pub struct FailExec {
    pub calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl TurnExecutor for FailExec {
    async fn execute_stream(
        &self,
        _session: &mut SessionContext,
        _prompt: &str,
        _callback: &mut (dyn FnMut(String) + Send),
    ) -> anyhow::Result<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(anyhow::anyhow!("scripted executor failure"))
    }
}

#[derive(Clone)]
pub struct MockState {
    pub inner: Arc<MockInner>,
}

pub struct MockInner {
    pub identified: AtomicBool,
    pub identify_count: AtomicUsize,
    pub resume_count: AtomicUsize,
    pub acks_enabled: AtomicBool,
    pub group_sends: StdMutex<Vec<(String, Value)>>,
    pub c2c_sends: StdMutex<Vec<(String, Value)>>,
    pending: StdMutex<VecDeque<Value>>,
    inject_notify: tokio::sync::Notify,
}

impl MockState {
    fn new() -> Self {
        Self {
            inner: Arc::new(MockInner {
                identified: AtomicBool::new(false),
                identify_count: AtomicUsize::new(0),
                resume_count: AtomicUsize::new(0),
                acks_enabled: AtomicBool::new(true),
                group_sends: StdMutex::new(Vec::new()),
                c2c_sends: StdMutex::new(Vec::new()),
                pending: StdMutex::new(VecDeque::new()),
                inject_notify: tokio::sync::Notify::new(),
            }),
        }
    }

    pub fn inject(&self, event: Value) {
        self.inner.pending.lock().unwrap().push_back(event);
        self.inner.inject_notify.notify_one();
    }

    pub fn group_sends_snapshot(&self) -> Vec<(String, Value)> {
        self.inner.group_sends.lock().unwrap().clone()
    }

    pub fn c2c_sends_snapshot(&self) -> Vec<(String, Value)> {
        self.inner.c2c_sends.lock().unwrap().clone()
    }
}

async fn token_handler() -> Json<Value> {
    Json(json!({"access_token": TOKEN, "expires_in": "7200"}))
}

async fn group_message_handler(
    State(st): State<MockState>,
    Path(group_openid): Path<String>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    let auth = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    st.inner
        .group_sends
        .lock()
        .unwrap()
        .push((auth, json!({"group_openid": group_openid, "body": body})));
    Json(json!({"id": "ROBOT1.0_mock"}))
}

async fn c2c_message_handler(
    State(st): State<MockState>,
    Path(user_openid): Path<String>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    let auth = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    st.inner
        .c2c_sends
        .lock()
        .unwrap()
        .push((auth, json!({"user_openid": user_openid, "body": body})));
    Json(json!({"id": "ROBOT1.0_mock"}))
}

async fn ws_handler(ws: WebSocketUpgrade, State(st): State<MockState>) -> impl IntoResponse {
    ws.on_upgrade(move |sock| run_mock_ws(sock, st))
}

async fn send_ready(sock: &mut axum::extract::ws::WebSocket, seq: u64) -> bool {
    let ready = json!({
        "op": 0, "s": seq, "t": "READY",
        "d": {"session_id": "sess-mock", "user": {"id": "BOTID"}}
    });
    sock.send(axum::extract::ws::Message::Text(ready.to_string().into()))
        .await
        .is_ok()
}

async fn run_mock_ws(mut sock: axum::extract::ws::WebSocket, st: MockState) {
    let hello = json!({"op": 10, "d": {"heartbeat_interval": 200}});
    sock.send(axum::extract::ws::Message::Text(hello.to_string().into()))
        .await
        .unwrap();

    let mut seq: u64 = 0;
    loop {
        let next = st.inner.pending.lock().unwrap().pop_front();
        if let Some(mut event) = next {
            let is_control = event.get("op").is_some();
            seq += 1;
            if !is_control {
                event["op"] = json!(0);
                event["s"] = json!(seq);
            }
            if sock
                .send(axum::extract::ws::Message::Text(event.to_string().into()))
                .await
                .is_err()
            {
                break;
            }
            continue;
        }
        tokio::select! {
            incoming = sock.recv() => {
                let Some(Ok(axum::extract::ws::Message::Text(text))) = incoming else { break };
                let Ok(frame) = serde_json::from_str::<Value>(&text) else { continue };
                match frame["op"].as_u64() {
                    Some(2) => {
                        st.inner.identified.store(true, Ordering::SeqCst);
                        st.inner.identify_count.fetch_add(1, Ordering::SeqCst);
                        seq += 1;
                        if !send_ready(&mut sock, seq).await {
                            break;
                        }
                    }
                    Some(6) => {
                        st.inner.resume_count.fetch_add(1, Ordering::SeqCst);
                        seq += 1;
                        if !send_ready(&mut sock, seq).await {
                            break;
                        }
                    }
                    Some(1) if st.inner.acks_enabled.load(Ordering::SeqCst) => {
                        let ack = json!({"op": 11});
                        if sock
                            .send(axum::extract::ws::Message::Text(ack.to_string().into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            _ = st.inner.inject_notify.notified() => {}
        }
    }
}

pub fn group_event_in(group: &str, msg_id: &str, member: &str, content: &str) -> Value {
    json!({
        "t": "GROUP_AT_MESSAGE_CREATE",
        "d": {
            "id": msg_id,
            "group_openid": group,
            "content": content,
            "author": {"id": "uX", "member_openid": member}
        }
    })
}

pub fn group_event(msg_id: &str, member: &str, content: &str) -> Value {
    group_event_in("gOK", msg_id, member, content)
}

pub fn c2c_event(msg_id: &str, user: &str, content: &str) -> Value {
    json!({
        "t": "C2C_MESSAGE_CREATE",
        "d": {"id": msg_id, "content": content, "author": {"id": user}}
    })
}

pub async fn wait_until<F: Fn() -> bool>(timeout: Duration, pred: F) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if pred() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    pred()
}

pub struct Stack {
    _claim: tokio::sync::MutexGuard<'static, ()>,
    pub tmp: tempfile::TempDir,
    pub mock: MockState,
    pub state_db: std::path::PathBuf,
    drain_tx: tokio::sync::watch::Sender<bool>,
    done_rx: tokio::sync::oneshot::Receiver<()>,
    serve_task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

fn bootstrap_lock() -> &'static tokio::sync::Mutex<()> {
    static BOOT: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    BOOT.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Reverses `ZenWiring`'s process-wide NOFILE lowering so sibling embedded
/// daemons in one test binary do not hit EMFILE (mirrors qqbot_channel.rs).
fn restore_process_soft_limits() {
    #[cfg(unix)]
    unsafe {
        let mut rl = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) == 0 {
            rl.rlim_cur = rl.rlim_max;
            libc::setrlimit(libc::RLIMIT_NOFILE, &rl);
        }
    }
}

pub async fn spawn_stack(executor: Arc<dyn TurnExecutor>, accept_mode: Option<&str>) -> Stack {
    // Keep the production NOFILE=256 cap from wedging sibling daemons.
    unsafe { std::env::set_var("ZEN_SKIP_RLIMIT", "1") };
    let boot_guard = bootstrap_lock().lock().await;
    restore_process_soft_limits();
    let tmp = tempfile::tempdir().unwrap();
    let mock = MockState::new();

    let mock_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = mock_listener.local_addr().unwrap();
    let mock_router = Router::new()
        .route("/app/getAppAccessToken", post(token_handler))
        .route(
            "/v2/groups/{group_openid}/messages",
            post(group_message_handler),
        )
        .route(
            "/v2/users/{user_openid}/messages",
            post(c2c_message_handler),
        )
        .route("/websocket", get(ws_handler))
        .with_state(mock.clone());
    tokio::spawn(async move {
        axum::serve(mock_listener, mock_router.into_make_service())
            .await
            .unwrap();
    });
    let mock_base = format!("http://{mock_addr}");

    let http_port = next_carrier_port();
    let state_db = tmp.path().join("state.db");
    let config = GatewayDaemonConfig {
        socket_path: tmp.path().join("gateway.sock"),
        memory_path: Some(tmp.path().join("memory.mv2")),
        db_path: Some(state_db.clone()),
        audit_path: Some(tmp.path().join("audit.jsonl")),
        drain_window: Duration::from_secs(2),
        http: Some(zen_gateway::transport::http::HttpCarrierConfig {
            bind_addr: "127.0.0.1".to_string(),
            port: http_port,
        }),
        qqbot: Some(QqBotAdapterOptions {
            app_id: "APPID".to_string(),
            client_secret: "SECRET".to_string(),
            chat_base: String::new(),
            ws_url: format!("ws://{mock_addr}/websocket"),
            api_base: mock_base.clone(),
            token_url: format!("{mock_base}/app/getAppAccessToken"),
            allowed_users: vec![ALLOWED_MEMBER.to_string(), ALLOWED_C2C_USER.to_string()],
            accept_mode: accept_mode.map(str::to_string),
            bindings_db: state_db.clone(),
            audit_path: Some(tmp.path().join("audit.jsonl")),
            outbox_drain_interval: Duration::from_secs(3600),
        }),
        turn_executor: Some(executor),
        ..GatewayDaemonConfig::default()
    };
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    let serve_shutdown = shutdown_tx.clone();
    let serve_task = tokio::spawn(async move {
        let result = GatewayService::serve_with_shutdown(config, serve_shutdown, shutdown_rx).await;
        if let Err(e) = &result {
            eprintln!("[serve] daemon exited early: {e}");
        }
        let _ = done_tx.send(());
        result
    });

    await_carrier(&format!("http://127.0.0.1:{http_port}")).await;

    Stack {
        _claim: boot_guard,
        tmp,
        mock,
        state_db,
        drain_tx: shutdown_tx,
        done_rx,
        serve_task,
    }
}

async fn await_carrier(http_base: &str) {
    let probe = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    for i in 0..100 {
        match probe.get(format!("{http_base}/health")).send().await {
            Ok(resp) if resp.status().is_success() => return,
            Ok(resp) => eprintln!("[bootstrap] probe #{i}: status {}", resp.status()),
            Err(e) => eprintln!("[bootstrap] probe #{i}: {e}"),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("carrier at {http_base} never became healthy");
}

pub async fn wait_identified(stack: &Stack) {
    assert!(
        wait_until(Duration::from_secs(10), || stack
            .mock
            .inner
            .identified
            .load(Ordering::SeqCst))
        .await,
        "bot never completed Identify"
    );
}

/// Opens the same `state.db` the seam writes (the ledger view).
pub async fn open_db(stack: &Stack) -> SqliteClient {
    SqliteClient::open(&stack.state_db).await.unwrap()
}

/// Newest-first decision rows, read exactly as `zen admission list` does.
pub async fn decisions(client: &SqliteClient) -> Vec<DecisionRow> {
    AdmissionRepo::new(client)
        .list_decisions(DispositionFilter::default(), 100)
        .await
        .unwrap()
}

/// Polls the ledger until at least `n` decision rows exist (the worker writes
/// asynchronously after the WS event is injected).
pub async fn wait_for_decisions(client: &SqliteClient, n: usize) -> Vec<DecisionRow> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let rows = decisions(client).await;
        if rows.len() >= n {
            return rows;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "expected {n} admission decisions, saw {}: {rows:?}",
                rows.len()
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

pub async fn wait_for_group_reply(stack: &Stack, msg_id: &str) -> Vec<(String, Value)> {
    assert!(
        wait_until(Duration::from_secs(20), || stack
            .mock
            .group_sends_snapshot()
            .iter()
            .any(|(_, v)| v["body"]["msg_id"] == msg_id))
        .await,
        "no group reply for {msg_id}: {:?}",
        stack.mock.group_sends_snapshot()
    );
    stack.mock.group_sends_snapshot()
}

impl Stack {
    pub async fn shutdown(self) {
        self.drain_tx.send_replace(true);
        let _ = self.done_rx.await;
        assert!(
            self.serve_task.await.unwrap().is_ok(),
            "serve_with_shutdown must exit Ok"
        );
    }

    pub fn audit_text(&self) -> String {
        std::fs::read_to_string(self.tmp.path().join("audit.jsonl")).unwrap_or_default()
    }
}
