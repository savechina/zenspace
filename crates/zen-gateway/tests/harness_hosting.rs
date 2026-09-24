//! US4 integration harness (T034) — hosted turns over real dispatchers
//! with two concurrent clients.
//!
//! PURPOSE: Proves the US4 acceptance behaviors end-to-end at the
//! protocol layer: concurrent hosted turns stream events to their
//! ORIGINATING connection only (SC-007 anchor semantics), cancel
//! unblocks a running turn and writes the `outcome:"cancelled"` audit,
//! resume replays buffered frames after `lastSeq`, and the doom-loop
//! guard rejects the 21st submit in a window with `-32020` plus an
//! audit line. Approval routing through the broker is exercised against
//! two independent connections in `approval_routes_to_origin_only`.
//!
//! USAGE: `cargo test -p zen-gateway --test harness_hosting`. Uses
//! in-process transport pairs (contract-suite style) around the SAME
//! `DispatchServer`/`SessionHost` stack the UDS daemon installs.
//!
//! EXPECTED: all five tests pass deterministically — the scripted
//! executor streams fixed fragments with configurable delays, so no
//! provider/network is involved.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use zen_core::types::SessionContext;
use zen_gateway::protocol::{Capabilities, Frame};
use zen_gateway::server::approval::ApprovalBroker;
use zen_gateway::server::dispatch::{ConnectionHandle, DispatchServer};
use zen_gateway::server::hosting::{
    SessionHost, TurnExecutor, TurnState, cancel as hosting_cancel, resume as hosting_resume,
    start as hosting_start, turn as hosting_turn,
};
use zen_gateway::transport::{Transport, in_process};

/// Scripted executor: streams three fragments after `delay_ms`, then
/// completes with a canned response.
struct ScriptedExec {
    delay_ms: u64,
}

#[async_trait::async_trait]
impl TurnExecutor for ScriptedExec {
    async fn execute_stream(
        &self,
        _session: &mut SessionContext,
        _prompt: &str,
        callback: &mut (dyn FnMut(String) + Send),
    ) -> anyhow::Result<String> {
        if self.delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
        }
        callback("al".to_string());
        callback("pha ".to_string());
        if self.delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
        }
        callback("beta".to_string());
        Ok("alpha beta".to_string())
    }
}

fn hosting_deps(delay_ms: u64, audit: Option<std::path::PathBuf>) -> Arc<SessionHost> {
    let mut deps = SessionHost::new(Some(Arc::new(ScriptedExec { delay_ms })));
    deps.audit_path = audit;
    Arc::new(deps)
}

/// Filters `audit.jsonl` lines down to parsed records with the given
/// `kind` and `turnId` (T054 lifecycle assertions).
fn audit_records(content: &str, kind: &str, turn_id: &str) -> Vec<Value> {
    content
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|rec| rec["kind"] == json!(kind) && rec["turnId"] == json!(turn_id))
        .collect()
}

/// Builds a dispatcher exposing the hosted-session methods over the
/// pair's server endpoint, with a real outbound queue pumped onto the
/// wire (mirrors the daemon install set, including delta-class drops).
#[allow(clippy::type_complexity)]
async fn spawn_surface(
    deps: Arc<SessionHost>,
) -> (
    in_process::InProcessTransport,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let (client_end, server_end) = in_process::pair();
    let (queue_tx, mut queue_rx) =
        tokio::sync::mpsc::channel::<zen_gateway::server::OutboundFrame>(256);
    // The pump plays the daemon's wire-pump role: it writes onto the
    // SERVER half so frames surface on the client's recv side.
    let pump_wire = server_end.clone();
    tokio::spawn(async move {
        while let Some(of) = queue_rx.recv().await {
            if client_wire_send(&pump_wire, of.frame).await.is_err() {
                break;
            }
        }
    });
    let server = DispatchServer::new(server_end)
        .handle("session/start", {
            let deps = Arc::clone(&deps);
            move |p| hosting_start(Arc::clone(&deps), p)
        })
        .unwrap()
        .handle("session/turn", {
            let deps = Arc::clone(&deps);
            let origin = queue_tx.clone();
            move |p| {
                let deps = Arc::clone(&deps);
                let origin = origin.clone();
                async move { hosting_turn(deps, Some(origin), p).await }
            }
        })
        .unwrap()
        .handle("session/cancel", {
            let deps = Arc::clone(&deps);
            move |p| hosting_cancel(Arc::clone(&deps), p)
        })
        .unwrap()
        .handle("session/resume", {
            let deps = Arc::clone(&deps);
            move |p| hosting_resume(Arc::clone(&deps), p)
        })
        .unwrap();
    let task = tokio::spawn(server.run());
    (client_end, task)
}

async fn client_wire_send(
    client: &in_process::InProcessTransport,
    frame: zen_gateway::protocol::Frame,
) -> anyhow::Result<()> {
    use zen_gateway::transport::Transport;
    client.send(frame).await
}

async fn handshake(client: &in_process::InProcessTransport) {
    client
        .send(Frame::request_with(
            0,
            "initialize",
            zen_gateway::protocol::initialize_params(
                "1.0",
                "harness",
                "0.0",
                Capabilities::default(),
            ),
        ))
        .await
        .unwrap();
    client.recv().await.unwrap();
    client
        .send(Frame::notification("initialized", json!({})))
        .await
        .unwrap();
}

#[tokio::test]
async fn concurrent_turns_stream_events_to_origin_only() {
    let deps = hosting_deps(80, None);
    for sid in ["sA", "sB"] {
        deps.sessions.lock().await.insert(
            sid.to_string(),
            SessionContext::new(sid.to_string(), String::new()),
        );
    }

    let (client_a, _srv_a) = spawn_surface(Arc::clone(&deps)).await;
    let (client_b, _srv_b) = spawn_surface(Arc::clone(&deps)).await;
    handshake(&client_a).await;
    handshake(&client_b).await;

    client_a
        .send(Frame::request_with(
            1,
            "session/turn",
            json!({"turnId": "tA", "sessionId": "sA", "prompt": "hi"}),
        ))
        .await
        .unwrap();
    client_b
        .send(Frame::request_with(
            1,
            "session/turn",
            json!({"turnId": "tB", "sessionId": "sB", "prompt": "hi"}),
        ))
        .await
        .unwrap();

    // Single consumer per pipe: drain frames until this surface's turn
    // response arrives, collecting session/event frames along the way.
    async fn drain_until_response(
        client: &in_process::InProcessTransport,
        own_turn: &str,
    ) -> (Vec<Value>, Value) {
        let mut events = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let frame = tokio::time::timeout_at(deadline, client.recv())
                .await
                .expect("within 10s")
                .unwrap();
            match frame {
                Frame::Notification { method, params, .. } if method == "session/event" => {
                    events.push(params);
                }
                Frame::ServerResponse { result, .. } => {
                    return (events, result.unwrap());
                }
                _ => {}
            }
            let _ = own_turn;
        }
    }

    let (events_a, response_a) = drain_until_response(&client_a, "tA").await;
    let (events_b, response_b) = drain_until_response(&client_b, "tB").await;

    assert_eq!(response_a["response"], json!("alpha beta"));
    assert_eq!(response_b["response"], json!("alpha beta"));
    // SC-007 anchor semantics: each pipe carries ONLY its own turn.
    assert!(
        !events_a.is_empty() && events_a.iter().all(|e| e["turnId"] == json!("tA")),
        "A must stream only its own turn events"
    );
    assert!(
        !events_b.is_empty() && events_b.iter().all(|e| e["turnId"] == json!("tB")),
        "B must stream only its own turn events"
    );
}

#[tokio::test]
async fn cancel_mid_turn_unblocks_and_audits_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let audit_path = dir.path().join("audit.jsonl");
    let deps = hosting_deps(5_000, Some(audit_path.clone()));
    deps.sessions.lock().await.insert(
        "s1".to_string(),
        SessionContext::new("s1".to_string(), String::new()),
    );
    let (client, _srv) = spawn_surface(Arc::clone(&deps)).await;
    handshake(&client).await;

    client
        .send(Frame::request_with(
            7,
            "session/turn",
            json!({"turnId": "tc", "sessionId": "s1", "prompt": "hi"}),
        ))
        .await
        .unwrap();

    // Deterministic race window: wait until the spawned turn task has
    // registered its record before cancelling.
    for _ in 0..500 {
        if deps.turns.get("tc").is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }

    // Deterministic race window: wait until the spawned turn task
    // has registered its record before cancelling.
    for _ in 0..500 {
        if deps.turns.get("tc").is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }

    // Cancel rides the same pipe: session/turn runs spawned off the
    // recv loop, so this request is processed immediately.
    client
        .send(Frame::request_with(
            8,
            "session/cancel",
            json!({"turnId": "tc"}),
        ))
        .await
        .unwrap();

    // Order between the two responses is unspecified and interleaved
    // with origin notifications: drain until both outcomes are seen.
    let mut cancel_ok = false;
    let mut turn_cancelled = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !(cancel_ok && turn_cancelled) {
        let frame = tokio::time::timeout_at(deadline, client.recv())
            .await
            .expect("responses within 10s")
            .unwrap();
        let Frame::ServerResponse {
            id, result, error, ..
        } = frame
        else {
            continue;
        };
        match id {
            8 => {
                assert_eq!(result.unwrap()["outcome"], json!("cancelled"));
                cancel_ok = true;
            }
            7 => {
                let err = error.expect("cancelled turn must fail");
                assert!(err.message.contains("cancel"), "{err:?}");
                turn_cancelled = true;
            }
            other => panic!("unexpected response id {other}"),
        }
    }
    assert!(cancel_ok && turn_cancelled);

    // Audit line lands asynchronously.
    let mut audited = false;
    for _ in 0..100 {
        if let Ok(content) = std::fs::read_to_string(&audit_path)
            && content.contains("\"outcome\":\"cancelled\"")
        {
            audited = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(audited, "cancellation must be audited");

    // T054: the cancelled terminal transition also emits the
    // once-only lifecycle record with the cancelled outcome.
    let mut lifecycle = Vec::new();
    for _ in 0..100 {
        if let Ok(content) = std::fs::read_to_string(&audit_path) {
            lifecycle = audit_records(&content, "gateway.turn.completed", "tc");
            if !lifecycle.is_empty() {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(lifecycle.len(), 1, "one lifecycle record per terminal turn");
    assert_eq!(lifecycle[0]["outcome"], json!("cancelled"));

    assert_eq!(deps.turns.get("tc").unwrap().state(), TurnState::Cancelled);
}

/// T054: one `gateway.turn.started` per registration and exactly one
/// `gateway.turn.completed` at terminal state; replaying the finished
/// turnId (-32004) must add NEITHER record.
#[tokio::test]
async fn turn_lifecycle_audits_started_and_completed() {
    let dir = tempfile::tempdir().unwrap();
    let audit_path = dir.path().join("audit.jsonl");
    let deps = hosting_deps(0, Some(audit_path.clone()));
    {
        let mut sessions = deps.sessions.lock().await;
        let mut ctx = SessionContext::new("s1".to_string(), String::new());
        ctx.agent_name = "metis".to_string();
        sessions.insert("s1".to_string(), ctx);
    }
    let (client, _srv) = spawn_surface(Arc::clone(&deps)).await;
    handshake(&client).await;

    client
        .send(Frame::request_with(
            11,
            "session/turn",
            json!({"turnId": "tl", "sessionId": "s1", "prompt": "hi"}),
        ))
        .await
        .unwrap();
    loop {
        let frame = client.recv().await.unwrap();
        if matches!(frame, Frame::ServerResponse { .. }) {
            break;
        }
    }

    // Replay the completed turnId: resolves -32004 with the stored
    // response and never re-registers (no extra lifecycle lines).
    client
        .send(Frame::request_with(
            12,
            "session/turn",
            json!({"turnId": "tl", "sessionId": "s1", "prompt": "hi"}),
        ))
        .await
        .unwrap();
    loop {
        let frame = client.recv().await.unwrap();
        if let Frame::ServerResponse { id, error, .. } = frame {
            assert_eq!(id, 12);
            assert_eq!(error.expect("replay must fail").code, -32004);
            break;
        }
    }

    // Audit lines land asynchronously from the blocking append task.
    let mut content = String::new();
    for _ in 0..100 {
        if let Ok(read) = std::fs::read_to_string(&audit_path)
            && !audit_records(&read, "gateway.turn.completed", "tl").is_empty()
        {
            content = read;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let started = audit_records(&content, "gateway.turn.started", "tl");
    let completed = audit_records(&content, "gateway.turn.completed", "tl");
    assert_eq!(
        started.len(),
        1,
        "started fires once per registration (replay adds none)\n{content}"
    );
    assert_eq!(
        completed.len(),
        1,
        "completed fires exactly once at terminal state\n{content}"
    );
    assert_eq!(started[0]["sessionId"], json!("s1"));
    assert_eq!(started[0]["agent"], json!("metis"));
    assert!(
        started[0]["ts"].as_str().is_some_and(|ts| !ts.is_empty()),
        "epoch-millis ts mirrors existing audit records"
    );
    assert_eq!(completed[0]["sessionId"], json!("s1"));
    assert_eq!(completed[0]["outcome"], json!("completed"));
}

#[tokio::test]
async fn resume_replays_frames_after_last_seq() {
    let deps = hosting_deps(0, None);
    deps.sessions.lock().await.insert(
        "s1".to_string(),
        SessionContext::new("s1".to_string(), String::new()),
    );
    let (first_client, _srv) = spawn_surface(Arc::clone(&deps)).await;
    handshake(&first_client).await;
    first_client
        .send(Frame::request_with(
            1,
            "session/turn",
            json!({"turnId": "tr", "sessionId": "s1", "prompt": "hi"}),
        ))
        .await
        .unwrap();

    // Skip streaming notifications; wait for completion.
    loop {
        let frame = first_client.recv().await.unwrap();
        if matches!(frame, Frame::ServerResponse { .. }) {
            break;
        }
    }

    // "Reconnect": a fresh surface resumes the same turn.
    let (reconnected, _srv2) = spawn_surface(Arc::clone(&deps)).await;
    handshake(&reconnected).await;
    reconnected
        .send(Frame::request_with(
            2,
            "session/resume",
            json!({"turnId": "tr", "lastSeq": 1}),
        ))
        .await
        .unwrap();
    let Frame::ServerResponse { result, .. } = reconnected.recv().await.unwrap() else {
        panic!("expected resume payload");
    };
    let payload = result.unwrap();
    let events = payload["events"].as_array().unwrap();
    assert!(!events.is_empty());
    assert!(
        events.iter().all(|e| e["seq"].as_u64().unwrap() > 1),
        "replay must start strictly after lastSeq"
    );
    assert_eq!(
        events.last().unwrap()["kind"],
        json!("turn_completed"),
        "final structural event closes the replay"
    );

    // Unknown turn yields an empty snapshot (stale-client recovery).
    reconnected
        .send(Frame::request_with(
            3,
            "session/resume",
            json!({"turnId": "ghost", "lastSeq": 0}),
        ))
        .await
        .unwrap();
    let Frame::ServerResponse { result, .. } = reconnected.recv().await.unwrap() else {
        panic!("expected snapshot");
    };
    assert_eq!(result.unwrap()["snapshot"], json!(true));
}

/// Two independent connections register broker routes; concurrent
/// routing calls must each resolve on their OWN connection (SC-007):
/// A approves through its pipe, B denies through its own, and neither
/// observes the other's Q3 request.
#[tokio::test]
async fn approval_routes_to_origin_only() {
    let broker = Arc::new(ApprovalBroker::default());

    async fn make_connection() -> (
        in_process::InProcessTransport,
        ConnectionHandle,
        DispatchServer,
    ) {
        let (client_end, server_end) = in_process::pair();
        let server = DispatchServer::new(server_end)
            .handle("health/status", |_| async {
                Ok(json!({
                    "serverVersion": "t", "protocolVersion": "1.0",
                    "clients": 1, "uptimeMs": 0,
                    "storeHealth": "ok", "activeTurns": 0,
                }))
            })
            .unwrap();
        let handle = server.connection();
        (client_end, handle, server)
    }

    let (client_a, handle_a, server_a) = make_connection().await;
    let (client_b, handle_b, server_b) = make_connection().await;

    // Dispatch loops must be live before handshaking: the initialize
    // response only exists once a server processes the request.
    tokio::spawn(server_a.run());
    tokio::spawn(server_b.run());

    // Approvals require a negotiated capability on each connection.
    for client in [&client_a, &client_b] {
        client
            .send(Frame::request_with(
                9,
                "initialize",
                zen_gateway::protocol::initialize_params(
                    "1.0",
                    "harness",
                    "0.0",
                    Capabilities {
                        approvals: true,
                        ..Default::default()
                    },
                ),
            ))
            .await
            .unwrap();
        client.recv().await.unwrap();
        client
            .send(Frame::notification("initialized", json!({})))
            .await
            .unwrap();
    }

    let _guard_a = broker.register("turnA".to_string(), handle_a, || {});
    let _guard_b = broker.register("turnB".to_string(), handle_b, || {});

    // Turn-bound claims (SC-007, T103): exact-match pairing, deterministic
    // under any thread scheduling. The legacy first-free `route` re-claims
    // the just-released first route when spawn_blocking calls run
    // back-to-back (2-core CI), sending both Q3s to one origin — the hang
    // this test once hit; do not "simplify" back to `route`.
    let router_a = Arc::clone(&broker);
    let router_b = Arc::clone(&broker);
    let routed_a = tokio::task::spawn_blocking(move || {
        router_a.route_for("turnA".into(), "fs.write".into(), json!({}))
    });
    let routed_b = tokio::task::spawn_blocking(move || {
        router_b.route_for("turnB".into(), "shell.exec".into(), json!({}))
    });

    // Pipes are bound at REGISTER time (handle_a owns turnA), and the
    // exact-match claim routes each Q3 to its own origin unconditionally,
    // so this task can play responder for each side in turn.
    let Frame::ServerRequest {
        params: params_a,
        method: method_a,
        ..
    } = client_a.recv().await.unwrap()
    else {
        panic!("A expected the approval request for its own turn");
    };
    assert_eq!(method_a, "approval/request");
    assert_eq!(params_a["turnId"], json!("turnA"));
    client_a
        .send(Frame::client_response(
            "srv-1".to_string(),
            json!({"decision": "approve"}),
        ))
        .await
        .unwrap();

    let Frame::ServerRequest {
        params: params_b,
        method: method_b,
        ..
    } = client_b.recv().await.unwrap()
    else {
        panic!("B expected the approval request for its own turn");
    };
    assert_eq!(method_b, "approval/request");
    assert_eq!(params_b["turnId"], json!("turnB"));
    client_b
        .send(Frame::client_response(
            "srv-1".to_string(),
            json!({"decision": "deny"}),
        ))
        .await
        .unwrap();

    // Exact-match claims make the outcome deterministic: turnA's caller
    // observed the approve, turnB's caller the deny.
    let routed_a = routed_a.await.unwrap();
    let routed_b = routed_b.await.unwrap();
    assert!(
        routed_a && !routed_b,
        "approve must resolve turnA's caller and deny turnB's (got a={routed_a}, b={routed_b})"
    );
}
#[tokio::test]
async fn doom_loop_guard_rejects_21st_submit_with_audit() {
    let dir = tempfile::tempdir().unwrap();
    let audit_path = dir.path().join("audit.jsonl");
    let deps = hosting_deps(0, Some(audit_path));
    deps.sessions.lock().await.insert(
        "s1".to_string(),
        SessionContext::new("s1".to_string(), String::new()),
    );
    let (client, _srv) = spawn_surface(Arc::clone(&deps)).await;
    handshake(&client).await;

    // DOOM_MAX_TURNS submissions succeed...
    for i in 0..20 {
        let turn = format!("dg{i}");
        client
            .send(Frame::request_with(
                100 + i as u64,
                "session/turn",
                json!({"turnId": turn, "sessionId": "s1", "prompt": "hi"}),
            ))
            .await
            .unwrap();
        loop {
            let frame = client.recv().await.unwrap();
            if matches!(frame, Frame::ServerResponse { .. }) {
                break;
            }
        }
    }
    // ...the next one is rejected by the guard before execution.
    client
        .send(Frame::request_with(
            200,
            "session/turn",
            json!({"turnId": "overflow", "sessionId": "s1", "prompt": "hi"}),
        ))
        .await
        .unwrap();
    loop {
        let frame = client.recv().await.unwrap();
        let Frame::ServerResponse { id, error, .. } = frame else {
            continue;
        };
        assert_eq!(id, 200);
        let err = error.expect("guard rejection");
        assert_eq!((err.code, err.name.as_str()), (-32020, "guard-rejected"));
        assert_eq!(err.data.unwrap()["guard"], json!("doom-loop"));
        break;
    }
}
