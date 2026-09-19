//! RLVR Tier-1 MemoryCard reward sidecar (FR-034).
//!
//! One JSON object per card at `memories/.reward/{card_id}.json`, replaced
//! whole-file atomically via tmp+fsync+rename (precedent: `zen-core/src/audit.rs`).
//! Increments are read-modify-write cycles serialized across processes by an
//! advisory flock on `memories/.reward/.lock` (fs2) — without it, a daemon-hosted
//! turn and a TUI turn hitting the same card lose updates.
//!
//! Three instrumentation points per spec §FR-034:
//! 1. `access_count` — increments on every `retrieve_memories()` hit
//! 2. `downstream_citations` — when retrieved content appears in agent response
//! 3. `correction_count` — when a Correction references the memory source

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::Utc;
use fs2::FileExt;
use tracing::{debug, warn};

use super::types::MemoryReward;

/// Directory-level advisory lock serializing sidecar increments.
fn acquire_dir_lock(reward_dir: &Path) -> Result<File, String> {
    fs::create_dir_all(reward_dir).map_err(|e| format!("create reward dir: {e}"))?;
    let lock_path = reward_dir.join(".lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("open reward lock: {e}"))?;
    file.lock_exclusive()
        .map_err(|e| format!("lock reward dir: {e}"))?;
    Ok(file)
}

/// Read the reward sidecar for a card, returning defaults if absent.
/// The sidecar is replaced atomically, so unlocked reads always see a
/// whole object; corruption falls back to defaults with a warn.
pub fn read_reward(reward_dir: &Path, card_id: &str) -> MemoryReward {
    let path = sidecar_path(reward_dir, card_id);
    if !path.exists() {
        return MemoryReward::default();
    }
    match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
            warn!(
                path = %path.display(),
                error = %e,
                "corrupt reward sidecar, returning defaults (counters reset)"
            );
            MemoryReward::default()
        }),
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

/// Write a reward sidecar atomically (tmp + fsync + rename).
pub fn write_reward(reward_dir: &Path, card_id: &str, reward: &MemoryReward) -> Result<(), String> {
    fs::create_dir_all(reward_dir).map_err(|e| format!("create reward dir: {e}"))?;
    let path = sidecar_path(reward_dir, card_id);
    let tmp = path.with_extension("json.tmp");
    let content =
        serde_json::to_string_pretty(reward).map_err(|e| format!("serialize reward: {e}"))?;
    let mut file = File::create(&tmp).map_err(|e| format!("write tmp: {e}"))?;
    file.write_all(content.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|e| format!("write+fsync tmp: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

/// Serialize a read-modify-write increment under the directory lock.
fn locked_increment<F>(
    reward_dir: &Path,
    card_id: &str,
    field: &str,
    apply: F,
) -> Result<(), String>
where
    F: FnOnce(&mut MemoryReward),
{
    let _lock = acquire_dir_lock(reward_dir)?;
    let mut reward = read_reward(reward_dir, card_id);
    apply(&mut reward);
    reward.last_reward_at = Some(Utc::now());
    debug!(
        card_id,
        field,
        access_count = reward.access_count,
        citations = reward.downstream_citations,
        corrections = reward.correction_count,
        "reward sidecar incremented"
    );
    write_reward(reward_dir, card_id, &reward)
}

/// Instrumentation point 1: increment `access_count` on memory retrieval hit.
pub fn increment_access(reward_dir: &Path, card_id: &str) -> Result<(), String> {
    locked_increment(reward_dir, card_id, "access_count", |r| r.access_count += 1)
}

/// Instrumentation point 2: increment `downstream_citations` when retrieved
/// content appears in agent response.
pub fn increment_citations(reward_dir: &Path, card_id: &str) -> Result<(), String> {
    locked_increment(reward_dir, card_id, "downstream_citations", |r| {
        r.downstream_citations += 1
    })
}

/// Instrumentation point 3: increment `correction_count` when a Correction
/// references the memory source.
pub fn increment_corrections(reward_dir: &Path, card_id: &str) -> Result<(), String> {
    locked_increment(reward_dir, card_id, "correction_count", |r| {
        r.correction_count += 1
    })
}

/// Compute a card_id from a note path (full relative path, filesystem-safe).
///
/// Keys by the full path — not the filename stem — so `report.md` under
/// different directories never share one sidecar record. Path separators
/// are flattened to `_` so the id stays a single filename.
pub fn card_id_from_path(path: &str) -> String {
    let p = std::path::Path::new(path);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown");
    match p.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            let dir = parent.to_string_lossy().replace(['/', '\\'], "_");
            format!("{dir}_{stem}")
        }
        _ => stem.to_string(),
    }
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
    fn card_id_from_path_keys_by_full_relative_path() {
        // Distinct directories must never share a sidecar record (T160).
        assert_eq!(
            card_id_from_path("vault/wiki/notions/rust.md"),
            "vault_wiki_notions_rust"
        );
        assert_eq!(
            card_id_from_path("memories/journal/report.md"),
            "memories_journal_report"
        );
        assert_eq!(
            card_id_from_path("memories/archive/report.md"),
            "memories_archive_report"
        );
        assert_eq!(card_id_from_path("no-ext"), "no-ext");
    }

    #[test]
    fn corrupt_sidecar_returns_defaults() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bad.json");
        fs::write(&path, "{ not json").unwrap();
        let reward = read_reward(dir.path(), "bad");
        assert_eq!(reward.access_count, 0);
    }

    #[test]
    fn missing_fields_decode_via_serde_default() {
        // Sidecar written by an older build (no correction_count field)
        // must decode, defaulting only the absent fields.
        let dir = tempdir().unwrap();
        let path = dir.path().join("legacy.json");
        fs::write(&path, r#"{"access_count": 7, "downstream_citations": 1}"#).unwrap();
        let reward = read_reward(dir.path(), "legacy");
        assert_eq!(reward.access_count, 7);
        assert_eq!(reward.downstream_citations, 1);
        assert_eq!(reward.correction_count, 0);
        assert!(reward.last_reward_at.is_none());
    }

    #[test]
    fn atomic_write_leaves_no_tmp_residue() {
        let dir = tempdir().unwrap();
        let reward = MemoryReward {
            access_count: 42,
            ..Default::default()
        };
        write_reward(dir.path(), "atomic-test", &reward).unwrap();

        let path = dir.path().join("atomic-test.json");
        assert!(path.exists());
        assert!(!dir.path().join("atomic-test.json.tmp").exists());

        let loaded: MemoryReward =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.access_count, 42);
    }

    #[test]
    fn concurrent_increments_do_not_lose_updates() {
        // The flock must serialize read-modify-write cycles across threads
        // (same contract as across processes): 8 threads × 25 increments
        // must land exactly 200.
        let dir = tempdir().unwrap();
        let reward_dir: std::sync::Arc<PathBuf> = std::sync::Arc::new(dir.path().to_path_buf());

        let mut handles = Vec::new();
        for _ in 0..8 {
            let reward_dir = reward_dir.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..25 {
                    increment_access(&reward_dir, "contended").unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(read_reward(&reward_dir, "contended").access_count, 200);
    }
}
