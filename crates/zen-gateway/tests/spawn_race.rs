//! US2 spawn-race stress (task T020, quickstart V2 / SC-004): ten
//! concurrent client startups on a clean environment, twenty trials;
//! exactly one daemon must exist and every surface must connect.
//!
//! PURPOSE: Proves the client-side race orchestration — all racers probe
//! the empty socket, all invoke their spawner, only the atomic-bind
//! winner serves while the rest exit quietly, and every racer's
//! wait-for-ready lands on that single winner.
//!
//! SCOPE NOTE: Racers run in one process with an injected spawner (test
//! binaries cannot exec themselves as `zen`); the cross-process bind
//! guarantee itself is proven by `transport::uds::bind_twice_*` unit
//! tests and sole_owner's double-bind assertion.
//!
//! USAGE: cargo test -p zen-gateway --test spawn_race
//!
//! EXPECTED: 20/20 trials pass well under a second each.
//!
//! ERRORS: any racer failure or a successful second bind fails the trial.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use zen_gateway::client::{DaemonSpawnFn, GatewayClient};
use zen_gateway::protocol::Capabilities;
use zen_gateway::server::stub_server;
use zen_gateway::transport::uds;

const RACERS: usize = 10;
const TRIALS: usize = 20;

/// One full trial on a fresh socket: race `RACERS` connect-or-spawn
/// callers, handshake each, verify liveness and single-daemon invariant.
async fn run_trial(trial: usize) -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let sock: PathBuf = tmp.path().join(format!("race-{trial}.sock"));

    let config_sock = sock.clone();
    let spawn_fn: DaemonSpawnFn = Arc::new(move || {
        let sock = config_sock.clone();
        tokio::spawn(async move {
            // Bind loser exits quietly; winner serves all comers.
            let listener = match uds::bind_socket(&sock).await {
                Ok(l) => l,
                Err(_) => return,
            };
            loop {
                if let Ok(server_side) = uds::accept_transport(&listener).await
                    && let Ok(server) = stub_server(server_side)
                {
                    let _ = server.run().await;
                }
            }
        });
        Ok(())
    });

    let mut handles = Vec::with_capacity(RACERS);
    for racer in 0..RACERS {
        let spawn_fn = Arc::clone(&spawn_fn);
        let sock = sock.clone();
        handles.push(tokio::spawn(async move {
            let client = GatewayClient::connect_or_spawn(&sock, Some(spawn_fn))
                .await
                .map_err(|e| anyhow::anyhow!("racer {racer}: connect failed: {e}"))?;
            client
                .handshake("racer", "0.0", Capabilities::default())
                .await
                .map_err(|e| anyhow::anyhow!("racer {racer}: handshake failed: {e}"))?;
            let status = client
                .request("health/status", json!({}))
                .await
                .map_err(|e| anyhow::anyhow!("racer {racer}: status failed: {e}"))?;
            assert_eq!(status["storeHealth"], "ok");
            Ok::<(), anyhow::Error>(())
        }));
    }

    for handle in handles {
        handle.await??;
    }

    // Exactly-one-daemon invariant still holds after the storm.
    assert!(
        uds::bind_socket(&sock).await.is_err(),
        "trial {trial}: a second daemon bound the socket"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ten_racers_twenty_trials_exactly_one_daemon() {
    for trial in 0..TRIALS {
        tokio::time::timeout(Duration::from_secs(10), run_trial(trial))
            .await
            .expect("trial timed out")
            .expect("trial failed");
    }
}
