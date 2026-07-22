//! Fact representation for extracted knowledge.
//!
//! Facts are the core knowledge type in the evolution engine,
//! representing atomic pieces of extracted knowledge with
//! associated notions and source tracking.
//!
//! Storage: `wiki/wisdom/facts/{id}.md`

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;
use uuid::Uuid;

use crate::frontmatter::{extract_frontmatter, parse_field, parse_yaml_array};

// ─── Error type ─────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum FactError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("missing field: {0}")]
    MissingField(String),
}

// ─── Data types ────────────────────────────────────────────────────────

/// An atomic piece of extracted knowledge with notion associations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Fact {
    /// Unique identifier (UUID v4).
    pub id: String,
    /// The factual content/knowledge statement.
    pub what: String,
    /// When this fact was recorded.
    pub when: DateTime<Utc>,
    /// Entities associated with this fact.
    pub notions: Vec<String>,
    /// Source of the fact (e.g., "consolidation", "manual", file path).
    pub source: String,
}

// ─── Fact methods ──────────────────────────────────────────────────────

impl Fact {
    /// Create a new fact with auto-generated UUID.
    pub fn new(what: &str, source: &str, notions: Vec<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            what: what.to_string(),
            when: Utc::now(),
            notions,
            source: source.to_string(),
        }
    }

    /// Generate the slug for this fact (used as filename).
    pub fn slug(&self) -> String {
        self.id.clone()
    }
}

// ─── File persistence ──────────────────────────────────────────────────

impl Fact {
    /// Serialize fact to markdown format with YAML frontmatter.
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();
        md.push_str("---\n");
        md.push_str(&format!("id: {}\n", self.id));
        md.push_str(&format!("what: \"{}\"\n", self.what.replace('"', "\\\"")));
        md.push_str(&format!("when: {}\n", self.when.to_rfc3339()));
        md.push_str(&format!(
            "source: \"{}\"\n",
            self.source.replace('"', "\\\"")
        ));
        if !self.notions.is_empty() {
            md.push_str("notions:\n");
            for e in &self.notions {
                md.push_str(&format!("  - {e}\n"));
            }
        }
        md.push_str("---\n\n");
        md.push_str("# Fact\n\n");
        md.push_str(&format!("**What**: {}\n\n", self.what));
        md.push_str(&format!(
            "**When**: {}\n",
            self.when.format("%Y-%m-%d %H:%M UTC")
        ));
        md.push_str(&format!("**Source**: {}\n", self.source));
        if !self.notions.is_empty() {
            md.push_str(&format!("**Entities**: {}\n", self.notions.join(", ")));
        }
        md
    }

    /// Save fact to `dir/{slug}.md`. Returns the path written.
    pub fn save(&self, dir: &Path) -> Result<PathBuf, FactError> {
        fs::create_dir_all(dir)?;
        let path = dir.join(format!("{}.md", self.slug()));
        let content = self.to_markdown();
        fs::write(&path, content)?;
        Ok(path)
    }

    /// Load a fact from a markdown file.
    pub fn load(path: &Path) -> Result<Self, FactError> {
        let content = fs::read_to_string(path)?;
        Self::from_markdown(&content)
    }

    /// Load all facts from a directory of `.md` files.
    pub fn load_all(dir: &Path) -> Result<Vec<Fact>, FactError> {
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut facts = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "md") {
                match Self::load(&path) {
                    Ok(f) => facts.push(f),
                    Err(e) => {
                        warn!(
                            path = %path.display(),
                            error = %e,
                            "failed to parse fact file, skipping"
                        );
                    }
                }
            }
        }
        Ok(facts)
    }

    /// Parse fact from markdown string (frontmatter + body).
    pub fn from_markdown(content: &str) -> Result<Self, FactError> {
        let fm = extract_frontmatter(content)
            .ok_or_else(|| FactError::Parse("missing frontmatter".into()))?;
        let id = parse_field(&fm, "id").ok_or_else(|| FactError::MissingField("id".into()))?;
        let what = parse_field(&fm, "what")
            .map(|s| s.trim_matches('"').to_string())
            .ok_or_else(|| FactError::MissingField("what".into()))?;
        let source = parse_field(&fm, "source")
            .map(|s| s.trim_matches('"').to_string())
            .unwrap_or_default();
        let when = parse_field(&fm, "when")
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);
        let notions = parse_yaml_array(&fm, "notions");

        Ok(Fact {
            id,
            what,
            when,
            notions,
            source,
        })
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_fact() -> Fact {
        Fact::new(
            "The Rust compiler uses LLVM as its backend",
            "consolidation",
            vec!["rust".into(), "llvm".into(), "compiler".into()],
        )
    }

    #[test]
    fn test_new_fact() {
        let f = sample_fact();
        assert!(!f.id.is_empty());
        assert_eq!(f.what, "The Rust compiler uses LLVM as its backend");
        assert_eq!(f.source, "consolidation");
        assert_eq!(f.notions.len(), 3);
        assert!(f.notions.contains(&"rust".to_string()));
    }

    #[test]
    fn test_to_markdown_format() {
        let f = sample_fact();
        let md = f.to_markdown();
        assert!(md.starts_with("---\n"));
        assert!(md.contains(&format!("id: {}", f.id)));
        assert!(md.contains("what: \"The Rust compiler uses LLVM as its backend\""));
        assert!(md.contains("source: \"consolidation\""));
        assert!(md.contains("  - rust"));
        assert!(md.contains("  - llvm"));
        assert!(md.contains("  - compiler"));
        assert!(md.contains("# Fact"));
        assert!(md.contains("**What**: The Rust compiler uses LLVM as its backend"));
        assert!(md.contains("**Source**: consolidation"));
        assert!(md.contains("**Entities**: rust, llvm, compiler"));
    }

    #[test]
    fn test_roundtrip() {
        let f = sample_fact();
        let md = f.to_markdown();
        let parsed = Fact::from_markdown(&md).unwrap();
        assert_eq!(parsed.id, f.id);
        assert_eq!(parsed.what, f.what);
        assert_eq!(parsed.source, f.source);
        assert_eq!(parsed.notions, f.notions);
    }

    #[test]
    fn test_save_load_roundtrip() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("facts");

        let f = sample_fact();
        let path = f.save(&dir).unwrap();
        assert!(path.exists());

        let loaded = Fact::load(&path).unwrap();
        assert_eq!(loaded.id, f.id);
        assert_eq!(loaded.what, f.what);
        assert_eq!(loaded.source, f.source);
        assert_eq!(loaded.notions, f.notions);
    }

    #[test]
    fn test_load_all() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("facts");

        let f1 = Fact::new("fact one", "test", vec![]);
        let f2 = Fact::new("fact two", "test", vec!["notion".into()]);
        f1.save(&dir).unwrap();
        f2.save(&dir).unwrap();

        let loaded = Fact::load_all(&dir).unwrap();
        assert_eq!(loaded.len(), 2);
        let whats: Vec<&str> = loaded.iter().map(|f| f.what.as_str()).collect();
        assert!(whats.contains(&"fact one"));
        assert!(whats.contains(&"fact two"));
    }

    #[test]
    fn test_load_all_empty_dir() {
        let tmp = tempdir().unwrap();
        let facts = Fact::load_all(tmp.path()).unwrap();
        assert!(facts.is_empty());
    }

    #[test]
    fn test_load_all_nonexistent_dir() {
        let facts = Fact::load_all(Path::new("/nonexistent/path/xyz")).unwrap();
        assert!(facts.is_empty());
    }

    #[test]
    fn test_from_markdown_invalid() {
        let result = Fact::from_markdown("not frontmatter");
        assert!(result.is_err());
    }

    #[test]
    fn test_from_markdown_missing_close() {
        let result = Fact::from_markdown("---\nno closing");
        assert!(result.is_err());
    }

    #[test]
    fn test_slug() {
        let f = sample_fact();
        assert_eq!(f.slug(), f.id);
    }
}
