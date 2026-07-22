use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;

use crate::frontmatter::{extract_frontmatter, parse_field};

#[derive(Debug, Error)]
pub enum FeedbackError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("missing field: {0}")]
    MissingField(String),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FeedbackProperties {
    pub timely: bool,
    pub reasonable: bool,
    pub actionable: bool,
    pub constructive: bool,
    pub interactive: bool,
}

impl FeedbackProperties {
    pub fn count_true(&self) -> u32 {
        [
            self.timely,
            self.reasonable,
            self.actionable,
            self.constructive,
            self.interactive,
        ]
        .iter()
        .filter(|&&b| b)
        .count() as u32
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackDisposition {
    Accepted,
    Rejected,
    Partial,
    Pending,
}

impl fmt::Display for FeedbackDisposition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            FeedbackDisposition::Accepted => "accepted",
            FeedbackDisposition::Rejected => "rejected",
            FeedbackDisposition::Partial => "partial",
            FeedbackDisposition::Pending => "pending",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Feedback {
    pub id: String,
    pub source: String,
    pub content: String,
    pub properties: FeedbackProperties,
    pub disposition: FeedbackDisposition,
    pub created_at: DateTime<Utc>,
}

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

impl Feedback {
    pub fn new(source: &str, content: &str) -> Self {
        let now = Utc::now();
        let ts = now.format("%Y%m%d%H%M%S");
        let slug_source = slugify(source);
        let id = format!("feedback-{slug_source}-{ts}");
        Self {
            id,
            source: source.to_string(),
            content: content.to_string(),
            properties: FeedbackProperties::default(),
            disposition: FeedbackDisposition::Pending,
            created_at: now,
        }
    }

    pub fn slug(&self) -> String {
        self.id.clone()
    }

    /// Quality score 0.0–1.0 based on how many properties are true (5 trues = 1.0).
    pub fn quality_score(&self) -> f64 {
        self.properties.count_true() as f64 / 5.0
    }

    /// Whether quality_score >= 0.6 (at least 3 of 5 properties true).
    pub fn is_high_quality(&self) -> bool {
        self.quality_score() >= 0.6
    }
}

impl Feedback {
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();
        md.push_str("---\n");
        md.push_str(&format!("id: {}\n", self.id));
        md.push_str(&format!(
            "source: \"{}\"\n",
            self.source.replace('"', "\\\"")
        ));
        md.push_str(&format!(
            "content: \"{}\"\n",
            self.content.replace('"', "\\\"")
        ));
        md.push_str(&format!("disposition: {}\n", self.disposition));
        md.push_str(&format!("timely: {}\n", self.properties.timely));
        md.push_str(&format!("reasonable: {}\n", self.properties.reasonable));
        md.push_str(&format!("actionable: {}\n", self.properties.actionable));
        md.push_str(&format!("constructive: {}\n", self.properties.constructive));
        md.push_str(&format!("interactive: {}\n", self.properties.interactive));
        md.push_str(&format!("created_at: {}\n", self.created_at.to_rfc3339()));
        md.push_str("---\n\n");
        md.push_str(&format!("# Feedback from {}\n\n", self.source));
        md.push_str(&format!("{}\n\n", self.content));
        md.push_str(&format!("**Disposition**: {}\n", self.disposition));
        md.push_str(&format!(
            "**Quality**: {:.0}%\n",
            self.quality_score() * 100.0
        ));
        md
    }

    pub fn save(&self, dir: &Path) -> Result<PathBuf, FeedbackError> {
        fs::create_dir_all(dir)?;
        let path = dir.join(format!("{}.md", self.slug()));
        let content = self.to_markdown();
        fs::write(&path, content)?;
        Ok(path)
    }

    pub fn load(path: &Path) -> Result<Self, FeedbackError> {
        let content = fs::read_to_string(path)?;
        Self::from_markdown(&content)
    }

    pub fn load_all(dir: &Path) -> Result<Vec<Feedback>, FeedbackError> {
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut feedbacks = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "md") {
                match Self::load(&path) {
                    Ok(f) => feedbacks.push(f),
                    Err(e) => {
                        warn!(
                            path = %path.display(),
                            error = %e,
                            "failed to parse feedback file, skipping"
                        );
                    }
                }
            }
        }
        Ok(feedbacks)
    }

    pub fn from_markdown(content: &str) -> Result<Self, FeedbackError> {
        let fm = extract_frontmatter(content)
            .ok_or_else(|| FeedbackError::Parse("missing frontmatter".into()))?;
        let id =
            parse_field(&fm, "id").ok_or_else(|| FeedbackError::MissingField("id".into()))?;
        let source = parse_field(&fm, "source")
            .map(|s| s.trim_matches('"').to_string())
            .unwrap_or_default();
        let content_str = parse_field(&fm, "content")
            .map(|s| s.trim_matches('"').to_string())
            .unwrap_or_default();
        let disposition = parse_field(&fm, "disposition")
            .map(|s| match s.as_str() {
                "accepted" => FeedbackDisposition::Accepted,
                "rejected" => FeedbackDisposition::Rejected,
                "partial" => FeedbackDisposition::Partial,
                _ => FeedbackDisposition::Pending,
            })
            .unwrap_or(FeedbackDisposition::Pending);
        let properties = FeedbackProperties {
            timely: parse_field(&fm, "timely")
                .map(|s| s == "true")
                .unwrap_or(false),
            reasonable: parse_field(&fm, "reasonable")
                .map(|s| s == "true")
                .unwrap_or(false),
            actionable: parse_field(&fm, "actionable")
                .map(|s| s == "true")
                .unwrap_or(false),
            constructive: parse_field(&fm, "constructive")
                .map(|s| s == "true")
                .unwrap_or(false),
            interactive: parse_field(&fm, "interactive")
                .map(|s| s == "true")
                .unwrap_or(false),
        };
        let created_at = parse_field(&fm, "created_at")
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        Ok(Feedback {
            id,
            source,
            content: content_str,
            properties,
            disposition,
            created_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_new_feedback() {
        let f = Feedback::new("mentor", "Great work on the refactor");
        assert!(f.id.starts_with("feedback-mentor-"));
        assert_eq!(f.source, "mentor");
        assert_eq!(f.content, "Great work on the refactor");
        assert_eq!(f.disposition, FeedbackDisposition::Pending);
        assert_eq!(f.properties, FeedbackProperties::default());
    }

    #[test]
    fn test_quality_score_all_true() {
        let mut f = Feedback::new("a", "b");
        f.properties = FeedbackProperties {
            timely: true,
            reasonable: true,
            actionable: true,
            constructive: true,
            interactive: true,
        };
        assert!((f.quality_score() - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_quality_score_partial() {
        let mut f = Feedback::new("a", "b");
        f.properties = FeedbackProperties {
            timely: true,
            reasonable: true,
            actionable: false,
            constructive: false,
            interactive: false,
        };
        assert!((f.quality_score() - 0.4).abs() < 0.001);
        assert!(!f.is_high_quality());
    }

    #[test]
    fn test_high_quality_threshold() {
        let mut f = Feedback::new("a", "b");
        f.properties = FeedbackProperties {
            timely: true,
            reasonable: true,
            actionable: true,
            constructive: false,
            interactive: false,
        };
        assert!((f.quality_score() - 0.6).abs() < 0.001);
        assert!(f.is_high_quality());

        let mut f2 = Feedback::new("a", "b");
        f2.properties = FeedbackProperties {
            timely: true,
            reasonable: true,
            actionable: false,
            constructive: false,
            interactive: false,
        };
        assert!(!f2.is_high_quality());
    }

    #[test]
    fn test_save_load_roundtrip() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("feedback");

        let mut f = Feedback::new("peer-review", "Needs better error handling");
        f.properties.timely = true;
        f.properties.actionable = true;
        f.disposition = FeedbackDisposition::Accepted;
        let path = f.save(&dir).unwrap();

        assert!(path.exists());
        let loaded = Feedback::load(&path).unwrap();
        assert_eq!(loaded.id, f.id);
        assert_eq!(loaded.source, "peer-review");
        assert_eq!(loaded.content, "Needs better error handling");
        assert!(loaded.properties.timely);
        assert!(loaded.properties.actionable);
        assert!(!loaded.properties.constructive);
        assert_eq!(loaded.disposition, FeedbackDisposition::Accepted);
    }

    #[test]
    fn test_load_all() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("feedback");

        let f1 = Feedback::new("s1", "c1");
        let f2 = Feedback::new("s2", "c2");
        f1.save(&dir).unwrap();
        f2.save(&dir).unwrap();

        let loaded = Feedback::load_all(&dir).unwrap();
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn test_disposition_display() {
        assert_eq!(FeedbackDisposition::Accepted.to_string(), "accepted");
        assert_eq!(FeedbackDisposition::Rejected.to_string(), "rejected");
        assert_eq!(FeedbackDisposition::Partial.to_string(), "partial");
        assert_eq!(FeedbackDisposition::Pending.to_string(), "pending");
    }

    #[test]
    fn test_load_all_empty_dir() {
        let tmp = tempdir().unwrap();
        let feedbacks = Feedback::load_all(tmp.path()).unwrap();
        assert!(feedbacks.is_empty());
    }

    #[test]
    fn test_to_markdown_format() {
        let f = Feedback::new("mentor", "Good insight");
        let md = f.to_markdown();
        assert!(md.starts_with("---\n"));
        assert!(md.contains("id: feedback-mentor-"));
        assert!(md.contains("disposition: pending"));
        assert!(md.contains("timely: false"));
        assert!(md.contains("# Feedback from mentor"));
        assert!(md.contains("Good insight"));
        assert!(md.contains("**Quality**: 0%"));
    }

    #[test]
    fn test_from_markdown_invalid() {
        let result = Feedback::from_markdown("not frontmatter");
        assert!(result.is_err());
    }
}
