pub mod writer;
pub use writer::AtomicWikiWriter;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

/// A wiki page in the knowledge base.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WikiPage {
    pub title: String,
    pub path: PathBuf,
    pub created_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
    pub tags: Vec<String>,
    pub wikilinks: Vec<String>,
    pub para: Option<String>,
    pub okf_type: Option<String>,
    pub content: String,
}

impl WikiPage {
    /// Extract wikilinks from content — scans for `[[...]]` patterns.
    pub fn extract_wikilinks(content: &str) -> Vec<String> {
        let mut links = Vec::new();
        let mut chars = content.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '[' && chars.peek() == Some(&'[') {
                chars.next();
                let mut link = String::new();
                while let Some(c) = chars.next() {
                    if c == ']' {
                        if chars.peek() == Some(&']') {
                            chars.next();
                            if !link.is_empty() {
                                links.push(link);
                            }
                            break;
                        }
                    } else {
                        link.push(c);
                    }
                }
            }
        }
        links
    }
}

/// Directory structure for the wiki.
pub struct WikiStructure {
    base_dir: PathBuf,
}

impl WikiStructure {
    pub fn new(base_dir: &Path) -> Self {
        Self {
            base_dir: base_dir.to_path_buf(),
        }
    }

    /// Create all wiki subdirectories.
    pub fn ensure_directories(&self) -> Result<()> {
        let dirs = [
            "sources", "notions/technology", "notions/concepts", "coding", "research", "reports",
            "topics",
        ];
        for dir in &dirs {
            std::fs::create_dir_all(self.base_dir.join(dir))
                .with_context(|| format!("failed to create wiki directory: {}", dir))?;
        }
        Ok(())
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }
}

/// Generator for wiki/index.md — central content catalog.
pub struct WikiIndex {
    base_dir: PathBuf,
}

impl WikiIndex {
    pub fn new(base_dir: &Path) -> Self {
        Self {
            base_dir: base_dir.to_path_buf(),
        }
    }

    /// Generate index.md from a list of wiki pages.
    pub fn generate(&self, pages: &[WikiPage]) -> Result<PathBuf> {
        let mut content = String::from("# Knowledge Index\n\n");

        // Group pages by parent directory
        let mut grouped: std::collections::BTreeMap<String, Vec<&WikiPage>> =
            std::collections::BTreeMap::new();
        for page in pages {
            let category = page
                .path
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "root".to_string());
            grouped.entry(category).or_default().push(page);
        }

        for (category, cat_pages) in &grouped {
            content.push_str(&format!("## {category}\n\n"));
            for page in cat_pages {
                let rel = page.path.strip_prefix(&self.base_dir).unwrap_or(&page.path);
                content.push_str(&format!("- [{}]({})\n", page.title, rel.display()));
            }
            content.push('\n');
        }

        let index_path = self.base_dir.join("index.md");
        std::fs::write(&index_path, &content)
            .with_context(|| format!("failed to write index: {}", index_path.display()))?;
        Ok(index_path)
    }
}

/// Append-only operation log for the wiki.
pub struct WikiLog {
    log_path: PathBuf,
}

impl WikiLog {
    pub fn new(base_dir: &Path) -> Self {
        Self {
            log_path: base_dir.join("log.md"),
        }
    }

    /// Append a timestamped operation entry.
    pub fn append(&self, operation: &str, details: &str) -> Result<()> {
        let entry = format!(
            "- [{}] {}: {}\n",
            Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
            operation,
            details
        );
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
            .with_context(|| format!("failed to open log: {}", self.log_path.display()))?
            .write_all(entry.as_bytes())
            .with_context(|| format!("failed to write log entry: {}", self.log_path.display()))?;
        Ok(())
    }
}

use std::io::Write;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_wikilinks_single() {
        let content = "See [[Rust]] for details.";
        let links = WikiPage::extract_wikilinks(content);
        assert_eq!(links, vec!["Rust"]);
    }

    #[test]
    fn test_extract_wikilinks_multiple() {
        let content = "Related: [[Tokio]], [[async]], [[rust]]";
        let links = WikiPage::extract_wikilinks(content);
        assert_eq!(links, vec!["Tokio", "async", "rust"]);
    }

    #[test]
    fn test_extract_wikilinks_none() {
        let content = "No links here.";
        let links = WikiPage::extract_wikilinks(content);
        assert!(links.is_empty());
    }

    #[test]
    fn test_wiki_structure_creates_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let wiki = WikiStructure::new(tmp.path());
        wiki.ensure_directories().unwrap();
        assert!(tmp.path().join("sources").exists());
        assert!(tmp.path().join("notions/technology").exists());
        assert!(tmp.path().join("notions/concepts").exists());
    }
}
