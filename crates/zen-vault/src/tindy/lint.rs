use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use tracing::info;

use crate::tindy::learning_loop::{GapType, LearningLoop};

/// One page failing OKF v0.1 conformance (compile-hygiene ①).
#[derive(Debug, Clone, PartialEq)]
pub struct OkfFinding {
    /// Vault-relative page path as reported to the user.
    pub page: String,
    /// Required OKF keys with a missing or empty value.
    pub missing: Vec<&'static str>,
}

/// Result of a lint pass over a wiki directory.
#[derive(Debug, Default)]
pub struct LintResult {
    pub orphan_pages: Vec<String>,
    pub broken_wikilinks: Vec<String>,
    pub stale_claims: Vec<String>,
    pub knowledge_gaps: Vec<String>,
    pub okf_missing: Vec<OkfFinding>,
    /// Machine-rendered pages lacking a non-empty `sources:` list
    /// (compile-hygiene E2). The machine-rendered signature is
    /// `created_at:` + `updated_at:` frontmatter; hand-written pages
    /// carry neither key and are never reported.
    pub sources_missing: Vec<String>,
}

/// Required OKF v0.1 frontmatter keys checked by the conformance rule.
const OKF_REQUIRED_KEYS: &[&str] = &["type", "description"];

/// Stems of generated files that are exempt from frontmatter requirements:
/// `index.md` deliberately has none per OKF §6, `log.md` is the compiler's
/// operation journal.
const OKF_EXEMPT_STEMS: &[&str] = &["index", "log"];

/// Linter that delegates to LearningLoop for comprehensive gap detection.
pub struct Linter;

impl Linter {
    pub fn new() -> Self {
        Linter
    }

    /// Analyze `wiki_dir` via LearningLoop and partition gaps into LintResult fields.
    pub fn run(&self, wiki_dir: &Path) -> Result<LintResult> {
        self.run_with_okf(wiki_dir, true)
    }

    /// Same as [`run`](Linter::run), with the OKF conformance pass gated by
    /// `okf_enabled` (compile-hygiene ①, `[agentic.compile] okf_lint`).
    pub fn run_with_okf(&self, wiki_dir: &Path, okf_enabled: bool) -> Result<LintResult> {
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

        if okf_enabled {
            result.okf_missing = scan_okf_conformance(wiki_dir);
        }
        result.sources_missing = scan_sources_provenance(wiki_dir);

        info!(
            orphan = result.orphan_pages.len(),
            broken = result.broken_wikilinks.len(),
            stale = result.stale_claims.len(),
            gaps = result.knowledge_gaps.len(),
            okf_missing = result.okf_missing.len(),
            sources_missing = result.sources_missing.len(),
            "lint complete via LearningLoop"
        );

        Ok(result)
    }
}

/// Check every content page under `wiki_dir` for the required OKF v0.1
/// frontmatter keys. Advisory by design: a page with no frontmatter at all
/// yields one finding listing every missing key, never an error.
fn scan_okf_conformance(wiki_dir: &Path) -> Vec<OkfFinding> {
    let mut findings = Vec::new();
    let Ok(entries) = std::fs::read_dir(wiki_dir) else {
        return findings;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            findings.extend(scan_okf_conformance(&path));
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if OKF_EXEMPT_STEMS.iter().any(|s| *s == stem) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let keys = frontmatter_keys(&content);
        let missing: Vec<&'static str> = OKF_REQUIRED_KEYS
            .iter()
            .copied()
            .filter(|key| match keys.get(*key) {
                Some(value) => value.trim().is_empty(),
                None => true,
            })
            .collect();
        if !missing.is_empty() {
            let rel = path
                .strip_prefix(wiki_dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            findings.push(OkfFinding { page: rel, missing });
        }
    }
    findings.sort_by(|a, b| a.page.cmp(&b.page));
    findings
}

/// Compile-hygiene E2 provenance check: report machine-rendered pages
/// (the `render_page` signature — `created_at:` + `updated_at:` keys)
/// whose `sources:` list is absent or empty. Advisory only; hand-written
/// pages carry neither signature key and are never reported.
fn scan_sources_provenance(wiki_dir: &Path) -> Vec<String> {
    let mut missing = Vec::new();
    let Ok(entries) = std::fs::read_dir(wiki_dir) else {
        return missing;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            missing.extend(scan_sources_provenance(&path));
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if OKF_EXEMPT_STEMS.iter().any(|s| *s == stem) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let keys = frontmatter_keys(&content);
        let machine_rendered = keys.contains_key("created_at") && keys.contains_key("updated_at");
        let sources_present = keys
            .get("sources")
            .is_some_and(|v| !v.trim().is_empty() && v.trim() != "[]");
        if machine_rendered && !sources_present {
            missing.push(
                path.strip_prefix(wiki_dir)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string(),
            );
        }
    }
    missing.sort();
    missing
}

/// Parse the YAML frontmatter block (if any) into a `key → raw value` map.
/// Quotes are stripped from values; anything outside the `---` fence is
/// ignored. Returns an empty map for pages without frontmatter.
fn frontmatter_keys(content: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let mut lines = content.lines().peekable();
    if lines.peek().is_none_or(|first| first.trim() != "---") {
        return map;
    }
    lines.next();
    for line in lines {
        let trimmed = line.trim_end();
        if trimmed == "---" || trimmed == "..." {
            break;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches('"').to_string();
        map.insert(key.trim().to_string(), value);
    }
    map
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
        write_page(
            &wiki,
            "B",
            "This is page B with enough words to not be thin. Contains extra content here.",
        );
        write_page(
            &wiki,
            "C",
            "I link to [[B]] but nobody links to me. Extra words here too.",
        );

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
        write_page(&wiki, "A", "Link to [[B]] and [[C]] here.");
        write_page(
            &wiki,
            "B",
            "Page B has enough content. Extra words to fill out here too.",
        );
        write_page(
            &wiki,
            "C",
            "Page C has enough content. Extra words to fill out here too.",
        );

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

    // ── OKF conformance (compile-hygiene ①) ─────────────────────

    #[test]
    fn okf_compliant_page_has_no_finding() {
        let (_tmp, wiki) = setup_test_wiki();
        write_page(
            &wiki,
            "good",
            "---\ntype: concept\ndescription: \"A fine page\"\n---\n\nBody.",
        );

        let result = Linter::new().run(&wiki).expect("lint run");
        assert!(
            result.okf_missing.iter().all(|f| f.page != "good.md"),
            "compliant page flagged: {:?}",
            result.okf_missing
        );
    }

    #[test]
    fn okf_missing_description_is_reported() {
        let (_tmp, wiki) = setup_test_wiki();
        write_page(&wiki, "partial", "---\ntype: concept\n---\n\nBody.");

        let result = Linter::new().run(&wiki).expect("lint run");
        let finding = result
            .okf_missing
            .iter()
            .find(|f| f.page == "partial.md")
            .expect("missing-description page not flagged");
        assert_eq!(finding.missing, vec!["description"]);
    }

    #[test]
    fn okf_page_without_frontmatter_reports_both_keys_not_error() {
        let (_tmp, wiki) = setup_test_wiki();
        write_page(&wiki, "legacy", "Just body text, no frontmatter.");

        let result = Linter::new().run(&wiki).expect("lint run must succeed");
        let finding = result
            .okf_missing
            .iter()
            .find(|f| f.page == "legacy.md")
            .expect("legacy page not flagged");
        assert_eq!(finding.missing.len(), 2);
        assert!(finding.missing.contains(&"type"));
        assert!(finding.missing.contains(&"description"));
    }

    #[test]
    fn okf_empty_value_counts_as_missing() {
        let (_tmp, wiki) = setup_test_wiki();
        write_page(
            &wiki,
            "empty",
            "---\ntype: concept\ndescription: \"\"\n---\n\nBody.",
        );

        let result = Linter::new().run(&wiki).expect("lint run");
        let finding = result
            .okf_missing
            .iter()
            .find(|f| f.page == "empty.md")
            .expect("empty-description page not flagged");
        assert_eq!(finding.missing, vec!["description"]);
    }

    #[test]
    fn okf_gate_disabled_yields_no_findings() {
        let (_tmp, wiki) = setup_test_wiki();
        write_page(&wiki, "legacy", "Just body text, no frontmatter.");

        let result = Linter::new().run_with_okf(&wiki, false).expect("lint run");
        assert!(result.okf_missing.is_empty());
    }

    #[test]
    fn okf_generated_index_and_log_are_exempt() {
        let (_tmp, wiki) = setup_test_wiki();
        write_page(&wiki, "index", "# Knowledge Index\n\n- [x](x.md)\n");
        write_page(&wiki, "log", "- [ts] compile_start: x\n");

        let result = Linter::new().run(&wiki).expect("lint run");
        assert!(
            result.okf_missing.is_empty(),
            "generated files flagged: {:?}",
            result.okf_missing
        );
    }

    #[test]
    fn okf_finding_pages_are_sorted() {
        let (_tmp, wiki) = setup_test_wiki();
        write_page(&wiki, "zebra", "no frontmatter");
        write_page(&wiki, "alpha", "no frontmatter");

        let result = Linter::new().run(&wiki).expect("lint run");
        let pages: Vec<&str> = result.okf_missing.iter().map(|f| f.page.as_str()).collect();
        assert_eq!(pages, vec!["alpha.md", "zebra.md"]);
    }

    // ── Source provenance (compile-hygiene E2) ──────────────────

    #[test]
    fn e2_compiled_page_with_sources_not_reported() {
        let (_tmp, wiki) = setup_test_wiki();
        write_page(
            &wiki,
            "good",
            "---\ntitle: \"good\"\ncreated_at: \"2026-01-01\"\nupdated_at: \"2026-01-02\"\nsources: [\"/vault/inbox/note.md\"]\n---\n\nBody.",
        );

        let result = Linter::new().run(&wiki).expect("lint run");
        assert!(
            !result.sources_missing.iter().any(|p| p == "good.md"),
            "provenance-complete page flagged: {:?}",
            result.sources_missing
        );
    }

    #[test]
    fn e2_machine_rendered_page_without_sources_reported() {
        let (_tmp, wiki) = setup_test_wiki();
        write_page(
            &wiki,
            "pre-e2",
            "---\ntitle: \"pre-e2\"\ncreated_at: \"2026-01-01\"\nupdated_at: \"2026-01-02\"\n---\n\nLegacy body.",
        );

        let result = Linter::new().run(&wiki).expect("lint run");
        assert!(
            result.sources_missing.iter().any(|p| p == "pre-e2.md"),
            "legacy compiled page must be reported: {:?}",
            result.sources_missing
        );
    }

    #[test]
    fn e2_hand_written_page_is_never_reported() {
        let (_tmp, wiki) = setup_test_wiki();
        write_page(&wiki, "manual", "# My own note\n\nNo frontmatter at all.");

        let result = Linter::new().run(&wiki).expect("lint run");
        assert!(
            !result.sources_missing.iter().any(|p| p == "manual.md"),
            "hand-written page must never be reported: {:?}",
            result.sources_missing
        );
    }

    #[test]
    fn e2_empty_sources_list_counts_as_missing() {
        let (_tmp, wiki) = setup_test_wiki();
        write_page(
            &wiki,
            "hollow",
            "---\ntitle: \"hollow\"\ncreated_at: \"2026-01-01\"\nupdated_at: \"2026-01-02\"\nsources: []\n---\n\nBody.",
        );

        let result = Linter::new().run(&wiki).expect("lint run");
        assert!(
            result.sources_missing.iter().any(|p| p == "hollow.md"),
            "empty sources list is not provenance: {:?}",
            result.sources_missing
        );
    }
}
