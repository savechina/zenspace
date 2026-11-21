//! Principle XIII #6: migration invariants for 016_admission.sql.
//!
//! PURPOSE: prove the three trust-ledger tables apply cleanly and round-trip
//! through AdmissionRepo — dedup by (source, event_id) and the content-hash
//! fallback, mandate withdraw-only mutation, and the live-ticket continuation
//! lookup the gate caller depends on (FR-003/FR-011/FR-013, research D6/D8).

use tempfile::tempdir;
use zen_repo::{
    AdmissionRepo, DecisionOutcome, DecisionRecord, DispositionFilter, MandateGrant, SqliteClient,
    TicketOpen,
};

async fn make_client(name: &str) -> (SqliteClient, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let client = SqliteClient::open(&dir.path().join(name)).await.unwrap();
    (client, dir)
}

fn decision<'a>(
    id: &'a str,
    source: &'a str,
    event_id: Option<&'a str>,
    content_hash: &'a str,
    disposition: &'a str,
    created_at: i64,
) -> DecisionRecord<'a> {
    DecisionRecord {
        id,
        source,
        event_id,
        content_hash,
        sender: "owner-openid",
        thread_key: "group_A",
        content: "please summarize the inbox",
        disposition,
        matched_rule: Some("allowlist"),
        scope: Some("work:read"),
        accept_mode: "observe",
        strength: Some("explicit"),
        policy_version: "pv1",
        reason: None,
        created_at,
    }
}

#[tokio::test]
async fn migration_016_is_recorded_and_creates_three_tables() {
    let (client, _dir) = make_client("m016.db").await;

    let applied: Option<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE version = 16")
            .fetch_optional(client.pool())
            .await
            .unwrap();
    assert_eq!(applied, Some(16), "_sqlx_migrations must record 016");

    for table in [
        "admission_decisions",
        "admission_mandates",
        "admission_tickets",
    ] {
        let name: Option<String> =
            sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1")
                .bind(table)
                .fetch_optional(client.pool())
                .await
                .unwrap();
        assert_eq!(name.as_deref(), Some(table), "table {table} must exist");
    }

    let index: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'index' \
         AND name = 'idx_admission_decisions_source_content'",
    )
    .fetch_optional(client.pool())
    .await
    .unwrap();
    assert!(
        index.is_some(),
        "the content-hash fallback partial index must exist"
    );
}

#[tokio::test]
async fn stable_event_replay_records_a_duplicate_and_never_readmits() {
    let (client, _dir) = make_client("m016_dedup.db").await;
    let repo = AdmissionRepo::new(&client);

    let first = repo
        .record_decision(decision(
            "d1",
            "qqbot",
            Some("evt-1"),
            "hash-1",
            "would_accept",
            100,
        ))
        .await
        .unwrap();
    assert_eq!(first, DecisionOutcome::New);

    let replay = repo
        .record_decision(decision(
            "d2",
            "qqbot",
            Some("evt-1"),
            "hash-1",
            "would_accept",
            101,
        ))
        .await
        .unwrap();
    let duplicate_id = match replay {
        DecisionOutcome::Duplicate { id } => id,
        other => panic!("expected duplicate outcome, got {other:?}"),
    };
    assert_ne!(duplicate_id, "d2", "the duplicate row gets a fresh id");

    let original = repo.get_decision("d1").await.unwrap().expect("original");
    assert_eq!(original.disposition, "would_accept");
    assert!(
        repo.get_decision("d2").await.unwrap().is_none(),
        "the replay must not land a second decision under the caller id"
    );

    let rows = repo
        .list_decisions(DispositionFilter::default(), 10)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2, "original + duplicate record");
    assert_eq!(
        rows.iter().filter(|r| r.disposition == "duplicate").count(),
        1
    );
}

#[tokio::test]
async fn missing_event_id_falls_back_to_content_hash_dedup() {
    let (client, _dir) = make_client("m016_hash.db").await;
    let repo = AdmissionRepo::new(&client);

    assert_eq!(
        repo.record_decision(decision("h1", "qqbot", None, "hash-body", "admitted", 100))
            .await
            .unwrap(),
        DecisionOutcome::New
    );
    let replay = repo
        .record_decision(decision("h2", "qqbot", None, "hash-body", "admitted", 101))
        .await
        .unwrap();
    let duplicate_id = match replay {
        DecisionOutcome::Duplicate { id } => id,
        other => panic!("expected content-hash duplicate, got {other:?}"),
    };
    let duplicate = repo
        .get_decision(&duplicate_id)
        .await
        .unwrap()
        .expect("duplicate row");
    assert!(
        duplicate
            .event_id
            .as_deref()
            .is_some_and(|e| e.starts_with("dup:")),
        "the duplicate row keeps a synthetic non-NULL event_id out of both indexes"
    );

    assert_eq!(
        repo.record_decision(decision("h3", "mail", None, "hash-body", "admitted", 102))
            .await
            .unwrap(),
        DecisionOutcome::New,
        "the same body on another source is a distinct event"
    );
    assert_eq!(
        repo.record_decision(decision("h4", "qqbot", None, "hash-other", "admitted", 103))
            .await
            .unwrap(),
        DecisionOutcome::New,
        "a different body on the same source is a distinct event"
    );
}

#[tokio::test]
async fn mandate_withdraw_touches_only_withdraw_columns() {
    let (client, _dir) = make_client("m016_mandate.db").await;
    let repo = AdmissionRepo::new(&client);

    repo.record_decision(decision(
        "d-m",
        "qqbot",
        Some("evt-m"),
        "hash-m",
        "admitted",
        100,
    ))
    .await
    .unwrap();
    let mandate_id = repo
        .grant_mandate(MandateGrant {
            decision_id: "d-m",
            matched_rule: Some("allowlist"),
            scope: "work:read",
            strength: "explicit",
            accept_mode: "auto",
            policy_version: "pv1",
            granted_at: 101,
        })
        .await
        .unwrap();

    assert!(
        repo.withdraw_mandate(&mandate_id, "owner-stop")
            .await
            .unwrap(),
        "open mandate withdraws"
    );
    let mandate = repo.get_mandate(&mandate_id).await.unwrap().expect("row");
    assert!(mandate.withdrawn_at.is_some_and(|t| t > 0));
    assert_eq!(mandate.withdrawn_reason.as_deref(), Some("owner-stop"));
    assert_eq!(
        mandate.scope, "work:read",
        "scope is never touched in place"
    );
    assert_eq!(mandate.strength, "explicit");
    assert_eq!(mandate.accept_mode, "auto");
    assert_eq!(mandate.granted_at, 101);

    assert!(
        !repo.withdraw_mandate(&mandate_id, "again").await.unwrap(),
        "withdraw is guarded WHERE withdrawn_at IS NULL"
    );
}

#[tokio::test]
async fn ticket_lifecycle_and_thread_continuation_lookup() {
    let (client, _dir) = make_client("m016_ticket.db").await;
    let repo = AdmissionRepo::new(&client);

    repo.record_decision(decision(
        "d-t",
        "qqbot",
        Some("evt-t"),
        "hash-t",
        "admitted",
        100,
    ))
    .await
    .unwrap();
    let mandate_id = repo
        .grant_mandate(MandateGrant {
            decision_id: "d-t",
            matched_rule: Some("allowlist"),
            scope: "work:read",
            strength: "explicit",
            accept_mode: "auto",
            policy_version: "pv1",
            granted_at: 101,
        })
        .await
        .unwrap();
    let ticket_id = repo
        .open_ticket(TicketOpen {
            mandate_id: &mandate_id,
            reply_address: "group:group_A",
            turn_ref: Some("turn-1"),
            created_at: 102,
        })
        .await
        .unwrap();

    let live = repo
        .open_ticket_for_thread("group_A")
        .await
        .unwrap()
        .expect("live ticket on the thread");
    assert_eq!(live.ticket_id, ticket_id);
    assert_eq!(live.status, "open");
    assert_eq!(live.report_state, None);
    assert_eq!(live.reply_address, "group:group_A");
    assert_eq!(live.turn_ref.as_deref(), Some("turn-1"));

    assert!(
        repo.open_ticket_for_thread("group_B")
            .await
            .unwrap()
            .is_none(),
        "another thread has no continuation"
    );

    assert!(repo.close_ticket(&ticket_id, "ready").await.unwrap());
    let closed: (String, Option<String>) =
        sqlx::query_as("SELECT status, report_state FROM admission_tickets WHERE ticket_id = ?1")
            .bind(&ticket_id)
            .fetch_one(client.pool())
            .await
            .unwrap();
    assert_eq!(closed.0, "ended");
    assert_eq!(closed.1.as_deref(), Some("ready"));
    assert!(
        repo.open_ticket_for_thread("group_A")
            .await
            .unwrap()
            .is_none(),
        "an ended ticket is no longer a continuation"
    );
    assert!(
        !repo.close_ticket(&ticket_id, "ready").await.unwrap(),
        "closing an ended ticket is a no-op"
    );
}

#[tokio::test]
async fn list_decisions_filters_by_disposition_and_since() {
    let (client, _dir) = make_client("m016_list.db").await;
    let repo = AdmissionRepo::new(&client);

    repo.record_decision(decision(
        "d-r1",
        "qqbot",
        Some("evt-r"),
        "hash-r",
        "rejected",
        100,
    ))
    .await
    .unwrap();
    repo.record_decision(decision(
        "d-w1",
        "qqbot",
        Some("evt-w"),
        "hash-w",
        "would_accept",
        200,
    ))
    .await
    .unwrap();

    let all = repo
        .list_decisions(DispositionFilter::default(), 10)
        .await
        .unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].id, "d-w1", "newest first");

    let rejected = repo
        .list_decisions(
            DispositionFilter {
                disposition: Some("rejected".to_string()),
                since: None,
            },
            10,
        )
        .await
        .unwrap();
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0].id, "d-r1");

    let recent = repo
        .list_decisions(
            DispositionFilter {
                disposition: None,
                since: Some(150),
            },
            10,
        )
        .await
        .unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].id, "d-w1");

    let capped = repo
        .list_decisions(DispositionFilter::default(), 1)
        .await
        .unwrap();
    assert_eq!(capped.len(), 1);
    assert_eq!(capped[0].id, "d-w1");
}

#[tokio::test]
async fn foreign_keys_are_enforced_on_the_admission_tables() {
    let (client, _dir) = make_client("m016_fk.db").await;
    let repo = AdmissionRepo::new(&client);

    let err = repo
        .grant_mandate(MandateGrant {
            decision_id: "ghost-decision",
            matched_rule: None,
            scope: "work:read",
            strength: "explicit",
            accept_mode: "auto",
            policy_version: "pv1",
            granted_at: 100,
        })
        .await;
    assert!(err.is_err(), "a mandate needs a real decision row");

    let err = repo
        .open_ticket(TicketOpen {
            mandate_id: "ghost-mandate",
            reply_address: "group:group_A",
            turn_ref: None,
            created_at: 100,
        })
        .await;
    assert!(err.is_err(), "a ticket needs a real mandate row");
}

#[tokio::test]
async fn reopen_keeps_admission_rows_and_does_not_reapply_016() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("m016_reopen.db");

    {
        let client = SqliteClient::open(&db).await.unwrap();
        let repo = AdmissionRepo::new(&client);
        repo.record_decision(decision(
            "keep",
            "qqbot",
            Some("evt-keep"),
            "hash-keep",
            "unresolved",
            100,
        ))
        .await
        .unwrap();
    }

    let client = SqliteClient::open(&db).await.unwrap();
    let repo = AdmissionRepo::new(&client);
    let row = repo
        .get_decision("keep")
        .await
        .unwrap()
        .expect("row survives");
    assert_eq!(row.disposition, "unresolved");

    let applied_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = 16")
            .fetch_one(client.pool())
            .await
            .unwrap();
    assert_eq!(applied_count, 1, "016 applies exactly once");
}
