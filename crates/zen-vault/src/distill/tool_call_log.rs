use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use zen_core::types::SessionStatus;

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
///
/// T166: expiry is gated on session STATUS, not just the log mtime — a
/// session that is still active (or compacted/resumable) must not be pruned
/// just because it has been tool-idle past the retention window. Only
/// sessions whose persisted status is terminal (Completed/Failed/Archived)
/// are pruned; removal failures are logged, never silent.
pub fn prune_expired_sessions(log_dir: &Path, retention_days: i64) {
    let cutoff = Utc::now() - Duration::days(retention_days);
    let cutoff_sys = std::time::SystemTime::from(cutoff);
    let Ok(entries) = fs::read_dir(log_dir) else {
        return;
    };
    // Lazily loaded: only sessions with an expired tool-call log need their
    // status checked, so the date-dir scan is skipped when nothing expired.
    let mut statuses: Option<HashMap<String, SessionStatus>> = None;
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let log_path = dir.join("tool_calls.jsonl");
        let Ok(meta) = fs::metadata(&log_path) else {
            continue;
        };
        let expired = meta.modified().map(|m| m < cutoff_sys).unwrap_or(false);
        if !expired {
            continue;
        }
        let Some(session_id) = dir.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let statuses = statuses.get_or_insert_with(|| session_statuses(log_dir));
        if !is_terminal(statuses.get(session_id).copied()) {
            tracing::debug!(
                session_id,
                "session not terminal — prune skipped (still active or resumable)"
            );
            continue;
        }
        if let Err(e) = fs::remove_file(&log_path) {
            tracing::warn!(
                session_dir = %dir.display(),
                error = %e,
                "failed to remove expired tool-call log"
            );
            continue;
        }
        if let Err(e) = fs::remove_dir(&dir) {
            tracing::warn!(
                session_dir = %dir.display(),
                error = %e,
                "failed to remove expired session dir"
            );
        } else {
            tracing::debug!(
                session_dir = %dir.display(),
                "pruned expired tool-call session log"
            );
        }
    }
}

/// Whether a session status is terminal and therefore safe to prune.
/// `None` (no conversation log found — status unknown) is treated as
/// non-terminal: the prune fails safe rather than deleting an active
/// session's tool-call data.
fn is_terminal(status: Option<SessionStatus>) -> bool {
    matches!(
        status,
        Some(SessionStatus::Completed | SessionStatus::Failed | SessionStatus::Archived)
    )
}

/// Read the persisted status of every session under `log_dir` from the
/// `session/meta` first line of each `YYYY/MM/DD/{id}.jsonl` conversation
/// log. Sessions without a conversation log are absent from the map.
fn session_statuses(log_dir: &Path) -> HashMap<String, SessionStatus> {
    let mut statuses = HashMap::new();
    let Ok(years) = fs::read_dir(log_dir) else {
        return statuses;
    };
    for year in years.flatten() {
        let year_path = year.path();
        if !year_path.is_dir() {
            continue;
        }
        let Ok(months) = fs::read_dir(&year_path) else {
            continue;
        };
        for month in months.flatten() {
            let month_path = month.path();
            if !month_path.is_dir() {
                continue;
            }
            let Ok(days) = fs::read_dir(&month_path) else {
                continue;
            };
            for day in days.flatten() {
                let day_path = day.path();
                if !day_path.is_dir() {
                    continue;
                }
                let Ok(files) = fs::read_dir(&day_path) else {
                    continue;
                };
                for file in files.flatten() {
                    let path = file.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                        continue;
                    }
                    let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                        continue;
                    };
                    if let Ok(session) = zen_core::types::SessionEvent::read_meta(&path) {
                        statuses.insert(id.to_string(), session.status);
                    }
                }
            }
        }
    }
    statuses
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

        let aggs = aggregate_all_sessions(log_dir);
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

        let aggs = aggregate_all_sessions(log_dir);
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

        let aggs = aggregate_all_sessions(log_dir);
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

        let aggs = aggregate_all_sessions(log_dir);
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
        // The stale session is terminal (Completed), so the status gate
        // admits it for pruning.
        write_session_meta(log_dir, "stale-session", SessionStatus::Completed);

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
    fn prune_skips_active_but_tool_idle_sessions() {
        let dir = tempdir().unwrap();
        let log_dir = dir.path();

        // A session whose tool_calls.jsonl is stale but whose status is still
        // Active must NOT be pruned — it is mid-flight, just tool-idle.
        append_tool_call(log_dir, "active-idle", &sample_record("fs.read", true, 10)).unwrap();
        write_session_meta(log_dir, "active-idle", SessionStatus::Active);

        // A terminal session with a stale log IS pruned.
        append_tool_call(log_dir, "done", &sample_record("fs.read", true, 10)).unwrap();
        write_session_meta(log_dir, "done", SessionStatus::Completed);

        for id in ["active-idle", "done"] {
            let log = log_dir.join(id).join("tool_calls.jsonl");
            let old =
                std::time::SystemTime::now() - std::time::Duration::from_secs(60 * 60 * 24 * 45);
            File::options()
                .write(true)
                .open(&log)
                .unwrap()
                .set_modified(old)
                .unwrap();
        }

        prune_expired_sessions(log_dir, 30);

        assert!(
            log_dir
                .join("active-idle")
                .join("tool_calls.jsonl")
                .exists(),
            "active-but-tool-idle session must survive the prune"
        );
        assert!(!log_dir.join("done").exists(), "terminal session is pruned");
    }

    #[test]
    fn prune_unknown_status_sessions_are_kept() {
        let dir = tempdir().unwrap();
        let log_dir = dir.path();

        // No conversation log → status unknown → the prune fails safe and
        // keeps the tool-call data rather than deleting an active session.
        append_tool_call(log_dir, "no-meta", &sample_record("fs.read", true, 10)).unwrap();
        let log = log_dir.join("no-meta").join("tool_calls.jsonl");
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(60 * 60 * 24 * 45);
        File::options()
            .write(true)
            .open(&log)
            .unwrap()
            .set_modified(old)
            .unwrap();

        prune_expired_sessions(log_dir, 30);

        assert!(log_dir.join("no-meta").join("tool_calls.jsonl").exists());
    }

    #[test]
    fn prune_logs_failed_dir_removal() {
        let dir = tempdir().unwrap();
        let log_dir = dir.path();

        // A stray file in the session dir makes the non-recursive remove_dir
        // fail after the tool_calls.jsonl is removed — the failure must be
        // logged, not silent, and the dir must survive.
        append_tool_call(log_dir, "stuck", &sample_record("fs.read", true, 10)).unwrap();
        write_session_meta(log_dir, "stuck", SessionStatus::Completed);
        fs::write(log_dir.join("stuck").join("stray.bin"), b"\x00").unwrap();

        let log = log_dir.join("stuck").join("tool_calls.jsonl");
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(60 * 60 * 24 * 45);
        File::options()
            .write(true)
            .open(&log)
            .unwrap()
            .set_modified(old)
            .unwrap();

        prune_expired_sessions(log_dir, 30);

        assert!(!log_dir.join("stuck").join("tool_calls.jsonl").exists());
        assert!(
            log_dir.join("stuck").join("stray.bin").exists(),
            "dir removal failure must not delete the stray file"
        );
    }

    /// Write a `session/meta` conversation log for `session_id` under
    /// `log_dir/YYYY/MM/DD/` with the given status (T166 status gate).
    fn write_session_meta(log_dir: &Path, session_id: &str, status: SessionStatus) {
        let session = zen_core::types::Session {
            id: session_id.to_string(),
            agent_name: "test".to_string(),
            title: None,
            parent_id: None,
            sensitivity_policy: zen_core::types::Sensitivity::Private,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            status,
            workspace: "test".to_string(),
        };
        let date_dir = log_dir.join("2026").join("08").join("01");
        fs::create_dir_all(&date_dir).unwrap();
        zen_core::types::SessionEvent::write_meta(
            &date_dir.join(format!("{session_id}.jsonl")),
            &session,
        )
        .unwrap();
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
