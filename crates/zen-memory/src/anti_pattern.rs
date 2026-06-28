use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;

// ─── Error type ─────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum AntiPatternSignalError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("missing field: {0}")]
    MissingField(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AntiPatternSignal {
    pub pattern: String,
    pub trigger: String,
    pub avoidance: String,
    pub detected_in: Vec<String>,
}

impl AntiPatternSignal {
    pub fn slug(&self) -> String {
        self.pattern
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("-")
    }

    pub fn to_markdown(&self) -> String {
        let mut md = String::new();
        md.push_str("---\n");
        md.push_str(&format!(
            "pattern: \"{}\"\n",
            self.pattern.replace('"', "\\\"")
        ));
        md.push_str(&format!(
            "trigger: \"{}\"\n",
            self.trigger.replace('"', "\\\"")
        ));
        md.push_str(&format!(
            "avoidance: \"{}\"\n",
            self.avoidance.replace('"', "\\\"")
        ));
        if !self.detected_in.is_empty() {
            md.push_str("detected_in:\n");
            for r in &self.detected_in {
                md.push_str(&format!("  - {r}\n"));
            }
        }
        md.push_str("---\n\n");
        md.push_str(&format!("# {}\n\n", self.pattern));
        md.push_str(&format!("**Trigger**: {}\n\n", self.trigger));
        md.push_str(&format!("**Avoidance**: {}\n", self.avoidance));
        md
    }

    /// Save signal to `dir/{slug}.md`. Returns the path written.
    pub fn save(&self, dir: &Path) -> Result<PathBuf, AntiPatternSignalError> {
        fs::create_dir_all(dir)?;
        let path = dir.join(format!("{}.md", self.slug()));
        let content = self.to_markdown();
        fs::write(&path, content)?;
        Ok(path)
    }

    /// Load a signal from a markdown file.
    pub fn load(path: &Path) -> Result<Self, AntiPatternSignalError> {
        let content = fs::read_to_string(path)?;
        Self::from_markdown(&content).ok_or_else(|| {
            AntiPatternSignalError::Parse("failed to parse anti-pattern signal".into())
        })
    }

    /// Load all signals from a directory of `.md` files.
    pub fn load_all(dir: &Path) -> Result<Vec<AntiPatternSignal>, AntiPatternSignalError> {
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut signals = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "md") {
                match Self::load(&path) {
                    Ok(s) => signals.push(s),
                    Err(e) => {
                        warn!(
                            path = %path.display(),
                            error = %e,
                            "failed to parse anti-pattern signal, skipping"
                        );
                    }
                }
            }
        }
        Ok(signals)
    }

    pub fn from_markdown(content: &str) -> Option<Self> {
        let fm = extract_frontmatter(content)?;
        let pattern = parse_field(&fm, "pattern")?;
        let trigger = parse_field(&fm, "trigger").unwrap_or_default();
        let avoidance = parse_field(&fm, "avoidance").unwrap_or_default();
        let detected_in = parse_list_field(&fm, "detected_in");
        Some(Self {
            pattern: pattern.trim_matches('"').to_string(),
            trigger: trigger.trim_matches('"').to_string(),
            avoidance: avoidance.trim_matches('"').to_string(),
            detected_in,
        })
    }
}

fn extract_frontmatter(content: &str) -> Option<String> {
    let mut lines = content.lines();
    let first = lines.next()?.trim();
    if first != "---" {
        return None;
    }
    let mut fm = String::new();
    for line in lines {
        if line.trim() == "---" {
            return Some(fm);
        }
        fm.push_str(line);
        fm.push('\n');
    }
    None
}

fn parse_field(frontmatter: &str, key: &str) -> Option<String> {
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

fn parse_list_field(frontmatter: &str, key: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut in_list = false;
    for line in frontmatter.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(&format!("{key}:")) {
            in_list = true;
            continue;
        }
        if in_list {
            if let Some(item) = trimmed.strip_prefix("- ") {
                items.push(item.trim().to_string());
            } else if !trimmed.is_empty() && !trimmed.starts_with('-') {
                break;
            }
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slug() {
        let ap = AntiPatternSignal {
            pattern: "Sunk Cost Fallacy".into(),
            trigger: "past investment".into(),
            avoidance: "evaluate fresh".into(),
            detected_in: vec![],
        };
        assert_eq!(ap.slug(), "sunk-cost-fallacy");
    }

    #[test]
    fn test_roundtrip() {
        let ap = AntiPatternSignal {
            pattern: "Loss Aversion".into(),
            trigger: "fear of loss".into(),
            avoidance: "frame as gain".into(),
            detected_in: vec!["decision-001".into()],
        };
        let md = ap.to_markdown();
        let parsed = AntiPatternSignal::from_markdown(&md).unwrap();
        assert_eq!(parsed.pattern, "Loss Aversion");
        assert_eq!(parsed.trigger, "fear of loss");
        assert_eq!(parsed.detected_in, vec!["decision-001"]);
    }
}
