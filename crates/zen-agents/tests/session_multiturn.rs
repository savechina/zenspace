//! T104 (T059 contract): multi-turn session — two consecutive turns
//! persist in `SessionContext.conversation` in order with roles intact.
//!
//! The live 2-turn LLM leg needs a model; this file pins the session
//! bookkeeping the orchestrator depends on (append order, role
//! alternation, window bound).

use zen_core::types::{Message, MessageRole, SessionContext};

/// Conversation window the orchestrator keeps per session (quickstart §8).
const SESSION_WINDOW: usize = 20;

fn turn(role: MessageRole, content: &str) -> Message {
    Message {
        role,
        content: content.to_string(),
        timestamp: None,
    }
}

#[test]
fn two_turn_session_persists_in_order() {
    let mut session = SessionContext::new("Sisyphus".into(), String::new());
    session.conversation.push(turn(MessageRole::User, "q1"));
    session
        .conversation
        .push(turn(MessageRole::Assistant, "a1"));
    session.conversation.push(turn(MessageRole::User, "q2"));
    session
        .conversation
        .push(turn(MessageRole::Assistant, "a2"));

    assert_eq!(session.conversation.len(), 4);
    let roles: Vec<MessageRole> = session.conversation.iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        vec![
            MessageRole::User,
            MessageRole::Assistant,
            MessageRole::User,
            MessageRole::Assistant
        ]
    );
    assert_eq!(session.conversation[0].content, "q1");
    assert_eq!(session.conversation[3].content, "a2");
}

#[test]
fn session_window_bound_holds_two_turns() {
    let mut session = SessionContext::new("Sisyphus".into(), String::new());
    for i in 0..4 {
        session.conversation.push(turn(
            if i % 2 == 0 {
                MessageRole::User
            } else {
                MessageRole::Assistant
            },
            &format!("q{i}"),
        ));
    }
    assert!(
        session.conversation.len() <= SESSION_WINDOW,
        "a 2-turn session ({} messages) must fit the window ({}) intact",
        session.conversation.len(),
        SESSION_WINDOW
    );
}
