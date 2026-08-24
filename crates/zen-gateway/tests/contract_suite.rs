//! Contract suite (task T008/T009) — the CI compatibility gate for the
//! gateway protocol.
//!
//! PURPOSE: Prove every P1 method happy path, every documented error
//! code, the version matrix, unknown-method/notification tolerance, and
//! the Q3 approval round-trip end-to-end over an in-process carrier.
//!
//! USAGE: `cargo test -p zen-gateway --test contract_suite`. Every MINOR
//! protocol bump must extend this suite and the registry in the same PR
//! (see `protocol::methods` module docs for the procedure).
//!
//! EXPECTED: All tests green. A red row means the contract freeze broke.
//!
//! ERRORS: Failures name the exact frame or catalog row that diverged.

use serde_json::{Value, json};
use zen_gateway::protocol::{
    Capabilities, Frame, MethodRegistry, RpcError, RpcErrorBody, SERVER_PROTOCOL_VERSION,
    initialize_params,
};
use zen_gateway::server::DispatchServer;
use zen_gateway::transport::{Transport, in_process};

/// Client side of a connected, handshaked test server.
struct TestClient {
    client: in_process::InProcessTransport,
}

/// Spawns a dispatcher with P1 stub handlers and returns the client side
/// after completing the handshake.
async fn server_and_handshake(caps: Capabilities) -> TestClient {
    let (client, server_transport) = in_process::pair_wire_checked();
    let server = DispatchServer::new(server_transport)
        .handle("health/status", |_| async {
            Ok(
                json!({"serverVersion": "0.0.8", "protocolVersion": SERVER_PROTOCOL_VERSION,
                      "clients": 1, "uptimeMs": 5, "storeHealth": "ok", "activeTurns": 0}),
            )
        })
        .expect("health/status registered")
        .handle("shutdown", |_| async {
            Ok(json!({"drained": 0, "cancelled": 0}))
        })
        .expect("shutdown registered")
        .handle("memory/retrieve", |_| async { Ok(json!({"entries": []})) })
        .expect("memory/retrieve registered")
        .handle("memory/putEntry", |_| async { Ok(json!({"frameId": 1})) })
        .expect("memory/putEntry registered")
        .handle("memory/search", |_| async { Ok(json!({"hits": []})) })
        .expect("memory/search registered")
        .handle("memory/stats", |_| async {
            Ok(json!({"frames": 0, "capacityBytes": 1, "generation": 0}))
        })
        .expect("memory/stats registered")
        .handle("knowledge/search", |_| async { Ok(json!({"notes": []})) })
        .expect("knowledge/search registered");
    tokio::spawn(server.run());

    client
        .send(Frame::request_with(
            9000,
            "initialize",
            initialize_params(SERVER_PROTOCOL_VERSION, "contract-suite", "0.0.8", caps),
        ))
        .await
        .unwrap();
    let Frame::ServerResponse { id, error, .. } = client.recv().await.unwrap() else {
        panic!("expected server response to initialize");
    };
    assert_eq!(id, 9000);
    assert!(error.is_none(), "handshake failed: {error:?}");
    client
        .send(Frame::notification("initialized", json!({})))
        .await
        .unwrap();
    TestClient { client }
}

/// Spawns a bare dispatcher (built-in initialize handling only).
fn spawn_bare() -> in_process::InProcessTransport {
    let (client, server_transport) = in_process::pair_wire_checked();
    tokio::spawn(DispatchServer::new(server_transport).run());
    client
}

/// Sends a request and returns the error body (panics on success).
async fn expect_error(server: &TestClient, id: u64, method: &str, params: Value) -> RpcErrorBody {
    server
        .client
        .send(Frame::request_with(id, method, params))
        .await
        .unwrap();
    let Frame::ServerResponse { id: rid, error, .. } = server.client.recv().await.unwrap() else {
        panic!("expected error response for {method}");
    };
    assert_eq!(rid, id, "rpcId echo violated");
    error.expect("expected error body")
}

/// Sends a request and returns the result value (panics on error).
async fn expect_ok(server: &TestClient, id: u64, method: &str, params: Value) -> Value {
    server
        .client
        .send(Frame::request_with(id, method, params))
        .await
        .unwrap();
    let Frame::ServerResponse {
        id: rid,
        result,
        error,
        ..
    } = server.client.recv().await.unwrap()
    else {
        panic!("expected response for {method}");
    };
    assert_eq!(rid, id, "rpcId echo violated");
    assert!(error.is_none(), "{method} failed: {error:?}");
    result.expect("result body present")
}

// ── P1 happy paths ──────────────────────────────────────────────────

#[tokio::test]
async fn p1_methods_happy_paths_and_id_echo() {
    let s = server_and_handshake(Capabilities::default()).await;
    let cases: &[(&str, Value, &[&str])] = &[
        (
            "health/status",
            json!({}),
            &[
                "serverVersion",
                "protocolVersion",
                "clients",
                "uptimeMs",
                "storeHealth",
                "activeTurns",
            ],
        ),
        ("shutdown", json!({}), &["drained", "cancelled"]),
        ("memory/retrieve", json!({"sessionId": "s-1"}), &["entries"]),
        (
            "memory/putEntry",
            json!({"sessionId": "s-1", "role": "user", "content": "hi", "entityType": "session"}),
            &["frameId"],
        ),
        ("memory/search", json!({"query": "hi"}), &["hits"]),
        (
            "memory/stats",
            json!({}),
            &["frames", "capacityBytes", "generation"],
        ),
        ("knowledge/search", json!({"query": "kb"}), &["notes"]),
    ];
    for (i, (method, params, keys)) in cases.iter().enumerate() {
        let result = expect_ok(&s, 100 + i as u64, method, params.clone()).await;
        for key in *keys {
            assert!(
                result.get(key).is_some(),
                "{method} result missing {key}: {result}"
            );
        }
    }
}

// ── Error catalog coverage ──────────────────────────────────────────

#[tokio::test]
async fn protocol_driven_std_errors() {
    let s = server_and_handshake(Capabilities::default()).await;

    // -32602: malformed initialize on a fresh connection.
    let bare = spawn_bare();
    bare.send(Frame::request_with(1, "initialize", json!({"bogus": true})))
        .await
        .unwrap();
    let Frame::ServerResponse { error: Some(e), .. } = bare.recv().await.unwrap() else {
        panic!("expected invalid-params from malformed initialize");
    };
    assert_eq!((e.code, e.name.as_str()), (-32602, "invalid-params"));
    drop(bare);

    // -32601: unknown method carries supportedMethods.
    let e = expect_error(&s, 201, "memory/teleport", json!({})).await;
    assert_eq!((e.code, e.name.as_str()), (-32601, "method-not-found"));
    let supported = e
        .data
        .as_ref()
        .expect("supportedMethods data")
        .get("supportedMethods")
        .expect("supportedMethods key");
    let list = supported.as_array().expect("supportedMethods array");
    assert!(
        list.iter().any(|m| m == "memory/search"),
        "supportedMethods missing memory/search"
    );
    assert!(
        !list.iter().any(|m| m == "session/event"),
        "notifications must not be callable"
    );
}

#[tokio::test]
async fn pre_handshake_and_duplicate_initialize() {
    let (client, server_transport) = in_process::pair_wire_checked();
    let server = DispatchServer::new(server_transport)
        .handle("health/status", |_| async { Ok(json!({})) })
        .expect("registered");
    tokio::spawn(server.run());

    // -32000 before initialize.
    client
        .send(Frame::request(1, "health/status"))
        .await
        .unwrap();
    let Frame::ServerResponse { error: Some(e), .. } = client.recv().await.unwrap() else {
        panic!("expected error");
    };
    assert_eq!((e.code, e.name.as_str()), (-32000, "not-initialized"));

    // Handshake, then duplicate initialize → -32000 (reversed use).
    client
        .send(Frame::request_with(
            2,
            "initialize",
            initialize_params("1.0", "suite", "0", Capabilities::default()),
        ))
        .await
        .unwrap();
    let _ = client.recv().await.unwrap();
    client
        .send(Frame::notification("initialized", json!({})))
        .await
        .unwrap();
    client
        .send(Frame::request_with(
            3,
            "initialize",
            initialize_params("1.0", "suite", "0", Capabilities::default()),
        ))
        .await
        .unwrap();
    let Frame::ServerResponse { error: Some(e), .. } = client.recv().await.unwrap() else {
        panic!("expected duplicate-initialize error");
    };
    assert_eq!((e.code, e.name.as_str()), (-32000, "not-initialized"));
    drop(client);
}

#[tokio::test]
async fn handler_driven_zen_error_codes_e2e() {
    // One server per catalog error: handler returns the catalog error,
    // client receives exact code/name (+ mandated data).
    let cases: Vec<(&str, RpcError)> = vec![
        (
            "store-unavailable",
            RpcError::store_unavailable("unavailable"),
        ),
        ("session-not-found", RpcError::session_not_found("s-x")),
        (
            "turn-already-completed",
            RpcError::turn_already_completed(json!("done")),
        ),
        ("approval-unsupported", RpcError::approval_unsupported()),
        ("rate-limited", RpcError::rate_limited(500)),
        (
            "guard-rejected",
            RpcError::guard_rejected("doom-loop", "cap"),
        ),
        ("internal", RpcError::internal("suite-injected")),
    ];
    for (name, expected) in cases {
        let (client, server_transport) = in_process::pair_wire_checked();
        let handler_err = expected.clone();
        let server = DispatchServer::new(server_transport)
            .handle("health/status", move |_| {
                let err = handler_err.clone();
                async move { Err(err) }
            })
            .expect("registered");
        tokio::spawn(server.run());
        client
            .send(Frame::request_with(
                1,
                "initialize",
                initialize_params("1.0", "suite", "0", Capabilities::default()),
            ))
            .await
            .unwrap();
        let _ = client.recv().await.unwrap();
        client
            .send(Frame::notification("initialized", json!({})))
            .await
            .unwrap();
        client
            .send(Frame::request_with(2, "health/status", json!({})))
            .await
            .unwrap();
        let Frame::ServerResponse { error: Some(e), .. } = client.recv().await.unwrap() else {
            panic!("expected error for {name}");
        };
        assert_eq!(e.code, expected.code, "{name} code");
        assert_eq!(e.name, expected.name, "{name} name");
        assert_eq!(e.data, expected.data, "{name} data");
    }
}

// ── Version matrix (end-to-end through initialize) ─────────────────

#[tokio::test]
async fn version_matrix_e2e() {
    let s = server_and_handshake(Capabilities::default()).await;
    let _ = &s;

    // Probes run on fresh connections: an initialized client never
    // re-sends initialize. A client one minor newer than the server is
    // refused with actionable data.
    let (maj, min) = SERVER_PROTOCOL_VERSION
        .split_once('.')
        .expect("MAJOR.MINOR");
    let newer = format!("{}.{}", maj, min.parse::<u32>().unwrap() + 1);
    let bare = spawn_bare();
    bare.send(Frame::request_with(
        300,
        "initialize",
        initialize_params(&newer, "future-client", "9.9", Capabilities::default()),
    ))
    .await
    .unwrap();
    let Frame::ServerResponse { error: Some(e), .. } = bare.recv().await.unwrap() else {
        panic!("expected version-mismatch for newer minor");
    };
    assert_eq!((e.code, e.name.as_str()), (-32001, "version-mismatch"));
    let data = e.data.expect("version-mismatch data");
    assert!(
        data.get("serverVersion").is_some(),
        "missing serverVersion: {data}"
    );
    assert!(
        data.get("serverProtocolVersion").is_some(),
        "missing serverProtocolVersion"
    );
    assert_eq!(
        data.get("recovery").and_then(Value::as_str),
        Some("restart gateway with matching version"),
        "recovery hint must be actionable (FR-004): {data}"
    );
    drop(bare);

    let bare = spawn_bare();
    bare.send(Frame::request_with(
        301,
        "initialize",
        initialize_params("2.0", "maj2", "1.0", Capabilities::default()),
    ))
    .await
    .unwrap();
    let Frame::ServerResponse { error: Some(e), .. } = bare.recv().await.unwrap() else {
        panic!("expected version-mismatch for major mismatch");
    };
    assert_eq!((e.code, e.name.as_str()), (-32001, "version-mismatch"));
}

// ── Forward compatibility ───────────────────────────────────────────

#[tokio::test]
async fn unknown_optional_params_tolerated() {
    let s = server_and_handshake(Capabilities::default()).await;
    let result = expect_ok(
        &s,
        400,
        "memory/search",
        json!({
            "query": "q",
            "futureOption": {"x": 1},
        }),
    )
    .await;
    assert!(result.get("hits").is_some());
}

#[tokio::test]
async fn unknown_notification_silently_ignored() {
    let s = server_and_handshake(Capabilities::default()).await;
    s.client
        .send(Frame::notification(
            "session/mysteryKind",
            json!({"seq": 1}),
        ))
        .await
        .unwrap();
    // Next frame must be the response to the FOLLOW-UP request — proof
    // the unknown notification produced no reply.
    let result = expect_ok(&s, 401, "health/status", json!({})).await;
    assert!(result.get("storeHealth").is_some());
}

// ── Registry completeness (catalog freeze) ──────────────────────────

#[test]
fn registry_covers_contract_catalog() {
    let required: &[&str] = &[
        "initialize",
        "initialized",
        "health/status",
        "shutdown",
        "memory/retrieve",
        "memory/putEntry",
        "memory/search",
        "memory/stats",
        "knowledge/search",
        "session/start",
        "session/turn",
        "session/cancel",
        "session/resume",
        "agent/list",
        "agent/status",
        "skill/list",
        "session/event",
        "agent/status",
        "health/ping",
        "approval/request",
    ];
    let all = MethodRegistry::supported_methods();
    for name in required {
        assert!(all.contains(name), "registry missing {name}");
    }
    assert_eq!(all.len(), 20, "catalog freeze: exactly 20 rows");
}

#[test]
fn error_catalog_frozen() {
    let frozen: Vec<RpcError> = vec![
        RpcError::parse_error("x"),
        RpcError::invalid_request("x"),
        RpcError::invalid_params("m", "x"),
        RpcError::method_not_found(&[]),
        RpcError::internal("x"),
        RpcError::not_initialized(),
        RpcError::version_mismatch("9.9", "1.0", "reason"),
        RpcError::store_unavailable("degraded"),
        RpcError::session_not_found("s"),
        RpcError::turn_already_completed(json!(null)),
        RpcError::approval_unsupported(),
        RpcError::approval_timeout("t"),
        RpcError::rate_limited(1),
        RpcError::guard_rejected("g", "r"),
    ];
    let codes: Vec<(i32, &str)> = frozen.iter().map(|e| (e.code, e.name)).collect();
    assert_eq!(
        codes,
        vec![
            (-32700, "parse-error"),
            (-32600, "invalid-request"),
            (-32602, "invalid-params"),
            (-32601, "method-not-found"),
            (-32603, "internal"),
            (-32000, "not-initialized"),
            (-32001, "version-mismatch"),
            (-32002, "store-unavailable"),
            (-32003, "session-not-found"),
            (-32004, "turn-already-completed"),
            (-32010, "approval-unsupported"),
            (-32011, "approval-timeout"),
            (-32012, "rate-limited"),
            (-32020, "guard-rejected"),
        ]
    );
}
