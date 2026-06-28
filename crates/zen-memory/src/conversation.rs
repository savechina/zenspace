use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::Utc;
use tracing::debug;

use zen_core::paths::ZenPaths;
use zen_core::types::{ChatTurnEvent, SessionEvent, session_created_at_from_id};

/// Conversation history manager — persists chat turns as typed events
/// in the session's single `.jsonl` file (Codex-style).
///
/// Storage: each session has one `<uuid>.jsonl` file at
/// `~/.zen/sessions/YYYY/MM/DD/<uuid>.jsonl`.
/// Chat turns are appended as `{"type":"chat/turn","payload":{...}}` lines.
///
/// The first line is a `session/meta` event written by `SessionEntity::save()`.
/// This store only appends `chat/turn` events to the same file.
pub struct ConversationStore {
    session_id: String,
    file_path: PathBuf,
}

impl ConversationStore {
    /// Create or open a conversation store for the given session.
    ///
    /// Constructs the path as `~/.zen/sessions/YYYY/MM/DD/<session_id>.jsonl`
    /// using the session's created_at date (which must be extractable from the
    /// UUID v7 session ID). For sessions where the date is unknown, use `with_file()`.
    pub fn open(session_id: &str) -> Result<Self> {
        let paths = ZenPaths::detect().context("failed to resolve zen paths")?;

        // Try to extract date from UUID v7 for path resolution
        let date_dir = if let Some(created_at) = session_created_at_from_id(session_id) {
            paths.session_dir_for_date(created_at)
        } else {
            paths.sessions().join(session_id)
        };

        let file_path = date_dir.join(format!("{}.jsonl", session_id));

        Ok(Self {
            session_id: session_id.to_string(),
            file_path,
        })
    }

    /// Create a conversation store using an explicit directory and session ID.
    ///
    /// The chat file will be placed at `<dir>/<session_id>.jsonl`.
    /// Prefer this over `open()` when the date directory is already known.
    pub fn with_dir(dir: PathBuf, session_id: &str) -> Result<Self> {
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create session directory: {}", dir.display()))?;

        let file_path = dir.join(format!("{}.jsonl", session_id));

        Ok(Self {
            session_id: session_id.to_string(),
            file_path,
        })
    }

    /// Create a conversation store from a specific file path.
    pub fn with_file(file_path: PathBuf, session_id: &str) -> Result<Self> {
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create parent directory: {}", parent.display())
            })?;
        }

        Ok(Self {
            session_id: session_id.to_string(),
            file_path,
        })
    }

    /// Append a chat turn to the session's `.jsonl` file.
    ///
    /// Writes a `chat/turn` event line. The file is created on first write;
    /// the `session/meta` event (written by `SessionEntity::save()`) must
    /// already exist as the first line.
    pub fn append(&self, role: &str, content: &str) -> Result<()> {
        let event = SessionEvent::Turn(ChatTurnEvent {
            role: role.to_string(),
            content: content.to_string(),
            timestamp: Utc::now(),
        });
        let line = serde_json::to_string(&event).context("failed to serialize chat/turn event")?;

        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file_path)
            .with_context(|| format!("failed to open session file: {}", self.file_path.display()))?
            .write_all(format!("{}\n", line).as_bytes())
            .with_context(|| {
                format!(
                    "failed to write chat/turn event: {}",
                    self.file_path.display()
                )
            })?;

        debug!(
            session_id = %self.session_id,
            role,
            content_len = content.len(),
            "chat/turn appended to session file"
        );
        Ok(())
    }

    /// Load all conversation turns from the session's `.jsonl` file.
    ///
    /// Reads every `chat/turn` event in order, skipping `session/meta` and
    /// any other event types (future: tool/call, etc.).
    /// Returns `Vec<(role, content)>` in chronological order.
    pub fn load(&self) -> Result<Vec<(String, String)>> {
        if !self.file_path.exists() {
            return Ok(Vec::new());
        }

        let content = std::fs::read_to_string(&self.file_path).with_context(|| {
            format!("failed to read session file: {}", self.file_path.display())
        })?;

        let mut entries = Vec::new();
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(event) = serde_json::from_str::<SessionEvent>(line) {
                match event {
                    SessionEvent::Turn(turn) => {
                        entries.push((turn.role, turn.content));
                    }
                    SessionEvent::Meta(_) => {
                        // skip metadata event
                    }
                }
            }
        }

        debug!(
            session_id = %self.session_id,
            count = entries.len(),
            "chat turns loaded from session file"
        );
        Ok(entries)
    }

    /// Load the last N conversation turns (for context window management).
    pub fn load_recent(&self, n: usize) -> Result<Vec<(String, String)>> {
        let all = self.load()?;
        let start = all.len().saturating_sub(n);
        Ok(all[start..].to_vec())
    }

    pub fn copy_to(&self, target_session_id: &str) -> Result<Self> {
        let target_path = Self::resolve_path_for_session(target_session_id)?;
        self.copy_to_path(target_path, target_session_id)
    }

    pub fn copy_to_dir(&self, target_dir: PathBuf, target_session_id: &str) -> Result<Self> {
        let target_path = target_dir.join(format!("{}.jsonl", target_session_id));
        self.copy_to_path(target_path, target_session_id)
    }

    fn copy_to_path(&self, target_path: PathBuf, target_session_id: &str) -> Result<Self> {
        let entries = self.load()?;
        let target = ConversationStore::with_file(target_path, target_session_id)?;
        for (role, content) in &entries {
            target.append(role, content)?;
        }
        Ok(target)
    }

    /// Resolve the `.jsonl` path for a session ID (used by copy_to).
    fn resolve_path_for_session(session_id: &str) -> Result<PathBuf> {
        let paths = ZenPaths::detect().context("failed to resolve zen paths")?;
        let date_dir = if let Some(created_at) = session_created_at_from_id(session_id) {
            paths.session_dir_for_date(created_at)
        } else {
            paths.sessions().join(session_id)
        };
        Ok(date_dir.join(format!("{}.jsonl", session_id)))
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
    use zen_core::types::SessionEntity;

    #[test]
    fn append_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.jsonl");

        // First write the meta event (as SessionEntity::save() would)
        let session = SessionEntity::new("test-agent", "/ws");
        SessionEvent::write_meta(&file_path, &session).unwrap();

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
    fn load_empty_file_with_only_meta_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.jsonl");
        let session = SessionEntity::new("test-agent", "/ws");
        SessionEvent::write_meta(&file_path, &session).unwrap();

        let store = ConversationStore {
            session_id: "test".to_string(),
            file_path,
        };

        // Only meta event, no chat turns
        let entries = store.load().unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn load_recent_returns_last_n() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.jsonl");

        let session = SessionEntity::new("test-agent", "/ws");
        SessionEvent::write_meta(&file_path, &session).unwrap();

        let store = ConversationStore {
            session_id: "test".to_string(),
            file_path: file_path.clone(),
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
        let file_path = dir.path().join("test.jsonl");

        let session = SessionEntity::new("test-agent", "/ws");
        SessionEvent::write_meta(&file_path, &session).unwrap();

        let store = ConversationStore {
            session_id: "test".to_string(),
            file_path: file_path.clone(),
        };

        store.append("user", "Hello").unwrap();
        store.append("assistant", "Hi!").unwrap();

        let md = store.export_markdown().unwrap();
        assert!(md.contains("# Chat Export"));
        assert!(md.contains("**You**: Hello"));
        assert!(md.contains("**Assistant**: Hi!"));
    }

    #[test]
    fn with_dir_creates_session_file_not_shared() {
        let dir = tempfile::tempdir().unwrap();

        let store1 = ConversationStore::with_dir(dir.path().to_path_buf(), "session-a").unwrap();
        store1.append("user", "msg from A").unwrap();

        let store2 = ConversationStore::with_dir(dir.path().to_path_buf(), "session-b").unwrap();
        store2.append("user", "msg from B").unwrap();

        // Each session has its own .jsonl file
        assert!(dir.path().join("session-a.jsonl").exists());
        assert!(dir.path().join("session-b.jsonl").exists());

        let entries_a = store1.load().unwrap();
        assert_eq!(entries_a.len(), 1);
        assert_eq!(entries_a[0].1, "msg from A");

        let entries_b = store2.load().unwrap();
        assert_eq!(entries_b.len(), 1);
        assert_eq!(entries_b[0].1, "msg from B");
    }
}
