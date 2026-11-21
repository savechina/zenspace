//! 007 T012 (US3) — mandate immutability pins.
//!
//! PURPOSE: prove the authorization record answers why-when-what purely from
//! persisted columns: the policy version is stamped on every decision, the
//! only mandate mutation is the withdraw columns (scope/strength/mode/version
//! never change in place), and widening scope mints a NEW mandate rather than
//! editing the old grant (FR-006/FR-013).
//! USAGE: `cargo test -p zen-repo --test admission_mandate`.
//! EXPECTED: deterministic repo-level pins; no daemon.

use tempfile::TempDir;
use zen_repo::{AdmissionRepo, DecisionRecord, MandateGrant, SqliteClient, TicketOpen};

async fn make_client(name: &str) -> (SqliteClient, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let client = SqliteClient::open(&dir.path().join(name)).await.unwrap();
    (client, dir)
}

fn decision<'a>(
    id: &'a str,
    event_id: &'a str,
    scope: &'a str,
    policy_version: &'a str,
) -> DecisionRecord<'a> {
    DecisionRecord {
        id,
        source: "qqbot",
        event_id: Some(event_id),
        content_hash: "hash",
        sender: "owner-openid",
        thread_key: "group_A",
        content: "please process the inbox",
        disposition: "admitted",
        matched_rule: Some("allowlist"),
        scope: Some(scope),
        accept_mode: "auto",
        strength: Some("explicit"),
        policy_version,
        reason: None,
        created_at: 1_700_000_000,
    }
}

async fn grant(
    repo: &AdmissionRepo<'_>,
    decision_id: &str,
    scope: &str,
    policy_version: &str,
) -> String {
    repo.grant_mandate(MandateGrant {
        decision_id,
        matched_rule: Some("allowlist"),
        scope,
        strength: "explicit",
        accept_mode: "auto",
        policy_version,
        granted_at: 1_700_000_001,
    })
    .await
    .unwrap()
}

async fn count_mandates(client: &SqliteClient) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM admission_mandates")
        .fetch_one(client.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn policy_version_is_recorded_on_every_decision() {
    let (client, _dir) = make_client("mandate_pv.db").await;
    let repo = AdmissionRepo::new(&client);

    repo.record_decision(decision("pv-1", "evt-1", "sender", "policy-alpha"))
        .await
        .unwrap();
    repo.record_decision(decision("pv-2", "evt-2", "sender", "policy-beta"))
        .await
        .unwrap();

    let first = repo.get_decision("pv-1").await.unwrap().unwrap();
    let second = repo.get_decision("pv-2").await.unwrap().unwrap();
    assert_eq!(first.policy_version, "policy-alpha");
    assert_eq!(second.policy_version, "policy-beta");
    assert_eq!(first.accept_mode, "auto");
    assert_eq!(first.matched_rule.as_deref(), Some("allowlist"));
}

#[tokio::test]
async fn withdraw_touches_only_withdraw_columns() {
    let (client, _dir) = make_client("mandate_withdraw.db").await;
    let repo = AdmissionRepo::new(&client);

    repo.record_decision(decision("dec-w", "evt-w", "sender", "policy-1"))
        .await
        .unwrap();
    let mandate_id = grant(&repo, "dec-w", "sender", "policy-1").await;
    let before = repo.get_mandate(&mandate_id).await.unwrap().unwrap();
    assert!(before.withdrawn_at.is_none());
    assert!(before.withdrawn_reason.is_none());

    assert!(
        repo.withdraw_mandate(&mandate_id, "owner-stop")
            .await
            .unwrap(),
        "an open mandate withdraws exactly once"
    );
    let after = repo.get_mandate(&mandate_id).await.unwrap().unwrap();

    assert_eq!(after.mandate_id, before.mandate_id);
    assert_eq!(after.decision_id, before.decision_id);
    assert_eq!(after.matched_rule, before.matched_rule);
    assert_eq!(after.scope, before.scope, "scope is never edited in place");
    assert_eq!(after.strength, before.strength);
    assert_eq!(after.accept_mode, before.accept_mode);
    assert_eq!(after.policy_version, before.policy_version);
    assert_eq!(after.granted_at, before.granted_at);
    assert!(after.withdrawn_at.is_some_and(|t| t > 0));
    assert_eq!(after.withdrawn_reason.as_deref(), Some("owner-stop"));

    assert!(
        !repo
            .withdraw_mandate(&mandate_id, "second-stop")
            .await
            .unwrap(),
        "withdraw is guarded (WHERE withdrawn_at IS NULL)"
    );
    let unchanged = repo.get_mandate(&mandate_id).await.unwrap().unwrap();
    assert_eq!(
        unchanged.withdrawn_reason.as_deref(),
        Some("owner-stop"),
        "a repeated withdraw never rewrites the first reason"
    );
}

#[tokio::test]
async fn widening_scope_mints_a_new_mandate_and_keeps_the_old_grant() {
    let (client, _dir) = make_client("mandate_widen.db").await;
    let repo = AdmissionRepo::new(&client);

    repo.record_decision(decision("dec-narrow", "evt-narrow", "sender", "policy-1"))
        .await
        .unwrap();
    let narrow_id = grant(&repo, "dec-narrow", "sender", "policy-1").await;

    repo.record_decision(decision(
        "dec-wide",
        "evt-wide",
        "sender+thread",
        "policy-2",
    ))
    .await
    .unwrap();
    let wide_id = grant(&repo, "dec-wide", "sender+thread", "policy-2").await;

    assert_ne!(narrow_id, wide_id, "widening mints a new record");
    assert_eq!(count_mandates(&client).await, 2);

    let narrow = repo.get_mandate(&narrow_id).await.unwrap().unwrap();
    let wide = repo.get_mandate(&wide_id).await.unwrap().unwrap();
    assert_eq!(narrow.scope, "sender");
    assert_eq!(narrow.policy_version, "policy-1");
    assert_eq!(narrow.decision_id, "dec-narrow");
    assert!(narrow.withdrawn_at.is_none());
    assert_eq!(wide.scope, "sender+thread");
    assert_eq!(wide.policy_version, "policy-2");
    assert_eq!(wide.decision_id, "dec-wide");
}

#[tokio::test]
async fn ticket_open_and_close_leave_the_grant_untouched() {
    let (client, _dir) = make_client("mandate_ticket.db").await;
    let repo = AdmissionRepo::new(&client);

    repo.record_decision(decision("dec-t", "evt-t", "sender", "policy-1"))
        .await
        .unwrap();
    let mandate_id = grant(&repo, "dec-t", "sender", "policy-1").await;
    let before = repo.get_mandate(&mandate_id).await.unwrap().unwrap();

    let ticket_id = repo
        .open_ticket(TicketOpen {
            mandate_id: &mandate_id,
            reply_address: "group_A",
            turn_ref: Some("turn-1"),
            created_at: 1_700_000_002,
        })
        .await
        .unwrap();
    assert!(repo.close_ticket(&ticket_id, "ready").await.unwrap());

    let after = repo.get_mandate(&mandate_id).await.unwrap().unwrap();
    assert_eq!(after, before, "ticket lifecycle never mutates the mandate");
}
