//! Session JSONL → mv2 incremental replayer (Phase 11, T048 + T059a).
//!
//! PURPOSE: Replays `chat/turn` events from `<sessions>/**/*.jsonl` into the
//! memvid store so `.mv2` stays a *derived* index of the canonical session
//! archives — never the sole copy of user data (Phase 11 baseline invariant).
//! Replay resumes from a per-file byte-offset checkpoint and is idempotent:
//! each turn carries a blake3 idempotency key that is checked against the
//! store before persisting, so full re-replay (daemon restart mid-batch,
//! checkpoint reset) can never double-write.
//!
//! B4 transparency: `*.jsonl.zst` cold archives are decoded on read via
//! [`crate::compression::load_session_bytes`]; compressed files skip the
//! trailing-newline repair (immutable by construction). Checkpoint keys
//! follow the renamed path — the compressor's caller translates rows.
//!
//! USAGE: Constructed by the gateway replay tick (T052) with the sole-owner
//! [`ZenMemvidStore`] and a checkpoint store backed by the daemon's state.db
//! `memvid_replay_offsets` table (zen-repo `MemvidReplayOffsetRepo` adapted
//! behind [`ReplayCheckpointStore`]). The embedded path may use
//! [`InMemoryReplayCheckpoints`]. `replay_dir` is invoked over the sessions
//! root every tick; live `persist_turn` writes stay immediate and are
//! serialized against replay by the same-process store mutex.
//!
//! EXPECTED: each file returns [`ReplayStats`] `{replayed, skipped,
//! duplicates, demoted, last_offset}` (exposed by health/status as
//! `replayed/skipped/lastOffset`; duplicates are tracked separately so a
//! duplicate-heavy re-replay does not trip the 5%-skipped silent-failure
//! guard).
//!
//! ERRORS: every replay call is error-tolerant — missing files are treated
//! as empty (WARN), corrupt/partial trailing lines are skipped and counted
//! (`skipped`), checkpoint I/O failures downgrade to WARN, and a failing
//! store write stops that file's replay at the last complete line so the
//! next tick resumes before the failed turn. No replay method returns Err.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use walkdir::WalkDir;
use zen_core::types::SessionEvent;

use crate::memvid::ZenMemvidStore;

/// Compute the per-turn idempotency key
/// `blake3(session_id|role|content|timestamp)` as lowercase hex (64 chars).
///
/// Canonicalization (stable across versions — do not change, it would break
/// dedup against already-recorded markers):
/// - the four fields are joined with a literal `|` (U+007C) separator;
/// - `role` is the canonical lowercase role name (`MessageRole::as_str`,
///   e.g. `"user"` / `"assistant"`);
/// - `timestamp` is chrono's RFC 3339 rendering (`DateTime::to_rfc3339`,
///   e.g. `2026-08-26T12:34:56.789012+00:00`) or the empty string when
///   `None` (synthesized/historical turns).
pub fn turn_replay_key(
    session_id: &str,
    role: &str,
    content: &str,
    timestamp: Option<DateTime<Utc>>,
) -> String {
    let ts = timestamp.map_or_else(String::new, |t| t.to_rfc3339());
    blake3::hash(format!("{session_id}|{role}|{content}|{ts}").as_bytes())
        .to_hex()
        .to_string()
}

/// Checkpoint persistence for incremental replay (adapter over zen-repo's
/// `MemvidReplayOffsetRepo`, injected by the gateway daemon).
///
/// PURPOSE: decouples the replayer from zen-repo — zen-memory has no data-layer
/// dependency, and the gateway (T052) owns state.db. Keys are session-file
/// path strings; offsets are the byte position after the last applied line.
#[async_trait]
pub trait ReplayCheckpointStore: Send + Sync {
    /// Load the applied byte offset for `session_path`.
    ///
    /// `Ok(None)` means "no checkpoint" — the caller full-replays from byte 0,
    /// which is safe by turn-key idempotency.
    async fn load_offset(&self, session_path: &str) -> anyhow::Result<Option<i64>>;

    /// Upsert the applied byte offset for `session_path` (monotonic advance
    /// or reset to 0 after offset-beyond-EOF detection).
    async fn save_offset(&self, session_path: &str, applied_offset: i64) -> anyhow::Result<()>;
}

/// In-memory [`ReplayCheckpointStore`] — for tests and embedded (non-daemon)
/// runs where checkpoints need not survive the process.
///
/// Checkpoint loss is harmless: the next run full-replays from 0 and every
/// turn resolves to a duplicate via its idempotency key.
#[derive(Default)]
pub struct InMemoryReplayCheckpoints {
    offsets: std::sync::Mutex<HashMap<String, i64>>,
}

#[async_trait]
impl ReplayCheckpointStore for InMemoryReplayCheckpoints {
    async fn load_offset(&self, session_path: &str) -> anyhow::Result<Option<i64>> {
        Ok(self
            .offsets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_path)
            .copied())
    }

    async fn save_offset(&self, session_path: &str, applied_offset: i64) -> anyhow::Result<()> {
        self.offsets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_path.to_string(), applied_offset);
        Ok(())
    }
}

/// Per-file replay outcome. Field names `replayed`/`skipped`/`last_offset`
/// are task-specified (T052 surfaces them in health/status; serde renders
/// them camelCase for the JSON status payload).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayStats {
    /// Chat turns newly persisted to the memvid store.
    pub replayed: u64,
    /// Corrupt / partial / non-UTF-8 lines skipped (silent-failure signal —
    /// T052 emits ERROR when skipped-ratio exceeds 5%).
    pub skipped: u64,
    /// Turns already present in the store (idempotent re-replay); NOT
    /// counted as `skipped` so re-replay never trips the silent-failure
    /// guard.
    pub duplicates: u64,
    /// `context/demoted` events seen — archive-only, no mv2 write (T050
    /// already archived them into the jsonl).
    pub demoted: u64,
    /// Checkpoint byte offset saved after this run.
    pub last_offset: u64,
}

/// Incremental session-archive replayer (Phase 11, T048).
///
/// Owns no globals: the memvid store handle and the checkpoint store are
/// injected explicitly. All methods are error-tolerant (see module docs).
pub struct SessionReplayer {
    store: ZenMemvidStore,
    checkpoints: Arc<dyn ReplayCheckpointStore>,
}

impl SessionReplayer {
    /// Create a replayer over `store` with `checkpoints` for resume support.
    pub fn new(store: ZenMemvidStore, checkpoints: Arc<dyn ReplayCheckpointStore>) -> Self {
        Self { store, checkpoints }
    }

    /// Replay every `*.jsonl` file under `sessions_root` (recursive,
    /// `YYYY/MM/DD/<uuid>.jsonl` layout), file-name-sorted for determinism.
    ///
    /// A missing/unreadable root is treated as empty (WARN, no files). Use a
    /// stable (absolute) root across ticks: the checkpoint key is the file
    /// path string exactly as walked from this root.
    pub async fn replay_dir(&self, sessions_root: &Path) -> Vec<(PathBuf, ReplayStats)> {
        if !sessions_root.is_dir() {
            tracing::warn!(
                root = %sessions_root.display(),
                "session replay root missing or not a directory; nothing to replay"
            );
            return Vec::new();
        }

        let mut results = Vec::new();
        for entry in WalkDir::new(sessions_root)
            .sort_by_file_name()
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let ext = entry.path().extension().and_then(|e| e.to_str());
            if !entry.file_type().is_file() || !matches!(ext, Some("jsonl") | Some("zst")) {
                continue;
            }
            let path = entry.into_path();
            let stats = self.replay_file(&path).await;
            results.push((path, stats));
        }
        results
    }

    /// Replay one session `.jsonl` file from its checkpoint.
    ///
    /// Behaviour: repairs a missing trailing newline (T059a) before reading;
    /// skips `session/meta`; persists `chat/turn` events idempotently;
    /// counts `context/demoted` archive events; skips-and-counts corrupt
    /// complete lines (advancing past them) and a trailing partial line
    /// (without advancing, so a later-completed line is replayed whole);
    /// resets the checkpoint to 0 when it points beyond EOF; stops at the
    /// last complete line on a store write failure. Never returns `Err`.
    pub async fn replay_file(&self, path: &Path) -> ReplayStats {
        let mut stats = ReplayStats::default();
        let session_id = session_id_from_path(path);

        if !path.is_file() {
            tracing::warn!(path = %path.display(), "session file missing; treating as empty");
            return stats;
        }

        // Fast path (tick cadence): stat before reading — when the checkpoint
        // already equals the current file length, nothing was appended since
        // the last complete pass, so skip the full read AND the append-mode
        // trailing-newline open (which would touch every archive each tick).
        let checkpoint_key = path.to_string_lossy().to_string();
        let file_len = std::fs::metadata(path)
            .map(|m| m.len() as i64)
            .unwrap_or(-1);
        if file_len > 0 {
            match self.checkpoints.load_offset(&checkpoint_key).await {
                Ok(Some(stored)) if stored == file_len => {
                    stats.last_offset = file_len as u64;
                    return stats;
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "checkpoint load failed; full replay from 0"
                    );
                }
            }
        }

        // B4: `.jsonl.zst` archives are immutable — decode instead of
        // repair (appending to a compressed twin is meaningless).
        let (bytes, compressed) = match crate::compression::load_session_bytes(path) {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "session archive unreadable");
                return stats;
            }
        };
        if !compressed && let Err(e) = repair_trailing_newline(path) {
            tracing::warn!(path = %path.display(), error = %e, "trailing-newline repair failed");
        }

        let stored = match self.checkpoints.load_offset(&checkpoint_key).await {
            Ok(offset) => offset,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "checkpoint load failed; full replay from 0"
                );
                None
            }
        };
        let mut pos = stored.unwrap_or(0);
        if pos > bytes.len() as i64 {
            tracing::info!(
                path = %path.display(),
                stored_offset = pos,
                file_len = bytes.len(),
                "checkpoint beyond EOF; resetting to 0 for full re-replay"
            );
            pos = 0;
        }
        let mut pos = pos as usize;

        while pos < bytes.len() {
            let Some(nl) = bytes[pos..].iter().position(|&b| b == b'\n') else {
                tracing::warn!(
                    path = %path.display(),
                    offset = pos,
                    "trailing partial line (no newline); skipping without advancing checkpoint"
                );
                stats.skipped += 1;
                break;
            };

            let line = &bytes[pos..pos + nl];
            let next = pos + nl + 1;

            if line.iter().all(|b| b.is_ascii_whitespace()) {
                pos = next;
                continue;
            }

            let event = std::str::from_utf8(line)
                .ok()
                .and_then(|s| serde_json::from_str::<SessionEvent>(s).ok());

            let mut store_failed = false;
            match event {
                Some(SessionEvent::Turn(message)) => {
                    let key = turn_replay_key(
                        &session_id,
                        message.role.as_str(),
                        &message.content,
                        message.timestamp,
                    );
                    // Fail-stop (review D1): a dedup-check error means we
                    // cannot know whether the turn is already stored —
                    // re-persisting could duplicate the frame in the derived
                    // index. Stop this file at the last complete line instead
                    // of guessing; the next tick resumes idempotently.
                    let already_present = match self.store.contains_turn(&session_id, &key) {
                        Ok(found) => found,
                        Err(e) => {
                            tracing::error!(
                                path = %path.display(),
                                error = %e,
                                "dedup check failed; stopping file replay at last complete line"
                            );
                            store_failed = true;
                            false
                        }
                    };
                    if !store_failed {
                        if already_present {
                            stats.duplicates += 1;
                        } else {
                            match self.store.persist_structured_turn(
                                &session_id,
                                message.role.as_str(),
                                &message.content,
                            ) {
                                Ok(_) => {
                                    if let Err(e) = self.store.record_turn_key(&session_id, &key) {
                                        tracing::warn!(
                                            path = %path.display(),
                                            error = %e,
                                            "replay-key marker write failed; re-replay may duplicate this turn"
                                        );
                                    }
                                    stats.replayed += 1;
                                }
                                Err(e) => {
                                    tracing::error!(
                                        path = %path.display(),
                                        error = %e,
                                        "turn persist failed; stopping file replay before this line"
                                    );
                                    store_failed = true;
                                }
                            }
                        }
                    }
                }
                Some(SessionEvent::Meta(_)) => {}
                Some(SessionEvent::ContextDemoted(_)) => {
                    stats.demoted += 1;
                }
                None => {
                    tracing::warn!(
                        path = %path.display(),
                        offset = pos,
                        "corrupt session line (invalid UTF-8 or JSON); skipping"
                    );
                    stats.skipped += 1;
                }
            }

            if store_failed {
                break;
            }
            pos = next;
        }

        stats.last_offset = pos as u64;
        if let Err(e) = self
            .checkpoints
            .save_offset(&checkpoint_key, stats.last_offset as i64)
            .await
        {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "checkpoint save failed; next run re-replays idempotently"
            );
        }
        stats
    }
}

/// Session id = `.jsonl` file stem (sessions are named `<session_id>.jsonl`).
fn session_id_from_path(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// T059a: append a missing trailing newline before reading so a crash-truncated
/// last line becomes its own complete (parse-failing) line instead of gluing
/// onto the next appended event — keeping checkpoint byte offsets stable.
fn repair_trailing_newline(path: &Path) -> std::io::Result<()> {
    use std::io::{Read, Seek, SeekFrom, Write};

    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .open(path)?;

    let len = file.seek(SeekFrom::End(0))?;
    if len == 0 {
        return Ok(());
    }

    let mut last = [0u8; 1];
    file.seek(SeekFrom::Start(len - 1))?;
    file.read_exact(&mut last)?;
    if last[0] != b'\n' {
        file.write_all(b"\n")?;
        tracing::info!(path = %path.display(), "repaired missing trailing newline");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts() -> Option<DateTime<Utc>> {
        Some(Utc.with_ymd_and_hms(2026, 8, 26, 12, 0, 0).unwrap())
    }

    #[test]
    fn key_is_deterministic_and_hex_encoded() {
        let a = turn_replay_key("sess-1", "user", "hello", ts());
        let b = turn_replay_key("sess-1", "user", "hello", ts());
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[test]
    fn key_differs_per_field_and_none_timestamp() {
        let base = turn_replay_key("sess-1", "user", "hello", ts());
        assert_ne!(base, turn_replay_key("sess-2", "user", "hello", ts()));
        assert_ne!(base, turn_replay_key("sess-1", "assistant", "hello", ts()));
        assert_ne!(base, turn_replay_key("sess-1", "user", "hello!", ts()));
        assert_ne!(base, turn_replay_key("sess-1", "user", "hello", None));
        // None timestamp canonicalizes to the empty string, distinct from any
        // rendered timestamp.
        assert_ne!(
            turn_replay_key("s", "u", "c", None),
            turn_replay_key("s", "u", "c", ts())
        );
    }

    /// B4 transparency end-to-end: a `.jsonl.zst` cold archive replays
    /// exactly like its plain twin — turns persist, second pass is all
    /// duplicates (idempotency keys), no repair attempt on compressed
    /// bytes.
    #[tokio::test]
    async fn replay_reads_compressed_archive_transparently() {
        use crate::memvid::ZenMemvidStore;

        let dir = tempfile::tempdir().unwrap();
        let memory_path = dir.path().join("memory.mv2");
        let store = ZenMemvidStore::new(memory_path).unwrap();

        let event = SessionEvent::Turn(zen_core::types::Message {
            role: zen_core::types::MessageRole::User,
            content: "compressed turn".into(),
            timestamp: ts(),
        });
        let line = serde_json::to_string(&event).unwrap();
        // Archives are newline-terminated per event (complete-line
        // semantics); compress the terminated form.
        let framed = format!("{line}\n");
        let encoded = zstd::stream::encode_all(framed.as_bytes(), 3).unwrap();
        let archive = dir.path().join("sess.jsonl.zst");
        std::fs::write(&archive, encoded).unwrap();

        let replayer = SessionReplayer::new(
            store.clone(),
            std::sync::Arc::new(InMemoryReplayCheckpoints::default()),
        );

        let stats = replayer.replay_file(&archive).await;
        assert_eq!(stats.replayed, 1, "turn from compressed archive persists");
        assert_eq!(stats.skipped, 0);
        assert_eq!(
            stats.last_offset,
            line.len() as u64 + 1,
            "offset over decoded bytes"
        );

        // Second pass WITH the saved checkpoint resumes at EOF: nothing
        // to do (incremental semantics).
        let again = replayer.replay_file(&archive).await;
        assert_eq!(again.replayed, 0);
        assert_eq!(again.duplicates, 0);

        // Lost checkpoints (daemon state.db wiped) force a byte-0
        // re-read: dedup keys must turn every line into a duplicate.
        let cold = SessionReplayer::new(
            store,
            std::sync::Arc::new(InMemoryReplayCheckpoints::default()),
        );
        let third = cold.replay_file(&archive).await;
        assert_eq!(third.replayed, 0);
        assert_eq!(third.duplicates, 1, "idempotency keys prevent double-write");
    }

    #[tokio::test]
    async fn in_memory_checkpoints_roundtrip_and_default_none() {
        let checkpoints = InMemoryReplayCheckpoints::default();
        assert!(checkpoints.load_offset("a.jsonl").await.unwrap().is_none());
        checkpoints.save_offset("a.jsonl", 42).await.unwrap();
        assert_eq!(checkpoints.load_offset("a.jsonl").await.unwrap(), Some(42));
        checkpoints.save_offset("a.jsonl", 0).await.unwrap();
        assert_eq!(checkpoints.load_offset("a.jsonl").await.unwrap(), Some(0));
    }
}
