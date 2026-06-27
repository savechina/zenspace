use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;

#[derive(Debug, Error)]
pub enum VirtueLogError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("missing field: {0}")]
    MissingField(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VirtueDomain {
    Health,
    Speech,
    Order,
    Resolution,
    Diligence,
    Balance,
    Tranquility,
}

impl VirtueDomain {
    pub fn as_str(&self) -> &'static str {
        match self {
            VirtueDomain::Health => "保身",
            VirtueDomain::Speech => "谨言",
            VirtueDomain::Order => "秩序",
            VirtueDomain::Resolution => "决心",
            VirtueDomain::Diligence => "勤俭",
            VirtueDomain::Balance => "中庸",
            VirtueDomain::Tranquility => "平静",
        }
    }

    pub fn slug(&self) -> &'static str {
        match self {
            VirtueDomain::Health => "health",
            VirtueDomain::Speech => "speech",
            VirtueDomain::Order => "order",
            VirtueDomain::Resolution => "resolution",
            VirtueDomain::Diligence => "diligence",
            VirtueDomain::Balance => "balance",
            VirtueDomain::Tranquility => "tranquility",
        }
    }

    pub fn all() -> &'static [VirtueDomain] {
        &[
            VirtueDomain::Health,
            VirtueDomain::Speech,
            VirtueDomain::Order,
            VirtueDomain::Resolution,
            VirtueDomain::Diligence,
            VirtueDomain::Balance,
            VirtueDomain::Tranquility,
        ]
    }

    pub fn from_slug(s: &str) -> Option<Self> {
        match s {
            "health" => Some(VirtueDomain::Health),
            "speech" => Some(VirtueDomain::Speech),
            "order" => Some(VirtueDomain::Order),
            "resolution" => Some(VirtueDomain::Resolution),
            "diligence" => Some(VirtueDomain::Diligence),
            "balance" => Some(VirtueDomain::Balance),
            "tranquility" => Some(VirtueDomain::Tranquility),
            _ => None,
        }
    }
}

impl fmt::Display for VirtueDomain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VirtueStatus {
    Kept,
    Broken,
    Partial,
}

impl VirtueStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            VirtueStatus::Kept => "kept",
            VirtueStatus::Broken => "broken",
            VirtueStatus::Partial => "partial",
        }
    }
}

impl fmt::Display for VirtueStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VirtueLog {
    pub id: String,
    pub virtue: VirtueDomain,
    pub status: VirtueStatus,
    pub streak: u32,
    pub date: NaiveDate,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl VirtueLog {
    pub fn new(virtue: VirtueDomain, status: VirtueStatus, date: NaiveDate) -> Self {
        let now = Utc::now();
        let id = format!("virtue-{}-{}", virtue.slug(), date.format("%Y%m%d"));
        let streak = match status {
            VirtueStatus::Kept => 1,
            VirtueStatus::Broken => 0,
            VirtueStatus::Partial => 0,
        };
        Self {
            id,
            virtue,
            status,
            streak,
            date,
            note: None,
            created_at: now,
        }
    }

    /// Auto-compute streak from yesterday's value.
    pub fn check_in(
        virtue: VirtueDomain,
        status: VirtueStatus,
        date: NaiveDate,
        streak_yesterday: u32,
    ) -> Self {
        let streak = match status {
            VirtueStatus::Kept => streak_yesterday + 1,
            VirtueStatus::Broken => 0,
            VirtueStatus::Partial => streak_yesterday,
        };
        let now = Utc::now();
        let id = format!("virtue-{}-{}", virtue.slug(), date.format("%Y%m%d"));
        Self {
            id,
            virtue,
            status,
            streak,
            date,
            note: None,
            created_at: now,
        }
    }

    /// Compute current streak from sorted logs for a given virtue.
    pub fn compute_streak(logs: &[VirtueLog], virtue: VirtueDomain) -> u32 {
        let mut sorted: Vec<&VirtueLog> = logs
            .iter()
            .filter(|l| l.virtue == virtue)
            .collect();
        sorted.sort_by_key(|b| std::cmp::Reverse(b.date));

        let mut streak = 0u32;
        for log in sorted {
            match log.status {
                VirtueStatus::Kept => streak += 1,
                VirtueStatus::Broken => break,
                VirtueStatus::Partial => continue,
            }
        }
        streak
    }
}

impl VirtueLog {
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();
        md.push_str("---\n");
        md.push_str(&format!("id: {}\n", self.id));
        md.push_str(&format!("virtue: {}\n", self.virtue.slug()));
        md.push_str(&format!("virtue_cn: \"{}\"\n", self.virtue.as_str()));
        md.push_str(&format!("status: {}\n", self.status));
        md.push_str(&format!("streak: {}\n", self.streak));
        md.push_str(&format!("date: {}\n", self.date.format("%Y-%m-%d")));
        md.push_str(&format!("created_at: {}\n", self.created_at.to_rfc3339()));
        md.push_str("---\n\n");
        md.push_str(&format!(
            "# Virtue: {} ({})\n\n",
            self.virtue.as_str(),
            self.virtue.slug()
        ));
        md.push_str(&format!("**Date**: {}\n", self.date.format("%Y-%m-%d")));
        md.push_str(&format!("**Status**: {}\n", self.status));
        md.push_str(&format!("**Streak**: {}\n", self.streak));
        if let Some(ref note) = self.note {
            md.push_str(&format!("\n{note}\n"));
        }
        md
    }

    pub fn save(&self, dir: &Path) -> Result<PathBuf, VirtueLogError> {
        let virtue_dir = dir.join(self.virtue.slug());
        fs::create_dir_all(&virtue_dir)?;
        let path = virtue_dir.join(format!("{}.md", self.date.format("%Y-%m-%d")));
        let content = self.to_markdown();
        fs::write(&path, content)?;
        Ok(path)
    }

    pub fn load(path: &Path) -> Result<Self, VirtueLogError> {
        let content = fs::read_to_string(path)?;
        Self::from_markdown(&content)
    }

    pub fn load_all(dir: &Path) -> Result<Vec<VirtueLog>, VirtueLogError> {
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut logs = Vec::new();
        self::collect_logs_recursive(dir, &mut logs)?;
        Ok(logs)
    }

    pub fn load_for_virtue(dir: &Path, virtue: VirtueDomain) -> Result<Vec<VirtueLog>, VirtueLogError> {
        let virtue_dir = dir.join(virtue.slug());
        if !virtue_dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut logs = Vec::new();
        for entry in fs::read_dir(&virtue_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "md") {
                match Self::load(&path) {
                    Ok(l) => logs.push(l),
                    Err(e) => {
                        warn!(
                            path = %path.display(),
                            error = %e,
                            "failed to parse virtue log file, skipping"
                        );
                    }
                }
            }
        }
        logs.sort_by_key(|a| a.date);
        Ok(logs)
    }

    pub fn from_markdown(content: &str) -> Result<Self, VirtueLogError> {
        let fm = extract_frontmatter(content)?;
        let id = parse_yaml_field(&fm, "id")
            .ok_or_else(|| VirtueLogError::MissingField("id".into()))?;
        let virtue_slug = parse_yaml_field(&fm, "virtue")
            .ok_or_else(|| VirtueLogError::MissingField("virtue".into()))?;
        let virtue = VirtueDomain::from_slug(&virtue_slug)
            .ok_or_else(|| VirtueLogError::Parse(format!("invalid virtue: {virtue_slug}")))?;
        let status_str = parse_yaml_field(&fm, "status")
            .ok_or_else(|| VirtueLogError::MissingField("status".into()))?;
        let status = match status_str.as_str() {
            "kept" => VirtueStatus::Kept,
            "broken" => VirtueStatus::Broken,
            "partial" => VirtueStatus::Partial,
            _ => return Err(VirtueLogError::Parse(format!("invalid status: {status_str}"))),
        };
        let streak: u32 = parse_yaml_field(&fm, "streak")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let date_str = parse_yaml_field(&fm, "date")
            .ok_or_else(|| VirtueLogError::MissingField("date".into()))?;
        let date = NaiveDate::parse_from_str(&date_str, "%Y-%m-%d")
            .map_err(|e| VirtueLogError::Parse(format!("invalid date: {e}")))?;
        let created_at = parse_yaml_field(&fm, "created_at")
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        let body = extract_body(content).unwrap_or_default();
        let note = body
            .lines()
            .skip_while(|l| l.starts_with('#') || l.starts_with("**") || l.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();
        let note = if note.is_empty() { None } else { Some(note) };

        Ok(VirtueLog {
            id,
            virtue,
            status,
            streak,
            date,
            note,
            created_at,
        })
    }
}

fn collect_logs_recursive(dir: &Path, logs: &mut Vec<VirtueLog>) -> Result<(), VirtueLogError> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_logs_recursive(&path, logs)?;
        } else if path.extension().is_some_and(|ext| ext == "md") {
            match VirtueLog::load(&path) {
                Ok(l) => logs.push(l),
                Err(e) => {
                    warn!(
                        path = %path.display(),
                        error = %e,
                        "failed to parse virtue log, skipping"
                    );
                }
            }
        }
    }
    Ok(())
}

fn extract_frontmatter(content: &str) -> Result<String, VirtueLogError> {
    let mut lines = content.lines();
    let first = lines.next().unwrap_or("").trim();
    if first != "---" {
        return Err(VirtueLogError::Parse(
            "missing frontmatter opening ---".into(),
        ));
    }
    let mut fm = String::new();
    for line in lines {
        if line.trim() == "---" {
            return Ok(fm);
        }
        fm.push_str(line);
        fm.push('\n');
    }
    Err(VirtueLogError::Parse(
        "missing frontmatter closing ---".into(),
    ))
}

fn parse_yaml_field(frontmatter: &str, key: &str) -> Option<String> {
    for line in frontmatter.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(&format!("{key}:")) {
            let val = rest.trim().to_string();
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

fn extract_body(content: &str) -> Result<String, VirtueLogError> {
    let mut lines = content.lines();
    lines.next();
    let mut past_frontmatter = false;
    let mut body = String::new();
    for line in lines {
        if !past_frontmatter {
            if line.trim() == "---" {
                past_frontmatter = true;
            }
            continue;
        }
        body.push_str(line);
        body.push('\n');
    }
    Ok(body.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_domain_all_count() {
        assert_eq!(VirtueDomain::all().len(), 7);
    }

    #[test]
    fn test_domain_from_slug() {
        assert_eq!(VirtueDomain::from_slug("health"), Some(VirtueDomain::Health));
        assert_eq!(VirtueDomain::from_slug("speech"), Some(VirtueDomain::Speech));
        assert_eq!(VirtueDomain::from_slug("order"), Some(VirtueDomain::Order));
        assert_eq!(VirtueDomain::from_slug("resolution"), Some(VirtueDomain::Resolution));
        assert_eq!(VirtueDomain::from_slug("diligence"), Some(VirtueDomain::Diligence));
        assert_eq!(VirtueDomain::from_slug("balance"), Some(VirtueDomain::Balance));
        assert_eq!(VirtueDomain::from_slug("tranquility"), Some(VirtueDomain::Tranquility));
        assert_eq!(VirtueDomain::from_slug("invalid"), None);
    }

    #[test]
    fn test_domain_chinese_names() {
        assert_eq!(VirtueDomain::Health.as_str(), "保身");
        assert_eq!(VirtueDomain::Speech.as_str(), "谨言");
        assert_eq!(VirtueDomain::Order.as_str(), "秩序");
        assert_eq!(VirtueDomain::Resolution.as_str(), "决心");
        assert_eq!(VirtueDomain::Diligence.as_str(), "勤俭");
        assert_eq!(VirtueDomain::Balance.as_str(), "中庸");
        assert_eq!(VirtueDomain::Tranquility.as_str(), "平静");
    }

    #[test]
    fn test_domain_slugs() {
        assert_eq!(VirtueDomain::Health.slug(), "health");
        assert_eq!(VirtueDomain::Speech.slug(), "speech");
        assert_eq!(VirtueDomain::Order.slug(), "order");
        assert_eq!(VirtueDomain::Resolution.slug(), "resolution");
        assert_eq!(VirtueDomain::Diligence.slug(), "diligence");
        assert_eq!(VirtueDomain::Balance.slug(), "balance");
        assert_eq!(VirtueDomain::Tranquility.slug(), "tranquility");
    }

    #[test]
    fn test_status_display() {
        assert_eq!(VirtueStatus::Kept.to_string(), "kept");
        assert_eq!(VirtueStatus::Broken.to_string(), "broken");
        assert_eq!(VirtueStatus::Partial.to_string(), "partial");
    }

    #[test]
    fn test_streak_kept_increments() {
        let date = NaiveDate::from_ymd_opt(2026, 6, 26).unwrap();
        let log = VirtueLog::check_in(VirtueDomain::Health, VirtueStatus::Kept, date, 5);
        assert_eq!(log.streak, 6);
    }

    #[test]
    fn test_streak_broken_resets() {
        let date = NaiveDate::from_ymd_opt(2026, 6, 26).unwrap();
        let log = VirtueLog::check_in(VirtueDomain::Health, VirtueStatus::Broken, date, 10);
        assert_eq!(log.streak, 0);
    }

    #[test]
    fn test_streak_partial_holds() {
        let date = NaiveDate::from_ymd_opt(2026, 6, 26).unwrap();
        let log = VirtueLog::check_in(VirtueDomain::Health, VirtueStatus::Partial, date, 7);
        assert_eq!(log.streak, 7);
    }

    #[test]
    fn test_compute_streak_from_logs() {
        let d1 = NaiveDate::from_ymd_opt(2026, 6, 24).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2026, 6, 25).unwrap();
        let d3 = NaiveDate::from_ymd_opt(2026, 6, 26).unwrap();

        let mut log1 = VirtueLog::new(VirtueDomain::Health, VirtueStatus::Kept, d1);
        log1.streak = 1;
        let mut log2 = VirtueLog::new(VirtueDomain::Health, VirtueStatus::Kept, d2);
        log2.streak = 2;
        let mut log3 = VirtueLog::new(VirtueDomain::Health, VirtueStatus::Kept, d3);
        log3.streak = 3;

        let logs = vec![log1, log2, log3];
        let streak = VirtueLog::compute_streak(&logs, VirtueDomain::Health);
        assert_eq!(streak, 3);
    }

    #[test]
    fn test_compute_streak_broken_resets() {
        let d1 = NaiveDate::from_ymd_opt(2026, 6, 24).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2026, 6, 25).unwrap();
        let d3 = NaiveDate::from_ymd_opt(2026, 6, 26).unwrap();

        let mut log1 = VirtueLog::new(VirtueDomain::Health, VirtueStatus::Kept, d1);
        log1.streak = 1;
        let mut log2 = VirtueLog::new(VirtueDomain::Health, VirtueStatus::Broken, d2);
        log2.streak = 0;
        let mut log3 = VirtueLog::new(VirtueDomain::Health, VirtueStatus::Kept, d3);
        log3.streak = 1;

        let logs = vec![log1, log2, log3];
        let streak = VirtueLog::compute_streak(&logs, VirtueDomain::Health);
        assert_eq!(streak, 1);
    }

    #[test]
    fn test_save_load_roundtrip() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("virtues");

        let date = NaiveDate::from_ymd_opt(2026, 6, 26).unwrap();
        let mut log = VirtueLog::check_in(VirtueDomain::Speech, VirtueStatus::Kept, date, 3);
        log.note = Some("Stayed quiet in meeting".to_string());
        let path = log.save(&dir).unwrap();

        assert!(path.exists());
        assert!(path.to_string_lossy().contains("speech"));
        assert!(path.to_string_lossy().contains("2026-06-26"));

        let loaded = VirtueLog::load(&path).unwrap();
        assert_eq!(loaded.id, log.id);
        assert_eq!(loaded.virtue, VirtueDomain::Speech);
        assert_eq!(loaded.status, VirtueStatus::Kept);
        assert_eq!(loaded.streak, 4);
        assert_eq!(loaded.date, date);
        assert_eq!(loaded.note, Some("Stayed quiet in meeting".to_string()));
    }

    #[test]
    fn test_load_for_virtue_filter() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("virtues");

        let d1 = NaiveDate::from_ymd_opt(2026, 6, 25).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2026, 6, 26).unwrap();

        let log1 = VirtueLog::new(VirtueDomain::Health, VirtueStatus::Kept, d1);
        let log2 = VirtueLog::new(VirtueDomain::Speech, VirtueStatus::Kept, d2);
        let log3 = VirtueLog::new(VirtueDomain::Health, VirtueStatus::Broken, d2);
        log1.save(&dir).unwrap();
        log2.save(&dir).unwrap();
        log3.save(&dir).unwrap();

        let health_logs = VirtueLog::load_for_virtue(&dir, VirtueDomain::Health).unwrap();
        assert_eq!(health_logs.len(), 2);

        let speech_logs = VirtueLog::load_for_virtue(&dir, VirtueDomain::Speech).unwrap();
        assert_eq!(speech_logs.len(), 1);
        assert_eq!(speech_logs[0].virtue, VirtueDomain::Speech);

        let empty_logs = VirtueLog::load_for_virtue(&dir, VirtueDomain::Order).unwrap();
        assert!(empty_logs.is_empty());
    }

    #[test]
    fn test_check_in_auto_streak() {
        let d1 = NaiveDate::from_ymd_opt(2026, 6, 25).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2026, 6, 26).unwrap();

        let log1 = VirtueLog::check_in(VirtueDomain::Health, VirtueStatus::Kept, d1, 0);
        assert_eq!(log1.streak, 1);

        let log2 = VirtueLog::check_in(VirtueDomain::Health, VirtueStatus::Kept, d2, log1.streak);
        assert_eq!(log2.streak, 2);

        let log3 = VirtueLog::check_in(VirtueDomain::Health, VirtueStatus::Broken, d2, log2.streak);
        assert_eq!(log3.streak, 0);

        let log4 = VirtueLog::check_in(VirtueDomain::Health, VirtueStatus::Partial, d2, log3.streak);
        assert_eq!(log4.streak, 0);
    }

    #[test]
    fn test_load_all_empty_dir() {
        let tmp = tempdir().unwrap();
        let logs = VirtueLog::load_all(tmp.path()).unwrap();
        assert!(logs.is_empty());
    }

    #[test]
    fn test_new_defaults() {
        let date = NaiveDate::from_ymd_opt(2026, 6, 26).unwrap();
        let log = VirtueLog::new(VirtueDomain::Order, VirtueStatus::Kept, date);
        assert!(log.id.starts_with("virtue-order-"));
        assert_eq!(log.streak, 1);
        assert_eq!(log.date, date);
        assert!(log.note.is_none());
    }

    #[test]
    fn test_to_markdown_format() {
        let date = NaiveDate::from_ymd_opt(2026, 6, 26).unwrap();
        let log = VirtueLog::new(VirtueDomain::Balance, VirtueStatus::Kept, date);
        let md = log.to_markdown();
        assert!(md.starts_with("---\n"));
        assert!(md.contains("virtue: balance"));
        assert!(md.contains("virtue_cn: \"中庸\""));
        assert!(md.contains("status: kept"));
        assert!(md.contains("date: 2026-06-26"));
        assert!(md.contains("# Virtue: 中庸 (balance)"));
    }
}
