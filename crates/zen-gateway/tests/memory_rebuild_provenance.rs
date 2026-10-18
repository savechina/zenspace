//! T223 (review R5): `memory/rebuild` must flip `.with_provenance_metadata`
//! from the SAME `[agentic.memory_strength]` gate the nightly memvid-indexer
//! worker uses (T210). Without it, a rebuild under an enabled gate writes
//! frames WITHOUT `{source_path, tier}`, degrading retention strength to
//! anchor-only and stalling the T211 increments until the next nightly.
//!
//! PURPOSE: drive the real `memory/rebuild` RPC end-to-end (sole_owner.rs
//! harness pattern) and assert on the stored frames: gate-ON writes
//! source_path/tier; gate-OFF writes byte-identical (provenance-key-free)
//! puts, exactly as the pre-T223 rebuild did.
//!
//! USAGE: cargo nextest run -p zen-gateway --test memory_rebuild_provenance
//!
//! EXPECTED: both rebuilds succeed; the gate-ON token's frames carry
//! `source_path=journal/…` + `tier=m2`; the gate-OFF token's frames carry
//! neither key.
//!
//! ERRORS: provenance keys on gate-OFF frames mean the flip is not gated;
//! missing keys on gate-ON frames mean the rebuild never flips (the R5 bug).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::json;
use zen_gateway::protocol::{Capabilities, Frame, SERVER_PROTOCOL_VERSION, initialize_params};
use zen_gateway::transport::Transport;
use zen_gateway::transport::uds::UdsTransport;
use zen_gateway::{GatewayDaemonConfig, GatewayService};
use zen_memory::ZenMemvidStore;
use zen_memory::memvid_index::{PROVENANCE_SOURCE_PATH_KEY, PROVENANCE_TIER_KEY};
use zen_memory::memvid_store::memvid_core::{SearchHit, SearchRequest};

const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);

fn daemon_config(dir: &std::path::Path) -> GatewayDaemonConfig {
    GatewayDaemonConfig {
        socket_path: dir.join("gateway.sock"),
        // memory_path/db_path/audit_path stay None: they resolve under the
        // ZEN_HOME this test sets, so rebuild roots and the daemon store
        // share one tree (the production shape).
        ..Default::default()
    }
}

async fn connect_client(socket: &std::path::Path) -> UdsTransport {
    let deadline = Instant::now() + Duration::from_secs(5);
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

async fn handshake(client: &UdsTransport) {
    client
        .send(Frame::request_with(
            0,
            "initialize",
            initialize_params(
                SERVER_PROTOCOL_VERSION,
                "memory-rebuild-provenance-test",
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

/// Direct store search (the `memory/search` RPC renders metadata away — the
/// provenance contract lives on the raw frames). The singleton cache returns
/// the daemon's own in-process store for the same path (memvid.rs
/// get_or_init), so no second file lock is taken.
fn search_frames(store_path: &std::path::Path, token: &str) -> Vec<SearchHit> {
    let store = ZenMemvidStore::new(store_path.to_path_buf()).unwrap();
    let request: SearchRequest = serde_json::from_value(json!({
        "query": token,
        "top_k": 10,
        "snippet_chars": 400,
        "uri": null,
    }))
    .unwrap();
    store
        .store()
        .search(request)
        .unwrap()
        .hits
        .into_iter()
        .filter(|h| h.text.contains(token))
        .collect()
}

fn extra_meta(hit: &SearchHit) -> BTreeMap<String, String> {
    hit.metadata
        .as_ref()
        .map(|m| m.extra_metadata.clone())
        .unwrap_or_default()
}

#[tokio::test]
async fn rebuild_writes_provenance_under_gate_and_is_byte_identical_without() {
    unsafe { std::env::set_var("ZEN_SKIP_RLIMIT", "1") };
    // Restore the NOFILE soft cap a prior test's ZenWiring lowered
    // (sole_owner.rs preamble — several daemons share one process here).
    #[cfg(unix)]
    unsafe {
        let mut rl = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) == 0 {
            rl.rlim_cur = rl.rlim_max;
            libc::setrlimit(libc::RLIMIT_NOFILE, &rl);
        }
    }

    // Single-test binary: ZEN_HOME + the config cache are process-global.
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().to_path_buf();
    // SAFETY: single-test binary; set before serve() reads any path/config.
    unsafe { std::env::set_var("ZEN_HOME", &home) };
    zen_core::config::invalidate_config_cache();

    // Gate ON via the operator surface the worker reads (5-layer config).
    std::fs::write(
        home.join("config.toml"),
        "[agentic.memory_strength]\nenabled = true\n",
    )
    .unwrap();

    let journal = home.join("memories").join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    std::fs::write(
        journal.join("2026-06-01.md"),
        "## Facts\n- gateonrebuildtoken journal fact\n",
    )
    .unwrap();

    let config = daemon_config(&home);
    let socket = config.socket_path.clone();
    let store_path = home
        .join("memories")
        .join(zen_core::constants::MEMVID_STORE_FILE);

    let server_task = tokio::spawn(GatewayService::serve(config.clone()));
    let client = connect_client(&socket).await;
    handshake(&client).await;

    // ── Phase 1: gate ON ⇒ rebuild frames carry source_path/tier ──
    let rebuild_on = rpc(&client, 1, "memory/rebuild", json!({}))
        .await
        .expect("gate-on rebuild ok");
    assert_eq!(rebuild_on["filesScanned"], 1);
    assert_eq!(rebuild_on["chunksIndexed"], 1);

    let on_hits = search_frames(&store_path, "gateonrebuildtoken");
    assert!(
        !on_hits.is_empty(),
        "gate-on rebuild must index the journal"
    );
    for hit in &on_hits {
        let meta = extra_meta(hit);
        assert_eq!(
            meta.get(PROVENANCE_SOURCE_PATH_KEY).map(String::as_str),
            Some("journal/2026-06-01.md"),
            "R5 fix: gate-on rebuild writes source_path, got {meta:?}"
        );
        assert_eq!(
            meta.get(PROVENANCE_TIER_KEY).map(String::as_str),
            Some("m2"),
            "R5 fix: gate-on rebuild writes tier, got {meta:?}"
        );
    }

    // ── Phase 2: gate OFF ⇒ byte-identical (no provenance keys) ──
    std::fs::write(
        home.join("config.toml"),
        "[agentic.memory_strength]\nenabled = false\n",
    )
    .unwrap();
    zen_core::config::invalidate_config_cache();
    std::fs::write(
        journal.join("2026-06-02.md"),
        "## Facts\n- gateoffrebuildtoken journal fact\n",
    )
    .unwrap();

    let rebuild_off = rpc(&client, 2, "memory/rebuild", json!({}))
        .await
        .expect("gate-off rebuild ok");
    assert_eq!(
        rebuild_off["filesScanned"], 2,
        "rebuild re-appends by design: both journal files rescan (manual, bounded; nightly dedups via T219)"
    );

    let off_hits = search_frames(&store_path, "gateoffrebuildtoken");
    assert!(!off_hits.is_empty(), "gate-off rebuild must still index");
    for hit in &off_hits {
        let meta = extra_meta(hit);
        // Gate-off pin (T208 style): NO zen provenance keys — memvid-core's
        // own enrichment entries (e.g. `extractous_metadata`) may ride along.
        assert!(
            !meta.contains_key(PROVENANCE_SOURCE_PATH_KEY)
                && !meta.contains_key(PROVENANCE_TIER_KEY),
            "gate-off rebuild must be byte-identical to the pre-T223 puts, got {meta:?}"
        );
    }

    let _ = rpc(&client, 7, "shutdown", json!({})).await;
    let finished = tokio::time::timeout(Duration::from_secs(15), server_task).await;
    match finished {
        Ok(Ok(Ok(()))) => {}
        other => panic!("serve did not end cleanly: {other:?}"),
    }
    assert!(!socket.exists(), "socket file removed after shutdown");

    // Release the process-lifetime memvid singletons so watcher threads
    // exit and the tempdir can be removed (sole_owner.rs precedent).
    zen_memory::memvid::clear_global_memvid_cache();
}
