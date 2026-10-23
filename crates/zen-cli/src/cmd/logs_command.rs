use std::path::{Path, PathBuf};
use std::time::SystemTime;

use clap::Subcommand;
use colored::Colorize;
use tracing::debug;

use zen_core::errors::ZenError;
use zen_core::jsonl;
use zen_core::paths::ZenPaths;

// ---------------------------------------------------------------------------
// Log subcommands (agent execution / audit trail)
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum LogCommands {
    /// Filter log entries by agent name
    Agent {
        /// Agent name to filter by
        name: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Filter log entries by session ID
    Session {
        /// Session ID to filter by
        id: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Search log entries with a pattern
    Search {
        /// Search pattern (substring match across all fields)
        pattern: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// List available log files with sizes
    List,
}

pub fn execute_command(operation: &LogCommands) -> Result<(), ZenError> {
    match operation {
        LogCommands::Agent { name, json } => {
            debug!("filtering logs by agent: {}", name);
            let entries = read_all_log_entries()?;
            let filtered: Vec<_> = entries
                .into_iter()
                .filter(|e| {
                    e.get("agent")
                        .and_then(|a| a.as_str())
                        .map(|a| a == name.as_str())
                        .unwrap_or(false)
                })
                .collect();

            if !filtered.is_empty() {
                output_entries(&filtered, *json)?;
            } else {
                let pattern = format!("agent=\"{}\"", name);
                grep_zen_log(&pattern, *json)?;
            }
            Ok(())
        }
        LogCommands::Session { id, json } => {
            debug!("filtering logs by session: {}", id);
            let entries = read_all_log_entries()?;
            let filtered: Vec<_> = entries
                .into_iter()
                .filter(|e| {
                    e.get("session_id")
                        .and_then(|s| s.as_str())
                        .map(|s| s == id.as_str())
                        .unwrap_or(false)
                })
                .collect();

            if !filtered.is_empty() {
                output_entries(&filtered, *json)?;
            } else {
                grep_zen_log(id, *json)?;
            }
            Ok(())
        }
        LogCommands::Search { pattern, json } => {
            debug!("searching logs: pattern={}", pattern);
            let entries = read_all_log_entries()?;
            let pattern_lower = pattern.to_lowercase();
            let filtered: Vec<_> = entries
                .into_iter()
                .filter(|e| {
                    let text = serde_json::to_string(e).unwrap_or_default();
                    text.to_lowercase().contains(&pattern_lower)
                })
                .collect();

            if !filtered.is_empty() {
                output_entries(&filtered, *json)?;
            } else {
                grep_zen_log(pattern, *json)?;
            }
            Ok(())
        }
        LogCommands::List => list_log_files(),
    }
}

pub fn execute_show(
    lines: usize,
    level: Option<&str>,
    follow: bool,
    json: bool,
) -> Result<(), ZenError> {
    if follow {
        // T252: `agent-session.jsonl` has no producer anywhere in the workspace,
        // so the old follow path tailed a file that never exists. Follow now
        // renders the freshest tail snapshot of the tracing log — a snapshot,
        // not a live tail: no polling, re-run the command to refresh.
        return show_zen_log_tail(lines, level, json);
    }

    debug!("showing logs: lines={} level={:?}", lines, level);
    let entries = read_all_log_entries()?;
    let mut filtered: Vec<_> = entries;

    if let Some(lvl) = level {
        let lvl_lower = lvl.to_lowercase();
        filtered.retain(|e| {
            e.get("sensitivity")
                .and_then(|s| s.as_str())
                .map(|s| s.to_lowercase() == lvl_lower)
                .unwrap_or(false)
        });
    }

    filtered.truncate(lines);

    if filtered.is_empty() {
        show_zen_log_tail(lines, level, json)?;
        return Ok(());
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&filtered)
                .map_err(|e| ZenError::Message(format!("JSON error: {e}")))?
        );
    } else {
        for entry in &filtered {
            print_log_entry(entry);
        }
    }
    Ok(())
}

/// Read all log entries from safety-audit.jsonl.
///
/// T252: `agent-session.jsonl` is intentionally NOT read — it has no producer
/// anywhere in the workspace (the only references were this reader and its
/// tests), so it could only ever return an empty contribution.
fn read_all_log_entries() -> Result<Vec<serde_json::Value>, ZenError> {
    let paths = ZenPaths::detect()
        .map_err(|e| ZenError::Message(format!("failed to resolve paths: {e}")))?;
    let log_dir = paths.logs();

    let safety_log = log_dir.join("safety-audit.jsonl");

    let mut entries = Vec::new();

    match jsonl::read_jsonl_lines(&safety_log) {
        Ok(safety_entries) => entries.extend(safety_entries),
        Err(e) => eprintln!("warn: skip {}: {e}", safety_log.display()),
    }

    // Sort by timestamp descending (most recent first)
    entries.sort_by(|a, b| {
        let ta = a.get("timestamp").and_then(|t| t.as_str()).unwrap_or("");
        let tb = b.get("timestamp").and_then(|t| t.as_str()).unwrap_or("");
        tb.cmp(ta)
    });

    Ok(entries)
}

/// Print a single log entry in colored human-readable format.
fn print_log_entry(entry: &serde_json::Value) {
    let timestamp = entry
        .get("timestamp")
        .and_then(|t| t.as_str())
        .unwrap_or("unknown");
    let agent = entry
        .get("agent")
        .and_then(|a| a.as_str())
        .unwrap_or("unknown");
    let duration = entry
        .get("duration_ms")
        .and_then(|d| d.as_u64())
        .unwrap_or(0);
    let sensitivity = entry
        .get("sensitivity")
        .and_then(|s| s.as_str())
        .unwrap_or("unknown");
    let query_len = entry.get("query_len").and_then(|q| q.as_u64()).unwrap_or(0);
    let response_len = entry
        .get("response_len")
        .and_then(|r| r.as_u64())
        .unwrap_or(0);

    println!(
        "{} {} {} {} q:{} r:{} {}ms sensitive:{}",
        timestamp.dimmed(),
        "▶".green(),
        agent.cyan().bold(),
        "·".dimmed(),
        query_len.to_string().dimmed(),
        response_len.to_string().dimmed(),
        duration.to_string().yellow(),
        sensitivity.magenta(),
    );
}

fn output_entries(entries: &[serde_json::Value], json: bool) -> Result<(), ZenError> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(entries)
                .map_err(|e| ZenError::Message(format!("JSON error: {e}")))?
        );
    } else {
        for entry in entries {
            print_log_entry(entry);
        }
    }
    Ok(())
}

/// Enumerate tracing-appender rollover files, newest dated first.
///
/// `init_tracing` rotates daily as `zen.log.YYYY-MM-dd`; the bare `zen.log`
/// name never exists for rotation installs, so readers must scan the dated
/// set. The legacy bare file is appended as a fallback for old trees.
fn zen_log_files(log_dir: &Path) -> Vec<PathBuf> {
    let mut dated: Vec<(String, PathBuf)> = Vec::new();
    let mut legacy: Option<PathBuf> = None;
    if let Ok(entries) = std::fs::read_dir(log_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if name == "zen.log" {
                legacy = Some(path);
            } else if let Some(date) = name.strip_prefix("zen.log.") {
                dated.push((date.to_string(), path));
            }
        }
    }
    dated.sort_by(|a, b| b.0.cmp(&a.0));
    let mut files: Vec<PathBuf> = dated.into_iter().map(|(_, p)| p).collect();
    if let Some(legacy) = legacy {
        files.push(legacy);
    }
    files
}

fn grep_zen_log(pattern: &str, json: bool) -> Result<(), ZenError> {
    let paths = ZenPaths::detect()
        .map_err(|e| ZenError::Message(format!("failed to resolve paths: {e}")))?;
    let files = zen_log_files(&paths.logs());

    if files.is_empty() {
        println!("No log entries found");
        return Ok(());
    }

    let mut contents: Vec<String> = Vec::new();
    for file in &files {
        match std::fs::read_to_string(file) {
            Ok(text) => contents.push(text),
            Err(e) => eprintln!("warn: skip {}: {e}", file.display()),
        }
    }

    let all_lines = concat_log_lines(&contents);
    let pattern_lower = pattern.to_lowercase();
    let matches: Vec<&String> = all_lines
        .iter()
        .filter(|line| line.to_lowercase().contains(&pattern_lower))
        .collect();

    if matches.is_empty() {
        println!("No log entries found in zen.log matching '{}'", pattern);
        return Ok(());
    }

    println!(
        "{} ({} matches in tracing output)",
        "zen.log".yellow().bold(),
        matches.len()
    );
    println!();

    if json {
        let entries: Vec<serde_json::Value> = matches
            .iter()
            .map(|line| serde_json::json!({"line": line}))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&entries)
                .map_err(|e| ZenError::Message(format!("JSON error: {e}")))?
        );
    } else {
        for line in &matches {
            println!("{}", line.dimmed());
        }
    }

    Ok(())
}

/// Concatenate ordered file contents into lines.
///
/// A `\n` separator is inserted between files when the accumulated content
/// does not already end with one, so a file lacking a trailing newline cannot
/// glue its last line to the next file's first line.
fn concat_log_lines(contents: &[String]) -> Vec<String> {
    let mut joined = String::new();
    for text in contents {
        if !joined.is_empty() && !joined.ends_with('\n') {
            joined.push('\n');
        }
        joined.push_str(text);
    }
    joined.lines().map(str::to_string).collect()
}

fn show_zen_log_tail(lines: usize, level: Option<&str>, json: bool) -> Result<(), ZenError> {
    let paths = ZenPaths::detect()
        .map_err(|e| ZenError::Message(format!("failed to resolve paths: {e}")))?;
    let files = zen_log_files(&paths.logs());

    if files.is_empty() {
        println!("No log entries found");
        return Ok(());
    }

    // `zen_log_files` is newest-first (dated desc, legacy last); iterate in
    // reverse so the concatenation is chronological (legacy → oldest dated →
    // newest dated) and the last-N tail window holds the freshest lines.
    let mut contents: Vec<String> = Vec::new();
    for file in files.iter().rev() {
        match std::fs::read_to_string(file) {
            Ok(text) => contents.push(text),
            Err(e) => eprintln!("warn: skip {}: {e}", file.display()),
        }
    }
    let all_lines = concat_log_lines(&contents);
    if all_lines.is_empty() {
        println!("No log entries found");
        return Ok(());
    }

    let mut filtered: Vec<&String> = all_lines.iter().collect();

    if let Some(lvl) = level {
        let target = format!(" {} ", lvl.to_uppercase());
        filtered.retain(|line| line.contains(&target));
    }

    let start = filtered.len().saturating_sub(lines);
    let tail = &filtered[start..];

    println!(
        "{} (tracing output, {} lines total)",
        "zen.log".yellow().bold(),
        filtered.len()
    );
    println!();

    if json {
        let entries: Vec<serde_json::Value> = tail
            .iter()
            .map(|line| serde_json::json!({"line": line}))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&entries)
                .map_err(|e| ZenError::Message(format!("JSON error: {e}")))?
        );
    } else {
        for line in tail {
            println!("{}", line.dimmed());
        }
    }

    Ok(())
}

fn list_log_files() -> Result<(), ZenError> {
    let paths = ZenPaths::detect()
        .map_err(|e| ZenError::Message(format!("failed to resolve paths: {e}")))?;
    let log_dir = paths.logs();

    if !log_dir.exists() {
        println!("No log directory found");
        return Ok(());
    }

    let mut files: Vec<_> = std::fs::read_dir(&log_dir)
        .map_err(ZenError::Io)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .collect();
    files.sort_by_key(|e| e.file_name());

    if files.is_empty() {
        println!(
            "No log files found in {}",
            log_dir.display().to_string().dimmed()
        );
        return Ok(());
    }

    println!("Log files in {}:", log_dir.display().to_string().cyan());
    println!();

    for entry in &files {
        let name = entry.file_name().to_string_lossy().to_string();
        let metadata = entry.metadata().ok();
        let size = metadata
            .as_ref()
            .map(|m| format_size(m.len()))
            .unwrap_or_else(|| "?".to_string());
        let modified = metadata
            .and_then(|m| m.modified().ok())
            .map(format_relative_time)
            .unwrap_or_else(|| "unknown".to_string());

        println!(
            "  {:<30} {:>8}  {}",
            name.dimmed(),
            size.yellow(),
            modified.bright_black(),
        );
    }

    Ok(())
}

fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;
    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    if unit_idx == 0 {
        format!("{:.0}{}", size, UNITS[unit_idx])
    } else {
        format!("{:.1}{}", size, UNITS[unit_idx])
    }
}

fn format_relative_time(mtime: SystemTime) -> String {
    let now = SystemTime::now();
    let Ok(duration) = now.duration_since(mtime) else {
        return "just now".to_string();
    };

    let secs = duration.as_secs();
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    /// Serializes tests that mutate `ZEN_HOME`. Rust runs test threads in
    /// parallel by default; without this lock, the log-read tests race on the
    /// process environment and the empty-dir assertion reads another test's
    /// fixture.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn env_lock() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn setup_test_logs() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("zen-test-logs");
        let log_dir = dir.join("logs");
        std::fs::create_dir_all(&log_dir).ok();

        let safety_log = log_dir.join("safety-audit.jsonl");
        std::fs::write(
            &safety_log,
            r#"{"timestamp":"2026-06-15T10:30:00Z","content_id":"sha256:abc","sensitivity_level":"Public","provider":"openai","status":"pass"}
"#,
        )
        .unwrap();

        dir
    }

    #[test]
    fn test_logs_show_reads_safety_audit_entries() {
        let _env = env_lock();
        let dir = setup_test_logs();
        // SAFETY: env_lock() ensures no concurrent env access in this module;
        // ZEN_HOME is removed explicitly at the end (no implicit restore).
        unsafe { std::env::set_var("ZEN_HOME", dir.to_str().unwrap()) };
        let entries = read_all_log_entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["content_id"], "sha256:abc");
        assert_eq!(entries[0]["status"], "pass");
        unsafe { std::env::remove_var("ZEN_HOME") };
        std::fs::remove_dir_all(&dir).ok();
    }

    // T252 pin: agent-session.jsonl has no producer; a stale file on disk must
    // contribute nothing to `zen logs` (safety-audit stays the only JSONL face).
    #[test]
    fn read_all_log_entries_ignores_dead_agent_session_face() {
        let _env = env_lock();
        let dir = setup_test_logs();
        let agent_log = dir.join("logs").join("agent-session.jsonl");
        std::fs::write(
            &agent_log,
            r#"{"timestamp":"2026-06-15T12:00:00Z","agent":"Sisyphus","session_id":"ghi"}
"#,
        )
        .unwrap();
        // SAFETY: env_lock() ensures no concurrent env access in this module;
        // ZEN_HOME is removed explicitly at the end (no implicit restore).
        unsafe { std::env::set_var("ZEN_HOME", dir.to_str().unwrap()) };
        let entries = read_all_log_entries().unwrap();
        assert_eq!(entries.len(), 1, "only safety-audit.jsonl may contribute");
        assert_eq!(entries[0]["content_id"], "sha256:abc");
        unsafe { std::env::remove_var("ZEN_HOME") };
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_logs_read_empty_dir() {
        let _env = env_lock();
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: env_lock() ensures no concurrent env access in this module;
        // ZEN_HOME is removed explicitly at the end (no implicit restore).
        unsafe { std::env::set_var("ZEN_HOME", dir.path().to_str().unwrap()) };
        let entries = read_all_log_entries().unwrap();
        assert!(entries.is_empty());
        unsafe { std::env::remove_var("ZEN_HOME") };
    }

    // P1.8 pin (eng-review 2026-10-22 A2): with daily rollover, only bare
    // `zen.log` exists for pre-rotation installs (legacy fallback last); a
    // rotation tree must resolve dated files newest-first and tail from the
    // latest.
    #[test]
    fn zen_log_files_prefers_dated_rotations_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let log_dir = dir.path();
        std::fs::write(log_dir.join("zen.log.20261020"), "old").unwrap();
        std::fs::write(log_dir.join("zen.log.20261022"), "new").unwrap();
        std::fs::write(log_dir.join("zen.log"), "legacy").unwrap();
        std::fs::write(log_dir.join("audit.jsonl"), "x").unwrap();

        let files = zen_log_files(log_dir);
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            names,
            vec!["zen.log.20261022", "zen.log.20261020", "zen.log"]
        );
    }

    #[test]
    fn zen_log_files_empty_when_no_rotation_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("audit.jsonl"), "x").unwrap();
        assert!(zen_log_files(dir.path()).is_empty());
    }

    // P1 pin (eng-review 2026-10-22 D3): the tail window must hold the freshest
    // lines. Files reach the helper in chronological order (`zen_log_files`
    // reversed: legacy → oldest dated → newest dated); taking the last N of the
    // concatenation must end on the newest line, even when the newest file
    // lacks a trailing newline.
    #[test]
    fn tail_window_last_n_ends_on_newest_line_across_rotations() {
        let contents = vec![
            "legacy-oldest line\n".to_string(),
            "20261020 older line\n".to_string(),
            "20261022 newest line".to_string(),
        ];
        let all_lines = concat_log_lines(&contents);
        assert_eq!(all_lines.len(), 3);

        let n = 2;
        let start = all_lines.len().saturating_sub(n);
        let tail = &all_lines[start..];
        assert_eq!(tail.len(), n);
        assert_eq!(tail[0], "20261020 older line");
        assert_eq!(
            tail[tail.len() - 1],
            "20261022 newest line",
            "newest line must be the LAST element of the tail window"
        );
    }

    // P1 pin (eng-review 2026-10-22 grep separator): a file lacking a trailing
    // newline must not glue its last line to the next file's first line — the
    // separator keeps them as two separate match lines.
    #[test]
    fn grep_concat_separator_keeps_unterminated_lines_separate() {
        let contents = vec!["ERROR alpha hit".to_string(), "ERROR beta hit".to_string()];
        let all_lines = concat_log_lines(&contents);
        let matches: Vec<&String> = all_lines
            .iter()
            .filter(|line| line.to_lowercase().contains("error"))
            .collect();
        assert_eq!(
            matches.len(),
            2,
            "unterminated line must not glue to the next file's first line"
        );
        assert_eq!(matches[0].as_str(), "ERROR alpha hit");
        assert_eq!(matches[1].as_str(), "ERROR beta hit");
    }
}
