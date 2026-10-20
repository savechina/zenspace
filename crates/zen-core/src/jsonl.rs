//! JSONL (JSON Lines) read/write helpers with corruption tolerance.
//!
//! Used by `zen logs` command and agent audit logging. Provides:
//! - `read_jsonl_lines`: read all lines, skip malformed entries with warning
//! - `append_jsonl_line`: append one serializable value as a JSON line

use anyhow::{Context, Result};
use fs2::FileExt;
use serde::Serialize;
use serde_json::Value;
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tracing::warn;

/// Process-wide serialization for JSONL appends (audits, reports). Concurrent
/// in-process writers interleaving raw bytes produced 5/3795 garbled lines in
/// production audit.jsonl; the mutex closes the in-process gap, the fs2 flock
/// below closes the cross-process one (daemon workers + CLI separate processes).
static APPEND_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

const APPEND_LOCK_DEADLINE: Duration = Duration::from_secs(10);
const APPEND_LOCK_POLL: Duration = Duration::from_millis(50);

/// Read all JSONL lines from a file, returning parsed `serde_json::Value` entries.
///
/// - Returns empty `Vec` if the file does not exist (no error).
/// - Skips malformed lines (logs a warning with the skip count).
/// - Handles trailing empty lines gracefully.
pub fn read_jsonl_lines(path: &Path) -> Result<Vec<Value>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read JSONL file: {}", path.display()))?;

    let mut entries = Vec::new();
    let mut skipped = 0u32;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(trimmed) {
            Ok(value) => entries.push(value),
            Err(_) => {
                skipped += 1;
            }
        }
    }

    if skipped > 0 {
        warn!(
            path = %path.display(),
            skipped,
            total = entries.len() + skipped as usize,
            "skipped malformed JSONL lines"
        );
    }

    Ok(entries)
}

/// Append a single JSON line to a file.
///
/// - Creates the file and parent directories if they don't exist.
/// - Appends a trailing newline after the JSON.
/// - Uses `serde_json::to_string` (compact, not pretty-printed) for one-line-per-entry.
/// - Line-atomic under concurrency: an in-process mutex plus a cross-process
///   flock (10s deadline) guarantee no interleaved partial lines from the
///   daemon and CLI writing the same file simultaneously.
pub fn append_jsonl_line(path: &Path, value: &impl Serialize) -> Result<()> {
    let json = serde_json::to_string(value).context("failed to serialize JSONL entry")?;
    let line = format!("{json}\n").into_bytes();

    let lock = APPEND_LOCK.get_or_init(|| Mutex::new(()));
    let _in_process = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    // Hot path: open directly and only fall back to create_dir_all when the
    // parent genuinely does not exist — the unconditional pre-pass made
    // every audit append pay a directory-stat walk on an already-cached
    // filesystem.
    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        Ok(file) => file,
        Err(_) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create directory: {}", parent.display()))?;
            }
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .with_context(|| format!("failed to open JSONL file: {}", path.display()))?
        }
    };

    lock_exclusive_with_deadline(&file, path)?;
    file.write_all(&line)
        .with_context(|| format!("failed to write JSONL entry: {}", path.display()))?;

    Ok(())
}

fn lock_exclusive_with_deadline(file: &std::fs::File, path: &Path) -> Result<()> {
    let deadline = Instant::now() + APPEND_LOCK_DEADLINE;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(()),
            Err(_) if Instant::now() < deadline => std::thread::sleep(APPEND_LOCK_POLL),
            Err(_) => {
                anyhow::bail!(
                    "failed to acquire JSONL append lock within {:?}: {}",
                    APPEND_LOCK_DEADLINE,
                    path.display()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("zen-test-{name}.jsonl"))
    }

    #[test]
    fn test_read_jsonl_normal() {
        let path = tmp_path("read-normal");
        std::fs::write(&path, "{\"a\":1}\n{\"b\":2}\n{\"c\":3}\n").unwrap();

        let entries = read_jsonl_lines(&path).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0]["a"], 1);
        assert_eq!(entries[1]["b"], 2);
        assert_eq!(entries[2]["c"], 3);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_read_jsonl_empty_file() {
        let path = tmp_path("read-empty");
        std::fs::write(&path, "").unwrap();

        let entries = read_jsonl_lines(&path).unwrap();
        assert!(entries.is_empty());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_read_jsonl_missing_file() {
        let path = tmp_path("read-nonexistent");
        // Don't create the file
        let entries = read_jsonl_lines(&path).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_read_jsonl_corrupted_line() {
        let path = tmp_path("read-corrupted");
        std::fs::write(&path, "{\"a\":1}\nnot-json\n{\"c\":3}\n").unwrap();

        let entries = read_jsonl_lines(&path).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["a"], 1);
        assert_eq!(entries[1]["c"], 3);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_read_jsonl_trailing_empty_lines() {
        let path = tmp_path("read-trailing");
        std::fs::write(&path, "{\"a\":1}\n\n\n").unwrap();

        let entries = read_jsonl_lines(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["a"], 1);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_append_jsonl_roundtrip() {
        let path = tmp_path("append-roundtrip");

        #[derive(Serialize)]
        struct Entry {
            name: String,
            count: u32,
        }

        append_jsonl_line(
            &path,
            &Entry {
                name: "test".to_string(),
                count: 42,
            },
        )
        .unwrap();
        append_jsonl_line(
            &path,
            &Entry {
                name: "other".to_string(),
                count: 7,
            },
        )
        .unwrap();

        let entries = read_jsonl_lines(&path).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["name"], "test");
        assert_eq!(entries[0]["count"], 42);
        assert_eq!(entries[1]["name"], "other");
        assert_eq!(entries[1]["count"], 7);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_append_jsonl_creates_parent_dirs() {
        let parent = std::env::temp_dir().join("zen-test-jsonl-subdir");
        let path = parent.join("nested").join("test.jsonl");

        #[derive(Serialize)]
        struct Entry {
            msg: String,
        }

        append_jsonl_line(
            &path,
            &Entry {
                msg: "hello".to_string(),
            },
        )
        .unwrap();

        let entries = read_jsonl_lines(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["msg"], "hello");

        // Clean up
        std::fs::remove_dir_all(&parent).ok();
    }

    #[test]
    fn test_append_jsonl_concurrent_threads_no_interleaving() {
        let path = tmp_path("append-concurrent");
        std::fs::remove_file(&path).ok();

        const THREADS: usize = 4;
        const APPENDS: usize = 250;

        let handles: Vec<_> = (0..THREADS)
            .map(|t| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for i in 0..APPENDS {
                        append_jsonl_line(&path, &serde_json::json!({"thread": t, "seq": i}))
                            .unwrap();
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), THREADS * APPENDS);
        for line in &lines {
            assert!(
                serde_json::from_str::<Value>(line).is_ok(),
                "malformed line: {line}"
            );
        }

        std::fs::remove_file(&path).ok();
    }
}
