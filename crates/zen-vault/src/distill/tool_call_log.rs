use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use chrono::{Duration, Utc};
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

pub fn aggregate_tool_calls(log_dir: &Path, session_id: &str) -> Vec<ToolCallAggregate> {
    let path = log_dir.join(session_id).join("tool_calls.jsonl");
    if !path.exists() {
        return Vec::new();
    }

    let window_start = Utc::now() - Duration::days(30);
    let file = match fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(file);

    let mut by_tool: HashMap<String, Vec<(bool, u64)>> = HashMap::new();
    let mut error_counts: HashMap<String, HashMap<String, u64>> = HashMap::new();

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(_) => continue,
        };
        let record: ToolCall = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(_) => continue,
        };

        if record.recorded_at < window_start {
            continue;
        }

        by_tool
            .entry(record.tool.clone())
            .or_default()
            .push((record.success, record.latency_ms));

        if !record.success
            && let Some(cat) = &record.error_category
        {
            *error_counts
                .entry(record.tool.clone())
                .or_default()
                .entry(cat.clone())
                .or_insert(0) += 1;
        }
    }

    let mut aggregates: Vec<ToolCallAggregate> = Vec::new();

    for (tool, entries) in &by_tool {
        let total = entries.len() as u64;
        let success_count = entries.iter().filter(|(s, _)| *s).count() as u64;
        let success_rate = if total > 0 {
            success_count as f64 / total as f64 * 100.0
        } else {
            0.0
        };

        let mut latencies: Vec<u64> = entries.iter().map(|(_, l)| *l).collect();
        latencies.sort_unstable();
        let median = percentile(&latencies, 0.5);
        let p95 = percentile(&latencies, 0.95);

        let most_common_error = error_counts
            .get(tool)
            .and_then(|errs| errs.iter().max_by_key(|(_, c)| *c).map(|(k, _)| k.clone()));

        aggregates.push(ToolCallAggregate {
            tool: tool.clone(),
            total_calls: total,
            success_count,
            success_rate,
            median_latency_ms: median,
            p95_latency_ms: p95,
            most_common_error,
        });
    }

    aggregates.sort_by_key(|b| std::cmp::Reverse(b.total_calls));
    aggregates
}

pub fn aggregate_all_sessions(log_dir: &Path) -> Vec<ToolCallAggregate> {
    let window_start = Utc::now() - Duration::days(30);

    let mut by_tool: HashMap<String, Vec<(bool, u64)>> = HashMap::new();
    let mut error_counts: HashMap<String, HashMap<String, u64>> = HashMap::new();

    let entries = match fs::read_dir(log_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    for entry in entries.flatten() {
        let tool_calls_path = entry.path().join("tool_calls.jsonl");
        if !tool_calls_path.exists() {
            continue;
        }
        let file = match fs::File::open(&tool_calls_path) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let reader = BufReader::new(file);

        for line_result in reader.lines() {
            let line = match line_result {
                Ok(l) => l,
                Err(_) => continue,
            };
            let record: ToolCall = match serde_json::from_str(&line) {
                Ok(r) => r,
                Err(_) => continue,
            };

            if record.recorded_at < window_start {
                continue;
            }

            by_tool
                .entry(record.tool.clone())
                .or_default()
                .push((record.success, record.latency_ms));

            if !record.success
                && let Some(cat) = &record.error_category
            {
                *error_counts
                    .entry(record.tool.clone())
                    .or_default()
                    .entry(cat.clone())
                    .or_insert(0) += 1;
            }
        }
    }

    let mut aggregates: Vec<ToolCallAggregate> = Vec::new();

    for (tool, entries) in &by_tool {
        let total = entries.len() as u64;
        let success_count = entries.iter().filter(|(s, _)| *s).count() as u64;
        let success_rate = if total > 0 {
            success_count as f64 / total as f64 * 100.0
        } else {
            0.0
        };

        let mut latencies: Vec<u64> = entries.iter().map(|(_, l)| *l).collect();
        latencies.sort_unstable();
        let median = percentile(&latencies, 0.5);
        let p95 = percentile(&latencies, 0.95);

        let most_common_error = error_counts
            .get(tool)
            .and_then(|errs| errs.iter().max_by_key(|(_, c)| *c).map(|(k, _)| k.clone()));

        aggregates.push(ToolCallAggregate {
            tool: tool.clone(),
            total_calls: total,
            success_count,
            success_rate,
            median_latency_ms: median,
            p95_latency_ms: p95,
            most_common_error,
        });
    }

    aggregates.sort_by_key(|b| std::cmp::Reverse(b.total_calls));
    aggregates
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
}
