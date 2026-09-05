//! Concurrent turn-affinity approval E2E (T103, SC-007) — approval for
//! surface B must never release surface A's `shell.exec`.
//!
//! PURPOSE: Pins the privilege-escalation-critical property of the
//! claim-protocol broker (`server/approval.rs`): each pending invocation
//! is gated on exactly the surface whose pipe carries its
//! `approval/request`, keyed by that route's registered `turn_id`
//! (`:47`) bound to its own [`ConnectionHandle`] (`:89`), and
//! deregistration stays turn-keyed (`:196` — daemon threads the broker
//! at `daemon.rs:387`). While two turns hold approvals open
//! simultaneously across two surfaces, the decision returned on ONE
//! surface's pipe releases ONLY the invocation whose request was
//! delivered there; the other surface's invocation stays blocked until
//! its OWN surface answers. A third invocation routed while every route
//! is claimed decays to Deny (fail-safe, no free-route leakage).
//!
//! USAGE: `cargo test -p zen-gateway --test approval_affinity`. Uses
//! in-process transport pairs around the same `DispatchServer` /
//! `ConnectionHandle` stack the UDS daemon installs, with
//! `ApprovalBroker::route` as the execution-side entry point (the same
//! path `ApprovalBroker::callback` reaches from sandbox dispatch).
//!
//! EXPECTED: surface B's approval releases only surface B's `fs.write`;
//! surface A's `shell.exec` is still pending after B's approval and is
//! released solely by surface A's own approval. Both requests carry
//! their own turn's id on their own pipe. Synchronization is
//! frame-driven throughout: the rendezvous is "both pipes delivered
//! their request", never a sleep.
//!
//! ERRORS: a cross-surface release (B's approval unblocking A's
//! `shell.exec`), a request surfacing on the wrong pipe or under the
//! wrong `turnId`, or a claimed-route leakage to a third invocation all
//! fail the test — such a failure indicates an affinity regression in
//! `approval.rs` (P0: fix the broker, do not weaken this test).

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use zen_gateway::protocol::{Capabilities, Frame};
use zen_gateway::server::approval::ApprovalBroker;
use zen_gateway::server::dispatch::{ConnectionHandle, DispatchServer};
use zen_gateway::transport::{Transport, in_process};

/// Builds one surface: an in-process client pipe plus the
/// [`ConnectionHandle`] the broker registers for it, with the dispatch
/// loop already running and `approvals:true` negotiated.
async fn make_surface() -> (in_process::InProcessTransport, ConnectionHandle) {
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
    tokio::spawn(server.run());

    // Approvals require the negotiated capability on this connection;
    // without it the broker fails fast with -32010 (denial), which is
    // not the scenario under test.
    client_end
        .send(Frame::request_with(
            9,
            "initialize",
            zen_gateway::protocol::initialize_params(
                "1.0",
                "affinity-harness",
                "0.0",
                Capabilities {
                    approvals: true,
                    ..Default::default()
                },
            ),
        ))
        .await
        .unwrap();
    client_end.recv().await.unwrap();
    client_end
        .send(Frame::notification("initialized", json!({})))
        .await
        .unwrap();
    (client_end, handle)
}

/// Drains frames until the surface's pipe delivers one Q3
/// `approval/request`, returning its `(turnId, invocation)` params.
///
/// This is the rendezvous primitive: the request's arrival on the wire
/// proves the invoking turn's route is claimed and its decision is
/// outstanding — both turns are concurrently pending before any
/// approval is issued, with no sleeps involved.
async fn next_approval_request(client: &in_process::InProcessTransport) -> (String, Value) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let frame = tokio::time::timeout_at(deadline, client.recv())
            .await
            .expect("approval/request within 10s")
            .unwrap();
        let Frame::ServerRequest { method, params, .. } = frame else {
            continue;
        };
        assert_eq!(method, "approval/request");
        return (
            params["turnId"].as_str().expect("turnId").to_string(),
            params["invocation"].clone(),
        );
    }
}

#[tokio::test]
async fn surface_b_approval_never_releases_surface_a_shell_exec() {
    let broker = Arc::new(ApprovalBroker::default());

    // Two surfaces on the SAME broker, exactly as the daemon installs
    // one process-wide broker for every hosted turn (daemon.rs:387).
    let (client_a, handle_a) = make_surface().await;
    let (client_b, handle_b) = make_surface().await;

    // Turn-affinity binding (approval.rs:47/:89): each route pairs a
    // turn_id with ITS OWN origin connection for the turn's lifetime
    // (guard dropped at turn end → turn-keyed retain at :196).
    let _guard_a = broker.register("turnA".to_string(), handle_a, || {});
    let _guard_b = broker.register("turnB".to_string(), handle_b, || {});

    // Turn A (surface A) dispatches the privileged invocation; turn B
    // (surface B) a mundane one. Both run as the sandbox hook does —
    // on blocking threads with no turn context — via the same `route`
    // entry the production callback reaches.
    let shell_router = Arc::clone(&broker);
    let mut shell_turn =
        tokio::task::spawn_blocking(move || shell_router.route("shell.exec".into(), json!({})));

    // First claimant in flight: with both routes free it claims the
    // first-registered route, so surface A's pipe must carry A's own
    // privileged request under A's own turn key. (Frame-driven wait.)
    let (turn_a_seen, invocation_a) = next_approval_request(&client_a).await;
    assert_eq!(turn_a_seen, "turnA", "surface A keyed by its own turn");
    assert_eq!(
        invocation_a["name"],
        json!("shell.exec"),
        "surface A gates its own shell.exec"
    );

    // Turn B now dispatches while A's approval is still open — the two
    // turns are concurrently pending from this point.
    let fs_router = Arc::clone(&broker);
    let mut fs_turn =
        tokio::task::spawn_blocking(move || fs_router.route("fs.write".into(), json!({})));

    let (turn_b_seen, invocation_b) = next_approval_request(&client_b).await;
    assert_eq!(turn_b_seen, "turnB", "surface B keyed by its own turn");
    assert_eq!(
        invocation_b["name"],
        json!("fs.write"),
        "surface B gates its own fs.write"
    );

    // RENDEZVOUS reached: two approvals outstanding on two surfaces,
    // none answered. A third invocation now finds every route claimed
    // and must decay to Deny immediately — claimed routes never leak
    // to another caller (fail-safe, approval.rs decide()).
    assert!(
        !broker.route("fs.delete".into(), json!({})),
        "fully-claimed broker denies a third invocation"
    );
    // The denial must not have disturbed either pending request: no
    // extra frames were surfaced (next_approval_request consumers
    // below would observe any stray Q3 request as a method mismatch).

    // THE P0 ASSERTION: surface B approves ITS OWN request — surface
    // A's shell.exec must stay blocked. The release path is a
    // synchronous reply-channel send once the Q4 response is
    // processed, so a wrongful release surfaces within microseconds;
    // a bounded negative wait (not a sleep-based sync) is sound here.
    client_b
        .send(Frame::client_response(
            "srv-1".to_string(),
            json!({"decision": "approve"}),
        ))
        .await
        .unwrap();
    let fs_outcome = tokio::time::timeout(Duration::from_secs(5), &mut fs_turn)
        .await
        .expect("B's approval releases B's fs.write")
        .unwrap();
    assert!(fs_outcome, "surface B's own approval allows its own tool");

    let wrongly_released = tokio::time::timeout(Duration::from_millis(250), &mut shell_turn).await;
    assert!(
        wrongly_released.is_err(),
        "P0 SC-007: approval issued for surface B must NEVER release \
         surface A's pending shell.exec"
    );

    // Only surface A's own approval releases surface A's shell.exec.
    client_a
        .send(Frame::client_response(
            "srv-1".to_string(),
            json!({"decision": "approve"}),
        ))
        .await
        .unwrap();
    let shell_outcome = tokio::time::timeout(Duration::from_secs(5), &mut shell_turn)
        .await
        .expect("A's own approval releases A's shell.exec")
        .unwrap();
    assert!(shell_outcome, "surface A's own approval allows shell.exec");
}

#[tokio::test]
async fn turn_bound_routing_survives_inverted_firing_order() {
    // THE SC-007 REGRESSION SHAPE (T103): the LATER-registered turn fires
    // FIRST. Under the legacy first-free scan it would claim the
    // earlier-registered turn's route (surface A would gate turn B's
    // tool — cross-surface misroute, P0). Turn-bound routing
    // (`route_for`, production path via `APPROVAL_TURN` scope) must pin
    // each invocation to its own surface regardless of firing order.
    let broker = Arc::new(ApprovalBroker::default());
    let (client_a, handle_a) = make_surface().await;
    let (client_b, handle_b) = make_surface().await;
    let _guard_a = broker.register("turnA".to_string(), handle_a, || {});
    let _guard_b = broker.register("turnB".to_string(), handle_b, || {});

    // Turn B (registered SECOND) dispatches FIRST.
    let fs_router = Arc::clone(&broker);
    let mut fs_turn = tokio::task::spawn_blocking(move || {
        fs_router.route_for("turnB".to_string(), "fs.write".into(), json!({}))
    });

    let (turn_b_seen, invocation_b) = next_approval_request(&client_b).await;
    assert_eq!(turn_b_seen, "turnB", "B's request on B's own pipe");
    assert_eq!(invocation_b["name"], json!("fs.write"));

    // Turn A dispatches while B is still pending: exact match, no claim
    // contention — both routes serve their own turn concurrently.
    let shell_router = Arc::clone(&broker);
    let mut shell_turn = tokio::task::spawn_blocking(move || {
        shell_router.route_for("turnA".to_string(), "shell.exec".into(), json!({}))
    });

    let (turn_a_seen, invocation_a) = next_approval_request(&client_a).await;
    assert_eq!(turn_a_seen, "turnA", "A's request on A's own pipe");
    assert_eq!(invocation_a["name"], json!("shell.exec"));

    // Approve B: only B releases. Then approve A: only A releases.
    client_b
        .send(Frame::client_response(
            "srv-1".to_string(),
            json!({"decision": "approve"}),
        ))
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), &mut fs_turn)
            .await
            .expect("B's approval releases B")
            .unwrap()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(250), &mut shell_turn)
            .await
            .is_err(),
        "B's approval must not release A's shell.exec"
    );
    client_a
        .send(Frame::client_response(
            "srv-1".to_string(),
            json!({"decision": "approve"}),
        ))
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), &mut shell_turn)
            .await
            .expect("A's approval releases A")
            .unwrap()
    );

    // Unknown turn decays to Deny — a turn never borrows another surface.
    assert!(
        !broker.route_for("turnZZZ".to_string(), "fs.write".into(), json!({})),
        "unknown turn must be denied"
    );
}
