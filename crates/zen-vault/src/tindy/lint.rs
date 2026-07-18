use std::path::Path;

use anyhow::Result;
use tracing::info;

use crate::tindy::learning_loop::{GapType, LearningLoop};

/// Result of a lint pass over a wiki directory.
#[derive(Debug, Default)]
pub struct LintResult {
    pub orphan_pages: Vec<String>,
    pub broken_wikilinks: Vec<String>,
    pub stale_claims: Vec<String>,
    pub knowledge_gaps: Vec<String>,
}

/// Linter that delegates to LearningLoop for comprehensive gap detection.
pub struct Linter;

impl Linter {
    pub fn new() -> Self {
        Linter
    }

    /// Analyze `wiki_dir` via LearningLoop and partition gaps into LintResult fields.
    pub fn run(&self, wiki_dir: &Path) -> Result<LintResult> {
        let mut result = LintResult::default();

        if !wiki_dir.is_dir() {
            info!("lint: wiki directory does not exist, returning empty result");
            return Ok(result);
        }

        let gaps = LearningLoop::analyze_gaps(wiki_dir)?;

        for gap in &gaps {
            match gap.detection_type {
                GapType::OrphanPage => result.orphan_pages.push(gap.reason.clone()),
                GapType::BrokenWikilink => result.broken_wikilinks.push(gap.reason.clone()),
                GapType::StalePage => result.stale_claims.push(gap.reason.clone()),
                GapType::ThinPage | GapType::MissingCrossReference => {
                    result.knowledge_gaps.push(gap.reason.clone())
                }
            }
        }

        info!(
            orphan = result.orphan_pages.len(),
            broken = result.broken_wikilinks.len(),
            stale = result.stale_claims.len(),
            gaps = result.knowledge_gaps.len(),
            "lint complete via LearningLoop"
        );

        Ok(result)
    }
}

impl Default for Linter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write as _;
    use tempfile::TempDir;

    fn setup_test_wiki() -> (TempDir, std::path::PathBuf) {
        let tmp = TempDir::new().expect("create temp dir");
        let wiki = tmp.path().join("wiki");
        fs::create_dir_all(&wiki).expect("create wiki dir");
        (tmp, wiki)
    }

    fn write_page(wiki: &Path, name: &str, content: &str) {
        let path = wiki.join(format!("{name}.md"));
        let mut f = File::create(&path).expect("create page");
        f.write_all(content.as_bytes()).expect("write page");
    }

    #[test]
    fn test_orphan_page_detection_through_linter() {
        let (_tmp, wiki) = setup_test_wiki();

        // Page A links to B, but C has no incoming links (orphan)
        write_page(&wiki, "A", "See [[B]] for details.");
        write_page(&wiki, "B", "This is page B with enough words to not be thin. Contains extra content here.");
        write_page(&wiki, "C", "I link to [[B]] but nobody links to me. Extra words here too.");

        let linter = Linter::new();
        let result = linter.run(&wiki).expect("lint run");

        assert!(
            !result.orphan_pages.is_empty(),
            "Expected orphan page detection, got: {:?}",
            result
        );
        assert!(
            result.orphan_pages.iter().any(|msg| msg.contains("C")),
            "Expected C to be flagged as orphan"
        );
    }

    #[test]
    fn test_stale_thin_broken_detection_through_linter() {
        let (_tmp, wiki) = setup_test_wiki();

        // Thin page (fewer than 100 words)
        write_page(&wiki, "thin", "Short page.");

        // Page with broken wikilink
        write_page(
            &wiki,
            "linker",
            "Check [[NonExistent]] for more info. Extra words to fill out content here.",
        );

        // Normal page (not thin, not broken, not orphan)
        write_page(
            &wiki,
            "A",
            "Link to [[B]] and [[C]] here.",
        );
        write_page(&wiki, "B", "Page B has enough content. Extra words to fill out here too.");
        write_page(&wiki, "C", "Page C has enough content. Extra words to fill out here too.");

        let linter = Linter::new();
        let result = linter.run(&wiki).expect("lint run");

        assert!(
            !result.broken_wikilinks.is_empty(),
            "Expected broken wikilink detection"
        );
        assert!(
            !result.knowledge_gaps.is_empty(),
            "Expected knowledge gap detection (thin page)"
        );
    }
}
