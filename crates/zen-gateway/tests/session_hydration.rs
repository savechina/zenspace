//! T226: daemon session-context hydration on `session/start`.
//!
//! PURPOSE: the daemon holds session state in memory only and implicit
//! daemons self-exit after 30 idle minutes, so a restart/idle-exit must
//! rebuild `SessionContext.conversation` from the persisted session jsonl
//! the TUI writes each turn. Proves: (a) adopting a session whose jsonl
//! carries prior turns yields a non-empty daemon conversation (user +
//! assistant only), (b) a session with no jsonl is adopted empty exactly
//! as before, and (c) a session already live in the map is never
//! re-hydrated (byte-identity).
//!
//! USAGE: cargo nextest run -p zen-gateway --test session_hydration
//!
//! EXPECTED: the test passes; hydration reads the SAME date-sharded
//! `{sessions}/YYYY/MM/DD/<id>.jsonl` path `ConversationStore::open`
//! resolves, so no new store or path convention is introduced.
//!
//! ERRORS: an empty conversation after a populated jsonl means the seam
//! never fired; changed turns on the live-session case mean byte-identity
//! regressed.
//!
//! Single-test binary: ZEN_HOME is process-global, so the three scenarios
//! run sequentially under one env mutex (memory_rebuild_provenance precedent).

use std::sync::Arc;

use serde_json::json;
use zen_core::paths::ZenPaths;
use zen_core::types::{MessageRole, SessionContext, session_created_at_from_id};
use zen_gateway::server::hosting::{SessionHost, start as hosting_start};
use zen_memory::conversation::ConversationStore;

/// Seeds a persisted session jsonl at the exact path `ConversationStore::open`
/// resolves for `session_id` (date-sharded from the UUIDv7 timestamp).
fn seed_jsonl(session_id: &str, turns: &[(&str, &str)]) {
    let paths = ZenPaths::detect().expect("zen paths");
    let created = session_created_at_from_id(session_id).expect("uuidv7 timestamp");
    let store = ConversationStore::with_dir(paths.session_dir_for_date(created), session_id)
        .expect("open conversation store");
    for (role, content) in turns {
        store.append(role, content).expect("append turn");
    }
}

async fn start_session(session_id: &str) -> Arc<SessionHost> {
    let deps = Arc::new(SessionHost::new(None));
    hosting_start(deps.clone(), json!({ "sessionId": session_id }))
        .await
        .expect("session/start");
    deps
}

async fn conversation_of(deps: &Arc<SessionHost>, session_id: &str) -> Vec<(MessageRole, String)> {
    deps.sessions
        .lock()
        .await
        .get(session_id)
        .expect("session adopted")
        .conversation
        .iter()
        .map(|m| (m.role, m.content.clone()))
        .collect()
}

#[tokio::test]
async fn start_hydrates_conversation_from_persisted_jsonl() {
    let tmp = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("ZEN_HOME", tmp.path()) };

    // (a) jsonl present ⇒ user + assistant turns adopted, system filtered.
    let with_history = uuid::Uuid::now_v7().to_string();
    seed_jsonl(
        &with_history,
        &[
            ("system", "ignored instruction"),
            ("user", "prior question marker Q1"),
            ("assistant", "prior answer marker A1"),
        ],
    );
    let deps = start_session(&with_history).await;
    let turns = conversation_of(&deps, &with_history).await;
    assert_eq!(turns.len(), 2, "hydrated turn count");
    assert_eq!(
        turns[0],
        (MessageRole::User, "prior question marker Q1".into())
    );
    assert_eq!(
        turns[1],
        (MessageRole::Assistant, "prior answer marker A1".into())
    );

    // (b) no jsonl ⇒ empty, exactly as before.
    let no_history = uuid::Uuid::now_v7().to_string();
    let deps = start_session(&no_history).await;
    assert!(
        conversation_of(&deps, &no_history).await.is_empty(),
        "no jsonl ⇒ empty, as before"
    );

    // (c) live session present in the map ⇒ never re-hydrated.
    let live_id = uuid::Uuid::now_v7().to_string();
    seed_jsonl(
        &live_id,
        &[
            ("user", "persisted turn 1"),
            ("assistant", "persisted turn 2"),
            ("user", "persisted turn 3"),
        ],
    );
    let mut live = SessionContext::new(live_id.clone(), String::new());
    live.add_turn(MessageRole::User, "in-process turn");
    let live_deps = Arc::new(SessionHost::new(None));
    live_deps
        .sessions
        .lock()
        .await
        .insert(live_id.clone(), live);
    hosting_start(live_deps.clone(), json!({ "sessionId": live_id }))
        .await
        .expect("session/start");
    let turns = conversation_of(&live_deps, &live_id).await;
    assert_eq!(turns.len(), 1, "live session untouched");
    assert_eq!(turns[0], (MessageRole::User, "in-process turn".into()));
}
