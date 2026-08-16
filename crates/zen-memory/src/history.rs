use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::debug;

use zen_core::constants::{HISTORY_DEFAULT_MAX_BYTES, HISTORY_FILE, HISTORY_SOFT_CAP_RATIO};
use zen_core::paths::ZenPaths;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HistoryEntry {
    text: String,
    ts: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
}

/// Global command history manager — persists user input to `~/.zen/history.jsonl`.
///
/// Schema: `{"text":"...","ts":...,"session_id":"..."}` (Codex CLI compatible).
/// Byte-capped: oldest entries trimmed when file exceeds max_bytes.
#[derive(Debug, Clone)]
pub struct HistoryStore {
    file_path: PathBuf,
    max_bytes: u64,
}

impl HistoryStore {
    /// Open the global history file. Creates it if missing.
    pub fn open(max_bytes: Option<u64>) -> Result<Self> {
        let paths = ZenPaths::detect().context("failed to resolve zen paths")?;
        let file_path = paths.global_root().join(HISTORY_FILE);
        Ok(Self {
            file_path,
            max_bytes: max_bytes.unwrap_or(HISTORY_DEFAULT_MAX_BYTES),
        })
    }

    /// Open history from a specific path (for tests / isolation).
    pub fn with_path(path: PathBuf, max_bytes: Option<u64>) -> Self {
        Self {
            file_path: path,
            max_bytes: max_bytes.unwrap_or(HISTORY_DEFAULT_MAX_BYTES),
        }
    }

    /// Append a user command to history, skipping empty or duplicate of last entry.
    pub fn append(&self, text: &str, session_id: Option<&str>) -> Result<()> {
        // Skip empty
        if text.trim().is_empty() {
            return Ok(());
        }

        // Dedup: skip if same as last entry
        if self.last_entry().is_ok_and(|last| last.text == text) {
            return Ok(());
        }

        let entry = HistoryEntry {
            text: text.to_string(),
            ts: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            session_id: session_id.map(String::from),
        };
        let line = serde_json::to_string(&entry).context("failed to serialize history entry")?;

        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file_path)
            .with_context(|| format!("failed to open history: {}", self.file_path.display()))?
            .write_all(format!("{}\n", line).as_bytes())
            .with_context(|| {
                format!(
                    "failed to write history entry: {}",
                    self.file_path.display()
                )
            })?;

        debug!(path = %self.file_path.display(), "history entry appended");

        // Trim if over max_bytes
        self.trim_to_max_bytes().ok();

        Ok(())
    }

    /// Load all history entries in chronological order (oldest first).
    pub fn load(&self) -> Result<Vec<(String, Option<String>)>> {
        if !self.file_path.exists() {
            return Ok(Vec::new());
        }

        let file = std::fs::File::open(&self.file_path)
            .with_context(|| format!("failed to read history: {}", self.file_path.display()))?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<HistoryEntry>(&line) {
                entries.push((entry.text, entry.session_id));
            }
        }

        Ok(entries)
    }

    /// Load the last N history entries (for arrow-up recall).
    pub fn load_recent(&self, n: usize) -> Result<Vec<String>> {
        let all = self.load()?;
        let start = all.len().saturating_sub(n);
        Ok(all[start..].iter().map(|(text, _)| text.clone()).collect())
    }

    /// Get the last entry (for dedup check).
    fn last_entry(&self) -> Result<HistoryEntry> {
        if !self.file_path.exists() {
            anyhow::bail!("history file does not exist");
        }

        let file = std::fs::File::open(&self.file_path)
            .with_context(|| format!("failed to read history: {}", self.file_path.display()))?;
        let reader = BufReader::new(file);
        let mut last: Option<HistoryEntry> = None;

        for line in reader.lines() {
            let line = line?;
            if !line.trim().is_empty()
                && let Ok(entry) = serde_json::from_str::<HistoryEntry>(&line)
            {
                last = Some(entry);
            }
        }

        last.context("history file is empty")
    }

    /// Trim history file when it exceeds max_bytes.
    /// Removes oldest entries until file is below `max_bytes * HISTORY_SOFT_CAP_RATIO`.
    fn trim_to_max_bytes(&self) -> Result<()> {
        let metadata = match std::fs::metadata(&self.file_path) {
            Ok(m) => m,
            Err(_) => return Ok(()),
        };

        if metadata.len() <= self.max_bytes {
            return Ok(());
        }

        let target = (self.max_bytes as f64 * HISTORY_SOFT_CAP_RATIO) as u64;
        let entries = self.load()?;
        if entries.is_empty() {
            return Ok(());
        }

        // Keep last N entries that fit in target
        // Walk from end, building up until we hit target, then discard everything before
        let mut kept: Vec<&(String, Option<String>)> = Vec::new();
        let mut size: u64 = 0;
        for entry in entries.iter().rev() {
            let line = serde_json::to_string(&HistoryEntry {
                text: entry.0.clone(),
                ts: 0,
                session_id: entry.1.clone(),
            })?;
            let line_size = line.len() as u64 + 1;
            if size + line_size > target && !kept.is_empty() {
                break;
            }
            size += line_size;
            kept.push(entry);
        }
        kept.reverse();

        // Rewrite file with kept entries
        let mut tmp = tempfile::NamedTempFile::new()?;
        for entry in kept {
            let line = serde_json::to_string(&HistoryEntry {
                text: entry.0.clone(),
                ts: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                session_id: entry.1.clone(),
            })?;
            writeln!(tmp, "{}", line)?;
        }
        tmp.persist(&self.file_path).with_context(|| {
            format!(
                "failed to rewrite trimmed history: {}",
                self.file_path.display()
            )
        })?;

        debug!(
            path = %self.file_path.display(),
            before = metadata.len(),
            after = target,
            "history trimmed"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let store = HistoryStore::with_path(path, Some(1_000_000));

        store.append("hello", None).unwrap();
        store.append("world", Some("sess-1")).unwrap();

        let entries = store.load().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], ("hello".to_string(), None));
        assert_eq!(
            entries[1],
            ("world".to_string(), Some("sess-1".to_string()))
        );
    }

    #[test]
    fn dedup_skips_duplicate_of_last() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let store = HistoryStore::with_path(path, Some(1_000_000));

        store.append("hello", None).unwrap();
        store.append("hello", None).unwrap(); // duplicate → skipped

        let entries = store.load().unwrap();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn skips_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let store = HistoryStore::with_path(path, Some(1_000_000));

        store.append("", None).unwrap();
        store.append("   ", None).unwrap();

        assert!(store.load().unwrap().is_empty());
    }

    #[test]
    fn load_recent_returns_last_n() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let store = HistoryStore::with_path(path, Some(1_000_000));

        for i in 0..5 {
            store.append(&format!("msg {}", i), None).unwrap();
        }

        let recent = store.load_recent(2).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0], "msg 3");
        assert_eq!(recent[1], "msg 4");
    }

    #[test]
    fn trim_exceeding_max_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        // Very small cap — 100 bytes
        let store = HistoryStore::with_path(path.clone(), Some(100));

        // Write entries that add up
        for i in 0..20 {
            store
                .append(
                    &format!("long command number {} with extra padding", i),
                    None,
                )
                .unwrap();
        }

        // File should be <= 100 bytes after trim
        let meta = std::fs::metadata(&path).unwrap();
        assert!(
            meta.len() <= 100,
            "file size {} exceeds max_bytes 100",
            meta.len()
        );
    }
}
