use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::debug;

use zen_core::paths::ZenPaths;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatEntry {
    role: String,
    content: String,
    timestamp: DateTime<Utc>,
}

/// Conversation history manager — persists and loads chat history per session.
///
/// Storage: JSONL append to `~/.zen/sessions/<session_id>/chat.jsonl`.
/// One JSON object per line: `{role, content, timestamp}`.
/// Crash-safe, append-only (per spec.md ADR-003).
pub struct ConversationStore {
    session_id: String,
    file_path: PathBuf,
}

impl ConversationStore {
    /// Create or open a conversation store for the given session.
    pub fn open(session_id: &str) -> Result<Self> {
        let paths = ZenPaths::detect().context("failed to resolve zen paths")?;
        let dir = paths.sessions().join(session_id);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create session directory: {}", dir.display()))?;

        let file_path = dir.join("chat.jsonl");

        Ok(Self {
            session_id: session_id.to_string(),
            file_path,
        })
    }

    /// Append a message to the conversation log.
    pub fn append(&self, role: &str, content: &str) -> Result<()> {
        let entry = ChatEntry {
            role: role.to_string(),
            content: content.to_string(),
            timestamp: Utc::now(),
        };
        let line = serde_json::to_string(&entry).context("failed to serialize chat entry")?;

        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file_path)
            .with_context(|| format!("failed to open chat log: {}", self.file_path.display()))?
            .write_all(format!("{}\n", line).as_bytes())
            .with_context(|| format!("failed to write chat entry: {}", self.file_path.display()))?;

        debug!(
            session_id = %self.session_id,
            role,
            content_len = content.len(),
            "chat entry appended"
        );
        Ok(())
    }

    /// Load all conversation entries for this session.
    /// Returns vec of (role, content) pairs in chronological order.
    pub fn load(&self) -> Result<Vec<(String, String)>> {
        if !self.file_path.exists() {
            return Ok(Vec::new());
        }

        let content = std::fs::read_to_string(&self.file_path)
            .with_context(|| format!("failed to read chat log: {}", self.file_path.display()))?;

        let mut entries = Vec::new();
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let entry: ChatEntry = serde_json::from_str(line)
                .with_context(|| format!("failed to parse chat entry: {line}"))?;
            entries.push((entry.role, entry.content));
        }

        debug!(
            session_id = %self.session_id,
            count = entries.len(),
            "chat entries loaded"
        );
        Ok(entries)
    }

    /// Load the last N conversation entries (for context window management).
    pub fn load_recent(&self, n: usize) -> Result<Vec<(String, String)>> {
        let all = self.load()?;
        let start = all.len().saturating_sub(n);
        Ok(all[start..].to_vec())
    }

    pub fn copy_to(&self, target_session_id: &str) -> Result<Self> {
        let entries = self.load()?;
        let target = ConversationStore::open(target_session_id)?;
        for (role, content) in &entries {
            target.append(role, content)?;
        }
        Ok(target)
    }

    /// Export conversation to Markdown format.
    pub fn export_markdown(&self) -> Result<String> {
        let entries = self.load()?;
        let mut md = String::from("# Chat Export\n\n");
        md.push_str(&format!("Session: {}\n", self.session_id));
        md.push_str(&format!("Exported: {}\n\n", Utc::now().to_rfc3339()));
        md.push_str("---\n\n");

        for (role, content) in &entries {
            let role_label = match role.as_str() {
                "user" => "You",
                "assistant" => "Assistant",
                "system" => "System",
                other => other,
            };
            md.push_str(&format!("**{}**: {}\n\n", role_label, content));
        }

        Ok(md)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("chat.jsonl");

        let store = ConversationStore {
            session_id: "test".to_string(),
            file_path: file_path.clone(),
        };

        store.append("user", "Hello").unwrap();
        store.append("assistant", "Hi there!").unwrap();

        let entries = store.load().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], ("user".to_string(), "Hello".to_string()));
        assert_eq!(
            entries[1],
            ("assistant".to_string(), "Hi there!".to_string())
        );
    }

    #[test]
    fn load_empty_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConversationStore {
            session_id: "test".to_string(),
            file_path: dir.path().join("nonexistent.jsonl"),
        };
        assert!(store.load().unwrap().is_empty());
    }

    #[test]
    fn load_recent_returns_last_n() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConversationStore {
            session_id: "test".to_string(),
            file_path: dir.path().join("chat.jsonl"),
        };

        for i in 0..5 {
            store.append("user", &format!("msg {i}")).unwrap();
        }

        let recent = store.load_recent(2).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0], ("user".to_string(), "msg 3".to_string()));
        assert_eq!(recent[1], ("user".to_string(), "msg 4".to_string()));
    }

    #[test]
    fn export_markdown_format() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConversationStore {
            session_id: "test".to_string(),
            file_path: dir.path().join("chat.jsonl"),
        };

        store.append("user", "Hello").unwrap();
        store.append("assistant", "Hi!").unwrap();

        let md = store.export_markdown().unwrap();
        assert!(md.contains("# Chat Export"));
        assert!(md.contains("**You**: Hello"));
        assert!(md.contains("**Assistant**: Hi!"));
    }
}
