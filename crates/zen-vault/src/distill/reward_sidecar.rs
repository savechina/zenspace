//! RLVR Tier-1 MemoryCard reward sidecar (FR-034).
//!
//! Append-only JSONL-style sidecar at `memories/.reward/{card_id}.json`.
//! Each sidecar file is a single `MemoryReward` JSON object, written
//! atomically via tmp+rename (reuse `atomic_write` precedent from
//! `zen-core/src/audit.rs` and `zen-memory/src/dream.rs`).
//!
//! Three instrumentation points per spec §FR-034:
//! 1. `access_count` — increments on every `retrieve_memories()` hit
//! 2. `downstream_citations` — when retrieved content appears in agent response
//! 3. `correction_count` — when a Correction references the memory source

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use tracing::{debug, warn};

use super::types::MemoryReward;

/// Read the reward sidecar for a card, returning defaults if absent.
pub fn read_reward(reward_dir: &Path, card_id: &str) -> MemoryReward {
    let path = sidecar_path(reward_dir, card_id);
    if !path.exists() {
        return MemoryReward::default();
    }
    match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(e) => {
            warn!(
                path = %path.display(),
                error = %e,
                "failed to read reward sidecar, returning defaults"
            );
            MemoryReward::default()
        }
    }
}

/// Write a reward sidecar atomically (tmp+rename).
pub fn write_reward(reward_dir: &Path, card_id: &str, reward: &MemoryReward) -> Result<(), String> {
    fs::create_dir_all(reward_dir).map_err(|e| format!("create reward dir: {e}"))?;
    let path = sidecar_path(reward_dir, card_id);
    let tmp = path.with_extension("json.tmp");
    let content =
        serde_json::to_string_pretty(reward).map_err(|e| format!("serialize reward: {e}"))?;
    fs::write(&tmp, &content).map_err(|e| format!("write tmp: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

/// Instrumentation point 1: increment `access_count` on memory retrieval hit.
pub fn increment_access(reward_dir: &Path, card_id: &str) -> Result<(), String> {
    let mut reward = read_reward(reward_dir, card_id);
    reward.access_count += 1;
    reward.last_reward_at = Some(Utc::now());
    debug!(
        card_id,
        access_count = reward.access_count,
        "reward sidecar: access incremented"
    );
    write_reward(reward_dir, card_id, &reward)
}

/// Instrumentation point 2: increment `downstream_citations` when retrieved
/// content appears in agent response.
pub fn increment_citations(reward_dir: &Path, card_id: &str) -> Result<(), String> {
    let mut reward = read_reward(reward_dir, card_id);
    reward.downstream_citations += 1;
    reward.last_reward_at = Some(Utc::now());
    debug!(
        card_id,
        citations = reward.downstream_citations,
        "reward sidecar: citation incremented"
    );
    write_reward(reward_dir, card_id, &reward)
}

/// Instrumentation point 3: increment `correction_count` when a Correction
/// references the memory source.
pub fn increment_corrections(reward_dir: &Path, card_id: &str) -> Result<(), String> {
    let mut reward = read_reward(reward_dir, card_id);
    reward.correction_count += 1;
    reward.last_reward_at = Some(Utc::now());
    debug!(
        card_id,
        corrections = reward.correction_count,
        "reward sidecar: correction incremented"
    );
    write_reward(reward_dir, card_id, &reward)
}

/// Compute a card_id from a note path (filename stem, filesystem-safe).
pub fn card_id_from_path(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}

fn sidecar_path(reward_dir: &Path, card_id: &str) -> PathBuf {
    reward_dir.join(format!("{card_id}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn read_reward_returns_defaults_when_absent() {
        let dir = tempdir().unwrap();
        let reward = read_reward(dir.path(), "nonexistent");
        assert_eq!(reward.access_count, 0);
        assert_eq!(reward.downstream_citations, 0);
        assert_eq!(reward.correction_count, 0);
        assert!(reward.last_reward_at.is_none());
    }

    #[test]
    fn write_and_read_roundtrip() {
        let dir = tempdir().unwrap();
        let reward = MemoryReward {
            access_count: 5,
            downstream_citations: 2,
            last_reward_at: Some(Utc::now()),
            ..MemoryReward::default()
        };
        write_reward(dir.path(), "card-abc", &reward).unwrap();

        let loaded = read_reward(dir.path(), "card-abc");
        assert_eq!(loaded.access_count, 5);
        assert_eq!(loaded.downstream_citations, 2);
        assert_eq!(loaded.correction_count, 0);
        assert!(loaded.last_reward_at.is_some());
    }

    #[test]
    fn increment_access_increments_correctly() {
        let dir = tempdir().unwrap();
        increment_access(dir.path(), "card-1").unwrap();
        increment_access(dir.path(), "card-1").unwrap();
        let reward = read_reward(dir.path(), "card-1");
        assert_eq!(reward.access_count, 2);
        assert!(reward.last_reward_at.is_some());
    }

    #[test]
    fn increment_citations_increments_correctly() {
        let dir = tempdir().unwrap();
        increment_citations(dir.path(), "card-2").unwrap();
        let reward = read_reward(dir.path(), "card-2");
        assert_eq!(reward.downstream_citations, 1);
        assert_eq!(reward.access_count, 0);
    }

    #[test]
    fn increment_corrections_increments_correctly() {
        let dir = tempdir().unwrap();
        increment_corrections(dir.path(), "card-3").unwrap();
        increment_corrections(dir.path(), "card-3").unwrap();
        increment_corrections(dir.path(), "card-3").unwrap();
        let reward = read_reward(dir.path(), "card-3");
        assert_eq!(reward.correction_count, 3);
    }

    #[test]
    fn card_id_from_path_extracts_stem() {
        assert_eq!(card_id_from_path("vault/wiki/notions/rust.md"), "rust");
        assert_eq!(card_id_from_path("/a/b/c/my-card.md"), "my-card");
        assert_eq!(card_id_from_path("no-ext"), "no-ext");
    }

    #[test]
    fn concurrent_safe_atomic_write() {
        // Verify tmp file doesn't leak on rename (atomic semantics)
        let dir = tempdir().unwrap();
        let reward = MemoryReward {
            access_count: 42,
            ..Default::default()
        };
        write_reward(dir.path(), "atomic-test", &reward).unwrap();

        // The final file should exist, no .tmp residue
        let path = dir.path().join("atomic-test.json");
        assert!(path.exists());
        let tmp_path = dir.path().join("atomic-test.json.tmp");
        assert!(!tmp_path.exists());

        let loaded: MemoryReward =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.access_count, 42);
    }
}
