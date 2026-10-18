//! P1-2 regression pin: query embedding must never park the async executor.
//!
//! PURPOSE: `SearchService::search_fused_with_diagnostics` computes the query
//! embedding on a blocking thread; a dead/hung provider must not stall the
//! caller (the pre-fix code ran the sync provider call straight on the tokio
//! worker, which a 240s timeout could pin).
//!
//! USAGE: `cargo nextest run -p zen-vault -E 'test(query_embed_blocking)'`.
//!
//! EXPECTED: the fused search returns well within the timeout, and the O16
//! diagnostics it reports match a direct probe — the spawn_blocking refactor
//! must not change which branch produced the query embedding.

use std::time::Duration;

use zen_provider::{DefaultRouter, LlmConfig};
use zen_vault::search::SearchService;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn query_embedding_cannot_hang_the_fused_search() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("note.md"), "buffett letters archive\n").unwrap();
    let client = zen_repo::SqliteClient::open(&tmp.path().join("state.db"))
        .await
        .expect("open temp state.db");
    let service = SearchService::new(DefaultRouter::new(LlmConfig {
        default_provider: Some("mock".to_string()),
        ..Default::default()
    }));

    let (_, probe_degraded) =
        zen_vault::tindy::compute_embeddings_for_text_tagged("buffett").expect("probe");

    let outcome = tokio::time::timeout(
        // Statically-linked ort means the query embedding really computes
        // (no degraded fast-fail); on CI runners a warm BGE-M3 encode under
        // nextest parallel load can exceed 2s. The pinned regression was a
        // dead provider parking the executor for 240s+ — a 60s budget (the
        // z-provider FASTEMBED_INIT_TIMEOUT_SECS ceiling) still catches any
        // true hang while being immune to slow-runner compute.
        Duration::from_secs(60),
        service.search_fused_with_diagnostics("buffett", tmp.path(), &client, Some(10)),
    )
    .await;

    assert!(
        outcome.is_ok(),
        "query embedding must not park the async executor past the timeout"
    );
    let smoothed = outcome.unwrap();
    // The provider chain ends in an infallible hash fallback, so on a
    // provider-less host the observable outcome is Ok(degraded) rather than
    // Err. The pinned invariant is completion within the timeout, not the
    // exact branch.
    if let Ok((_results, diagnostics)) = smoothed {
        assert_eq!(
            diagnostics.query_embedding_degraded, probe_degraded,
            "spawn_blocking must preserve the query-embedding diagnostics"
        );
    }
}
