//! JSONL (JSON Lines) read/write helpers with corruption tolerance.
//!
//! Used by `zen logs` command and agent audit logging. Provides:
//! - `read_jsonl_lines`: read all lines, skip malformed entries with warning
//! - `append_jsonl_line`: append one serializable value as a JSON line

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::io::Write;
use std::path::Path;
use tracing::warn;

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
pub fn append_jsonl_line(path: &Path, value: &impl Serialize) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory: {}", parent.display()))?;
    }

    let json = serde_json::to_string(value).context("failed to serialize JSONL entry")?;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open JSONL file: {}", path.display()))?;

    writeln!(file, "{json}").context("failed to write JSONL entry")?;

    Ok(())
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
}
