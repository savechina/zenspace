//! D7: daemon-side conversation persistence (single writer).
//!
//! PURPOSE: session-conversation jsonl persistence moved from the TUI
//! client into the gateway daemon so ALL hosted surfaces (TUI, `zen
//! chat`, qqbot, HTTP) persist conversations — fixing hydration
//! starvation (hosting.rs `hydrate_session` reads the jsonl that
//! non-TUI surfaces never wrote) and memory-replay starvation
//! (`SessionReplayer` feeds the same jsonl into memvid). Pins: (a) a
//! completed hosted turn lands at the hydrate-discoverable
//! `{sessions}/YYYY/MM/DD/<id>.jsonl` path with `session/meta` first,
//! then user and assistant `chat/turn` lines (event shapes identical to
//! the removed TUI writer), and a FRESH daemon adopting the session
//! hydrates those turns; (b) `session/start` alone mints the meta
//! first-event; (c) cancelled turns append nothing; (d) execution-error
//! turns append nothing (mirrors client Err semantics); (e) two
//! sequential turns append, never overwrite; (f) persist failure (read-
//! only home) leaves the turn Ok. The warn-on-failure half of (f) is
//! structural (every Err path in `persist_completed_turn` warns); log
//! capture is not asserted (no subscriber harness precedent in this
//! suite).
//!
//! USAGE: cargo nextest run -p zen-gateway --test session_persistence
//!
//! EXPECTED: all pins pass; the daemon-written file is byte-compatible
//! with `zen-memory::ConversationStore` readers (`SessionReplayer`,
//! `hydrate_session`).
//!
//! ERRORS: an empty hydration in (a) means the writer path diverged
//! from `ConversationStore::open` resolution; turn lines after a
//! cancelled/error turn mean the completed-only contract regressed.
//!
//! Single-test binary: ZEN_HOME is process-global, so the scenarios run
//! sequentially under one env mutex (session_hydration precedent).

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use zen_core::paths::ZenPaths;
use zen_core::types::{MessageRole, SessionContext, SessionEvent, session_created_at_from_id};
use zen_gateway::server::hosting::{
    SessionHost, TurnExecutor, TurnState, cancel as hosting_cancel, start as hosting_start,
    turn as hosting_turn,
};
use zen_memory::conversation::ConversationStore;

/// Executor completing instantly with a canned reply.
struct QuickExec;

#[async_trait::async_trait]
impl TurnExecutor for QuickExec {
    async fn execute_stream(
        &self,
        _session: &mut SessionContext,
        _prompt: &str,
        _callback: &mut (dyn FnMut(String) + Send),
    ) -> anyhow::Result<String> {
        Ok("canned reply".to_string())
    }
}

/// Executor that hangs long enough for a client cancel to win.
struct SlowExec;

#[async_trait::async_trait]
impl TurnExecutor for SlowExec {
    async fn execute_stream(
        &self,
        _session: &mut SessionContext,
        _prompt: &str,
        _callback: &mut (dyn FnMut(String) + Send),
    ) -> anyhow::Result<String> {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok("never".to_string())
    }
}

/// Executor that fails execution (agent error, not cancel).
struct FailingExec;

#[async_trait::async_trait]
impl TurnExecutor for FailingExec {
    async fn execute_stream(
        &self,
        _session: &mut SessionContext,
        _prompt: &str,
        _callback: &mut (dyn FnMut(String) + Send),
    ) -> anyhow::Result<String> {
        Err(anyhow::anyhow!("boom"))
    }
}

fn deps_with(executor: Option<Arc<dyn TurnExecutor>>) -> Arc<SessionHost> {
    Arc::new(SessionHost::new(executor))
}

fn turn_params(turn: &str, session: &str, prompt: &str) -> Value {
    json!({"turnId": turn, "sessionId": session, "prompt": prompt})
}

async fn start_and_register(deps: Arc<SessionHost>, session_id: &str) -> Arc<SessionHost> {
    hosting_start(deps.clone(), json!({ "sessionId": session_id }))
        .await
        .expect("session/start");
    deps
}

fn session_jsonl(session_id: &str) -> std::path::PathBuf {
    let paths = ZenPaths::detect().expect("zen paths");
    let created = session_created_at_from_id(session_id).expect("uuidv7 timestamp");
    paths
        .session_dir_for_date(created)
        .join(format!("{session_id}.jsonl"))
}

fn read_lines(session_id: &str) -> Vec<String> {
    std::fs::read_to_string(session_jsonl(session_id))
        .expect("session jsonl exists")
        .lines()
        .map(str::to_string)
        .collect()
}

/// Extracts `(role, content)` from a `chat/turn` event line.
fn turn_of(line: &str) -> (String, String) {
    let event: SessionEvent = serde_json::from_str(line).expect("line parses as SessionEvent");
    match event {
        SessionEvent::Turn(message) => (message.role.to_string(), message.content),
        other => panic!("expected chat/turn event, got {other:?}"),
    }
}

#[tokio::test]
async fn daemon_persists_completed_turns_single_writer_contract() {
    let tmp = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("ZEN_HOME", tmp.path()) };

    // ── (b) session/start alone mints the meta first-event ──
    let deps = deps_with(None);
    let started = hosting_start(deps, json!({})).await.expect("session/start");
    let minted_id = started["sessionId"]
        .as_str()
        .expect("sessionId")
        .to_string();
    let minted_lines = read_lines(&minted_id);
    assert_eq!(minted_lines.len(), 1, "session/start ⇒ meta-only jsonl");
    let meta_event: SessionEvent =
        serde_json::from_str(&minted_lines[0]).expect("meta line parses as SessionEvent");
    assert!(
        matches!(meta_event, SessionEvent::Meta(_)),
        "first event must be session/meta"
    );

    // ── (a) completed hosted turn → meta-first jsonl at the hydrate path ──
    let session_a = uuid::Uuid::now_v7().to_string();
    let deps_a = start_and_register(deps_with(Some(Arc::new(QuickExec))), &session_a).await;
    let result = hosting_turn(
        deps_a.clone(),
        None,
        turn_params("t1", &session_a, "first question"),
    )
    .await
    .expect("turn completes");
    assert_eq!(result["response"], "canned reply");

    // Path-parity shape: the exact {sessions}/YYYY/MM/DD/<id>.jsonl the
    // removed TUI writer used.
    let path = session_jsonl(&session_a);
    assert!(path.exists(), "daemon-written jsonl at the TUI-writer path");

    let lines = read_lines(&session_a);
    assert_eq!(lines.len(), 3, "meta + user + assistant");
    let first: Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(first["type"], "session/meta", "meta stays the first line");
    let (role1, content1) = turn_of(&lines[1]);
    assert_eq!(role1, MessageRole::User.to_string());
    assert_eq!(content1, "first question");
    let (role2, content2) = turn_of(&lines[2]);
    assert_eq!(role2, MessageRole::Assistant.to_string());
    assert_eq!(content2, "canned reply");

    // Hydrate discoverability: a FRESH daemon adopting the session reads
    // the daemon-written file back (the D7 starvation fix, pinned).
    let fresh = deps_with(None);
    hosting_start(fresh.clone(), json!({ "sessionId": session_a }))
        .await
        .expect("fresh session/start");
    let hydrated = fresh.sessions.lock().await[&session_a].conversation.clone();
    assert_eq!(
        hydrated.len(),
        2,
        "fresh daemon hydrates daemon-written turns"
    );
    assert_eq!(hydrated[0].role, MessageRole::User);
    assert_eq!(hydrated[0].content, "first question");
    assert_eq!(hydrated[1].role, MessageRole::Assistant);
    assert_eq!(hydrated[1].content, "canned reply");

    // Reader-parity: `ConversationStore::open` (the hydrate_session /
    // SessionReplayer entry point) sees the same turns from the file the
    // daemon wrote.
    let store = ConversationStore::open(&session_a).expect("store open");
    let entries = store.load().expect("store load");
    assert_eq!(
        entries,
        vec![
            ("user".to_string(), "first question".to_string()),
            ("assistant".to_string(), "canned reply".to_string()),
        ]
    );

    // ── (e) two sequential turns → append, never overwrite ──
    hosting_turn(
        deps_a.clone(),
        None,
        turn_params("t2", &session_a, "second question"),
    )
    .await
    .expect("second turn completes");
    let lines = read_lines(&session_a);
    assert_eq!(lines.len(), 5, "meta + 2×(user+assistant) appended");
    let (role4, content4) = turn_of(&lines[3]);
    assert_eq!(
        (role4.as_str(), content4.as_str()),
        ("user", "second question")
    );
    let first_meta = lines[0].clone();
    assert_eq!(read_lines(&session_a)[0], first_meta, "meta line untouched");

    // ── (c) cancelled turn → no turn lines appended ──
    let session_c = uuid::Uuid::now_v7().to_string();
    let deps_c = start_and_register(deps_with(Some(Arc::new(SlowExec))), &session_c).await;
    let host = tokio::spawn({
        let deps = Arc::clone(&deps_c);
        let session = session_c.clone();
        async move { hosting_turn(deps, None, turn_params("t-c", &session, "slow")).await }
    });
    for _ in 0..250 {
        if deps_c.turns.get("t-c").is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    hosting_cancel(Arc::clone(&deps_c), json!({ "turnId": "t-c" }))
        .await
        .expect("cancel");
    let err = host.await.unwrap().expect_err("cancelled turn errors");
    assert_eq!(err.code, -32603);
    assert_eq!(
        deps_c.turns.get("t-c").unwrap().state(),
        TurnState::Cancelled
    );
    assert_eq!(
        read_lines(&session_c).len(),
        1,
        "cancelled turn appends nothing (client-semantics parity)"
    );

    // ── (d) execution-error turn → no turn lines appended ──
    let session_e = uuid::Uuid::now_v7().to_string();
    let deps_e = start_and_register(deps_with(Some(Arc::new(FailingExec))), &session_e).await;
    let err = hosting_turn(deps_e, None, turn_params("t-e", &session_e, "doomed"))
        .await
        .expect_err("execution failure surfaces as RpcError");
    assert_eq!(err.code, -32603);
    assert_eq!(
        read_lines(&session_e).len(),
        1,
        "error turn appends nothing (client Err-path parity)"
    );

    // ── (f) persist failure (read-only target dir) → turn still Ok ──
    let session_f = uuid::Uuid::now_v7().to_string();
    use std::os::unix::fs::PermissionsExt;
    // Creation permission is governed by the DEEPEST directory: a read-only
    // home root would not block writes into the (already existing) date dir,
    // so the date dir itself is the one made read-only.
    let date_dir = {
        let paths = ZenPaths::detect().expect("zen paths");
        let created = session_created_at_from_id(&session_f).expect("uuidv7 timestamp");
        paths.session_dir_for_date(created)
    };
    std::fs::create_dir_all(&date_dir).unwrap();
    let mut perms = std::fs::metadata(&date_dir).unwrap().permissions();
    perms.set_mode(0o555);
    std::fs::set_permissions(&date_dir, perms.clone()).unwrap();
    let deps_f = start_and_register(deps_with(Some(Arc::new(QuickExec))), &session_f).await;
    let result = hosting_turn(deps_f, None, turn_params("t-f", &session_f, "resilient"))
        .await
        .expect("persist failure must not fail the turn");
    assert_eq!(result["response"], "canned reply");
    assert!(
        !session_jsonl(&session_f).exists(),
        "no jsonl materialized under a read-only target dir"
    );
    perms.set_mode(0o755);
    std::fs::set_permissions(&date_dir, perms).unwrap();
}
