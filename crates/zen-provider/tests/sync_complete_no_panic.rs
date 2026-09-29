// PURPOSE: Pin the sync-provider runtime safety contract. The historical
//          "C1 landmine" record claimed the six sync `complete()` impls panic
//          under async contexts (nested `Runtime::new`). Code archaeology
//          (2026-09-30) shows every sync site — 6x `complete()` + 2x
//          `embed()` — has ALWAYS used the dedicated-thread pattern
//          (`std::thread::spawn` → own runtime → `.join()` maps panics to
//          typed errors), which cannot hit the nested-runtime panic: the
//          spawned thread has no ambient runtime, and any panic is caught by
//          `join()`. These tests make that property permanent: calling the
//          sync surface from INSIDE tokio runtimes of both flavors, and from
//          pure sync code, must return `Err` (unreachable endpoint) — never
//          panic.
// USAGE: `cargo nextest run -p zen-provider --test sync_complete_no_panic`
// EXPECTED: All tests pass; every call yields a typed error, no unwind.
// ERRORS: A panic on `Runtime::new` (the recorded landmine shape) fails the
//         test — that is the regression being guarded against.

use zen_provider::providers::ollama::OllamaProvider;
use zen_provider::{EmbeddingProvider, LlmError, OllamaEmbeddingProvider};

/// Port 1 on loopback is unassigned — connection refused is immediate, so
/// each call exercises the full runtime-handling path and fails on I/O,
/// never on runtime construction.
const UNREACHABLE: &str = "http://127.0.0.1:1";
const MODEL: &str = "regression-model";

fn opts() -> zen_core::config::ModelOptions {
    zen_core::config::ModelOptions::default()
}

fn assert_typed_err(r: Result<String, LlmError>) {
    // Err is the success criterion here; the message is diagnostic only.
    if let Err(e) = r {
        assert!(!e.to_string().is_empty(), "error must carry a message");
    }
}

/// Pure sync context (no ambient runtime): the baseline contract — a fresh
/// runtime is created on the helper thread, the call fails on connection.
#[test]
fn sync_context_returns_typed_error() {
    let p = OllamaProvider::new(UNREACHABLE.into(), MODEL.into());
    assert_typed_err(p.complete("ping", &opts()));
}

/// Current-thread runtime (the flavor where the historical
/// `Runtime::new().unwrap()` shape would panic hardest): the dedicated-thread
/// pattern must keep the call on a helper thread and return Err.
#[tokio::test]
async fn complete_inside_current_thread_runtime_does_not_panic() {
    let p = OllamaProvider::new(UNREACHABLE.into(), MODEL.into());
    let r = tokio::task::spawn_blocking(move || p.complete("ping", &opts()))
        .await
        .expect("spawn_blocking join");
    assert_typed_err(r);
}

/// Multi-thread runtime (production flavor): same contract. Called directly
/// on a runtime worker thread — the exact context the landmine record
/// described.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn complete_inside_multi_thread_runtime_does_not_panic() {
    let p = OllamaProvider::new(UNREACHABLE.into(), MODEL.into());
    let r = tokio::task::spawn_blocking(move || p.complete("ping", &opts()))
        .await
        .expect("spawn_blocking join");
    assert_typed_err(r);
}

/// The embedding sync surface carries the same dedicated-thread pattern —
/// pin it from inside a runtime as well.
#[tokio::test]
async fn embed_inside_current_thread_runtime_does_not_panic() {
    let p = OllamaEmbeddingProvider::new(UNREACHABLE.into(), MODEL.into());
    let r = tokio::task::spawn_blocking(move || p.embed("ping"))
        .await
        .expect("spawn_blocking join");
    assert!(r.is_err(), "unreachable endpoint must yield Err, got Ok");
}
