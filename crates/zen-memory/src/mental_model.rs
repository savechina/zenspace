use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;

use crate::frontmatter::{extract_frontmatter, parse_field};

// ─── Error type ─────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum MentalModelSignalError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("missing field: {0}")]
    MissingField(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MentalModelSignal {
    pub model: String,
    pub domain: String,
    pub application: String,
    pub source: String,
}

impl MentalModelSignal {
    pub fn slug(&self) -> String {
        self.model
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("-")
    }

    pub fn to_markdown(&self) -> String {
        let mut md = String::new();
        md.push_str("---\n");
        md.push_str(&format!("model: \"{}\"\n", self.model.replace('"', "\\\"")));
        md.push_str(&format!("domain: {}\n", self.domain));
        md.push_str(&format!(
            "source: \"{}\"\n",
            self.source.replace('"', "\\\"")
        ));
        md.push_str("---\n\n");
        md.push_str(&format!("# {}\n\n", self.model));
        md.push_str(&format!("**Application**: {}\n", self.application));
        md
    }

    /// Save signal to `dir/{slug}.md`. Returns the path written.
    pub fn save(&self, dir: &Path) -> Result<PathBuf, MentalModelSignalError> {
        fs::create_dir_all(dir)?;
        let path = dir.join(format!("{}.md", self.slug()));
        let content = self.to_markdown();
        fs::write(&path, content)?;
        Ok(path)
    }

    /// Load a signal from a markdown file.
    pub fn load(path: &Path) -> Result<Self, MentalModelSignalError> {
        let content = fs::read_to_string(path)?;
        Self::from_markdown(&content).ok_or_else(|| {
            MentalModelSignalError::Parse("failed to parse mental model signal".into())
        })
    }

    /// Load all signals from a directory of `.md` files.
    pub fn load_all(dir: &Path) -> Result<Vec<MentalModelSignal>, MentalModelSignalError> {
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
                            "failed to parse mental model signal, skipping"
                        );
                    }
                }
            }
        }
        Ok(signals)
    }

    pub fn from_markdown(content: &str) -> Option<Self> {
        let fm = extract_frontmatter(content)?;
        let model = parse_field(&fm, "model")?;
        let domain = parse_field(&fm, "domain").unwrap_or_default();
        let source = parse_field(&fm, "source").unwrap_or_default();
        let application = extract_application(content).unwrap_or_default();
        Some(Self {
            model: model.trim_matches('"').to_string(),
            domain,
            application,
            source: source.trim_matches('"').to_string(),
        })
    }
}

fn extract_application(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("**Application**:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slug() {
        let mm = MentalModelSignal {
            model: "First Principles".into(),
            domain: "reasoning".into(),
            application: "break down to fundamentals".into(),
            source: "Aristotle".into(),
        };
        assert_eq!(mm.slug(), "first-principles");
    }

    #[test]
    fn test_roundtrip() {
        let mm = MentalModelSignal {
            model: "Inversion".into(),
            domain: "problem-solving".into(),
            application: "think backwards from failure".into(),
            source: "Charlie Munger".into(),
        };
        let md = mm.to_markdown();
        let parsed = MentalModelSignal::from_markdown(&md).unwrap();
        assert_eq!(parsed.model, "Inversion");
        assert_eq!(parsed.domain, "problem-solving");
        assert_eq!(parsed.application, "think backwards from failure");
        assert_eq!(parsed.source, "Charlie Munger");
    }
}
