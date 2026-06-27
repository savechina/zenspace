use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;

use crate::decision::Severity;
use crate::virtue_log::VirtueDomain;

// ─── Error type ─────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum ReflectionSignalError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("missing field: {0}")]
    MissingField(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReflectionSignal {
    pub what_wrong: String,
    pub why: String,
    pub severity: Severity,
    pub domain: VirtueDomain,
}

impl ReflectionSignal {
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();
        md.push_str("---\n");
        md.push_str(&format!("severity: {:?}\n", self.severity));
        md.push_str(&format!("domain: {}\n", serde_json::to_string(&self.domain).unwrap_or_default().trim_matches('"')));
        md.push_str("---\n\n");
        md.push_str("## What went wrong\n\n");
        md.push_str(&format!("{}\n\n", self.what_wrong));
        md.push_str("## Why\n\n");
        md.push_str(&format!("{}\n", self.why));
        md
    }

    pub fn slug(&self) -> String {
        self.what_wrong
            .to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect::<String>()
            .trim_matches('-')
            .to_string()
            .chars()
            .take(40)
            .collect()
    }

    /// Save signal to `dir/{slug}.md`. Returns the path written.
    pub fn save(&self, dir: &Path) -> Result<PathBuf, ReflectionSignalError> {
        fs::create_dir_all(dir)?;
        let path = dir.join(format!("{}.md", self.slug()));
        let content = self.to_markdown();
        fs::write(&path, content)?;
        Ok(path)
    }

    /// Load a signal from a markdown file.
    pub fn load(path: &Path) -> Result<Self, ReflectionSignalError> {
        let content = fs::read_to_string(path)?;
        Self::from_markdown(&content).ok_or_else(|| {
            ReflectionSignalError::Parse("failed to parse reflection signal".into())
        })
    }

    /// Load all signals from a directory of `.md` files.
    pub fn load_all(dir: &Path) -> Result<Vec<ReflectionSignal>, ReflectionSignalError> {
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
                            "failed to parse reflection signal, skipping"
                        );
                    }
                }
            }
        }
        Ok(signals)
    }

    pub fn from_markdown(content: &str) -> Option<Self> {
        let fm = extract_frontmatter(content)?;
        let severity = parse_field(&fm, "severity")?;
        let domain = parse_field(&fm, "domain")?;
        let what_wrong = extract_section(content, "## What went wrong").unwrap_or_default();
        let why = extract_section(content, "## Why").unwrap_or_default();

        let severity = match severity.to_lowercase().as_str() {
            "crit" => Severity::Crit,
            "high" => Severity::High,
            "med" => Severity::Med,
            _ => return None,
        };

        let domain = match domain.to_lowercase().as_str() {
            "health" => VirtueDomain::Health,
            "speech" => VirtueDomain::Speech,
            "order" => VirtueDomain::Order,
            "resolution" => VirtueDomain::Resolution,
            "diligence" => VirtueDomain::Diligence,
            "balance" => VirtueDomain::Balance,
            "tranquility" => VirtueDomain::Tranquility,
            _ => return None,
        };

        Some(Self {
            what_wrong,
            why,
            severity,
            domain,
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

fn extract_section(content: &str, header: &str) -> Option<String> {
    let lines = content.lines();
    let mut found = false;
    let mut text = String::new();
    for line in lines {
        if line.trim() == header {
            found = true;
            continue;
        }
        if found {
            if line.starts_with("## ") {
                break;
            }
            if !text.is_empty() || !line.trim().is_empty() {
                text.push_str(line);
                text.push('\n');
            }
        }
    }
    if found {
        Some(text.trim().to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip() {
        let r = ReflectionSignal {
            what_wrong: "Ignored warning signs".into(),
            why: "Overconfidence in initial plan".into(),
            severity: Severity::High,
            domain: VirtueDomain::Resolution,
        };
        let md = r.to_markdown();
        let parsed = ReflectionSignal::from_markdown(&md).unwrap();
        assert_eq!(parsed.what_wrong, "Ignored warning signs");
        assert_eq!(parsed.why, "Overconfidence in initial plan");
        assert_eq!(parsed.severity, Severity::High);
        assert_eq!(parsed.domain, VirtueDomain::Resolution);
    }
}
