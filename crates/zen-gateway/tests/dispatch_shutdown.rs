//! Dispatch hold-transport EOF contract (task T102): detached SPAWNED
//! tasks (`SPAWNED_METHODS` in `server/dispatch.rs`) pin the connection
//! carrier, so aborting the dispatch run loop alone can never deliver
//! EOF to the client — the sole EOF path is the server-side write
//! half-close `UdsTransport::shutdown_write`.
//!
//! PURPOSE: Mechanically prove the EOF contract documented on
//! `SPAWNED_METHODS`: (1) `abort` + `shutdown_write` fails an in-flight
//! spawned-method request fast with a visible "connection closed"
//! error; (2) `abort` alone leaves the request pending (bounded probe —
//! never a hang, never a premature EOF) because the detached handler
//! task still holds the `Arc<dyn Transport>`, and the SAME request
//! settles the moment `shutdown_write` lands — half-close is the sole
//! remaining EOF path.
//!
//! SCOPE NOTE: Harness follows the crash_recovery idiom (in-process
//! UDS pair, seen-guard for a deterministic mid-turn point, killer
//! handoff of the server-side transport) but aborts the dispatch run
//! loop DIRECTLY — precisely the teardown a connection owner performs.
//! Boot order is bind → dial (kernel backlog) → accept → serve, so the
//! accept never waits on a later step. The spawn model is intentionally
//! unchanged: responsiveness of the recv loop beats owned teardown.
//!
//! USAGE: cargo test -p zen-gateway --test dispatch_shutdown
//!
//! EXPECTED: both scenarios settle in well under a second each.
//!
//! ERRORS: an in-flight turn that settles after `abort` alone (EOF
//! leaked without a write half-close), a turn that does not settle
//! within 5s of `shutdown_write`, or a hung probe fails the test.

use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde_json::{Value, json};
use zen_gateway::client::GatewayClient;
use zen_gateway::protocol::{Capabilities, RpcErrorBody};
use zen_gateway::server::dispatch::DispatchServer;
use zen_gateway::transport::{
    Transport,
    uds::{self, UdsTransport},
};

/// How long an in-flight turn may stay pending after `abort` alone:
/// it must NOT settle (the spawned handler still pins the carrier);
/// the bound keeps the scenario hang-free.
const ABORT_ALONE_PROBE: Duration = Duration::from_millis(300);

/// Upper bound for the turn to settle once `shutdown_write` lands.
const EOF_SETTLE: Duration = Duration::from_secs(5);

/// Everything one scenario needs: the dispatch run-loop handle (abort ≈
/// abrupt teardown, no drain), the served connection's server side for
/// `shutdown_write`, the dialed client, and the in-flight turn task.
struct Scenario {
    run: tokio::task::JoinHandle<anyhow::Result<()>>,
    killer: Arc<UdsTransport>,
    client: GatewayClient,
    turn: tokio::task::JoinHandle<Result<Value, RpcErrorBody>>,
}

/// Boots one full scenario in deadlock-free order: bind the socket,
/// dial the client (connect lands in the kernel backlog), accept the
/// connection, serve it with a dispatcher whose `session/turn` (a
/// SPAWNED_METHOD) records the turn id and then stalls forever, then
/// put exactly one turn in flight. Returns once the handler has
/// observably started — the deterministic mid-turn point.
async fn boot_scenario(
    sock: &Path,
    seen: Arc<StdMutex<Vec<String>>>,
    turn_id: &'static str,
) -> anyhow::Result<Scenario> {
    let listener = uds::bind_socket(sock).await?;
    let client = GatewayClient::connect(sock).await?;
    let side = Arc::new(uds::accept_transport(&listener).await?);
    let transport: Arc<dyn Transport> = side.clone();
    let handler_seen = Arc::clone(&seen);
    let server = DispatchServer::from_arc(transport)
        .handle("session/turn", move |params| {
            let seen = Arc::clone(&handler_seen);
            async move {
                seen.lock().expect("seen lock").push(
                    params
                        .get("turnId")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                );
                // Hosted turn never completes: teardown lands first.
                loop {
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                }
            }
        })
        .expect("session/turn is a registered method");
    let run = tokio::spawn(server.run());

    client
        .handshake("dispatch-shutdown", "0.0", Capabilities::default())
        .await
        .expect("handshake");
    let turn_client = client.clone();
    let turn = tokio::spawn(async move {
        turn_client
            .request_timeout(
                "session/turn",
                json!({"turnId": turn_id, "sessionId": "s1", "prompt": "eof contract"}),
                Duration::from_secs(30),
            )
            .await
    });
    wait_for_handler(&seen, turn_id).await;
    Ok(Scenario {
        run,
        killer: side,
        client,
        turn,
    })
}

/// Bounded wait (2s, 2ms cadence) until the spawned `session/turn`
/// handler has recorded `turn_id` — the deterministic mid-turn point
/// before any teardown.
async fn wait_for_handler(seen: &StdMutex<Vec<String>>, turn_id: &str) {
    for _ in 0..1000 {
        if seen.lock().expect("seen lock").iter().any(|t| t == turn_id) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("turn must be in flight before teardown");
}

/// The kill path a connection owner must use: abort the run loop AND
/// half-close the write side — the detached spawned handler pins the
/// carrier, so the half-close is what actually delivers EOF.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn abort_plus_shutdown_write_delivers_eof_to_in_flight_turn() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("eof-kill.sock");
    let seen = Arc::new(StdMutex::new(Vec::new()));
    let scenario = boot_scenario(&sock, seen, "t-kill")
        .await
        .expect("boot dispatch");

    scenario.run.abort();
    scenario
        .killer
        .shutdown_write()
        .await
        .expect("kill connection");

    let outcome = tokio::time::timeout(EOF_SETTLE, scenario.turn)
        .await
        .expect("in-flight turn must settle within 5s")
        .expect("turn task join");
    let rpc_err = outcome.expect_err("killed turn must fail, never silently succeed");
    assert!(
        rpc_err.message.contains("connection closed"),
        "visible failure expected, got: {}",
        rpc_err.message
    );
    assert!(
        !scenario.client.is_connected(),
        "client must flag the dead link"
    );
    scenario.client.close().await;
}

/// Abort alone must NOT deliver EOF (the spawned handler still holds
/// the `Arc<dyn Transport>` — the carrier stays half-alive) and the
/// scenario must not hang; the same turn settles the instant
/// `shutdown_write` lands, proving the half-close is the sole
/// remaining EOF path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn abort_alone_cannot_deliver_eof_then_shutdown_write_does() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("eof-alone.sock");
    let seen = Arc::new(StdMutex::new(Vec::new()));
    let mut scenario = boot_scenario(&sock, seen, "t-alone")
        .await
        .expect("boot dispatch");

    scenario.run.abort();

    let probe = tokio::time::timeout(ABORT_ALONE_PROBE, &mut scenario.turn).await;
    assert!(
        probe.is_err(),
        "abort alone must NOT deliver EOF: in-flight turn settled early — {probe:?}"
    );

    scenario
        .killer
        .shutdown_write()
        .await
        .expect("shutdown_write");
    let outcome = tokio::time::timeout(EOF_SETTLE, scenario.turn)
        .await
        .expect("in-flight turn must settle within 5s of shutdown_write")
        .expect("turn task join");
    let rpc_err = outcome.expect_err("EOF turn must fail, never silently succeed");
    assert!(
        rpc_err.message.contains("connection closed"),
        "visible failure expected, got: {}",
        rpc_err.message
    );
    assert!(
        !scenario.client.is_connected(),
        "client must flag the dead link"
    );
    scenario.client.close().await;
}
