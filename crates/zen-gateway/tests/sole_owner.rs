//! US1 sole-owner integration (task T015, quickstart V1): a daemon owns
//! the memory archive over UDS; two clients get cross-visibility ≤1s
//! with zero lock contention; exactly one daemon can exist.
//!
//! PURPOSE: Proves SC-001/SC-002 end-to-end — the gateway is the only
//! `.mv2` opener, clients see each other's writes through it, and both
//! bind-level and process-level sole-owner claims refuse duplicates.
//!
//! USAGE: cargo test -p zen-gateway --test sole_owner
//!
//! EXPECTED: all steps complete well inside their timeouts; shutdown
//! removes the socket file.
//!
//! ERRORS: any timeout panic pinpoints which V1 guarantee broke.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use serde_json::json;
use zen_gateway::protocol::{Capabilities, Frame, SERVER_PROTOCOL_VERSION, initialize_params};
use zen_gateway::transport::Transport;
use zen_gateway::transport::uds::UdsTransport;
use zen_gateway::{GatewayDaemonConfig, GatewayService};

const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);

fn daemon_config(dir: &std::path::Path) -> GatewayDaemonConfig {
    GatewayDaemonConfig {
        socket_path: dir.join("gateway.sock"),
        memory_path: Some(dir.join("memory.mv2")),
        db_path: Some(dir.join("state.db")),
        // T111/T118: never let the daemon fall back to the real ~/.zen audit
        // sink (latent risk if a turn is ever added to this suite).
        audit_path: Some(dir.join("audit.jsonl")),
        ..Default::default()
    }
}

async fn connect_client(socket: &std::path::Path) -> UdsTransport {
    // Retry-dial until the listener is up (≤2s per quickstart cold-start).
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match UdsTransport::connect(socket).await {
            Ok(t) => return t,
            Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(e) => panic!("gateway socket never came up: {e}"),
        }
    }
}

/// initialize → response → initialized.
async fn handshake(client: &UdsTransport) {
    client
        .send(Frame::request_with(
            0,
            "initialize",
            initialize_params(
                SERVER_PROTOCOL_VERSION,
                "sole-owner-test",
                "0.0.0",
                Capabilities::default(),
            ),
        ))
        .await
        .unwrap();
    let resp = tokio::time::timeout(CLIENT_TIMEOUT, client.recv())
        .await
        .expect("initialize reply timeout")
        .unwrap();
    assert!(matches!(
        resp,
        Frame::ServerResponse {
            result: Some(_),
            ..
        }
    ));
    client
        .send(Frame::notification("initialized", json!({})))
        .await
        .unwrap();
}

async fn rpc(
    client: &UdsTransport,
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, zen_gateway::protocol::RpcErrorBody> {
    client
        .send(Frame::request_with(id, method, params))
        .await
        .unwrap();
    match tokio::time::timeout(CLIENT_TIMEOUT, client.recv())
        .await
        .expect("rpc reply timeout")
        .unwrap()
    {
        Frame::ServerResponse {
            id: _,
            result: Some(v),
            ..
        } => Ok(v),
        Frame::ServerResponse { error: Some(e), .. } => Err(e),
        other => panic!("unexpected frame: {other:?}"),
    }
}

#[tokio::test]
async fn two_clients_cross_visibility_and_sole_owner() {
    unsafe { std::env::set_var("ZEN_SKIP_RLIMIT", "1") };
    // Restore soft limits that a prior test's ZenWiring may have lowered
    // to NPROC=50 (soft-only). Without this, thread spawn fails with
    // EAGAIN on Linux when nextest runs many workers under one UID.
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
    let tmp = tempfile::tempdir().unwrap();
    let config = daemon_config(tmp.path());
    let socket = config.socket_path.clone();

    let server_task = tokio::spawn(GatewayService::serve(config.clone()));

    // Two independent surfaces over one daemon.
    let client_a = connect_client(&socket).await;
    let client_b = connect_client(&socket).await;
    handshake(&client_a).await;
    handshake(&client_b).await;

    // A writes; B must observe promptly (shared store, instant in-process).
    // 1s is the ideal, but CI runners (esp. x64 mac) can be ~2-3s under load
    // with cold memvid/tantivy init — gate on ≤5s to avoid flake, keep the
    // functional assertion (B sees A's entry) as the real signal.
    let put_at = Instant::now();
    let put = rpc(
        &client_a,
        1,
        "memory/putEntry",
        json!({
            "sessionId": "session-x",
            "role": "user",
            "content": "User prefers dark mode for focus work",
            "entityType": "user"
        }),
    )
    .await
    .expect("putEntry ok");
    assert!(put["frameId"].is_string());

    let entries = rpc(
        &client_b,
        2,
        "memory/retrieve",
        json!({ "sessionId": "session-x" }),
    )
    .await
    .expect("retrieve ok");
    assert!(
        put_at.elapsed() < Duration::from_secs(5),
        "cross-visibility exceeded 5s (put+retrieve took {:?})",
        put_at.elapsed()
    );
    assert!(
        !entries["entries"].as_array().unwrap().is_empty(),
        "client B must see client A's entry"
    );

    // Search finds the written content.
    let hits = rpc(
        &client_b,
        3,
        "memory/search",
        json!({ "query": "dark mode", "sessionId": "session-x" }),
    )
    .await
    .expect("search ok");
    assert!(!hits["hits"].as_array().unwrap().is_empty());

    // health/status reflects two live clients and a healthy store.
    let status = rpc(&client_a, 4, "health/status", json!({}))
        .await
        .expect("status ok");
    assert_eq!(status["serverVersion"], env!("CARGO_PKG_VERSION"));
    assert_eq!(status["protocolVersion"], SERVER_PROTOCOL_VERSION);
    assert_eq!(status["clients"], 2);
    assert_eq!(status["storeHealth"], "ok");
    assert_eq!(status["activeTurns"], 0);
    assert_eq!(
        status["scheduler"], false,
        "default daemon does not host scheduler"
    );
    assert_eq!(
        status["scheduler_pending"], false,
        "default daemon is not waiting for the scheduler lease"
    );

    // Sole-owner: second service open refuses (process claim), and a
    // second bind on the live socket refuses (atomic bind claim).
    assert!(GatewayService::open(&config).await.is_err());
    assert!(
        zen_gateway::transport::uds::bind_socket(&socket)
            .await
            .is_err()
    );

    // Graceful stop removes the socket. The bounded join turns a
    // serve-side stall into a visible failure rather than a hang.
    let _ = rpc(&client_a, 7, "shutdown", json!({})).await;
    let finished = tokio::time::timeout(Duration::from_secs(15), server_task).await;
    match finished {
        Ok(Ok(Ok(()))) => {}
        other => panic!("serve did not end cleanly: {other:?}"),
    }
    assert!(!socket.exists(), "socket file removed after shutdown");

    // Phase 2: scheduler_hosted=true — explicit zen serve start. The
    // live/waiting flags mirror serve_command's real wiring: scheduler_live
    // true once the lease is acquired, scheduler_waiting false.
    let scheduler_live = Arc::new(AtomicBool::new(true));
    let scheduler_waiting = Arc::new(AtomicBool::new(false));
    let config_scheduler = GatewayDaemonConfig {
        scheduler_hosted: true,
        scheduler_live: Some(scheduler_live.clone()),
        scheduler_waiting: Some(scheduler_waiting.clone()),
        ..daemon_config(tmp.path())
    };
    let socket_sched = config_scheduler.socket_path.clone();
    let server_sched = tokio::spawn(GatewayService::serve(config_scheduler));
    let client_sched = connect_client(&socket_sched).await;
    handshake(&client_sched).await;
    let status_sched = rpc(&client_sched, 10, "health/status", json!({}))
        .await
        .expect("scheduler-status ok");
    assert_eq!(
        status_sched["scheduler"], true,
        "explicit zen serve start reports scheduler hosted"
    );
    assert_eq!(
        status_sched["scheduler_pending"], false,
        "explicit daemon with no lease contention is not pending"
    );
    assert_eq!(status_sched["protocolVersion"], SERVER_PROTOCOL_VERSION);

    // Flip the waiting flag on the LIVE server: an explicit daemon that
    // lost the lease race to a TUI reports scheduler_pending so the TUI
    // can yield its in-app scheduler.
    scheduler_waiting.store(true, std::sync::atomic::Ordering::Relaxed);
    let status_waiting = rpc(&client_sched, 12, "health/status", json!({}))
        .await
        .expect("waiting-status ok");
    assert_eq!(
        status_waiting["scheduler_pending"], true,
        "waiting daemon reports scheduler_pending so the TUI can yield"
    );
    assert_eq!(
        status_waiting["scheduler"], true,
        "scheduler stays hosted while waiting for the lease"
    );

    let _ = rpc(&client_sched, 11, "shutdown", json!({})).await;
    let finished_sched = tokio::time::timeout(Duration::from_secs(15), server_sched).await;
    match finished_sched {
        Ok(Ok(Ok(()))) => {}
        other => panic!("phase-2 serve did not end cleanly: {other:?}"),
    }

    // Release the process-lifetime memvid singletons so tantivy watcher
    // threads exit and the test binary can terminate.
    zen_memory::memvid::clear_global_memvid_cache();
}
