//! SC-005 crash-recovery automation (task T047): killing the gateway
//! mid-turn is recovered (respawn + reconnect) within 3 seconds;
//! unrecoverable cases end in visible degraded mode within the retry
//! budget — never a hang and never silent data loss.
//!
//! PURPOSE: Proves the SC-005 acceptance criteria mechanically at the
//! UDS-client level — scenario A kills the daemon while a hosted turn
//! is in flight and requires the next connect-or-spawn to respawn,
//! re-handshake, re-register the session, and replay the SAME turn id
//! to completion within the 3s budget across all trials; scenario B
//! forces respawn/connect to be impossible and requires the failure to
//! surface fast (client error / `OfflineDegraded` banner) instead of
//! hanging.
//!
//! SCOPE NOTE: "Daemon" here follows the spawn_race idiom — an
//! in-process accept loop behind an injected `DaemonSpawnFn` (test
//! binaries cannot exec themselves as `zen`, and the process-wide
//! `GATEWAY_OPEN` sole-owner claim would not release on an aborted
//! real `GatewayService` task). "SIGKILL" is simulated by aborting the
//! daemon task — abrupt, no drain, stale socket file left on disk —
//! plus a hard write-shutdown of the served connection (the spawned
//! turn-handler task pins the fd, so `shutdown_write` is what delivers
//! the client-side EOF, the same mechanism the surface unit tests use).
//!
//! USAGE: cargo test -p zen-gateway --test crash_recovery
//!
//! EXPECTED: 5/5 kill trials recover in well under 3s each (SC-005
//! requires ≥95%; 5 trials ⇒ every one must pass); degraded-mode trials
//! settle in milliseconds, the impossible-bind trial within the 10s
//! readiness retry budget. Suite runtime ≈ 12s.
//!
//! ERRORS: any panic names the broken guarantee — recovery slower than
//! 3s, an in-flight turn that neither failed nor completed within 5s,
//! a degraded-mode path that did not settle within its bound, or a
//! respawn that lost the session/turn identity.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use serde_json::json;
use zen_gateway::client::{
    DaemonSpawnFn, GatewayClient, GatewayLinkState, SurfaceClient, SurfaceError,
};
use zen_gateway::protocol::{Capabilities, SERVER_PROTOCOL_VERSION};
use zen_gateway::server::dispatch::DispatchServer;
use zen_gateway::transport::uds::{self, UdsTransport};

/// SC-005 recovery budget: respawn + reconnect within 3 seconds.
const SC005_RECOVERY_BUDGET: Duration = Duration::from_secs(3);

/// Scenario A trial count. SC-005 demands ≥95% success; with 5 trials
/// every single one must recover inside the budget (4/5 = 80% < 95%).
const TRIALS: usize = 5;

/// Canned dispatcher used by respawned daemons: `session/start` echoes
/// the requested id; `session/turn` completes with a recovery marker.
/// The initialize/initialized handshake is dispatcher-built-in.
fn canned_server(side: Arc<UdsTransport>) -> anyhow::Result<DispatchServer> {
    DispatchServer::from_arc(side)
        .handle("session/start", |params| async move {
            let id = params
                .get("sessionId")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("minted")
                .to_string();
            Ok(json!({"sessionId": id, "agent": "auto"}))
        })?
        .handle("session/turn", |params| async move {
            let turn_id = params
                .get("turnId")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            Ok(json!({"turnId": turn_id, "response": "recovered-after-crash"}))
        })
}

/// Boots the pre-crash daemon on `sock`: binds (sole-owner claim),
/// serves exactly one connection whose `session/turn` handler records
/// the turn id and then stalls forever — a hosted turn still in flight
/// when the daemon dies. Returns the daemon task handle (abort ≈
/// SIGKILL: abrupt, no drain, stale socket file left behind) and the
/// served connection's server side for the hard write-shutdown.
#[allow(clippy::type_complexity)]
async fn stalling_daemon(
    sock: &Path,
    seen: Arc<StdMutex<Vec<String>>>,
) -> (
    tokio::task::JoinHandle<()>,
    tokio::sync::oneshot::Receiver<Arc<UdsTransport>>,
) {
    let sock = sock.to_path_buf();
    let (killer_tx, killer_rx) = tokio::sync::oneshot::channel();
    let daemon = tokio::spawn(async move {
        let Ok(listener) = uds::bind_socket(&sock).await else {
            return;
        };
        let Ok(side) = uds::accept_transport(&listener).await else {
            return;
        };
        let side = Arc::new(side);
        let server = DispatchServer::from_arc(side.clone())
            .handle("session/start", |params| async move {
                let id = params
                    .get("sessionId")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("minted")
                    .to_string();
                Ok(json!({"sessionId": id, "agent": "auto"}))
            })
            .and_then(|server| {
                server.handle("session/turn", move |params| {
                    let seen = Arc::clone(&seen);
                    async move {
                        seen.lock().expect("seen lock").push(
                            params
                                .get("turnId")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        );
                        // Hosted turn never completes: the daemon dies first.
                        loop {
                            tokio::time::sleep(Duration::from_secs(3600)).await;
                        }
                    }
                })
            });
        let Ok(server) = server else {
            return;
        };
        let _ = killer_tx.send(side);
        tokio::spawn(server.run());
        // Daemon keeps "running" until it is killed.
        loop {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    });
    (daemon, killer_rx)
}

/// Retry-dials a freshly started daemon until the socket answers
/// (≤2s, 25ms cadence — sole_owner's `connect_client` pattern).
async fn connect_with_retry(sock: &Path) -> anyhow::Result<GatewayClient> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match GatewayClient::connect(sock).await {
            Ok(client) => return Ok(client),
            Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(e) => anyhow::bail!("daemon socket never came up: {e}"),
        }
    }
}

/// One SC-005 scenario-A trial: dial → handshake → session → in-flight
/// turn → SIGKILL → respawn+reconnect (≤3s) → session re-registered and
/// the SAME turn id replayed to completion. Returns the measured
/// kill→recovered wall-clock time.
async fn kill_midturn_trial(trial: usize) -> Duration {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock = tmp.path().join(format!("crash-{trial}.sock"));
    let seen = Arc::new(StdMutex::new(Vec::new()));

    // Daemon boots on a fresh socket (atomic bind = sole-owner claim).
    let (daemon, killer_rx) = stalling_daemon(&sock, Arc::clone(&seen)).await;

    // Client dials, handshakes, and registers a session.
    let client = connect_with_retry(&sock).await.expect("dial daemon");
    client
        .handshake("crash-recovery", "0.0", Capabilities::default())
        .await
        .expect("handshake");
    let session_id = format!("sess-crash-{trial}");
    let started = client
        .request("session/start", json!({"sessionId": session_id}))
        .await
        .expect("session/start");
    assert_eq!(started["sessionId"], json!(session_id));

    // A hosted turn goes in flight on the live UDS link.
    let turn_id = format!("turn-crash-{trial}");
    let turn_client = client.clone();
    let turn_params = json!({
        "turnId": turn_id,
        "sessionId": session_id,
        "prompt": "mid-turn kill",
    });
    let turn = tokio::spawn(async move {
        turn_client
            .request_timeout("session/turn", turn_params, Duration::from_secs(20))
            .await
    });
    // Deterministic mid-turn point: the daemon has the request recorded.
    for _ in 0..1000 {
        if !seen.lock().expect("seen lock").is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(
        seen.lock().expect("seen lock").len(),
        1,
        "turn must be in flight before the kill"
    );
    let killer = tokio::time::timeout(Duration::from_secs(5), killer_rx)
        .await
        .expect("killer handoff bounded")
        .expect("killer channel live");

    // KILL (SIGKILL semantics): abrupt daemon abort — listener drops
    // without drain, stale socket file stays on disk — plus a hard
    // write-shutdown so the in-flight connection delivers EOF now.
    daemon.abort();
    killer.shutdown_write().await.expect("kill connection");

    // Never a silent hang: the in-flight turn fails visibly and fast.
    let outcome = tokio::time::timeout(Duration::from_secs(5), turn)
        .await
        .expect("in-flight turn must settle within 5s")
        .expect("turn task join");
    let rpc_err = outcome.expect_err("killed turn must fail, never silently succeed");
    assert!(
        rpc_err.message.contains("connection closed"),
        "visible failure expected, got: {}",
        rpc_err.message
    );
    assert!(!client.is_connected(), "client must flag the dead link");

    // Respawn + reconnect within the SC-005 budget: connect_or_spawn
    // probes the stale file, cleans it, invokes the injected spawner,
    // and waits for readiness — all bounded well under 3s in-process.
    let respawn_sock = sock.clone();
    let respawn: DaemonSpawnFn = Arc::new(move || {
        let sock = respawn_sock.clone();
        tokio::spawn(async move {
            let Ok(listener) = uds::bind_socket(&sock).await else {
                return;
            };
            loop {
                if let Ok(side) = uds::accept_transport(&listener).await
                    && let Ok(server) = canned_server(Arc::new(side))
                {
                    let _ = server.run().await;
                }
            }
        });
        Ok(())
    });
    let t0 = Instant::now();
    let client2 = tokio::time::timeout(Duration::from_secs(10), async {
        let client = GatewayClient::connect_or_spawn(&sock, Some(respawn)).await?;
        client
            .handshake("crash-recovery", "0.0", Capabilities::default())
            .await?;
        Ok::<GatewayClient, anyhow::Error>(client)
    })
    .await
    .expect("respawn+reconnect bounded")
    .expect("recovery failed");
    let elapsed = t0.elapsed();
    assert!(
        elapsed <= SC005_RECOVERY_BUDGET,
        "SC-005: respawn+handshake took {elapsed:?} > 3s"
    );

    // Never silent data loss: the same session re-registers and the
    // SAME turn id replays to completion on the respawned daemon.
    let re_started = client2
        .request("session/start", json!({"sessionId": session_id}))
        .await
        .expect("re-register session");
    assert_eq!(re_started["sessionId"], json!(session_id));
    let replay = client2
        .request_timeout(
            "session/turn",
            json!({
                "turnId": turn_id,
                "sessionId": session_id,
                "prompt": "mid-turn kill",
            }),
            Duration::from_secs(5),
        )
        .await
        .expect("turn replay after recovery");
    assert_eq!(replay["response"], json!("recovered-after-crash"));

    // Exactly one daemon owns the recovered socket.
    assert!(
        uds::bind_socket(&sock).await.is_err(),
        "single daemon after respawn"
    );

    client.close().await;
    client2.close().await;
    elapsed
}

/// Scenario A (SC-005 happy path): kill the daemon mid-turn; the next
/// connect-or-spawn must respawn + reconnect (handshake ok) within 3
/// seconds on every trial.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kill_midturn_respawns_and_reconnects_within_3s_across_trials() {
    let mut worst = Duration::ZERO;
    for trial in 0..TRIALS {
        let elapsed = tokio::time::timeout(Duration::from_secs(20), kill_midturn_trial(trial))
            .await
            .unwrap_or_else(|_| panic!("trial {trial} timed out"));
        worst = worst.max(elapsed);
    }
    // ≥95% of trials ⇒ 5/5 here; every trial already asserted ≤3s.
    eprintln!("SC-005 scenario A: {TRIALS}/{TRIALS} trials recovered, worst {worst:?}");
}

/// Scenario B1 (impossible respawn): when the spawner itself fails
/// (binary missing / launcher denied — the injectable equivalent of an
/// unwritable socket dir), connect_or_spawn must surface the error
/// fast, never hang.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn impossible_spawn_fails_visibly_without_hanging() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("denied.sock");
    let spawn_fn: DaemonSpawnFn =
        Arc::new(|| Err(anyhow::anyhow!("respawn impossible: launcher denied")));

    let t0 = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        GatewayClient::connect_or_spawn(&sock, Some(spawn_fn)),
    )
    .await
    .expect("connect_or_spawn must settle, never hang");
    let err = match result {
        Err(e) => e,
        Ok(_) => panic!("impossible spawn must surface an error, got a live link"),
    };
    assert!(
        err.to_string().contains("respawn impossible"),
        "spawn failure must stay visible, got: {err}"
    );
    assert!(
        t0.elapsed() < SC005_RECOVERY_BUDGET,
        "impossible spawn must fail fast, took {:?}",
        t0.elapsed()
    );
}

/// Scenario B2 (visible degraded): a live surface whose link dies
/// mid-session redials on the next call; when every redial lands on a
/// dead peer (unrecoverable), the surface must end in
/// `OfflineDegraded` with the contract banner — bounded, never a hang.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unrecoverable_peer_lands_surface_in_offline_degraded() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("degraded.sock");
    let listener = Arc::new(uds::bind_socket(&sock).await.unwrap());

    // Connection 1: the healthy pre-crash link. The acceptor reports
    // the connection's server side (killer) and its serve task so the
    // test can kill the link hard once the surface is live on it.
    let (c1_tx, c1_rx) = tokio::sync::oneshot::channel();
    {
        let listener = Arc::clone(&listener);
        tokio::spawn(async move {
            let Ok(side) = uds::accept_transport(&listener).await else {
                return;
            };
            let side = Arc::new(side);
            let Ok(server) = canned_server(Arc::clone(&side)) else {
                return;
            };
            let serve = tokio::spawn(server.run());
            let _ = c1_tx.send((side, serve));
        });
    }

    let surface = tokio::time::timeout(
        Duration::from_secs(10),
        SurfaceClient::open(sock.clone(), "crash-b2", "0.0"),
    )
    .await
    .expect("open must settle")
    .expect("surface must open on the live link");
    assert_eq!(
        surface.link_state().banner(),
        format!("gateway: ok (v{SERVER_PROTOCOL_VERSION})")
    );
    let sid = tokio::time::timeout(
        Duration::from_secs(5),
        surface.ensure_session(Some("s-b2"), None),
    )
    .await
    .expect("initial session bounded")
    .expect("initial session/start");
    assert_eq!(sid, "s-b2");

    let (killer, serve) = tokio::time::timeout(Duration::from_secs(5), c1_rx)
        .await
        .expect("c1 report bounded")
        .expect("c1 channel live");

    // KILL the link mid-session: abort the serve task and hard-close
    // the server side, then give the surface's reader a deterministic
    // margin to observe EOF and flag the cached link dead.
    serve.abort();
    killer.shutdown_write().await.expect("kill link");
    drop(killer);
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Every further accept is a dead peer: the redial can attach but
    // the handshake dies immediately — respawn is unrecoverable.
    {
        let listener = Arc::clone(&listener);
        tokio::spawn(async move {
            loop {
                if uds::accept_transport(&listener).await.is_err() {
                    break;
                }
            }
        });
    }

    let t0 = Instant::now();
    let outcome = tokio::time::timeout(
        Duration::from_secs(15),
        surface.ensure_session(Some("s-b2"), None),
    )
    .await
    .expect("degraded landing must settle within the retry budget — never a hang");
    let err = match outcome {
        Err(e) => e,
        Ok(sid) => panic!("unrecoverable redial must fail visibly, got session {sid}"),
    };
    assert!(matches!(err, SurfaceError::Offline(_)), "got: {err}");
    assert_eq!(
        surface.link_state(),
        GatewayLinkState::OfflineDegraded,
        "banner state must be degraded"
    );
    assert_eq!(
        surface.link_state().banner(),
        "gateway: offline — memory & agent features degraded (retrying)"
    );
    assert!(
        t0.elapsed() < SC005_RECOVERY_BUDGET,
        "degraded landing must be fast, took {:?}",
        t0.elapsed()
    );
}

/// Scenario B3 (impossible bind): when the socket path itself can
/// neither be dialled nor bound (here: occupied by a non-socket node —
/// the deterministic equivalent of an unwritable socket dir such as
/// /proc/nonexistent/gateway.sock), SurfaceClient::open must fail
/// visibly with an Offline-class error inside its readiness retry
/// budget instead of hanging. The embedded respawn fails at the atomic
/// bind, before any store is opened.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unwritable_socket_dir_open_fails_visibly_within_retry_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let sock: PathBuf = tmp.path().join("blocked.sock");
    std::fs::create_dir(&sock).expect("occupy the socket path");

    let t0 = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        SurfaceClient::open(sock, "crash-b3", "0.0"),
    )
    .await
    .expect("open must settle within the retry budget — never a hang");
    let err = match result {
        Err(e) => e,
        Ok(_) => panic!("impossible socket path must fail visibly, opened a link"),
    };
    assert!(matches!(err, SurfaceError::Offline(_)), "got: {err}");
    assert!(
        err.to_string().contains("gateway not ready"),
        "failure must name the readiness budget, got: {err}"
    );
    assert!(
        t0.elapsed() <= Duration::from_secs(15),
        "must stay inside the bounded retry budget, took {:?}",
        t0.elapsed()
    );
}
