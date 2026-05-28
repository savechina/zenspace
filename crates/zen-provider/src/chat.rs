use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub content: String,
    pub timestamp: DateTime<Utc>,
}

impl ChatMessage {
    pub fn system(content: &str) -> Self {
        Self {
            role: MessageRole::System,
            content: content.to_string(),
            timestamp: Utc::now(),
        }
    }

    pub fn user(content: &str) -> Self {
        Self {
            role: MessageRole::User,
            content: content.to_string(),
            timestamp: Utc::now(),
        }
    }

    pub fn assistant(content: &str) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: content.to_string(),
            timestamp: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSession {
    pub id: String,
    pub messages: Vec<ChatMessage>,
    pub context_window: usize,
}

impl ChatSession {
    pub fn new(id: &str, context_window: usize) -> Self {
        Self {
            id: id.to_string(),
            messages: Vec::new(),
            context_window,
        }
    }

    pub fn add_message(&mut self, msg: ChatMessage) {
        self.messages.push(msg);
    }

    pub fn system_prompt(&self) -> Option<&str> {
        self.messages
            .iter()
            .find(|m| m.role == MessageRole::System)
            .map(|m| m.content.as_str())
    }

    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    pub fn estimated_tokens(&self) -> usize {
        self.messages.iter().map(|m| m.content.len() / 4 + 1).sum()
    }
}
