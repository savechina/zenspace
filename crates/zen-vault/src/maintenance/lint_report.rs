use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;

use crate::maintenance::LintResult;

/// Generates a markdown lint report from a LintResult.
pub struct LintReportGenerator;

impl LintReportGenerator {
    pub fn new() -> Self {
        LintReportGenerator
    }

    /// Write a lint report to `reports_dir/lint-YYYY-MM-DD.md`.
    ///
    /// Creates `reports_dir` if it does not exist. Returns the path to the written file.
    pub fn generate(&self, result: &LintResult, reports_dir: &Path) -> Result<PathBuf> {
        fs::create_dir_all(reports_dir)?;

        let date = Utc::now().format("%Y-%m-%d");
        let report_path = reports_dir.join(format!("lint-{date}.md"));

        let mut report = String::new();
        report.push_str(&format!("# Lint Report — {date}\n\n"));

        report.push_str("## Orphan Pages\n\n");
        if result.orphan_pages.is_empty() {
            report.push_str("None detected.\n\n");
        } else {
            for page in &result.orphan_pages {
                report.push_str(&format!("- {page}\n"));
            }
            report.push('\n');
        }

        report.push_str("## Broken Wikilinks\n\n");
        if result.broken_wikilinks.is_empty() {
            report.push_str("None detected.\n\n");
        } else {
            for link in &result.broken_wikilinks {
                report.push_str(&format!("- {link}\n"));
            }
            report.push('\n');
        }

        report.push_str("## Stale Claims\n\n");
        if result.stale_claims.is_empty() {
            report.push_str("None detected.\n\n");
        } else {
            for claim in &result.stale_claims {
                report.push_str(&format!("- {claim}\n"));
            }
            report.push('\n');
        }

        report.push_str("## Knowledge Gaps\n\n");
        if result.knowledge_gaps.is_empty() {
            report.push_str("None detected.\n\n");
        } else {
            for gap in &result.knowledge_gaps {
                report.push_str(&format!("- {gap}\n"));
            }
            report.push('\n');
        }

        fs::write(&report_path, report)?;
        Ok(report_path)
    }
}

impl Default for LintReportGenerator {
    fn default() -> Self {
        Self::new()
    }
}
