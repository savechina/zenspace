//! Legacy intent classification — superseded by `AgentOrchestrator::route()` from zen-agents.
#![allow(deprecated)]

use serde::{Deserialize, Serialize};

#[deprecated(
    since = "0.1.0",
    note = "Use `AgentOrchestrator::route()` from zen-agents instead"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserIntent {
    KnowledgeQuery { query: String },
    NoteCreation { content: String },
    SystemCommand { command: String },
    Conversation { message: String },
}

#[deprecated(
    since = "0.1.0",
    note = "Use `AgentOrchestrator::route()` from zen-agents instead"
)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentResult {
    pub intent: UserIntent,
    pub confidence: f64,
}

#[deprecated(
    since = "0.1.0",
    note = "Use `AgentOrchestrator::route()` from zen-agents instead"
)]
pub fn classify_intent(input: &str) -> IntentResult {
    let trimmed = input.trim();

    if let Some(stripped) = trimmed.strip_prefix('/') {
        let cmd = stripped.split_whitespace().next().unwrap_or("");
        return IntentResult {
            intent: UserIntent::SystemCommand {
                command: cmd.to_string(),
            },
            confidence: 1.0,
        };
    }

    let lower = trimmed.to_lowercase();

    if lower.starts_with("note") || lower.starts_with("create") || lower.starts_with("save") {
        return IntentResult {
            intent: UserIntent::NoteCreation {
                content: trimmed.to_string(),
            },
            confidence: 0.8,
        };
    }

    if lower.starts_with("search") || lower.starts_with("find") || lower.starts_with("look up") {
        let query = trimmed
            .split_once(&[' ', ':'][..])
            .map(|x| x.1)
            .unwrap_or(trimmed)
            .trim();
        return IntentResult {
            intent: UserIntent::KnowledgeQuery {
                query: query.to_string(),
            },
            confidence: 0.85,
        };
    }

    if trimmed.contains('?') || trimmed.len() > 20 {
        return IntentResult {
            intent: UserIntent::Conversation {
                message: trimmed.to_string(),
            },
            confidence: 0.7,
        };
    }

    IntentResult {
        intent: UserIntent::KnowledgeQuery {
            query: trimmed.to_string(),
        },
        confidence: 0.5,
    }
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;

    #[test]
    fn classifies_slash_command() {
        let result = classify_intent("/search hello");
        assert!(matches!(result.intent, UserIntent::SystemCommand { .. }));
        assert_eq!(result.confidence, 1.0);
    }

    #[test]
    fn classifies_note_creation() {
        let result = classify_intent("note this is important");
        assert!(matches!(result.intent, UserIntent::NoteCreation { .. }));
    }

    #[test]
    fn classifies_search_query() {
        let result = classify_intent("search for rust patterns");
        assert!(matches!(result.intent, UserIntent::KnowledgeQuery { .. }));
    }

    #[test]
    fn classifies_conversation() {
        let result = classify_intent("What is the status of the auth refactor?");
        assert!(matches!(result.intent, UserIntent::Conversation { .. }));
    }

    #[test]
    fn defaults_to_knowledge_query() {
        let result = classify_intent("hello");
        assert!(matches!(result.intent, UserIntent::KnowledgeQuery { .. }));
    }
}
