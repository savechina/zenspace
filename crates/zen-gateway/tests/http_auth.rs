//! HTTP-carrier authentication (CSO audit #1): the bearer gate on the
//! loopback HTTP carrier, the `/health` exemption, the WS token paths
//! (header OR `?token=`), and the cross-origin drive-by defense.
//!
//! PURPOSE: Pin the auth contract — `Some(non-empty)` secret arms the
//! gate on every route except `/health`; `None` keeps the historical
//! loopback-only posture byte-for-byte; a foreign `Origin` header is
//! rejected regardless of auth state.
//!
//! USAGE: cargo test -p zen-gateway --test http_auth
//!
//! EXPECTED: missing/wrong bearer → 401 `{"error":"unauthorized"}`;
//! correct bearer → handler reached; `/health` always open; WS upgrade
//! without token → 401 handshake rejection; foreign Origin → 403.
//!
//! ERRORS: any step panic pinpoints which auth guarantee broke.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use zen_gateway::transport::http::{HttpCarrierConfig, router, router_with_auth};

const SECRET: &str = "test-bearer-secret";

fn daemon_config(dir: &std::path::Path) -> zen_gateway::GatewayDaemonConfig {
    zen_gateway::GatewayDaemonConfig {
        socket_path: dir.join("gateway.sock"),
        memory_path: Some(dir.join("memory.mv2")),
        db_path: Some(dir.join("state.db")),
        audit_path: Some(dir.join("audit.jsonl")),
        ..Default::default()
    }
}

/// Scripted hosted-turn executor (legacy_surface.rs parity): proves the
/// chat handler is REACHED (not merely gated) without a live provider.
struct ScriptedExecutor;

#[async_trait::async_trait]
impl zen_gateway::server::hosting::TurnExecutor for ScriptedExecutor {
    async fn execute_stream(
        &self,
        _session: &mut zen_core::types::SessionContext,
        prompt: &str,
        callback: &mut (dyn FnMut(String) + Send),
    ) -> anyhow::Result<String> {
        callback("echo:".to_string());
        Ok(format!("echo: {prompt}"))
    }
}

/// Serves the carrier router on an ephemeral loopback port; the TempDir
/// guard must stay bound for the service's lifetime.
async fn spawn_carrier(secret: Option<String>) -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let config = zen_gateway::GatewayDaemonConfig {
        turn_executor: Some(Arc::new(ScriptedExecutor)),
        ..daemon_config(tmp.path())
    };
    let service = Arc::new(
        zen_gateway::GatewayService::open(&config)
            .await
            .expect("service open"),
    );
    let app = router_with_auth(service, secret);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app.into_make_service()).await;
    });
    (tmp, format!("http://{addr}"))
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

async fn post_json(client: &reqwest::Client, url: &str, body: Value) -> (u16, Value) {
    let resp = tokio::time::timeout(Duration::from_secs(15), client.post(url).json(&body).send())
        .await
        .expect("http round-trip timeout")
        .expect("request failed");
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    let parsed = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::Null)
    };
    (status, parsed)
}

fn ws_url(base: &str) -> String {
    format!("{}/api/v1/ws", base.replace("http://", "ws://"))
}

/// (a) Missing token → 401 `{"error":"unauthorized"}`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_without_token_is_unauthorized_when_secret_set() {
    let (tmp, base) = spawn_carrier(Some(SECRET.into())).await;
    let (status, body) = post_json(
        &client(),
        &format!("{base}/api/v1/chat"),
        json!({"message": "hi"}),
    )
    .await;
    assert_eq!(status, 401);
    assert_eq!(body["error"], "unauthorized");
    zen_memory::memvid::clear_global_memvid_cache();
    drop(tmp);
}

/// (b) Correct bearer → the chat handler is actually reached.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_with_correct_bearer_reaches_handler() {
    let (tmp, base) = spawn_carrier(Some(SECRET.into())).await;
    let resp = client()
        .post(format!("{base}/api/v1/chat"))
        .bearer_auth(SECRET)
        .json(&json!({"message": "hello"}))
        .send()
        .await
        .expect("request failed");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["reply"], "echo: hello");
    zen_memory::memvid::clear_global_memvid_cache();
    drop(tmp);
}

/// (c) `/health` is the unauthenticated liveness probe.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_is_exempt_from_auth() {
    let (tmp, base) = spawn_carrier(Some(SECRET.into())).await;
    let resp = client()
        .get(format!("{base}/health"))
        .send()
        .await
        .expect("request failed");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "healthy");
    zen_memory::memvid::clear_global_memvid_cache();
    drop(tmp);
}

/// (d) Wrong token → 401 (constant-time compare, same rejection shape).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_with_wrong_token_is_unauthorized() {
    let (tmp, base) = spawn_carrier(Some(SECRET.into())).await;
    let resp = client()
        .post(format!("{base}/api/v1/chat"))
        .bearer_auth("wrong-secret")
        .json(&json!({"message": "hi"}))
        .send()
        .await
        .expect("request failed");
    assert_eq!(resp.status().as_u16(), 401);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "unauthorized");
    zen_memory::memvid::clear_global_memvid_cache();
    drop(tmp);
}

/// Every non-`/health` route is gated, not just chat.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agents_route_is_gated_too() {
    let (tmp, base) = spawn_carrier(Some(SECRET.into())).await;
    let resp = client()
        .get(format!("{base}/api/v1/agents"))
        .send()
        .await
        .expect("request failed");
    assert_eq!(resp.status().as_u16(), 401);
    zen_memory::memvid::clear_global_memvid_cache();
    drop(tmp);
}

/// (e) WS upgrade without a token is rejected before the upgrade.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ws_upgrade_without_token_is_rejected() {
    let (tmp, base) = spawn_carrier(Some(SECRET.into())).await;
    let err = tokio_tungstenite::connect_async(ws_url(&base))
        .await
        .expect_err("upgrade must be rejected");
    let status = match &err {
        tokio_tungstenite::tungstenite::Error::Http(resp) => Some(resp.status().as_u16()),
        _ => None,
    };
    assert_eq!(status, Some(401), "unexpected handshake error: {err}");
    zen_memory::memvid::clear_global_memvid_cache();
    drop(tmp);
}

/// WS clients that cannot set headers authenticate via `?token=`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ws_upgrade_with_query_token_upgrades() {
    let (tmp, base) = spawn_carrier(Some(SECRET.into())).await;
    let url = format!("{}?token={SECRET}", ws_url(&base));
    let (mut socket, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("upgrade with query token must succeed");
    let _ = socket.close(None).await;
    zen_memory::memvid::clear_global_memvid_cache();
    drop(tmp);
}

/// (f) A foreign `Origin` header is rejected even with a valid token.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cross_origin_chat_is_rejected() {
    let (tmp, base) = spawn_carrier(Some(SECRET.into())).await;
    let resp = client()
        .post(format!("{base}/api/v1/chat"))
        .header("Origin", "http://evil.example")
        .bearer_auth(SECRET)
        .json(&json!({"message": "hi"}))
        .send()
        .await
        .expect("request failed");
    assert_eq!(resp.status().as_u16(), 403);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "forbidden origin");
    zen_memory::memvid::clear_global_memvid_cache();
    drop(tmp);
}

/// Allowed origins (exact match) pass through to the handler.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn allowed_origin_passes_through() {
    let (tmp, base) = spawn_carrier(Some(SECRET.into())).await;
    let resp = client()
        .post(format!("{base}/api/v1/chat"))
        .header("Origin", "http://localhost")
        .bearer_auth(SECRET)
        .json(&json!({"message": "hi"}))
        .send()
        .await
        .expect("request failed");
    assert_eq!(resp.status().as_u16(), 200);
    zen_memory::memvid::clear_global_memvid_cache();
    drop(tmp);
}

/// The Origin gate is independent of token auth: armed even when no
/// secret is configured.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn origin_rejected_even_without_secret() {
    let (tmp, base) = spawn_carrier(None).await;
    let resp = client()
        .post(format!("{base}/api/v1/chat"))
        .header("Origin", "http://evil.example")
        .json(&json!({"message": "hi"}))
        .send()
        .await
        .expect("request failed");
    assert_eq!(resp.status().as_u16(), 403);
    zen_memory::memvid::clear_global_memvid_cache();
    drop(tmp);
}

/// (g) `jwt_secret=None` → no auth required (byte-equivalent old
/// behavior for headerless loopback clients).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_secret_keeps_loopback_only_posture() {
    let (tmp, base) = spawn_carrier(None).await;
    let (status, body) = post_json(
        &client(),
        &format!("{base}/api/v1/chat"),
        json!({"message": "hello"}),
    )
    .await;
    assert_eq!(status, 200, "body: {body}");
    assert_eq!(body["reply"], "echo: hello");
    zen_memory::memvid::clear_global_memvid_cache();
    drop(tmp);
}

/// `router()` (the legacy entry) stays auth-free; `router_with_auth`
/// with `None` is its byte-equivalent twin.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_router_entry_remains_auth_free() {
    let tmp = tempfile::tempdir().unwrap();
    let config = zen_gateway::GatewayDaemonConfig {
        turn_executor: Some(Arc::new(ScriptedExecutor)),
        ..daemon_config(tmp.path())
    };
    let service = Arc::new(
        zen_gateway::GatewayService::open(&config)
            .await
            .expect("service open"),
    );
    let app = router(service);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app.into_make_service()).await;
    });
    let base = format!("http://{addr}");
    let (status, body) = post_json(
        &client(),
        &format!("{base}/api/v1/chat"),
        json!({"message": "hi"}),
    )
    .await;
    assert_eq!(status, 200, "body: {body}");
    zen_memory::memvid::clear_global_memvid_cache();
    drop(tmp);
}

/// Loopback-only enforcement is untouched by the auth work.
#[test]
fn loopback_validation_still_refuses_non_loopback() {
    let non_loopback = HttpCarrierConfig {
        bind_addr: "0.0.0.0".to_string(),
        port: 9876,
    };
    assert!(non_loopback.validate_loopback().is_err());
}
