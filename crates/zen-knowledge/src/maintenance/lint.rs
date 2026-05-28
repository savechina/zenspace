use std::fs;
use std::path::Path;

use anyhow::Result;
use tracing::info;

/// Result of a lint pass over a wiki directory.
#[derive(Debug, Default)]
pub struct LintResult {
    pub orphan_pages: Vec<String>,
    pub broken_wikilinks: Vec<String>,
    pub stale_claims: Vec<String>,
    pub knowledge_gaps: Vec<String>,
}

/// Stub linter that walks a wiki directory and checks for broken wikilinks.
pub struct Linter;

impl Linter {
    pub fn new() -> Self {
        Linter
    }

    /// Walk `wiki_dir` for `.md` files, scan for `[[...]]` wikilinks,
    /// and verify target files exist. Returns a LintResult.
    pub fn run(&self, wiki_dir: &Path) -> Result<LintResult> {
        let mut result = LintResult::default();

        if !wiki_dir.is_dir() {
            return Ok(result);
        }

        for entry in fs::read_dir(wiki_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().is_some_and(|ext| ext == "md") {
                let content = fs::read_to_string(&path)?;
                self.scan_wikilinks(&content, wiki_dir, &path, &mut result);
            }
        }

        info!("Lint stub: scanned wiki directory");

        Ok(result)
    }

    /// Extract `[[target]]` patterns and check if `{wiki_dir}/{target}.md` exists.
    fn scan_wikilinks(
        &self,
        content: &str,
        wiki_dir: &Path,
        source: &Path,
        result: &mut LintResult,
    ) {
        let mut chars = content.chars().peekable();

        while let Some(c) = chars.next() {
            if c == '[' && chars.peek() == Some(&'[') {
                chars.next(); // consume second '['
                let mut target = String::new();

                while let Some(c) = chars.next() {
                    if c == ']' && chars.peek() == Some(&']') {
                        chars.next(); // consume second ']'
                        break;
                    }
                    target.push(c);
                }

                let target_file = wiki_dir.join(format!("{target}.md"));
                if !target_file.exists() {
                    let label = format!(
                        "{} -> [[{}]]",
                        source
                            .file_stem()
                            .map(|s| s.to_string_lossy())
                            .unwrap_or_default(),
                        target
                    );
                    result.broken_wikilinks.push(label);
                }
            }
        }
    }
}

impl Default for Linter {
    fn default() -> Self {
        Self::new()
    }
}
