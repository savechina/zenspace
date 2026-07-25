use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::debug;
use zen_core::paths::ZenPaths;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillExecutionRecord {
    pub skill_name: String,
    pub timestamp: String,
    pub duration_ms: u64,
    pub quality_rating: Option<u8>,
    pub context_summary: String,
    pub result_summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillStats {
    pub total_runs: usize,
    pub avg_quality: f64,
    pub total_time_ms: u64,
    pub last_run: Option<String>,
}

pub struct SkillHistory {
    history_dir: PathBuf,
}

impl SkillHistory {
    pub fn new(paths: &ZenPaths) -> Self {
        Self {
            history_dir: paths.skills(),
        }
    }

    pub fn log_execution(&self, record: SkillExecutionRecord) -> anyhow::Result<()> {
        let path = self
            .history_dir
            .join(format!("{}-history.jsonl", record.skill_name));

        if !self.history_dir.is_dir() {
            fs::create_dir_all(&self.history_dir)?;
        }

        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;

        let mut line = serde_json::to_string(&record)?;
        line.push('\n');
        file.write_all(line.as_bytes())?;

        debug!(skill = record.skill_name.as_str(), "logged skill execution");
        Ok(())
    }

    pub fn get_history(&self, skill_name: &str) -> anyhow::Result<Vec<SkillExecutionRecord>> {
        let path = self.history_dir.join(format!("{skill_name}-history.jsonl"));

        if !path.is_file() {
            return Ok(Vec::new());
        }

        let file = fs::File::open(&path)?;
        let reader = BufReader::new(file);
        let mut records = Vec::new();

        for line_result in reader.lines() {
            let line = line_result?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<SkillExecutionRecord>(trimmed) {
                Ok(record) if record.skill_name == skill_name => {
                    records.push(record);
                }
                _ => {}
            }
        }

        Ok(records)
    }

    pub fn get_stats(&self, skill_name: &str) -> anyhow::Result<SkillStats> {
        let history = self.get_history(skill_name)?;

        if history.is_empty() {
            return Ok(SkillStats {
                total_runs: 0,
                avg_quality: 0.0,
                total_time_ms: 0,
                last_run: None,
            });
        }

        let total_runs = history.len();
        let total_time_ms: u64 = history.iter().map(|r| r.duration_ms).sum();

        let quality_sum: u64 = history
            .iter()
            .filter_map(|r| r.quality_rating.map(u64::from))
            .sum();
        let quality_count = history
            .iter()
            .filter(|r| r.quality_rating.is_some())
            .count();
        let avg_quality = if quality_count > 0 {
            quality_sum as f64 / quality_count as f64
        } else {
            0.0
        };

        let last_run = history.last().map(|r| r.timestamp.clone());

        Ok(SkillStats {
            total_runs,
            avg_quality,
            total_time_ms,
            last_run,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_log_and_get_history() {
        let dir = tempdir().unwrap();
        let history_dir = dir.path();
        fs::create_dir_all(history_dir).unwrap();

        let history = SkillHistory {
            history_dir: history_dir.to_path_buf(),
        };

        let record = SkillExecutionRecord {
            skill_name: "test-skill".to_string(),
            timestamp: "2026-01-15T10:00:00Z".to_string(),
            duration_ms: 150,
            quality_rating: Some(8),
            context_summary: "test run".to_string(),
            result_summary: "passed".to_string(),
        };

        history.log_execution(record.clone()).unwrap();

        let records = history.get_history("test-skill").unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0], record);

        let records = history.get_history("other-skill").unwrap();
        assert!(records.is_empty());
    }

    #[test]
    fn test_get_stats() {
        let dir = tempdir().unwrap();
        let history_dir = dir.path();
        fs::create_dir_all(history_dir).unwrap();

        let history = SkillHistory {
            history_dir: history_dir.to_path_buf(),
        };

        for i in 0..3 {
            let record = SkillExecutionRecord {
                skill_name: "my-skill".to_string(),
                timestamp: format!("2026-01-1{}T10:00:00Z", i),
                duration_ms: (i as u64 + 1) * 100,
                quality_rating: Some(5 + i as u8),
                context_summary: format!("run {i}"),
                result_summary: "ok".to_string(),
            };
            history.log_execution(record).unwrap();
        }

        let stats = history.get_stats("my-skill").unwrap();
        assert_eq!(stats.total_runs, 3);
        assert_eq!(stats.total_time_ms, 600);
        assert!((stats.avg_quality - 6.0).abs() < 0.01);
        assert_eq!(stats.last_run, Some("2026-01-12T10:00:00Z".to_string()));
    }
}
