//! Cold-session archive compression (task B4, codex `compression.rs`
//! rollout precedent).
//!
//! PURPOSE: Session `.jsonl` archives older than [`COLD_THRESHOLD`] stop
//! receiving appends (live sessions are recent by construction), so they
//! are compressed to `<name>.jsonl.zst` at [`ZSTD_LEVEL`], the original
//! removed only after a verified encode, and at most [`MAX_CONCURRENT_JOBS`]
//! files are in flight at once. The replay path (T048) decompresses
//! transparently, keeping `mv2` a derived index regardless of on-disk
//! archive format.
//!
//! USAGE: invoked from the gateway GC tick after the replay tick, with
//! the sessions root; checkpoint rows for renamed files are translated
//! by the caller (daemon owns state.db) via the returned `renamed` pairs.
//!
//! EXPECTED: [`CompressionReport`] counts per-file outcomes; a file is
//! left untouched on any failure and its path lands in `errors`.
//!
//! ERRORS: read/encode/write failures are collected per-file — the pass
//! never aborts wholesale.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::Semaphore;

/// Sessions colder than this are compression candidates.
pub const COLD_THRESHOLD: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// zstd compression level (codex rollout value).
pub const ZSTD_LEVEL: i32 = 3;

/// Upper bound on concurrently compressing archives.
pub const MAX_CONCURRENT_JOBS: usize = 2;

/// Per-pass outcome. `renamed` maps `old.jsonl -> new.jsonl.zst` so the
/// caller can translate replay checkpoints onto the new path key.
#[derive(Debug, Default)]
pub struct CompressionReport {
    pub compressed: u32,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub renamed: Vec<(PathBuf, PathBuf)>,
    pub errors: Vec<String>,
}

/// Compress every cold `*.jsonl` under `sessions_root` (recursive).
///
/// "Cold" = filesystem mtime older than [`COLD_THRESHOLD`]. The original
/// file is removed only after the compressed twin exists AND decodes back
/// to the exact original byte length; otherwise the candidate is skipped
/// with an error entry and both files may briefly coexist.
pub async fn compress_cold_sessions(sessions_root: &Path) -> CompressionReport {
    let mut report = CompressionReport::default();
    if !sessions_root.is_dir() {
        return report;
    }

    let candidates: Vec<PathBuf> = walkdir::WalkDir::new(sessions_root)
        .sort_by_file_name()
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("jsonl"))
        .filter(|e| cold(e.path()))
        .map(|e| e.into_path())
        .collect();

    // Bound concurrent jobs across the whole pass (B4 contract).
    let jobs = std::sync::Arc::new(Semaphore::new(MAX_CONCURRENT_JOBS));
    let mut tasks = Vec::with_capacity(candidates.len());
    for path in candidates {
        let permit = std::sync::Arc::clone(&jobs).acquire_owned().await;
        tasks.push(tokio::task::spawn_blocking(move || {
            let _permit = permit;
            compress_one(&path)
        }));
    }

    for task in tasks {
        match task.await {
            Ok(Ok((src, dst, read, written))) => {
                report.compressed += 1;
                report.bytes_read += read;
                report.bytes_written += written;
                report.renamed.push((src, dst));
            }
            Ok(Err((path, err))) => report.errors.push(format!("{}: {err}", path.display())),
            Err(join_err) => report.errors.push(format!("join failure: {join_err}")),
        }
    }
    report
}

/// mtime-based coldness check; unreadable metadata = not cold.
fn cold(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age >= COLD_THRESHOLD)
}

type CompressOutcome = Result<(PathBuf, PathBuf, u64, u64), (PathBuf, String)>;

/// Encode → verify → rename → remove-source for one archive.
fn compress_one(src: &Path) -> CompressOutcome {
    let fail = |e: String| (src.to_path_buf(), e);

    let plain = std::fs::read(src).map_err(|e| fail(format!("read: {e}")))?;
    let encoded = zstd::stream::encode_all(&plain[..], ZSTD_LEVEL)
        .map_err(|e| fail(format!("zstd encode: {e}")))?;

    // Verification gate before destroying the source.
    let decoded =
        zstd::stream::decode_all(&encoded[..]).map_err(|e| fail(format!("verify decode: {e}")))?;
    if decoded.len() != plain.len() {
        return Err(fail(format!(
            "verify mismatch: decoded {} != original {}",
            decoded.len(),
            plain.len()
        )));
    }

    let dst = src.with_extension("jsonl.zst");
    let tmp = src.with_extension("jsonl.zst.tmp");
    {
        let mut out = std::fs::File::create(&tmp).map_err(|e| fail(format!("create tmp: {e}")))?;
        out.write_all(&encoded)
            .map_err(|e| fail(format!("write tmp: {e}")))?;
        out.sync_all()
            .map_err(|e| fail(format!("fsync tmp: {e}")))?;
    }
    std::fs::rename(&tmp, &dst).map_err(|e| fail(format!("rename: {e}")))?;

    let read = plain.len() as u64;
    let written = encoded.len() as u64;
    std::fs::remove_file(src).map_err(|e| fail(format!("remove source: {e}")))?;
    Ok((src.to_path_buf(), dst, read, written))
}

/// Reads one session archive's raw event bytes, transparently decoding
/// `.jsonl.zst` twins (replay-side counterpart of this module).
///
/// Returns `(bytes, was_compressed)`. Callers must NOT append-repair a
/// compressed archive — it is immutable by construction.
pub fn load_session_bytes(path: &Path) -> Result<(Vec<u8>, bool)> {
    let is_zst = path.extension().and_then(|x| x.to_str()) == Some("zst");
    let raw = std::fs::read(path)
        .with_context(|| format!("failed to read session archive: {}", path.display()))?;
    if !is_zst {
        return Ok((raw, false));
    }
    let decoded =
        zstd::stream::decode_all(&raw[..]).context("failed to decompress session archive")?;
    Ok((decoded, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_session(dir: &Path, name: &str, turns: usize) -> (PathBuf, String) {
        let p = dir.join(name);
        let mut body = String::new();
        body.push_str("{\"meta\":true}\n");
        for i in 0..turns {
            body.push_str(&format!(
                "{{\"chat/turn\":\"t{i}\",\"payload\":{{\"text\":\"turn {i}\"}}}}\n"
            ));
        }
        std::fs::write(&p, &body).unwrap();
        (p, body)
    }

    fn backdate(path: &Path, days: u64) {
        let f = std::fs::File::options().write(true).open(path).unwrap();
        f.set_modified(std::time::SystemTime::now() - Duration::from_secs(days * 24 * 60 * 60))
            .unwrap();
    }

    #[tokio::test]
    async fn compresses_only_cold_archives_and_verifies_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let (old, old_body) = write_session(dir.path(), "old-session.jsonl", 50);
        let (fresh, fresh_body) = write_session(dir.path(), "fresh-session.jsonl", 3);
        backdate(&old, 10);

        let report = compress_cold_sessions(dir.path()).await;

        assert_eq!(report.compressed, 1, "only the backdated file compresses");
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(!old.exists(), "source removed after verified encode");
        assert!(fresh.exists(), "recent file untouched");
        let zst = old.with_extension("jsonl.zst");
        assert!(zst.exists());
        assert!(
            report.bytes_written < report.bytes_read,
            "must actually shrink"
        );

        // Replay-side transparency: load_session_bytes decodes identically.
        let (bytes, compressed) = load_session_bytes(&zst).unwrap();
        assert!(compressed);
        assert_eq!(bytes, old_body.into_bytes(), "lossless roundtrip");
        let (fresh_bytes, fresh_compressed) = load_session_bytes(&fresh).unwrap();
        assert!(!fresh_compressed);
        assert_eq!(fresh_bytes, fresh_body.into_bytes());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn corrupted_candidate_is_skipped_with_error_entry() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        // Unreadable candidate: read fails → error entry, no panic.
        let weird = write_session(dir.path(), "weird.jsonl", 1).0;
        backdate(&weird, 9);
        std::fs::set_permissions(&weird, std::fs::Permissions::from_mode(0o000)).unwrap();

        let report = compress_cold_sessions(dir.path()).await;

        assert_eq!(report.compressed, 0);
        assert_eq!(report.errors.len(), 1);
        assert!(report.renamed.is_empty());
    }
}
