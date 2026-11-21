//! 007 T009/T010/T011 (US2) — auto-mode e2e pins.
//!
//! PURPOSE: prove `Admitted` dispatches the EXISTING hosted-turn pipeline,
//! reports the result through the existing QQ reply path, closes the ticket
//! with an honest report state, and that a live-ticket continuation keeps
//! riding its granted mandate even when the channel's current mode is
//! `observe` (FR-007/FR-008/FR-014).
//! USAGE: `cargo test -p zen-gateway --test admission_auto`.
//! EXPECTED: deterministic; daemons drain cleanly.

mod admission_harness;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use admission_harness::{
    ALLOWED_MEMBER, EchoExec, FailExec, group_event, open_db, spawn_stack, wait_for_decisions,
    wait_for_group_reply, wait_identified,
};
use zen_repo::{AdmissionRepo, DecisionRecord, MandateGrant, SqliteClient, TicketOpen};

const SEED_THREAD: &str = "gOK";

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

async fn latest_ticket(db: &SqliteClient, status: &str) -> (String, Option<String>) {
    let status = status.to_string();
    db.writer()
        .call(move |conn| {
            Ok(conn.query_row(
                "SELECT status, report_state FROM admission_tickets \
                 WHERE status = ?1 ORDER BY created_at DESC, rowid DESC LIMIT 1",
                [status.as_str()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )?)
        })
        .await
        .unwrap()
}

async fn mandate_for_decision(db: &SqliteClient, decision_id: &str) -> (String, String, String) {
    let decision_id = decision_id.to_string();
    db.writer()
        .call(move |conn| {
            Ok(conn.query_row(
                "SELECT scope, strength, policy_version FROM admission_mandates \
                 WHERE decision_id = ?1 ORDER BY granted_at DESC, rowid DESC LIMIT 1",
                [decision_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?)
        })
        .await
        .unwrap()
}

/// (a) SC-001 step 4: admitted work executes through the standard pipeline
/// and the reply lands in the originating chat with an ended `ready` ticket.
#[tokio::test]
async fn auto_admitted_executes_and_reports_back() {
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

    stack.mock.inject(group_event(
        "AUTO1",
        ALLOWED_MEMBER,
        "<@!BOTID> summarize my inbox",
    ));

    let sends = wait_for_group_reply(&stack, "AUTO1").await;
    let (_, reply) = sends
        .iter()
        .find(|(_, v)| v["body"]["msg_id"] == "AUTO1")
        .expect("reply for AUTO1");
    assert!(
        reply["body"]["content"]
            .as_str()
            .unwrap_or("")
            .contains("echo:summarize my inbox"),
        "reply must carry the pipeline result: {reply:?}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let rows = wait_for_decisions(&db, 1).await;
    let row = rows
        .iter()
        .find(|r| r.event_id.as_deref() == Some("AUTO1"))
        .expect("decision for AUTO1");
    assert_eq!(row.disposition, "admitted");
    assert_eq!(row.matched_rule.as_deref(), Some("allowlist"));
    assert_eq!(row.scope.as_deref(), Some("sender"));
    assert_eq!(row.strength.as_deref(), Some("explicit"));
    assert_eq!(row.accept_mode, "auto");
    assert!(!row.policy_version.is_empty());

    let (status, report_state) = latest_ticket(&db, "ended").await;
    assert_eq!(status, "ended");
    assert_eq!(report_state.as_deref(), Some("ready"));

    let (scope, strength, policy_version) = mandate_for_decision(&db, &row.id).await;
    assert_eq!(scope, "sender");
    assert_eq!(strength, "explicit");
    assert_eq!(policy_version, row.policy_version);

    stack.shutdown().await;
}

/// (b) FR-008: a mid-work failure is reported honestly and the authorization
/// record remains intact with a `failed` report state.
#[tokio::test]
async fn auto_failure_reports_honestly_and_keeps_records() {
    let calls = Arc::new(AtomicUsize::new(0));
    let stack = spawn_stack(
        Arc::new(FailExec {
            calls: Arc::clone(&calls),
        }),
        Some("auto"),
    )
    .await;
    wait_identified(&stack).await;
    let db = open_db(&stack).await;

    stack.mock.inject(group_event(
        "FAIL1",
        ALLOWED_MEMBER,
        "<@!BOTID> do the thing",
    ));

    let sends = wait_for_group_reply(&stack, "FAIL1").await;
    let (_, reply) = sends
        .iter()
        .find(|(_, v)| v["body"]["msg_id"] == "FAIL1")
        .expect("failure reply for FAIL1");
    assert!(
        reply["body"]["content"]
            .as_str()
            .unwrap_or("")
            .contains("agent 暂时不可用"),
        "failure must be reported honestly: {reply:?}"
    );
    assert!(
        calls.load(Ordering::SeqCst) >= 1,
        "the admitted work must have been attempted"
    );

    let rows = wait_for_decisions(&db, 1).await;
    let row = rows
        .iter()
        .find(|r| r.event_id.as_deref() == Some("FAIL1"))
        .expect("decision for FAIL1");
    assert_eq!(row.disposition, "admitted", "grant record survives failure");

    let (status, report_state) = latest_ticket(&db, "ended").await;
    assert_eq!(status, "ended");
    assert_eq!(report_state.as_deref(), Some("failed"));

    stack.shutdown().await;
}

/// (c) FR-014: with the channel currently in `observe`, a message continuing
/// a thread that already owns a live ticket is still `Admitted` — the grant
/// outlives the mode change (the gate's continuation-first ordering).
#[tokio::test]
async fn live_ticket_continuation_rides_grant_after_mode_switch() {
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

    let repo = AdmissionRepo::new(&db);
    let created_at = now_secs() - 60;
    repo.record_decision(DecisionRecord {
        id: "seed-decision",
        source: "qqbot",
        event_id: Some("seed-event"),
        content_hash: "seed-hash",
        sender: ALLOWED_MEMBER,
        thread_key: SEED_THREAD,
        content: "the earlier admitted request",
        disposition: "admitted",
        matched_rule: Some("allowlist"),
        scope: Some("sender"),
        accept_mode: "auto",
        strength: Some("explicit"),
        policy_version: "seed-policy",
        reason: None,
        created_at,
    })
    .await
    .unwrap();
    let mandate_id = repo
        .grant_mandate(MandateGrant {
            decision_id: "seed-decision",
            matched_rule: Some("allowlist"),
            scope: "sender",
            strength: "explicit",
            accept_mode: "auto",
            policy_version: "seed-policy",
            granted_at: created_at,
        })
        .await
        .unwrap();
    repo.open_ticket(TicketOpen {
        mandate_id: &mandate_id,
        reply_address: SEED_THREAD,
        turn_ref: Some("seed-turn"),
        created_at,
    })
    .await
    .unwrap();

    stack.mock.inject(group_event(
        "CONT1",
        ALLOWED_MEMBER,
        "<@!BOTID> continue the work",
    ));

    let sends = wait_for_group_reply(&stack, "CONT1").await;
    let (_, reply) = sends
        .iter()
        .find(|(_, v)| v["body"]["msg_id"] == "CONT1")
        .expect("reply for CONT1");
    assert!(
        reply["body"]["content"]
            .as_str()
            .unwrap_or("")
            .contains("echo:continue the work"),
        "continuation must execute under the grant: {reply:?}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let rows = wait_for_decisions(&db, 2).await;
    let row = rows
        .iter()
        .find(|r| r.event_id.as_deref() == Some("CONT1"))
        .expect("decision for CONT1");
    assert_eq!(row.disposition, "admitted");
    assert_eq!(row.matched_rule.as_deref(), Some("thread-continuation"));
    assert_eq!(row.scope.as_deref(), Some("thread"));
    assert_eq!(
        row.accept_mode, "observe",
        "the ledger records the mode in force, while the grant wins (FR-014)"
    );

    let (status, report_state) = latest_ticket(&db, "ended").await;
    assert_eq!(status, "ended");
    assert_eq!(report_state.as_deref(), Some("ready"));

    stack.shutdown().await;
}
