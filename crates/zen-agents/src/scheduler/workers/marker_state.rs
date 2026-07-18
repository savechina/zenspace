use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

/// Sidecar state for journal `.md` entries.
///
/// Each field is owned by a specific worker:
/// - `journaled_at`: SessionJournaler
/// - `memory_updated_at`: MemoryCurator
/// - `extracted_at` / `extraction_source`: NotionExtractorWorker
/// - `commitment_tracked_at`: CommitmentTracker
/// - `reflection_extracted_at`: ReflectionWorker
/// - `wisdom_synthesized_at`: WisdomSynthesizer
/// - `decision_tracked_at`: DecisionTracker
///
/// `save()` uses read-merge-write to prevent one worker from clobbering another's fields.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct JournalEntryState {
    pub journaled_at: Option<String>,
    pub memory_updated_at: Option<String>,
    pub extracted_at: Option<String>,
    pub extraction_source: Option<String>,
    pub commitment_tracked_at: Option<String>,
    pub reflection_extracted_at: Option<String>,
    pub wisdom_synthesized_at: Option<String>,
    pub decision_tracked_at: Option<String>,
}

/// Sidecar state for session `.jsonl` files.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionState {
    pub journaled: bool,
    pub journaled_at: Option<String>,
    pub journaled_source: Option<String>,
}

/// Appends `.state.json` to the original file's name.
/// `foo.md` → `foo.md.state.json`
fn sidecar_path(original: &Path) -> PathBuf {
    let mut p = original.to_path_buf();
    let mut name = p.file_name().unwrap().to_os_string();
    name.push(".state.json");
    p.set_file_name(name);
    p
}

impl JournalEntryState {
    /// Returns `Default` if the sidecar doesn't exist (not an error).
    pub fn load(md_path: &Path) -> Self {
        let path = sidecar_path(md_path);
        match fs::read_to_string(&path) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Atomic save via read-merge-write with advisory file lock.
    ///
    /// Acquires an exclusive lock on a `.lock` sidecar to prevent concurrent
    /// workers from clobbering each other's fields during the merge.
    pub fn save(&self, md_path: &Path) -> Result<()> {
        let path = sidecar_path(md_path);
        let lock_path = path.with_extension("state.json.lock");

        let lock_file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("failed to open lock file: {}", lock_path.display()))?;
        lock_file.lock_exclusive().with_context(|| {
            format!("failed to acquire exclusive lock: {}", lock_path.display())
        })?;

        let mut existing: Self = match fs::read_to_string(&path) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
            Err(_) => Self::default(),
        };

        if self.journaled_at.is_some() {
            existing.journaled_at = self.journaled_at.clone();
        }
        if self.memory_updated_at.is_some() {
            existing.memory_updated_at = self.memory_updated_at.clone();
        }
        if self.extracted_at.is_some() {
            existing.extracted_at = self.extracted_at.clone();
            existing.extraction_source = self.extraction_source.clone();
        }
        if self.commitment_tracked_at.is_some() {
            existing.commitment_tracked_at = self.commitment_tracked_at.clone();
        }
        if self.reflection_extracted_at.is_some() {
            existing.reflection_extracted_at = self.reflection_extracted_at.clone();
        }
        if self.wisdom_synthesized_at.is_some() {
            existing.wisdom_synthesized_at = self.wisdom_synthesized_at.clone();
        }
        if self.decision_tracked_at.is_some() {
            existing.decision_tracked_at = self.decision_tracked_at.clone();
        }

        let json = serde_json::to_string_pretty(&existing)
            .context("failed to serialize journal entry state")?;

        let tmp_path = path.with_extension("state.json.tmp");
        fs::write(&tmp_path, &json)
            .with_context(|| format!("failed to write sidecar tmp: {}", tmp_path.display()))?;
        fs::rename(&tmp_path, &path)
            .with_context(|| format!("failed to rename sidecar: {}", path.display()))?;

        drop(lock_file);
        Ok(())
    }

    pub fn is_journaled(md_path: &Path) -> bool {
        let state = Self::load(md_path);
        state.journaled_at.is_some() || Self::scan_frontmatter(md_path, "journaled_at:")
    }

    pub fn has_memory_updated(md_path: &Path) -> bool {
        let state = Self::load(md_path);
        state.memory_updated_at.is_some() || Self::scan_frontmatter(md_path, "memory_updated_at:")
    }

    pub fn has_extracted(md_path: &Path) -> bool {
        let state = Self::load(md_path);
        state.extracted_at.is_some() || Self::scan_frontmatter(md_path, "extracted_at:")
    }

    pub fn has_commitment_tracked(md_path: &Path) -> bool {
        let state = Self::load(md_path);
        state.commitment_tracked_at.is_some()
            || Self::scan_frontmatter(md_path, "commitment_tracked_at:")
    }

    pub fn has_reflection_extracted(md_path: &Path) -> bool {
        let state = Self::load(md_path);
        state.reflection_extracted_at.is_some()
            || Self::scan_frontmatter(md_path, "reflection_extracted_at:")
    }

    pub fn has_wisdom_synthesized(md_path: &Path) -> bool {
        let state = Self::load(md_path);
        state.wisdom_synthesized_at.is_some()
            || Self::scan_frontmatter(md_path, "wisdom_synthesized_at:")
    }

    pub fn has_decision_tracked(md_path: &Path) -> bool {
        let state = Self::load(md_path);
        state.decision_tracked_at.is_some()
            || Self::scan_frontmatter(md_path, "decision_tracked_at:")
    }

    fn scan_frontmatter(path: &Path, prefix: &str) -> bool {
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return false,
        };
        for line in content.lines().take(15) {
            if line.trim().starts_with(prefix) {
                return true;
            }
        }
        false
    }

    /// Migrates frontmatter markers to sidecar. Returns `true` if any migration happened.
    pub fn migrate_from_frontmatter(md_path: &Path) -> bool {
        let mut state = Self::load(md_path);
        let mut migrated = false;

        if state.journaled_at.is_none()
            && let Some(val) = Self::extract_frontmatter_value(md_path, "journaled_at:")
        {
            state.journaled_at = Some(val);
            migrated = true;
        }
        if state.memory_updated_at.is_none()
            && let Some(val) = Self::extract_frontmatter_value(md_path, "memory_updated_at:")
        {
            state.memory_updated_at = Some(val);
            migrated = true;
        }
        if state.extracted_at.is_none()
            && let Some(val) = Self::extract_frontmatter_value(md_path, "extracted_at:")
        {
            state.extracted_at = Some(val);
            if let Some(src) = Self::extract_frontmatter_value(md_path, "extraction_source:") {
                state.extraction_source = Some(src);
            }
            migrated = true;
        }
        if state.commitment_tracked_at.is_none()
            && let Some(val) = Self::extract_frontmatter_value(md_path, "commitment_tracked_at:")
        {
            state.commitment_tracked_at = Some(val);
            migrated = true;
        }
        if state.reflection_extracted_at.is_none()
            && let Some(val) = Self::extract_frontmatter_value(md_path, "reflection_extracted_at:")
        {
            state.reflection_extracted_at = Some(val);
            migrated = true;
        }
        if state.wisdom_synthesized_at.is_none()
            && let Some(val) = Self::extract_frontmatter_value(md_path, "wisdom_synthesized_at:")
        {
            state.wisdom_synthesized_at = Some(val);
            migrated = true;
        }
        if state.decision_tracked_at.is_none()
            && let Some(val) = Self::extract_frontmatter_value(md_path, "decision_tracked_at:")
        {
            state.decision_tracked_at = Some(val);
            migrated = true;
        }

        if migrated
            && let Err(e) = state.save(md_path) {
            tracing::warn!(error = %e, path = %md_path.display(), "failed to save migrated marker state");
        }
        migrated
    }

    fn extract_frontmatter_value(path: &Path, prefix: &str) -> Option<String> {
        let content = fs::read_to_string(path).ok()?;
        for line in content.lines().take(15) {
            let trimmed = line.trim();
            if let Some(val) = trimmed.strip_prefix(prefix) {
                let val = val.trim().to_string();
                if !val.is_empty() {
                    return Some(val);
                }
            }
        }
        None
    }
}

impl SessionState {
    /// Returns `Default` if the sidecar doesn't exist.
    pub fn load(jsonl_path: &Path) -> Self {
        let path = sidecar_path(jsonl_path);
        match fs::read_to_string(&path) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Atomic save via read-merge-write with advisory file lock.
    pub fn save(&self, jsonl_path: &Path) -> Result<()> {
        let path = sidecar_path(jsonl_path);
        let lock_path = path.with_extension("state.json.lock");

        let lock_file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("failed to open lock file: {}", lock_path.display()))?;
        lock_file.lock_exclusive().with_context(|| {
            format!("failed to acquire exclusive lock: {}", lock_path.display())
        })?;

        let mut existing: Self = match fs::read_to_string(&path) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
            Err(_) => Self::default(),
        };

        if self.journaled {
            existing.journaled = true;
        }
        if self.journaled_at.is_some() {
            existing.journaled_at = self.journaled_at.clone();
        }
        if self.journaled_source.is_some() {
            existing.journaled_source = self.journaled_source.clone();
        }

        let json =
            serde_json::to_string_pretty(&existing).context("failed to serialize session state")?;

        let tmp_path = path.with_extension("state.json.tmp");
        fs::write(&tmp_path, &json)
            .with_context(|| format!("failed to write sidecar tmp: {}", tmp_path.display()))?;
        fs::rename(&tmp_path, &path)
            .with_context(|| format!("failed to rename sidecar: {}", path.display()))?;

        drop(lock_file);
        Ok(())
    }

    /// Checks sidecar first, falls back to tail-scan of `.jsonl` for old markers.
    pub fn is_journaled(jsonl_path: &Path) -> bool {
        let state = Self::load(jsonl_path);
        state.journaled || Self::scan_jsonl_tail(jsonl_path)
    }

    fn scan_jsonl_tail(jsonl_path: &Path) -> bool {
        const MARKER_PREFIX: &str = r#"{"type":"system/journaled""#;
        const SEARCH_BYTES: usize = 200;

        let content = match fs::read_to_string(jsonl_path) {
            Ok(c) => c,
            Err(_) => return false,
        };

        if content.len() <= SEARCH_BYTES {
            return content.contains(MARKER_PREFIX);
        }

        let tail_start = content.floor_char_boundary(content.len().saturating_sub(SEARCH_BYTES));
        content[tail_start..].contains(MARKER_PREFIX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_sidecar_round_trip() {
        let dir = tempdir().unwrap();
        let md_path = dir.path().join("2026-06-24-test.md");
        fs::write(&md_path, "---\nsession_id: test\n---\n\ncontent\n").unwrap();

        let state = JournalEntryState {
            journaled_at: Some("2026-06-24T10:00:00Z".to_string()),
            ..Default::default()
        };
        state.save(&md_path).unwrap();

        let loaded = JournalEntryState::load(&md_path);
        assert_eq!(loaded.journaled_at.as_deref(), Some("2026-06-24T10:00:00Z"));
        assert!(loaded.memory_updated_at.is_none());
        assert!(loaded.extracted_at.is_none());
    }

    #[test]
    fn test_merge_preserves_other_fields() {
        let dir = tempdir().unwrap();
        let md_path = dir.path().join("2026-06-24-test.md");
        fs::write(&md_path, "---\nsession_id: test\n---\n\ncontent\n").unwrap();

        let state_a = JournalEntryState {
            journaled_at: Some("2026-06-24T10:00:00Z".to_string()),
            ..Default::default()
        };
        state_a.save(&md_path).unwrap();

        let state_b = JournalEntryState {
            memory_updated_at: Some("2026-06-24T10:05:00Z".to_string()),
            ..Default::default()
        };
        state_b.save(&md_path).unwrap();

        let state_c = JournalEntryState {
            extracted_at: Some("2026-06-24T10:10:00Z".to_string()),
            extraction_source: Some("llm".to_string()),
            ..Default::default()
        };
        state_c.save(&md_path).unwrap();

        let loaded = JournalEntryState::load(&md_path);
        assert_eq!(loaded.journaled_at.as_deref(), Some("2026-06-24T10:00:00Z"));
        assert_eq!(
            loaded.memory_updated_at.as_deref(),
            Some("2026-06-24T10:05:00Z")
        );
        assert_eq!(loaded.extracted_at.as_deref(), Some("2026-06-24T10:10:00Z"));
        assert_eq!(loaded.extraction_source.as_deref(), Some("llm"));
    }

    #[test]
    fn test_backward_compat_frontmatter_fallback() {
        let dir = tempdir().unwrap();
        let md_path = dir.path().join("2026-06-24-test.md");
        fs::write(
            &md_path,
            "---\nsession_id: test\njournaled_at: 2026-06-24T10:00:00Z\n---\n\ncontent\n",
        )
        .unwrap();

        assert!(JournalEntryState::is_journaled(&md_path));
    }

    #[test]
    fn test_backward_compat_memory_updated_fallback() {
        let dir = tempdir().unwrap();
        let md_path = dir.path().join("2026-06-24-test.md");
        fs::write(
            &md_path,
            "---\nsession_id: test\njournaled_at: 2026-06-24T10:00:00Z\nmemory_updated_at: 2026-06-24T10:05:00Z\n---\n\ncontent\n",
        )
        .unwrap();

        assert!(JournalEntryState::has_memory_updated(&md_path));
    }

    #[test]
    fn test_backward_compat_extracted_fallback() {
        let dir = tempdir().unwrap();
        let md_path = dir.path().join("2026-06-24-test.md");
        fs::write(
            &md_path,
            "---\nsession_id: test\nextracted_at: 2026-06-24T10:10:00Z\nextraction_source: llm\n---\n\ncontent\n",
        )
        .unwrap();

        assert!(JournalEntryState::has_extracted(&md_path));
    }

    #[test]
    fn test_migrate_from_frontmatter() {
        let dir = tempdir().unwrap();
        let md_path = dir.path().join("2026-06-24-test.md");
        fs::write(
            &md_path,
            "---\nsession_id: test\njournaled_at: 2026-06-24T10:00:00Z\nmemory_updated_at: 2026-06-24T10:05:00Z\n---\n\ncontent\n",
        )
        .unwrap();

        let migrated = JournalEntryState::migrate_from_frontmatter(&md_path);
        assert!(migrated);

        let state = JournalEntryState::load(&md_path);
        assert_eq!(state.journaled_at.as_deref(), Some("2026-06-24T10:00:00Z"));
        assert_eq!(
            state.memory_updated_at.as_deref(),
            Some("2026-06-24T10:05:00Z")
        );
    }

    #[test]
    fn test_no_sidecar_returns_default() {
        let dir = tempdir().unwrap();
        let md_path = dir.path().join("nonexistent.md");

        let state = JournalEntryState::load(&md_path);
        assert_eq!(state, JournalEntryState::default());
    }

    #[test]
    fn test_session_state_round_trip() {
        let dir = tempdir().unwrap();
        let jsonl_path = dir.path().join("conversation.jsonl");
        fs::write(&jsonl_path, "{\"type\":\"session/meta\"}\n").unwrap();

        let state = SessionState {
            journaled: true,
            journaled_at: Some("2026-06-24T10:00:00Z".to_string()),
            journaled_source: Some("llm".to_string()),
        };
        state.save(&jsonl_path).unwrap();

        let loaded = SessionState::load(&jsonl_path);
        assert!(loaded.journaled);
        assert_eq!(loaded.journaled_at.as_deref(), Some("2026-06-24T10:00:00Z"));
        assert_eq!(loaded.journaled_source.as_deref(), Some("llm"));
    }

    #[test]
    fn test_session_state_backward_compat() {
        let dir = tempdir().unwrap();
        let jsonl_path = dir.path().join("conversation.jsonl");
        let marker = r#"{"type":"system/journaled","payload":{"timestamp":"2026-06-20T14:30:00Z","source":"keyword"}}"#;
        fs::write(
            &jsonl_path,
            format!("{{\"type\":\"session/meta\"}}\n{marker}\n"),
        )
        .unwrap();

        assert!(SessionState::is_journaled(&jsonl_path));
    }

    #[test]
    fn test_sidecar_path_computation() {
        let path = Path::new("/foo/bar/journal.md");
        let sc = sidecar_path(path);
        assert_eq!(sc, PathBuf::from("/foo/bar/journal.md.state.json"));
    }
}
