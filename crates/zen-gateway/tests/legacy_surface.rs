//! Absorbed-surface integration (task T043, FR-019/020): the loopback
//! HTTP carrier serves the legacy `/api/v1` shapes through the SAME
//! MethodRegistry dispatcher as UDS, the MCP offering excludes
//! Confidential tools identically to stdio, and non-loopback binds are
//! refused.
//!
//! PURPOSE: Regression parity for pre-existing HTTP clients plus the
//! FR-020 sensitivity guarantee — one dispatcher, two carriers.
//!
//! USAGE: cargo test -p zen-gateway --test legacy_surface
//!
//! EXPECTED: `/health` keeps its legacy shape; empty chat → 400
//! `Empty message`; MCP `tools/list` never contains `shell.exec`;
//! `validate_loopback("0.0.0.0")` errors.
//!
//! ERRORS: any step panic pinpoints which parity guarantee broke.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use zen_gateway::transport::http::{HttpCarrierConfig, router};

fn daemon_config(dir: &std::path::Path) -> zen_gateway::GatewayDaemonConfig {
    zen_gateway::GatewayDaemonConfig {
        socket_path: dir.join("gateway.sock"),
        memory_path: Some(dir.join("memory.mv2")),
        db_path: Some(dir.join("state.db")),
        ..Default::default()
    }
}

/// Scripted hosted-turn executor (design P4 isomorphic seam): proves the
/// chat carrier path end-to-end without a live LLM provider.
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_http_surface_parity_and_mcp_filtering() {
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

    // Serve the carrier router on an ephemeral loopback port.
    let app = router(Arc::clone(&service));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app.into_make_service()).await;
    });

    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    // Legacy /health shape: {status, version, agents} (parity with the
    // retired routes.rs health_check).
    let health: Value = serde_json::from_str(
        &client
            .get(format!("{base}/health"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(health["status"], "healthy");
    assert_eq!(health["version"], env!("CARGO_PKG_VERSION"));
    assert!(health["agents"].is_u64());

    // Empty message → 400 "Empty message" (legacy chat_handler parity).
    let (status, body) = post_json(
        &client,
        &format!("{base}/api/v1/chat"),
        json!({"message": ""}),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["reply"], "Empty message");
    assert!(body["agent"].is_null());

    // Non-empty chat rides session/start + session/turn through the
    // dispatcher; the scripted executor answers deterministically.
    let (status, body) = post_json(
        &client,
        &format!("{base}/api/v1/chat"),
        json!({"message": "hello", "sessionId": "legacy-parity"}),
    )
    .await;
    assert_eq!(status, 200, "chat body: {body}");
    assert_eq!(body["reply"], "echo: hello");
    assert!(body["agent"].is_null() || body["agent"].is_string());

    // /api/v1/agents keeps the list shape.
    let agents: Value = serde_json::from_str(
        &client
            .get(format!("{base}/api/v1/agents"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(agents["agents"].is_array());

    // MCP Streamable-HTTP: initialize handshake.
    let (mcp_status, init) = post_json(
        &client,
        &format!("{base}/api/v1/mcp"),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2024-11-05", "clientInfo": {"name": "t", "version": "0"}}
        }),
    )
    .await;
    assert_eq!(mcp_status, 200);
    assert_eq!(init["result"]["protocolVersion"], "2024-11-05");

    // tools/list MUST exclude Confidential tools (shell.exec) — identical
    // filtering to the stdio server (FR-020).
    let (_, tools) = post_json(
        &client,
        &format!("{base}/api/v1/mcp"),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .await;
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(!names.is_empty(), "filtered registry must expose tools");
    assert!(
        !names.contains(&"shell.exec"),
        "Confidential tool leaked onto the HTTP carrier"
    );

    // Unknown tool → JSON-RPC error -32602.
    let (_, bad_call) = post_json(
        &client,
        &format!("{base}/api/v1/mcp"),
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "no.such.tool", "arguments": {}}
        }),
    )
    .await;
    assert_eq!(bad_call["error"]["code"], -32602);

    // Loopback-only enforcement (FR-019).
    let non_loopback = HttpCarrierConfig {
        bind_addr: "0.0.0.0".to_string(),
        port: 9876,
    };
    assert!(non_loopback.validate_loopback().is_err());

    server.abort();
    zen_memory::memvid::clear_global_memvid_cache();
}
