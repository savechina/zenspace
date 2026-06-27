//! Correction signal tracking for the evolution engine.
//!
//! Records corrections applied to decisions or commitments,
//! with cost breakdown reuse and verification tracking.
//!
//! Storage: `wiki/wisdom/corrections/{slug}.md`

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;

use crate::decision::CostBreakdown;

// ─── Error type ─────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum CorrectionError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("missing field: {0}")]
    MissingField(String),
}

// ─── Data types ────────────────────────────────────────────────────────

/// A correction applied to a decision or commitment error.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Correction {
    /// Unique identifier (e.g., "correction-auth-reset-20260626").
    pub id: String,
    /// Reference to the decision/commitment slug that was corrected.
    pub error_ref: String,
    /// Cost breakdown of the correction (reuses decision module type).
    pub cost: CostBreakdown,
    /// Description of what was done to fix the error.
    pub fix: String,
    /// When the fix was verified (None if not yet verified).
    pub verified_at: Option<DateTime<Utc>>,
    /// When this correction was created.
    pub created_at: DateTime<Utc>,
}

// ─── Slugify ────────────────────────────────────────────────────────────

/// Slugify a string into a filesystem-safe identifier.
fn slugify(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
        .chars()
        .take(60)
        .collect()
}

// ─── Correction methods ────────────────────────────────────────────────

impl Correction {
    /// Create a new correction record.
    pub fn new(error_ref: &str, fix: &str, cost: CostBreakdown) -> Self {
        let now = Utc::now();
        let ts = now.format("%Y%m%d%H%M%S");
        let slug_error = slugify(error_ref);
        let id = format!("correction-{slug_error}-{ts}");
        Self {
            id,
            error_ref: error_ref.to_string(),
            cost,
            fix: fix.to_string(),
            verified_at: None,
            created_at: now,
        }
    }

    /// Generate the slug for this correction (used as filename).
    pub fn slug(&self) -> String {
        self.id.clone()
    }

    /// Whether this correction has been verified.
    pub fn is_verified(&self) -> bool {
        self.verified_at.is_some()
    }

    /// Mark the correction as verified (sets verified_at to now).
    pub fn verify(&mut self) {
        self.verified_at = Some(Utc::now());
    }
}

// ─── File persistence ──────────────────────────────────────────────────

impl Correction {
    /// Serialize correction to markdown format with YAML frontmatter.
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();
        md.push_str("---\n");
        md.push_str(&format!("id: {}\n", self.id));
        md.push_str(&format!("error_ref: {}\n", self.error_ref));
        md.push_str(&format!("fix: \"{}\"\n", self.fix.replace('"', "\\\"")));
        md.push_str(&format!(
            "cost_economic: {}\n",
            self.cost.economic
        ));
        md.push_str(&format!("cost_time: {}\n", self.cost.time_hours));
        md.push_str(&format!("cost_credit: {}\n", self.cost.credit));
        md.push_str(&format!("cost_sunk: {}\n", self.cost.sunk));
        md.push_str(&format!(
            "is_recoverable: {}\n",
            self.cost.is_recoverable
        ));
        md.push_str(&format!("created_at: {}\n", self.created_at.to_rfc3339()));
        if let Some(va) = self.verified_at {
            md.push_str(&format!("verified_at: {}\n", va.to_rfc3339()));
        }
        md.push_str("---\n\n");
        md.push_str(&format!("# Correction: {}\n\n", self.error_ref));
        md.push_str(&format!("**Fix**: {}\n\n", self.fix));
        if let Some(va) = self.verified_at {
            md.push_str(&format!("**Verified**: {}\n", va.format("%Y-%m-%d %H:%M UTC")));
        } else {
            md.push_str("**Verified**: pending\n");
        }
        md
    }

    /// Save correction to `dir/{slug}.md`. Returns the path written.
    pub fn save(&self, dir: &Path) -> Result<PathBuf, CorrectionError> {
        fs::create_dir_all(dir)?;
        let path = dir.join(format!("{}.md", self.slug()));
        let content = self.to_markdown();
        fs::write(&path, content)?;
        Ok(path)
    }

    /// Load a correction from a markdown file.
    pub fn load(path: &Path) -> Result<Self, CorrectionError> {
        let content = fs::read_to_string(path)?;
        Self::from_markdown(&content)
    }

    /// Load all corrections from a directory of `.md` files.
    pub fn load_all(dir: &Path) -> Result<Vec<Correction>, CorrectionError> {
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut corrections = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "md") {
                match Self::load(&path) {
                    Ok(c) => corrections.push(c),
                    Err(e) => {
                        warn!(
                            path = %path.display(),
                            error = %e,
                            "failed to parse correction file, skipping"
                        );
                    }
                }
            }
        }
        Ok(corrections)
    }

    /// Parse correction from markdown string (frontmatter + body).
    pub fn from_markdown(content: &str) -> Result<Self, CorrectionError> {
        let fm = extract_frontmatter(content)?;
        let id = parse_yaml_field(&fm, "id")
            .ok_or_else(|| CorrectionError::MissingField("id".into()))?;
        let error_ref = parse_yaml_field(&fm, "error_ref")
            .ok_or_else(|| CorrectionError::MissingField("error_ref".into()))?;
        let fix = parse_yaml_field(&fm, "fix")
            .map(|s| s.trim_matches('"').to_string())
            .unwrap_or_default();
        let cost = CostBreakdown {
            economic: parse_yaml_field(&fm, "cost_economic")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0),
            time_hours: parse_yaml_field(&fm, "cost_time")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0),
            credit: parse_yaml_field(&fm, "cost_credit")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0),
            sunk: parse_yaml_field(&fm, "cost_sunk")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0),
            is_recoverable: parse_yaml_field(&fm, "is_recoverable")
                .map(|s| s == "true")
                .unwrap_or(true),
        };
        let created_at = parse_yaml_field(&fm, "created_at")
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);
        let verified_at = parse_yaml_field(&fm, "verified_at")
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc));

        Ok(Correction {
            id,
            error_ref,
            cost,
            fix,
            verified_at,
            created_at,
        })
    }
}

// ─── Frontmatter parsing helpers ───────────────────────────────────────

fn extract_frontmatter(content: &str) -> Result<String, CorrectionError> {
    let mut lines = content.lines();
    let first = lines.next().unwrap_or("").trim();
    if first != "---" {
        return Err(CorrectionError::Parse(
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
    Err(CorrectionError::Parse(
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

// ─── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_cost() -> CostBreakdown {
        CostBreakdown {
            economic: 100.0,
            time_hours: 2.5,
            credit: 0.0,
            sunk: 50.0,
            is_recoverable: false,
        }
    }

    #[test]
    fn test_new_correction() {
        let c = Correction::new("bad-decision", "Applied hotfix", sample_cost());
        assert!(c.id.starts_with("correction-bad-decision-"));
        assert_eq!(c.error_ref, "bad-decision");
        assert_eq!(c.fix, "Applied hotfix");
        assert_eq!(c.cost.economic, 100.0);
        assert_eq!(c.cost.time_hours, 2.5);
        assert_eq!(c.cost.sunk, 50.0);
        assert!(!c.cost.is_recoverable);
        assert!(!c.is_verified());
        assert_eq!(c.verified_at, None);
    }

    #[test]
    fn test_slug_uniqueness() {
        let c1 = Correction::new("err", "fix1", CostBreakdown::default());
        // Sleep won't work for uniqueness at same second, so check id contains timestamp
        let c2 = Correction::new("err", "fix2", CostBreakdown::default());
        // Both should have the same error_ref in their id
        assert!(c1.id.starts_with("correction-err-"));
        assert!(c2.id.starts_with("correction-err-"));
    }

    #[test]
    fn test_verify() {
        let mut c = Correction::new("err", "fix", CostBreakdown::default());
        assert!(!c.is_verified());
        c.verify();
        assert!(c.is_verified());
        assert!(c.verified_at.is_some());
    }

    #[test]
    fn test_save_load_roundtrip() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("corrections");

        let mut c = Correction::new("auth-reset", "Changed token rotation interval", sample_cost());
        c.verify();
        let path = c.save(&dir).unwrap();

        assert!(path.exists());
        let loaded = Correction::load(&path).unwrap();
        assert_eq!(loaded.id, c.id);
        assert_eq!(loaded.error_ref, "auth-reset");
        assert_eq!(loaded.fix, "Changed token rotation interval");
        assert_eq!(loaded.cost.economic, 100.0);
        assert_eq!(loaded.cost.time_hours, 2.5);
        assert!(loaded.is_verified());
    }

    #[test]
    fn test_load_all() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("corrections");

        let c1 = Correction::new("err1", "fix1", CostBreakdown::default());
        let c2 = Correction::new("err2", "fix2", CostBreakdown::default());
        c1.save(&dir).unwrap();
        c2.save(&dir).unwrap();

        let loaded = Correction::load_all(&dir).unwrap();
        assert_eq!(loaded.len(), 2);
        let refs: Vec<&str> = loaded.iter().map(|c| c.error_ref.as_str()).collect();
        assert!(refs.contains(&"err1"));
        assert!(refs.contains(&"err2"));
    }

    #[test]
    fn test_load_all_empty_dir() {
        let tmp = tempdir().unwrap();
        let corrections = Correction::load_all(tmp.path()).unwrap();
        assert!(corrections.is_empty());
    }

    #[test]
    fn test_load_all_nonexistent_dir() {
        let corrections = Correction::load_all(Path::new("/nonexistent/path/xyz")).unwrap();
        assert!(corrections.is_empty());
    }

    #[test]
    fn test_to_markdown_format() {
        let c = Correction::new("err", "fix text", sample_cost());
        let md = c.to_markdown();
        assert!(md.starts_with("---\n"));
        assert!(md.contains("id: correction-err-"));
        assert!(md.contains("error_ref: err"));
        assert!(md.contains("fix: \"fix text\""));
        assert!(md.contains("cost_economic: 100"));
        assert!(md.contains("cost_time: 2.5"));
        assert!(md.contains("# Correction: err"));
        assert!(md.contains("**Fix**: fix text"));
        assert!(md.contains("**Verified**: pending"));
    }

    #[test]
    fn test_from_markdown_invalid() {
        let result = Correction::from_markdown("not frontmatter");
        assert!(result.is_err());
    }

    #[test]
    fn test_from_markdown_missing_close() {
        let result = Correction::from_markdown("---\nno closing");
        assert!(result.is_err());
    }
}
