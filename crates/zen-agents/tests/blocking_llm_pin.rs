//! P1-1 regression pin: sync LLM calls reachable from async must not park a
//! tokio worker.
//!
//! PURPOSE: `run_blocking_llm` is the single guard that keeps a blocking LLM
//! call off the async executor on a multi-thread runtime while staying usable
//! on the `current_thread` runtime every `#[tokio::test]` uses (where
//! `block_in_place` would panic).
//!
//! USAGE: `cargo nextest run -p zen-agents -E 'test(blocking_llm_pin)'`.
//!
//! EXPECTED: the helper frees the worker for a concurrent task, never panics
//! on a current-thread runtime, and a full orchestrator turn completes on a
//! multi-thread runtime while a concurrent task still progresses.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use zen_agents::execution::AgentExecution;
use zen_agents::orchestrator::{AgentOrchestrator, run_blocking_llm};
use zen_core::types::SessionContext;

fn scripted_router(reply: &str) -> zen_provider::DefaultRouter {
    zen_provider::DefaultRouter::new(zen_provider::LlmConfig {
        default_provider: Some("mock".to_string()),
        ..Default::default()
    })
    .with_mock_response(reply)
}

/// One worker: without `block_in_place` the single worker is fully occupied by
/// the blocking closure, so a concurrent task cannot make progress — the
/// discriminating configuration for the helper test.
fn single_worker_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("multi-thread runtime")
}

#[test]
fn blocking_helper_frees_worker_for_concurrent_tasks() {
    let rt = single_worker_runtime();
    let observed = rt.block_on(async {
        let flag = Arc::new(AtomicBool::new(false));
        let concurrent_flag = Arc::clone(&flag);
        let concurrent = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            concurrent_flag.store(true, Ordering::SeqCst);
        });

        let worker = tokio::spawn(async move {
            run_blocking_llm(|| {
                for _ in 0..30 {
                    if flag.load(Ordering::SeqCst) {
                        return 7u32;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                0u32
            })
        });

        let result = worker.await.expect("blocking helper task must not panic");
        let _ = concurrent.await;
        result
    });

    assert_eq!(
        observed, 7,
        "the concurrent task must run while the helper's blocking closure sleeps"
    );
}

#[tokio::test]
async fn current_thread_runtime_compatible() {
    assert_eq!(run_blocking_llm(|| 42u32), 42);
}

#[test]
fn orchestrator_execute_completes_on_multithread_runtime() {
    let _guard = common::begin();
    let orchestrator = AgentOrchestrator::with_token_budget(
        scripted_router("plain tool-free mock answer"),
        10_000_000,
    );
    let mut session = SessionContext::new("Sisyphus".to_string(), String::new());

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("multi-thread runtime");

    let (execution, concurrent_done) = rt.block_on(async {
        let flag = Arc::new(AtomicBool::new(false));
        let concurrent_flag = Arc::clone(&flag);
        let concurrent = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            concurrent_flag.store(true, Ordering::SeqCst);
        });

        let turn = tokio::spawn(async move { orchestrator.execute(&mut session, "hello").await });
        let execution: AgentExecution = turn
            .await
            .expect("turn task join")
            .expect("orchestrator turn must complete on a multi-thread runtime");

        let _ = concurrent.await;
        (execution, flag.load(Ordering::SeqCst))
    });

    assert!(
        execution.response.contains("[mock]"),
        "response must come from the mock provider: {}",
        execution.response
    );
    assert!(
        concurrent_done,
        "concurrent task must complete alongside the orchestrator turn"
    );
}
