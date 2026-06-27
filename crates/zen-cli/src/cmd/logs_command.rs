use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::time::{Duration, SystemTime};

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

pub fn execute_show(lines: usize, level: Option<&str>, follow: bool, json: bool) -> Result<(), ZenError> {
    if follow {
        tail_logs(lines, level)?;
        return Ok(());
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

/// Read all log entries from agent-session.jsonl and safety-audit.jsonl.
fn read_all_log_entries() -> Result<Vec<serde_json::Value>, ZenError> {
    let paths =
        ZenPaths::detect().map_err(|e| ZenError::Message(format!("failed to resolve paths: {e}")))?;
    let log_dir = paths.logs();

    let agent_log = log_dir.join("agent-session.jsonl");
    let safety_log = log_dir.join("safety-audit.jsonl");

    let mut entries = Vec::new();

    if let Ok(agent_entries) = jsonl::read_jsonl_lines(&agent_log) {
        entries.extend(agent_entries);
    }

    if let Ok(safety_entries) = jsonl::read_jsonl_lines(&safety_log) {
        entries.extend(safety_entries);
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
    let query_len = entry
        .get("query_len")
        .and_then(|q| q.as_u64())
        .unwrap_or(0);
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

fn tail_logs(lines: usize, level_filter: Option<&str>) -> Result<(), ZenError> {
    let paths = ZenPaths::detect()
        .map_err(|e| ZenError::Message(format!("failed to resolve paths: {e}")))?;
    let log_file = paths.logs().join("agent-session.jsonl");

    if !log_file.exists() {
        println!("No log entries found");
        return Ok(());
    }

    let existing = jsonl::read_jsonl_lines(&log_file)
        .unwrap_or_default();
    for entry in existing.iter().rev().take(lines).rev() {
        if should_show_entry(entry, level_filter) {
            print_log_entry(entry);
        }
    }

    let file = std::fs::File::open(&log_file)
        .map_err(|e| ZenError::Message(format!("failed to open log file: {e}")))?;
    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::End(0)).ok();

    println!("{} Following {} (Ctrl-C to stop)", "⏳".yellow(), log_file.display().to_string().dimmed());

    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
            Ok(_) => {
                if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line.trim())
                    && should_show_entry(&entry, level_filter)
                {
                    print_log_entry(&entry);
                }
            }
            Err(_) => {
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
}

fn should_show_entry(entry: &serde_json::Value, level_filter: Option<&str>) -> bool {
    if let Some(lvl) = level_filter {
        entry
            .get("sensitivity")
            .and_then(|s| s.as_str())
            .map(|s| s.to_lowercase() == lvl.to_lowercase())
            .unwrap_or(false)
    } else {
        true
    }
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

fn grep_zen_log(pattern: &str, json: bool) -> Result<(), ZenError> {
    let paths = ZenPaths::detect()
        .map_err(|e| ZenError::Message(format!("failed to resolve paths: {e}")))?;
    let zen_log = paths.logs().join("zen.log");

    if !zen_log.exists() {
        println!("No log entries found");
        return Ok(());
    }

    let content = std::fs::read_to_string(&zen_log)
        .map_err(|e| ZenError::Message(format!("failed to read zen.log: {e}")))?;

    let pattern_lower = pattern.to_lowercase();
    let matches: Vec<&str> = content
        .lines()
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

fn show_zen_log_tail(lines: usize, level: Option<&str>, json: bool) -> Result<(), ZenError> {
    let paths = ZenPaths::detect()
        .map_err(|e| ZenError::Message(format!("failed to resolve paths: {e}")))?;
    let zen_log = paths.logs().join("zen.log");

    if !zen_log.exists() {
        println!("No log entries found");
        return Ok(());
    }

    let content = std::fs::read_to_string(&zen_log)
        .map_err(|e| ZenError::Message(format!("failed to read zen.log: {e}")))?;

    let all_lines: Vec<&str> = content.lines().collect();
    if all_lines.is_empty() {
        println!("No log entries found");
        return Ok(());
    }

    let mut filtered: Vec<&&str> = all_lines.iter().collect();

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
        println!("No log files found in {}", log_dir.display().to_string().dimmed());
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
    use zen_core::jsonl;

    fn setup_test_logs() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("zen-test-logs");
        let log_dir = dir.join("logs");
        std::fs::create_dir_all(&log_dir).ok();

        let agent_log = log_dir.join("agent-session.jsonl");
        std::fs::write(
            &agent_log,
            r#"{"timestamp":"2026-06-15T10:00:00Z","agent":"Sisyphus","session_id":"abc","query_len":50,"response_len":200,"duration_ms":1200,"sensitivity":"Public"}
{"timestamp":"2026-06-15T11:00:00Z","agent":"Hermes","session_id":"def","query_len":30,"response_len":100,"duration_ms":800,"sensitivity":"Private"}
{"timestamp":"2026-06-15T12:00:00Z","agent":"Sisyphus","session_id":"ghi","query_len":40,"response_len":300,"duration_ms":2000,"sensitivity":"Confidential"}
"#,
        )
        .unwrap();

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
    fn test_logs_show_reads_entries() {
        let dir = setup_test_logs();
        // SAFETY: test runs single-threaded, env is restored via drop
        unsafe { std::env::set_var("ZEN_HOME", dir.to_str().unwrap()) };
        let agent_log = dir.join("logs").join("agent-session.jsonl");
        let entries = jsonl::read_jsonl_lines(&agent_log).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0]["agent"], "Sisyphus");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_logs_read_empty_dir() {
        let dir = std::env::temp_dir().join("zen-test-logs-empty");
        std::fs::create_dir_all(&dir).ok();
        // SAFETY: test runs single-threaded, env is restored via drop
        unsafe { std::env::set_var("ZEN_HOME", dir.to_str().unwrap()) };
        let entries = read_all_log_entries().unwrap();
        assert!(entries.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
