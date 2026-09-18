use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use super::types::ToolCall;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolCallAggregate {
    pub tool: String,
    pub total_calls: u64,
    pub success_count: u64,
    pub success_rate: f64,
    pub median_latency_ms: u64,
    pub p95_latency_ms: u64,
    pub most_common_error: Option<String>,
}

pub fn append_tool_call(log_dir: &Path, session_id: &str, record: &ToolCall) -> Result<(), String> {
    let dir = log_dir.join(session_id);
    fs::create_dir_all(&dir).map_err(|e| format!("create session log dir: {e}"))?;
    let path = dir.join("tool_calls.jsonl");
    let line = serde_json::to_string(record).map_err(|e| format!("serialize: {e}"))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("open: {e}"))?;
    writeln!(file, "{line}").map_err(|e| format!("write: {e}"))?;
    Ok(())
}

/// Per-tool outcome/latency samples plus error-category tallies shared by
/// the aggregation folds.
#[derive(Default)]
struct ToolFold {
    by_tool: HashMap<String, Vec<(bool, u64)>>,
    error_counts: HashMap<String, HashMap<String, u64>>,
}

impl ToolFold {
    /// Fold one deserialized record into the tally, dropping entries older
    /// than the window start.
    fn fold_record(&mut self, record: ToolCall, window_start: DateTime<Utc>) {
        if record.recorded_at < window_start {
            return;
        }
        self.by_tool
            .entry(record.tool.clone())
            .or_default()
            .push((record.success, record.latency_ms));

        if !record.success
            && let Some(cat) = &record.error_category
        {
            *self
                .error_counts
                .entry(record.tool.clone())
                .or_default()
                .entry(cat.clone())
                .or_insert(0) += 1;
        }
    }

    /// Fold every JSONL line of a `tool_calls.jsonl` file, silently
    /// skipping malformed lines and I/O errors.
    fn fold_file(&mut self, path: &Path, window_start: DateTime<Utc>) {
        let file = match File::open(path) {
            Ok(f) => f,
            Err(_) => return,
        };
        let reader = BufReader::new(file);
        for line_result in reader.lines() {
            let Ok(line) = line_result else { break };
            if let Ok(record) = serde_json::from_str::<ToolCall>(&line) {
                self.fold_record(record, window_start);
            }
        }
    }

    fn finalize(self) -> Vec<ToolCallAggregate> {
        let ToolFold {
            by_tool,
            error_counts,
        } = self;
        let mut aggregates: Vec<ToolCallAggregate> = by_tool
            .into_iter()
            .map(|(tool, entries)| {
                let total = entries.len() as u64;
                let success_count = entries.iter().filter(|(s, _)| *s).count() as u64;
                let success_rate = if total > 0 {
                    success_count as f64 / total as f64 * 100.0
                } else {
                    0.0
                };
                let mut latencies: Vec<u64> = entries.iter().map(|(_, l)| *l).collect();
                latencies.sort_unstable();
                let most_common_error = error_counts
                    .get(&tool)
                    .and_then(|errs| errs.iter().max_by_key(|(_, c)| *c).map(|(k, _)| k.clone()));
                ToolCallAggregate {
                    tool,
                    total_calls: total,
                    success_count,
                    success_rate,
                    median_latency_ms: percentile(&latencies, 0.5),
                    p95_latency_ms: percentile(&latencies, 0.95),
                    most_common_error,
                }
            })
            .collect();
        aggregates.sort_by_key(|b| std::cmp::Reverse(b.total_calls));
        aggregates
    }
}

pub fn aggregate_tool_calls(log_dir: &Path, session_id: &str) -> Vec<ToolCallAggregate> {
    let window_start = Utc::now() - Duration::days(30);
    let mut fold = ToolFold::default();
    fold.fold_file(
        &log_dir.join(session_id).join("tool_calls.jsonl"),
        window_start,
    );
    fold.finalize()
}

pub fn aggregate_all_sessions(log_dir: &Path) -> Vec<ToolCallAggregate> {
    let window_start = Utc::now() - Duration::days(30);
    let mut fold = ToolFold::default();
    if let Ok(entries) = fs::read_dir(log_dir) {
        for entry in entries.flatten() {
            let path = entry.path().join("tool_calls.jsonl");
            if path.exists() {
                fold.fold_file(&path, window_start);
            }
        }
    }
    fold.finalize()
}

/// Best-effort GC for expired session logs: a `tool_calls.jsonl` whose
/// mtime predates the retention window cannot contain in-window records
/// (mtime tracks the last append), so its session directory is removed.
/// Keeps session logs bounded on disk and aggregation O(active sessions).
pub fn prune_expired_sessions(log_dir: &Path, retention_days: i64) {
    let cutoff = Utc::now() - Duration::days(retention_days);
    let cutoff_sys = std::time::SystemTime::from(cutoff);
    let Ok(entries) = fs::read_dir(log_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        let log_path = dir.join("tool_calls.jsonl");
        let Ok(meta) = fs::metadata(&log_path) else {
            continue;
        };
        let expired = meta.modified().map(|m| m < cutoff_sys).unwrap_or(false);
        if expired {
            let _ = fs::remove_file(&log_path);
            let _ = fs::remove_dir(&dir);
            tracing::debug!(
                session_dir = %dir.display(),
                "pruned expired tool-call session log"
            );
        }
    }
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_record(tool: &str, success: bool, latency_ms: u64) -> ToolCall {
        ToolCall {
            tool: tool.to_string(),
            success,
            latency_ms,
            error_category: if success {
                None
            } else {
                Some("Transient".to_string())
            },
            recorded_at: Utc::now(),
        }
    }

    #[test]
    fn append_and_aggregate_basic() {
        let dir = tempdir().unwrap();
        let log_dir = dir.path();

        append_tool_call(log_dir, "sess-1", &sample_record("fs.read", true, 50)).unwrap();
        append_tool_call(log_dir, "sess-1", &sample_record("fs.read", true, 120)).unwrap();
        append_tool_call(log_dir, "sess-1", &sample_record("web.search", false, 300)).unwrap();

        let aggs = aggregate_tool_calls(log_dir, "sess-1");
        assert_eq!(aggs.len(), 2);

        let fs_read = aggs.iter().find(|a| a.tool == "fs.read").unwrap();
        assert_eq!(fs_read.total_calls, 2);
        assert_eq!(fs_read.success_count, 2);
        assert!((fs_read.success_rate - 100.0).abs() < 0.1);

        let web_search = aggs.iter().find(|a| a.tool == "web.search").unwrap();
        assert_eq!(web_search.total_calls, 1);
        assert!((web_search.success_rate - 0.0).abs() < 0.1);
        assert_eq!(web_search.most_common_error, Some("Transient".to_string()));
    }

    #[test]
    fn old_entries_excluded_from_aggregation() {
        let dir = tempdir().unwrap();
        let log_dir = dir.path();

        let old_record = ToolCall {
            tool: "old.tool".to_string(),
            success: true,
            latency_ms: 10,
            error_category: None,
            recorded_at: Utc::now() - Duration::days(60),
        };
        append_tool_call(log_dir, "sess-old", &old_record).unwrap();

        let aggs = aggregate_tool_calls(log_dir, "sess-old");
        assert!(
            aggs.is_empty(),
            "entries older than 30 days must be excluded"
        );
    }

    #[test]
    fn aggregate_all_sessions_combines_across_dirs() {
        let dir = tempdir().unwrap();
        let log_dir = dir.path();

        append_tool_call(log_dir, "s1", &sample_record("fs.read", true, 50)).unwrap();
        append_tool_call(log_dir, "s2", &sample_record("fs.read", true, 100)).unwrap();
        append_tool_call(log_dir, "s2", &sample_record("fs.read", false, 200)).unwrap();

        let aggs = aggregate_all_sessions(log_dir);
        assert_eq!(aggs.len(), 1);
        assert_eq!(aggs[0].total_calls, 3);
        assert_eq!(aggs[0].success_count, 2);
    }

    #[test]
    fn non_session_dirs_are_skipped() {
        let dir = tempdir().unwrap();
        let log_dir = dir.path();

        fs::write(log_dir.join("not-a-session-dir"), "garbage").unwrap();
        append_tool_call(log_dir, "real-session", &sample_record("x", true, 1)).unwrap();

        let aggs = aggregate_tool_calls(log_dir, "real-session");
        assert_eq!(aggs.len(), 1);
    }

    #[test]
    fn malformed_lines_are_skipped_not_fatal() {
        let dir = tempdir().unwrap();
        let log_dir = dir.path();

        append_tool_call(log_dir, "s-mixed", &sample_record("fs.read", true, 50)).unwrap();
        let mut f = OpenOptions::new()
            .append(true)
            .open(log_dir.join("s-mixed").join("tool_calls.jsonl"))
            .unwrap();
        writeln!(f, "{{ garbage").unwrap();
        append_tool_call(log_dir, "s-mixed", &sample_record("fs.read", true, 80)).unwrap();

        let aggs = aggregate_tool_calls(log_dir, "s-mixed");
        assert_eq!(aggs[0].total_calls, 2);
    }

    #[test]
    fn prune_removes_expired_session_dirs_only() {
        let dir = tempdir().unwrap();
        let log_dir = dir.path();

        append_tool_call(
            log_dir,
            "fresh-session",
            &sample_record("fs.read", true, 50),
        )
        .unwrap();
        append_tool_call(
            log_dir,
            "stale-session",
            &sample_record("fs.read", true, 10),
        )
        .unwrap();

        // Age the stale session's log past the retention window.
        let stale_log = log_dir.join("stale-session").join("tool_calls.jsonl");
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(60 * 60 * 24 * 45);
        File::options()
            .write(true)
            .open(&stale_log)
            .unwrap()
            .set_modified(old)
            .unwrap();

        prune_expired_sessions(log_dir, 30);

        assert!(
            log_dir
                .join("fresh-session")
                .join("tool_calls.jsonl")
                .exists()
        );
        assert!(!log_dir.join("stale-session").exists());
    }

    #[test]
    fn prune_leaves_foreign_files_alone() {
        let dir = tempdir().unwrap();
        let log_dir = dir.path();

        fs::write(log_dir.join("stray.txt"), "not a session").unwrap();
        fs::create_dir(log_dir.join("empty-dir")).unwrap();

        prune_expired_sessions(log_dir, 30);

        assert!(log_dir.join("stray.txt").exists());
        assert!(log_dir.join("empty-dir").exists());
    }
}
