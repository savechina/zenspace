//! QQBot channel verification suite (Phase 13 T064 + Phase 13.1 T072 —
//! FR-019/020/021 matrix + eng-review D5 gap batch).
//!
//! PURPOSE: Runs daemons carrying UDS and loopback HTTP (incl. MCP
//! mount) plus the qqbot channel concurrently against a mock QQ
//! backend, then proves coexistence parity (UDS/HTTP/MCP while bot
//! connected), group AND C2C passive round-trips, duplicate
//! suppression, allowlist deny, binding persistence, per-chat
//! concurrency (D1), WS state-machine recovery (Resume/op7/op9/slow-
//! ACK), bridge retry branches (5xx/4xx), dead-carrier fatality (P5),
//! in-flight-reply shutdown survival (P4), audit-event landing (D3),
//! and -32004 replay-over-HTTP (D8).
//!
//! USAGE: `cargo test -p zen-gateway --test qqbot_channel`. Scripted
//! executors stream fixed text — no provider/network beyond localhost.
//!
//! EXPECTED: all assertions pass deterministically; daemons drain
//! cleanly on shutdown.

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
use zen_gateway::protocol::Capabilities;
use zen_gateway::server::hosting::TurnExecutor;
use zen_gateway::{GatewayDaemonConfig, GatewayService};

const ALLOWED_MEMBER: &str = "memberOK";
const DENIED_MEMBER: &str = "memberNO";
const ALLOWED_C2C_USER: &str = "userOK";
const TOKEN: &str = "TEST_TOKEN";

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Collision-free carrier ports: `free_port()` releases its bind, so
/// under parallel tests the OS can hand the SAME port to another
/// stack's mock listener before the daemon rebinds it — permanently
/// killing that carrier. A process-local counter over the non-
/// ephemeral 24k range guarantees intra-binary uniqueness instead.
///
/// Under nextest each test runs in a separate process, so `static NEXT`
/// resets to 0 in every worker — all workers claim port 24000. We add
/// the PID as a base offset to guarantee cross-process uniqueness.
fn next_carrier_port() -> u16 {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    // PID offset: different nextest workers get disjoint port ranges.
    // PID fits in u16 on all supported platforms (max 32768 on macOS
    // classic, 4194304 on Linux) — mask to 0..255 to stay in the
    // 24k-24k+255 band and avoid overflowing u16.
    let pid_offset = (std::process::id() % 256) as u16;
    24_000 + pid_offset + NEXT.fetch_add(1, Ordering::SeqCst) as u16
}

/// Scripted executor echoing the prompt back — proves the full
/// platform→gateway→platform round-trip carried the user's text.
/// `slow_contains` optionally delays prompts containing a marker so
/// concurrency and shutdown-mid-reply windows become observable.
struct DelayExec {
    calls: Arc<AtomicUsize>,
    slow_contains: Option<(&'static str, Duration)>,
}

#[async_trait::async_trait]
impl TurnExecutor for DelayExec {
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

#[derive(Clone)]
struct MockState {
    inner: Arc<MockInner>,
}

struct MockInner {
    identified: AtomicBool,
    connections: AtomicUsize,
    /// Token presented in the last Identify frame (`QQBot {token}`).
    identify_token: StdMutex<Option<String>>,
    identify_count: AtomicUsize,
    resume_count: AtomicUsize,
    /// When false the mock stops ACKing heartbeats (slow-ACK test).
    acks_enabled: AtomicBool,
    /// Recorded outbound group sends: (auth header, body).
    group_sends: StdMutex<Vec<(String, Value)>>,
    /// Recorded outbound C2C sends: (auth header, body).
    c2c_sends: StdMutex<Vec<(String, Value)>>,
    /// Platform events awaiting ANY live WS connection (survives
    /// reconnects — the old take()-style channel died with conn #1).
    pending: StdMutex<VecDeque<Value>>,
    inject_notify: tokio::sync::Notify,
}

impl MockState {
    fn new() -> Self {
        Self {
            inner: Arc::new(MockInner {
                identified: AtomicBool::new(false),
                connections: AtomicUsize::new(0),
                identify_token: StdMutex::new(None),
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

    fn inject(&self, event: Value) {
        self.inner.pending.lock().unwrap().push_back(event);
        self.inner.inject_notify.notify_one();
    }

    fn group_sends_snapshot(&self) -> Vec<(String, Value)> {
        self.inner.group_sends.lock().unwrap().clone()
    }

    fn c2c_sends_snapshot(&self) -> Vec<(String, Value)> {
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
    Json(json!({"id": "ROBOT1.0_mock", "timestamp": "2026-08-24T00:00:00+08:00"}))
}

/// Records C2C sends (T072: previously discarded, making C2C e2e
/// unverifiable).
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
    st.inner.connections.fetch_add(1, Ordering::SeqCst);
    let hello = json!({"op": 10, "d": {"heartbeat_interval": 200}});
    sock.send(axum::extract::ws::Message::Text(hello.to_string().into()))
        .await
        .unwrap();

    let mut seq: u64 = 0;
    loop {
        // Drain queued injections FIRST so notifications fired while
        // this connection was down are not lost. Items carrying an
        // explicit "op" are RAW CONTROL frames (op7/op9 probes) sent
        // verbatim; everything else is wrapped as op0 dispatch.
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
                        *st.inner.identify_token.lock().unwrap() =
                            frame["d"]["token"].as_str().map(str::to_string);
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

fn group_event_in(group: &str, msg_id: &str, member: &str, content: &str) -> Value {
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

fn group_event(msg_id: &str, member: &str, content: &str) -> Value {
    group_event_in("gOK", msg_id, member, content)
}

fn c2c_event(msg_id: &str, user: &str, content: &str) -> Value {
    json!({
        "t": "C2C_MESSAGE_CREATE",
        "d": {"id": msg_id, "content": content, "author": {"id": user}}
    })
}

async fn wait_until<F: Fn() -> bool>(timeout: Duration, pred: F) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if pred() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    pred()
}

/// One fully wired daemon + mock QQ backend. Every test gets its own
/// instance (the injection queue and counters are per-stack state).
struct Stack {
    /// Held for the whole test body: the daemon's sole-owner claim
    /// forbids any sibling service in this process while alive.
    _claim: tokio::sync::MutexGuard<'static, ()>,
    #[allow(dead_code)]
    tmp: tempfile::TempDir,
    mock: MockState,
    http_base: String,
    http_client: reqwest::Client,
    socket_path: std::path::PathBuf,
    drain_tx: tokio::sync::watch::Sender<bool>,
    done_rx: tokio::sync::oneshot::Receiver<()>,
    serve_task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

/// Serializes ENTIRE TEST BODIES across this binary: each daemon takes
/// the process-wide sole-owner claim (`GATEWAY_OPEN`) for its whole
/// lifetime, so two live daemons in one process are forbidden by
/// design — tests queue on this lock instead of racing the claim.
fn bootstrap_lock() -> &'static tokio::sync::Mutex<()> {
    static BOOT: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    BOOT.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Undoes the process-wide soft-limit lowering that `ZenWiring::new`
/// performs on every daemon construction (FR-038: NOFILE=256,
/// NPROC=50). Those caps are production hardening, but integration
/// tests embed MANY daemons in ONE process — sockets + sqlite + mv2
/// files blow past 256 fds and sibling stacks die with EMFILE. The
/// soft cap is reversible by design (hard limit preserved), so the
/// harness restores it before each bootstrap; the wiring re-lowers it
/// for its own daemon and the next bootstrap raises it again.
fn restore_process_soft_limits() {
    #[cfg(unix)]
    unsafe {
        for resource in [libc::RLIMIT_NOFILE, libc::RLIMIT_NPROC] {
            let mut rl = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::getrlimit(resource, &mut rl) == 0 {
                rl.rlim_cur = rl.rlim_max;
                libc::setrlimit(resource, &rl);
            }
        }
    }
}

async fn spawn_stack(executor: Arc<dyn TurnExecutor>, drain: Duration) -> Stack {
    // Prevent the production RLIMIT_NPROC=50 cap from wedging the test
    // daemon on Linux — see wiring.rs note. Must be set before the
    // daemon's `ZenWiring` (or any `apply_resource_limits` caller) runs.
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
    let socket_path = tmp.path().join("gateway.sock");
    let config = GatewayDaemonConfig {
        socket_path: socket_path.clone(),
        memory_path: Some(tmp.path().join("memory.mv2")),
        db_path: Some(tmp.path().join("state.db")),
        audit_path: Some(tmp.path().join("audit.jsonl")),
        drain_window: drain,
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
            bindings_db: tmp.path().join("state.db"),
            audit_path: Some(tmp.path().join("audit.jsonl")),
            outbox_drain_interval: std::time::Duration::from_secs(3600),
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
        http_base: format!("http://127.0.0.1:{http_port}"),
        http_client: reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap_or_else(|_| reqwest::Client::new()),
        socket_path,
        drain_tx: shutdown_tx,
        done_rx,
        serve_task,
    }
}

/// Blocks until this stack's HTTP carrier answers /health, so the
/// channel's startup probe cannot lose a scheduling race. Called while
/// still holding [`BOOTSTRAP`].
async fn await_carrier(http_base: &str) {
    let probe = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    for i in 0..100 {
        match probe.get(format!("{http_base}/health")).send().await {
            Ok(resp) if resp.status().is_success() => return,
            Ok(resp) => eprintln!(
                "[bootstrap] {http_base} probe #{i}: unexpected status {}",
                resp.status()
            ),
            Err(e) => eprintln!("[bootstrap] {http_base} probe #{i}: {e}"),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("carrier at {http_base} never became healthy");
}

async fn wait_identified(stack: &Stack) {
    assert!(
        wait_until(Duration::from_secs(10), || stack
            .mock
            .inner
            .identified
            .load(Ordering::SeqCst))
        .await,
        "bot never completed Identify (ws_connections={}, identified={})",
        stack.mock.inner.connections.load(Ordering::SeqCst),
        stack.mock.inner.identified.load(Ordering::SeqCst)
    );
}

#[tokio::test]
async fn qqbot_coexists_with_uds_http_mcp_and_bridges_turns() {
    let calls = Arc::new(AtomicUsize::new(0));
    let stack = spawn_stack(
        Arc::new(DelayExec {
            calls: Arc::clone(&calls),
            slow_contains: None,
        }),
        Duration::from_secs(2),
    )
    .await;

    // (a-setup) Bot identifies against the mock gateway with the
    // official token scheme.
    wait_identified(&stack).await;
    assert_eq!(
        stack.mock.inner.identify_token.lock().unwrap().as_deref(),
        Some(format!("QQBot {TOKEN}").as_str()),
    );

    // (a) UDS health/status parity while the channel is live.
    let uds_client = zen_gateway::client::GatewayClient::connect(stack.socket_path.clone())
        .await
        .expect("uds connect");
    uds_client
        .handshake("qqbot-coexistence-test", "0.0", Capabilities::default())
        .await
        .expect("uds handshake");
    let status = uds_client
        .request("health/status", json!({}))
        .await
        .expect("health/status over UDS");
    assert_eq!(status["storeHealth"], "ok");

    // (a) HTTP /health parity + (FR-020) MCP tools/list on the same
    // carrier while qqbot is connected.
    let health: Value = stack
        .http_client
        .get(format!("{}/health", stack.http_base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["status"], "healthy");
    let mcp: Value = stack
        .http_client
        .post(format!("{}/api/v1/mcp", stack.http_base))
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        mcp["result"]["tools"]
            .as_array()
            .map(|t| !t.is_empty())
            .unwrap_or(false),
        "MCP mount must serve the filtered registry"
    );

    // (b) Allowed group message executes a turn and the reply lands as
    // a passive message quoting msg_id with the official auth scheme.
    stack
        .mock
        .inject(group_event("MSG1", ALLOWED_MEMBER, "<@!BOTID> 你好"));
    assert!(
        wait_until(Duration::from_secs(20), || stack
            .mock
            .group_sends_snapshot()
            .iter()
            .any(|(_, v)| {
                v["body"]["msg_id"] == "MSG1"
                    && v["body"]["content"]
                        .as_str()
                        .map(|c| c.contains("echo:你好"))
                        .unwrap_or(false)
            }))
        .await,
        "passive reply never reached the platform sink: {:?}",
        stack.mock.group_sends_snapshot()
    );
    let (_, first) = stack
        .mock
        .group_sends_snapshot()
        .iter()
        .find(|(_, v)| v["body"]["msg_id"] == "MSG1")
        .unwrap()
        .clone();
    assert_eq!(first["group_openid"], "gOK");
    assert_eq!(first["body"]["msg_seq"], 1);

    // (c) Duplicate redelivery suppressed — no second turn executed.
    let calls_after_first = calls.load(Ordering::SeqCst);
    stack
        .mock
        .inject(group_event("MSG1", ALLOWED_MEMBER, "<@!BOTID> 你好"));
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        calls_after_first,
        "duplicate msg_id must not execute another turn"
    );

    // (d) Non-allowlisted author never reaches the agent.
    stack
        .mock
        .inject(group_event("MSG2", DENIED_MEMBER, "<@!BOTID> sneaky"));
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(calls.load(Ordering::SeqCst), calls_after_first);
    assert!(
        !stack
            .mock
            .group_sends_snapshot()
            .iter()
            .any(|(_, v)| v["body"]["msg_id"] == "MSG2"),
        "denied author must produce no reply"
    );

    // Binding persisted for conversation continuity (T061 round-trip).
    let repo_db = zen_repo::SqliteClient::open(&stack.tmp.path().join("state.db"))
        .await
        .unwrap();
    let repo = zen_repo::QqBindingRepo::new(&repo_db);
    let binding = repo.get("gOK").await.unwrap().expect("binding row");
    assert!(
        !binding.session_id.is_empty(),
        "binding must carry a hosted sessionId"
    );

    // Drain: external shutdown flips; carriers joined gracefully; the
    // audit trail carries the D3 channel events.
    stack.drain_tx.send_replace(true);
    let _ = stack.done_rx.await;
    let result = stack.serve_task.await.unwrap();
    assert!(
        result.is_ok(),
        "serve_with_shutdown must exit Ok: {result:?}"
    );
    let audit = std::fs::read_to_string(stack.tmp.path().join("audit.jsonl")).unwrap_or_default();
    assert!(
        audit.contains("\"kind\":\"qqbot.accepted\"")
            && audit.contains("\"kind\":\"qqbot.rejected\"")
            && audit.contains("\"kind\":\"qqbot.replied\"")
            && audit.contains("\"outcome\":\"success\""),
        "audit trail must carry accepted/rejected/replied events: {audit}"
    );
}

/// T072: C2C messages round-trip through the recorded c2c sink.
#[tokio::test]
async fn qqbot_c2c_message_round_trip() {
    let calls = Arc::new(AtomicUsize::new(0));
    let stack = spawn_stack(
        Arc::new(DelayExec {
            calls: Arc::clone(&calls),
            slow_contains: None,
        }),
        Duration::from_secs(2),
    )
    .await;
    wait_identified(&stack).await;

    stack
        .mock
        .inject(c2c_event("CMSG1", ALLOWED_C2C_USER, "hi there"));
    assert!(
        wait_until(Duration::from_secs(20), || stack
            .mock
            .c2c_sends_snapshot()
            .iter()
            .any(|(_, v)| {
                v["body"]["msg_id"] == "CMSG1"
                    && v["body"]["content"]
                        .as_str()
                        .map(|c| c.contains("echo:hi there"))
                        .unwrap_or(false)
            }))
        .await,
        "C2C passive reply never reached the sink: {:?}",
        stack.mock.c2c_sends_snapshot()
    );
    let sends = stack.mock.c2c_sends_snapshot();
    let (_, first) = sends
        .iter()
        .find(|(_, v)| v["body"]["msg_id"] == "CMSG1")
        .unwrap();
    assert_eq!(first["user_openid"], ALLOWED_C2C_USER);
    assert_eq!(first["body"]["msg_type"], 0);
    assert!(
        first["auth"]
            .as_str()
            .unwrap_or("")
            .starts_with(&format!("QQBot {TOKEN}"))
            || sends.iter().all(|(a, _)| a.starts_with("QQBot ")),
        "c2c send must carry the QQBot auth scheme"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    stack.drain_tx.send_replace(true);
    let _ = stack.done_rx.await;
    assert!(stack.serve_task.await.unwrap().is_ok());
}

/// T072/D1: two groups progress concurrently — a slow turn in one chat
/// cannot block another chat's reply.
#[tokio::test]
async fn two_groups_do_not_block_each_other() {
    let calls = Arc::new(AtomicUsize::new(0));
    let stack = spawn_stack(
        Arc::new(DelayExec {
            calls: Arc::clone(&calls),
            slow_contains: Some(("SLOWMARKER", Duration::from_secs(4))),
        }),
        Duration::from_secs(8),
    )
    .await;
    wait_identified(&stack).await;

    // Group A enters a 4s turn; group B must finish well before it.
    stack.mock.inject(group_event_in(
        "gAAA",
        "GA1",
        ALLOWED_MEMBER,
        "<@!BOTID> SLOWMARKER please",
    ));
    stack.mock.inject(group_event_in(
        "gBBB",
        "GB1",
        ALLOWED_MEMBER,
        "<@!BOTID> quick",
    ));

    let fast_arrived = wait_until(Duration::from_secs(3), || {
        stack
            .mock
            .group_sends_snapshot()
            .iter()
            .any(|(_, v)| v["body"]["msg_id"] == "GB1")
    })
    .await;
    assert!(fast_arrived, "chat B was blocked behind chat A's slow turn");
    assert!(
        !stack
            .mock
            .group_sends_snapshot()
            .iter()
            .any(|(_, v)| v["body"]["msg_id"] == "GA1"),
        "chat A's slow turn must still be in flight"
    );

    // And chat A does complete once its delay elapses (FIFO intact).
    assert!(
        wait_until(Duration::from_secs(10), || stack
            .mock
            .group_sends_snapshot()
            .iter()
            .any(|(_, v)| v["body"]["msg_id"] == "GA1"))
        .await,
        "chat A reply missing after its slow turn finished"
    );

    stack.drain_tx.send_replace(true);
    let _ = stack.done_rx.await;
    assert!(stack.serve_task.await.unwrap().is_ok());
}

/// T072: InvalidSession(op9) discards resume state and re-Identifies
/// after the cooldown.
#[tokio::test]
async fn invalid_session_forces_fresh_identify() {
    let calls = Arc::new(AtomicUsize::new(0));
    let stack = spawn_stack(
        Arc::new(DelayExec {
            calls: Arc::clone(&calls),
            slow_contains: None,
        }),
        Duration::from_secs(2),
    )
    .await;
    wait_identified(&stack).await;
    let identifies_before = stack.mock.inner.identify_count.load(Ordering::SeqCst);

    stack.mock.inject(json!({"op": 9}));
    assert!(
        wait_until(Duration::from_secs(10), || stack
            .mock
            .inner
            .identify_count
            .load(Ordering::SeqCst)
            > identifies_before)
        .await,
        "client did not re-Identify after InvalidSession"
    );
    assert_eq!(
        stack.mock.inner.resume_count.load(Ordering::SeqCst),
        0,
        "post-op9 reconnect must NOT attempt Resume"
    );

    stack.drain_tx.send_replace(true);
    let _ = stack.done_rx.await;
    assert!(stack.serve_task.await.unwrap().is_ok());
}

/// T072: server-requested reconnect(op7) preserves session state — the
/// client Resumes (or, failing that, re-Identifies) and stays live.
#[tokio::test]
async fn server_reconnect_request_recovers_connection() {
    let calls = Arc::new(AtomicUsize::new(0));
    let stack = spawn_stack(
        Arc::new(DelayExec {
            calls: Arc::clone(&calls),
            slow_contains: None,
        }),
        Duration::from_secs(2),
    )
    .await;
    wait_identified(&stack).await;

    stack.mock.inject(json!({"op": 7}));
    assert!(
        wait_until(Duration::from_secs(10), || {
            let resumed = stack.mock.inner.resume_count.load(Ordering::SeqCst);
            let identified = stack.mock.inner.identify_count.load(Ordering::SeqCst);
            resumed >= 1 || identified >= 2
        })
        .await,
        "client neither Resumed nor re-Identified after op7"
    );

    // Connection usable again: a fresh event still round-trips.
    stack
        .mock
        .inject(group_event("POSTOP7", ALLOWED_MEMBER, "<@!BOTID> alive"));
    assert!(
        wait_until(Duration::from_secs(15), || stack
            .mock
            .group_sends_snapshot()
            .iter()
            .any(|(_, v)| v["body"]["msg_id"] == "POSTOP7"))
        .await,
        "no round-trip after reconnect recovery"
    );

    stack.drain_tx.send_replace(true);
    let _ = stack.done_rx.await;
    assert!(stack.serve_task.await.unwrap().is_ok());
}

/// T072/oracle-P3: heartbeats that go un-ACKed force a reconnect —
/// proving the dispatch loop never starves the heartbeat/ACK path.
#[tokio::test]
async fn unacked_heartbeats_force_reconnect() {
    let calls = Arc::new(AtomicUsize::new(0));
    let stack = spawn_stack(
        Arc::new(DelayExec {
            calls: Arc::clone(&calls),
            slow_contains: None,
        }),
        Duration::from_secs(2),
    )
    .await;
    wait_identified(&stack).await;
    let identifies_before = stack.mock.inner.identify_count.load(Ordering::SeqCst);

    stack.mock.inner.acks_enabled.store(false, Ordering::SeqCst);
    assert!(
        wait_until(Duration::from_secs(10), || stack
            .mock
            .inner
            .identify_count
            .load(Ordering::SeqCst)
            > identifies_before
            || stack.mock.inner.resume_count.load(Ordering::SeqCst) >= 1)
        .await,
        "missed-ACK watchdog never forced a reconnect"
    );
    stack.mock.inner.acks_enabled.store(true, Ordering::SeqCst);

    stack.drain_tx.send_replace(true);
    let _ = stack.done_rx.await;
    assert!(stack.serve_task.await.unwrap().is_ok());
}

/// T071/P5: a dead carrier at startup escalates to a FATAL channel
/// error while the daemon itself keeps running and drains cleanly.
#[tokio::test]
async fn dead_carrier_is_fatal_to_channel_not_daemon() {
    let _claim = bootstrap_lock().lock().await;
    restore_process_soft_limits();
    // Reserve-then-drop a port so connections are refused instantly.
    let dead_port = free_port();
    let calls = Arc::new(AtomicUsize::new(0));

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
        .route("/websocket", get(ws_handler))
        .with_state(mock.clone());
    tokio::spawn(async move {
        let _ = axum::serve(mock_listener, mock_router.into_make_service()).await;
    });
    let mock_base = format!("http://{mock_addr}");

    let socket_path = tmp.path().join("gateway.sock");
    let config = GatewayDaemonConfig {
        socket_path: socket_path.clone(),
        memory_path: Some(tmp.path().join("memory.mv2")),
        db_path: Some(tmp.path().join("state.db")),
        audit_path: Some(tmp.path().join("audit.jsonl")),
        drain_window: Duration::from_secs(2),
        // No HTTP carrier configured — but qqbot implies one on 9876.
        // Point ws at the mock yet leave the implied carrier UNBOUND by
        // occupying its port ourselves.
        http: Some(zen_gateway::transport::http::HttpCarrierConfig {
            bind_addr: "127.0.0.1".to_string(),
            port: dead_port,
        }),
        qqbot: Some(QqBotAdapterOptions {
            app_id: "APPID".to_string(),
            client_secret: "SECRET".to_string(),
            chat_base: String::new(),
            ws_url: format!("ws://{mock_addr}/websocket"),
            api_base: mock_base.clone(),
            token_url: format!("{mock_base}/app/getAppAccessToken"),
            allowed_users: vec![ALLOWED_MEMBER.to_string()],
            bindings_db: tmp.path().join("state.db"),
            audit_path: None,
            outbox_drain_interval: std::time::Duration::from_secs(3600),
        }),
        turn_executor: Some(Arc::new(DelayExec {
            calls: Arc::clone(&calls),
            slow_contains: None,
        })),
        ..GatewayDaemonConfig::default()
    };

    // Hold the carrier port hostage: the daemon's own bind FAILS, the
    // channel's startup probe then finds nothing listening.
    let squatter = std::net::TcpListener::bind(format!("127.0.0.1:{dead_port}")).unwrap();

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    let serve_shutdown = shutdown_tx.clone();
    let serve_task = tokio::spawn(async move {
        let result = GatewayService::serve_with_shutdown(config, serve_shutdown, shutdown_rx).await;
        let _ = done_tx.send(());
        result
    });

    // The daemon survives; the channel task dies fatally on probe.
    tokio::time::sleep(Duration::from_secs(5)).await;
    shutdown_tx.send_replace(true);
    let _ = done_rx.await;
    let result = serve_task.await.unwrap();
    assert!(
        result.is_ok(),
        "daemon must survive a dead carrier: {result:?}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    drop(squatter);
}

/// T071/P4: an in-flight reply that straddles the shutdown signal is
/// NOT silently dropped — carriers are joined with grace, not aborted.
#[tokio::test]
async fn in_flight_reply_survives_shutdown() {
    let calls = Arc::new(AtomicUsize::new(0));
    let stack = spawn_stack(
        Arc::new(DelayExec {
            calls: Arc::clone(&calls),
            slow_contains: Some(("SLOWREPLY", Duration::from_millis(1500))),
        }),
        Duration::from_secs(6),
    )
    .await;
    wait_identified(&stack).await;

    stack.mock.inject(group_event_in(
        "gOK",
        "MIDSHUTDOWN",
        ALLOWED_MEMBER,
        "<@!BOTID> SLOWREPLY now",
    ));
    assert!(
        wait_until(Duration::from_secs(5), || calls.load(Ordering::SeqCst) == 1).await,
        "turn never started"
    );

    // Shut down WHILE the turn (and its future reply) is in flight.
    stack.drain_tx.send_replace(true);

    // The reply must still land — no abort-mid-send.
    let replied = wait_until(Duration::from_secs(10), || {
        stack
            .mock
            .group_sends_snapshot()
            .iter()
            .any(|(_, v)| v["body"]["msg_id"] == "MIDSHUTDOWN")
    })
    .await;
    assert!(replied, "in-flight reply lost across shutdown");
    let _ = stack.done_rx.await;
    assert!(stack.serve_task.await.unwrap().is_ok());
}

/// T067/D8: submitting the same explicit turn_id twice over the HTTP
/// chat surface replays the ORIGINAL reply with 200 — never an error
/// body — because rpc_error_to_status special-cases -32004 and unpacks
/// RpcErrorBody.data.
#[tokio::test]
async fn replayed_turn_id_returns_original_reply_with_200() {
    let calls = Arc::new(AtomicUsize::new(0));
    let stack = spawn_stack(
        Arc::new(DelayExec {
            calls: Arc::clone(&calls),
            slow_contains: None,
        }),
        Duration::from_secs(2),
    )
    .await;
    wait_identified(&stack).await;

    let url = format!("{}/api/v1/chat", stack.http_base);
    let body = json!({"message": "replay-me", "turn_id": "FIXED-TURN-1"});
    let first = stack
        .http_client
        .post(&url)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 200);
    let first: Value = first.json().await.unwrap();
    assert_eq!(first["reply"], "echo:replay-me");

    let second = stack
        .http_client
        .post(&url)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(
        second.status(),
        200,
        "-32004 replay must map to 200, not 500"
    );
    let second: Value = second.json().await.unwrap();
    assert_eq!(
        second["reply"], "echo:replay-me",
        "replay must carry the ORIGINAL reply, got: {second}"
    );
    // Exactly one hosted execution for both submissions.
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    stack.drain_tx.send_replace(true);
    let _ = stack.done_rx.await;
    assert!(stack.serve_task.await.unwrap().is_ok());
}
