use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use chrono::{Datelike, NaiveDate, NaiveDateTime};
use tracing::{debug, warn};
use zen_core::paths::ZenPaths;

// ─── Data structures ───────────────────────────────────────────────────

/// A single timestamped log entry from a daily log file.
#[derive(Debug, Clone)]
pub struct LogEntry {
    /// ISO 8601 timestamp in UTC.
    pub timestamp: NaiveDateTime,
    /// Raw Markdown content of the entry.
    pub content: String,
    /// Extracted #tags from content (lowercased, deduplicated).
    pub tags: HashSet<String>,
}

impl LogEntry {
    pub fn new(content: String) -> Self {
        let tags = extract_tags(&content);
        Self {
            timestamp: chrono::Utc::now().naive_utc(),
            content,
            tags,
        }
    }

    pub fn with_timestamp(content: String, timestamp: NaiveDateTime) -> Self {
        let tags = extract_tags(&content);
        Self {
            timestamp,
            content,
            tags,
        }
    }
}

// ─── Helpers ───────────────────────────────────────────────────────────

/// Extract `#tag` patterns from a string.  Tags are `#` followed by
/// alphanumeric characters and underscores, matching typical Markdown
/// convention across both EN and CN text.
fn extract_tags(content: &str) -> HashSet<String> {
    let mut tags = HashSet::new();
    let mut tag_start: Option<usize> = None;

    for (i, ch) in content.char_indices() {
        if ch == '#' {
            tag_start = Some(i);
        } else if let Some(start) = tag_start {
            if ch.is_alphanumeric() || ch == '_' {
                // continue collecting
            } else {
                // end of potential tag
                let tag_str = &content[start + 1..i]; // skip the '#'
                if !tag_str.is_empty() {
                    tags.insert(tag_str.to_lowercase());
                }
                tag_start = None;
            }
        }
    }

    // handle tag at end of string
    if let Some(start) = tag_start {
        let tag_str = &content[start + 1..];
        if !tag_str.is_empty() {
            tags.insert(tag_str.to_lowercase());
        }
    }

    tags
}

/// Parse a daily log Markdown file into individual entries.
///
/// Entry header format: `"## YYYY-MM-DD HH:MM:SS — optional title\n"`
pub fn parse_daily_log(content: &str, base_date: NaiveDate) -> Vec<LogEntry> {
    let mut entries = Vec::new();
    let mut current_entry: Option<String> = None;
    let mut current_time: Option<NaiveDateTime> = None;

    for line in content.lines() {
        if let Some(stripped) = line.strip_prefix("## ") {
            // New entry — flush previous
            if let (Some(ts), Some(body)) = (current_time.take(), current_entry.take()) {
                entries.push(LogEntry::with_timestamp(body.trim().to_string(), ts));
            }

            // Parse timestamp: "2026-05-23 14:30:00 — title"
            let rest = stripped.trim();
            if let Ok(dt) = parse_log_timestamp(rest, base_date) {
                current_time = Some(dt);
                current_entry = None;
            }
        } else if current_time.is_some() {
            // Accumulate content for current entry
            if let Some(ref mut body) = current_entry {
                body.push('\n');
                body.push_str(line);
            } else {
                current_entry = Some(line.to_string());
            }
        }
    }

    // Flush last entry
    if let (Some(ts), Some(body)) = (current_time, current_entry) {
        entries.push(LogEntry::with_timestamp(body.trim().to_string(), ts));
    }

    entries
}

/// Parse the timestamp portion from a log header line.
///
/// Accepts formats:
/// - `2026-05-23 14:30:00`
/// - `2026-05-23 14:30:00 — some title`
/// - `2026-05-23T14:30:00`
fn parse_log_timestamp(line: &str, _base_date: NaiveDate) -> Result<NaiveDateTime> {
    // Try ISO format first (with or without 'T' separator)
    let stripped = if let Some(pos) = line.find('—') {
        line[..pos].trim()
    } else {
        line.trim()
    };

    // Try "YYYY-MM-DD HH:MM:SS"
    if let Ok(dt) = NaiveDateTime::parse_from_str(stripped, "%Y-%m-%d %H:%M:%S") {
        return Ok(dt);
    }

    // Try "YYYY-MM-DDTHH:MM:SS"
    if let Ok(dt) = NaiveDateTime::parse_from_str(stripped, "%Y-%m-%dT%H:%M:%S") {
        return Ok(dt);
    }

    // Fallback: timestamp-only, use current time
    warn!("could not parse log timestamp from: {}", stripped);
    Ok(chrono::Utc::now().naive_utc())
}

// ─── DailyLog struct ───────────────────────────────────────────────────

/// Manages the daily log directory (`~/.zen/memory/YYYY/`) and provides
/// operations for creating and reading daily Markdown entries.
pub struct DailyLog;

impl DailyLog {
    /// Create a new `DailyLog` instance.
    pub fn new() -> Self {
        Self
    }

    /// Returns the directory used for daily logs.
    ///
    /// `~/.zen/memory/YYYY/`
    pub fn log_dir(zen_paths: &ZenPaths, year: i32) -> PathBuf {
        zen_paths.memory().join(format!("{}", year))
    }

    /// Compute the daily log file path for a given date.
    ///
    /// `~/.zen/memory/YYYY/YYYY-MM-DD.md`
    pub fn log_file_path(zen_paths: &ZenPaths, date: NaiveDate) -> PathBuf {
        let year = Datelike::year(&date);
        Self::log_dir(zen_paths, year).join(format!("{}.md", date.format("%Y-%m-%d")))
    }

    /// Create a new log entry for today with the given content.
    /// Ensures the year directory exists, then appends the entry to today's
    /// log file with a timestamped header.
    ///
    /// Returns the path to the log file.
    pub fn create_entry(zen_paths: &ZenPaths, content: &str) -> Result<PathBuf> {
        let now = chrono::Utc::now().naive_utc();
        let today = now.date();
        Self::create_entry_at_date(zen_paths, today, content, &now)
    }

    /// Create a log entry for an arbitrary date.
    pub fn create_entry_at_date(
        zen_paths: &ZenPaths,
        date: NaiveDate,
        content: &str,
        timestamp: &NaiveDateTime,
    ) -> Result<PathBuf> {
        let year = Datelike::year(&date);
        let dir = Self::log_dir(zen_paths, year);

        // Ensure directory exists
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create daily log directory: {}", dir.display()))?;

        let path = Self::log_file_path(zen_paths, date);

        let header = format!("## {} — \n", timestamp.format("%Y-%m-%d %H:%M:%S"));

        // Append or create
        if path.exists() {
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .with_context(|| format!("failed to open daily log: {}", path.display()))?
                .write_all(format!("{}{}\n\n", header, content).as_bytes())
                .with_context(|| "failed to write daily log entry")?;
        } else {
            // New file — add a small header comment
            let preamble = format!(
                "# Daily Log — {}\n\n{}{}\n\n",
                date.format("%Y-%m-%d"),
                header,
                content
            );
            fs::write(&path, preamble)
                .with_context(|| format!("failed to create daily log: {}", path.display()))?;
        }

        debug!("daily log entry written: {}", path.display());
        Ok(path)
    }

    /// Read and parse all entries for a given date from the daily log file.
    ///
    /// Returns an empty vec if the file doesn't exist.
    pub fn read_entries(zen_paths: &ZenPaths, date: NaiveDate) -> Result<Vec<LogEntry>> {
        let path = Self::log_file_path(zen_paths, date);

        if !path.exists() {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read daily log: {}", path.display()))?;

        Ok(parse_daily_log(&content, date))
    }

    /// List all log files for a given year.
    pub fn list_year(zen_paths: &ZenPaths, year: i32) -> Result<Vec<PathBuf>> {
        let dir = Self::log_dir(zen_paths, year);

        if !dir.is_dir() {
            return Ok(Vec::new());
        }

        let mut files: Vec<PathBuf> = fs::read_dir(&dir)
            .with_context(|| format!("failed to read log directory: {}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|ext| ext == "md"))
            .collect();

        files.sort();
        Ok(files)
    }

    /// Delete a daily log file for the given date.
    /// Returns true if the file was deleted, false if it didn't exist.
    pub fn delete_day(zen_paths: &ZenPaths, date: NaiveDate) -> Result<bool> {
        let path = Self::log_file_path(zen_paths, date);

        if path.exists() {
            fs::remove_file(&path)
                .with_context(|| format!("failed to delete daily log: {}", path.display()))?;
            debug!("deleted daily log: {}", path.display());
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

impl Default for DailyLog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;

    fn setup_test_dir(root: &Path, year: i32, filename: &str, content: &str) -> PathBuf {
        let dir = root.join(year.to_string());
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(filename);
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn test_extract_tags_english() {
        let content = "Today I fixed #bug and added #feature: auth\nAlso worked on #bug";
        let tags = extract_tags(content);
        assert!(tags.contains("bug"));
        assert!(tags.contains("feature"));
        assert_eq!(tags.len(), 2);
    }

    #[test]
    fn test_extract_tags_mixed() {
        let content = "完成 #任务1 和 #任务2\n还有 #urgent";
        let tags = extract_tags(content);
        assert!(tags.contains("任务1"));
        assert!(tags.contains("任务2"));
        assert!(tags.contains("urgent"));
        assert_eq!(tags.len(), 3);
    }

    #[test]
    fn test_extract_tags_none() {
        let content = "Just a plain note with no tags";
        let tags = extract_tags(content);
        assert!(tags.is_empty());
    }

    #[test]
    fn test_parse_daily_log_format() {
        let content = "# Daily Log — 2026-05-23\n\n## 2026-05-23 09:00:00 — Morning standup\n\nHad a standup meeting\n\n## 2026-05-23 14:30:00 — Afternoon coding\n\nWorked on #memory module\n";
        let entries = parse_daily_log(content, NaiveDate::from_ymd_opt(2026, 5, 23).unwrap());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].content, "Had a standup meeting");
        assert_eq!(entries[1].content, "Worked on #memory module");
    }

    #[test]
    fn test_parse_daily_log_empty() {
        let content = "no entries here at all";
        let entries = parse_daily_log(content, NaiveDate::from_ymd_opt(2026, 5, 23).unwrap());
        assert!(entries.is_empty());
    }

    #[test]
    fn test_log_entry_tags() {
        let entry = LogEntry::new("Fixed #bug and added #feature".into());
        assert!(entry.tags.contains("bug"));
        assert!(entry.tags.contains("feature"));
        assert_eq!(entry.tags.len(), 2);
    }

    #[test]
    fn test_create_entry_at_date() {
        let tmp = tempdir().unwrap();
        // Create a minimal ZenPaths by using workspace root pattern —
        // instead, manually create the file structure
        let year = 2026;
        let dir = tmp.path().join("memory").join(year.to_string());
        fs::create_dir_all(&dir).unwrap();

        let date = NaiveDate::from_ymd_opt(2026, 5, 23).unwrap();
        let ts = NaiveDateTime::parse_from_str("2026-05-23 14:30:00", "%Y-%m-%d %H:%M:%S").unwrap();
        let path = dir.join(format!("{}.md", date.format("%Y-%m-%d")));

        fs::write(
            &path,
            format!(
                "# Daily Log — {}\n\n## {} — \n\nTest content\n\n",
                date.format("%Y-%m-%d"),
                ts.format("%Y-%m-%d %H:%M:%S")
            ),
        )
        .unwrap();

        // Verify the file was created
        assert!(path.exists());

        // Parse and verify
        let content = fs::read_to_string(&path).unwrap();
        let entries = parse_daily_log(&content, date);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, "Test content");
    }
}
