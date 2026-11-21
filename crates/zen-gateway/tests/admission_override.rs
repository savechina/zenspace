//! 007 deviation closeout (migration 017) — runtime accept-mode override e2e.
//!
//! PURPOSE: prove a `zen admission mode` override row flips a LIVE stack
//! (observe → auto) without restart, that the ledger stamps the effective
//! mode and its policy version, and that an invalid stored value degrades
//! loudly to observe semantics (E11 — never silent, never a gate bypass).
//! USAGE: `cargo test -p zen-gateway --test admission_override`.
//! EXPECTED: daemons drain cleanly; flips land on the next inbound message.

mod admission_harness;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use admission_harness::{
    ALLOWED_MEMBER, EchoExec, group_event, open_db, spawn_stack, wait_for_decisions,
    wait_identified,
};
use zen_core::config::{AcceptMode, QqBotChannelConfig, admission_policy_version_for_mode};
use zen_repo::AdmissionRepo;

fn version_for_mode(mode: AcceptMode) -> String {
    admission_policy_version_for_mode(
        mode,
        Some(&QqBotChannelConfig {
            app_id: String::new(),
            client_secret: String::new(),
            allowed_users: vec![
                ALLOWED_MEMBER.to_string(),
                admission_harness::ALLOWED_C2C_USER.to_string(),
            ],
            home_channel: None,
            accept_mode: Some("observe".to_string()),
            outbox_drain_interval_secs: None,
        }),
    )
}

#[tokio::test]
async fn override_row_flips_a_live_stack_from_observe_to_auto_without_restart() {
    let calls = Arc::new(AtomicUsize::new(0));
    let stack = spawn_stack(
        Arc::new(EchoExec {
            calls: Arc::clone(&calls),
            slow_contains: None,
        }),
        Some("observe"),
    )
    .await;
    wait_identified(&stack).await;
    let db = open_db(&stack).await;

    AdmissionRepo::new(&db)
        .set_mode_override("auto")
        .await
        .unwrap();

    stack.mock.inject(group_event(
        "OVR1",
        ALLOWED_MEMBER,
        "<@!BOTID> run the plan",
    ));

    let rows = wait_for_decisions(&db, 1).await;
    let row = &rows[0];
    assert_eq!(
        row.disposition, "admitted",
        "the override flips the live gate: observe → auto, no restart"
    );
    assert_eq!(
        row.accept_mode, "auto",
        "the ledger stamps the EFFECTIVE mode"
    );
    assert_eq!(
        row.policy_version,
        version_for_mode(AcceptMode::Auto),
        "the ledger stamps the flipped policy's version, not the boot-time one"
    );

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        calls.load(Ordering::SeqCst) >= 1,
        "auto dispatches through the hosted-turn path"
    );
    stack.shutdown().await;
}

#[tokio::test]
async fn invalid_override_row_degrades_loudly_to_observe() {
    let calls = Arc::new(AtomicUsize::new(0));
    let stack = spawn_stack(
        Arc::new(EchoExec {
            calls: Arc::clone(&calls),
            slow_contains: None,
        }),
        Some("auto"),
    )
    .await;
    wait_identified(&stack).await;
    let db = open_db(&stack).await;

    AdmissionRepo::new(&db)
        .set_mode_override("bogus")
        .await
        .unwrap();

    stack.mock.inject(group_event(
        "OVR2",
        ALLOWED_MEMBER,
        "<@!BOTID> run anything",
    ));

    let rows = wait_for_decisions(&db, 1).await;
    let row = &rows[0];
    assert_eq!(
        row.disposition, "would_accept",
        "an invalid stored value (hand-edited db) degrades to observe semantics"
    );
    assert_eq!(
        row.accept_mode, "observe",
        "the ledger records the degraded mode"
    );
    assert_eq!(
        row.policy_version,
        version_for_mode(AcceptMode::Observe),
        "the degraded policy's version is stamped"
    );

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "degraded observe never dispatches"
    );
    stack.shutdown().await;
}
