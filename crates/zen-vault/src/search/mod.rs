use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

mod service;
mod tier2;
mod tier3;
mod tier4;
mod tier5;
pub mod tier_selector;

pub use service::SearchService;
pub use tier_selector::TierSelector;
pub use tier2::Tier2Search;
pub use tier3::Tier3Search;
pub use tier4::{GraphResult, Tier4Search};
pub use tier5::Tier5Search;

/// Result of a search operation (shared across tiers).
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub file: PathBuf,
    pub line: u32,
    pub content: String,
}

/// Tier 1 search: exact keyword search via ripgrep.
///
/// Target latency: <50ms (FR-007).
pub struct Tier1Search;

impl Tier1Search {
    /// Search for an exact keyword match using ripgrep.
    ///
    /// Returns matching lines across all files in `base_dir`.
    pub fn search(query: &str, base_dir: &Path) -> Result<Vec<SearchResult>> {
        // Verify ripgrep is installed
        which::which("rg").context("ripgrep not installed. Install with: brew install ripgrep")?;

        let output = Command::new("rg")
            .arg("--line-number")
            .arg("--no-heading")
            .arg("--color=never")
            .arg("--fixed-strings")
            .arg(query)
            .arg(
                base_dir
                    .to_str()
                    .context("base_dir contains invalid UTF-8")?,
            )
            .output()
            .context("failed to execute ripgrep")?;

        if !output.status.success() && output.stdout.is_empty() {
            // ripgrep returns exit code 1 when no matches found — that's fine
            if output.status.code() == Some(1) {
                return Ok(Vec::new());
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("ripgrep failed: {stderr}");
        }

        let stdout = String::from_utf8(output.stdout)?;
        let results = stdout.lines().filter_map(parse_rg_line).collect();

        Ok(results)
    }
}

/// Parse a single ripgrep output line: `filepath:line_number:content`
fn parse_rg_line(line: &str) -> Option<SearchResult> {
    // Split on ':' — first field is path, second is line number, rest is content
    let (file_part, rest) = line.split_once(':')?;
    let (line_part, content) = rest.split_once(':')?;

    let file = PathBuf::from(file_part);
    let line: u32 = line_part.parse().ok()?;
    let content = content.to_string();

    Some(SearchResult {
        file,
        line,
        content,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_tier1_search_finds_matches() {
        if which::which("rg").is_err() {
            eprintln!("Skipping test: ripgrep not installed");
            return;
        }

        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.txt");
        fs::write(&file_path, "hello world\nfoo bar\nhello again\n").unwrap();

        let results = Tier1Search::search("hello", tmp.path()).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results[0].content.contains("hello world"));
        assert_eq!(results[0].line, 1);
        assert!(results[1].content.contains("hello again"));
        assert_eq!(results[1].line, 3);
    }

    #[test]
    fn test_tier1_search_no_matches_returns_empty() {
        if which::which("rg").is_err() {
            eprintln!("Skipping test: ripgrep not installed");
            return;
        }

        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.txt");
        fs::write(&file_path, "nothing here\n").unwrap();

        let results = Tier1Search::search("nonexistent", tmp.path()).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_parse_rg_line_valid() {
        let line = "src/main.rs:42:    let x = 1;";
        let result = parse_rg_line(line).unwrap();
        assert_eq!(result.file, PathBuf::from("src/main.rs"));
        assert_eq!(result.line, 42);
        assert_eq!(result.content, "    let x = 1;");
    }

    #[test]
    fn test_parse_rg_line_content_with_colons() {
        let line = "file.rs:10:key: value: extra";
        let result = parse_rg_line(line).unwrap();
        assert_eq!(result.file, PathBuf::from("file.rs"));
        assert_eq!(result.line, 10);
        assert_eq!(result.content, "key: value: extra");
    }

    #[test]
    fn test_domain_filter() {
        use super::service::filter_by_domain;
        let tmp = TempDir::new().unwrap();

        let work_file = tmp.path().join("work_note.md");
        fs::write(
            &work_file,
            "---\nid: \"w1\"\ntags: []\nsource: test\nsensitivity: private\ncreated_at: \"2026-01-01T00:00:00Z\"\nupdated_at: \"2026-01-01T00:00:00Z\"\ndomain: [work]\n---\nWork content",
        ).unwrap();

        let personal_file = tmp.path().join("personal_note.md");
        fs::write(
            &personal_file,
            "---\nid: \"p1\"\ntags: []\nsource: test\nsensitivity: private\ncreated_at: \"2026-01-01T00:00:00Z\"\nupdated_at: \"2026-01-01T00:00:00Z\"\ndomain: [personal]\n---\nPersonal content",
        ).unwrap();

        let results = vec![
            SearchResult { file: work_file.clone(), line: 1, content: "work".to_string() },
            SearchResult { file: personal_file.clone(), line: 1, content: "personal".to_string() },
        ];

        let filtered = filter_by_domain(results, "work").unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].file, work_file);

        let results2 = vec![
            SearchResult { file: work_file, line: 1, content: "work".to_string() },
            SearchResult { file: personal_file, line: 1, content: "personal".to_string() },
        ];
        let filtered2 = filter_by_domain(results2, "personal").unwrap();
        assert_eq!(filtered2.len(), 1);
    }
}
