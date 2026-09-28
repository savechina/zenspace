//! Semantic wiki lint (compile-hygiene E1): an opt-in LLM audit of wiki
//! pages for contradictions, gaps, stale claims and redundancy.
//!
//! Contract (docs/designs/e1-semantic-lint-writers.md): strictly READ-ONLY
//! over wiki pages — findings ride the existing lint reporting path and are
//! never written back. Gate (`[agentic.lint] semantic`, default false)
//! ships closed; with the gate open every failure (unconfigured provider,
//! unreachable model, unparsable output) is fail-open to zero findings.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use zen_provider::{DefaultRouter, LlmRouter as _, TaskRequirements};

/// Digest caps: a large wiki must not produce an unbounded prompt.
const MAX_PAGES: usize = 40;
const MAX_PAGE_CHARS: usize = 2_000;

/// One LLM-reported finding, validated against the audited page set.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SemanticFinding {
    pub page: String,
    pub kind: SemanticFindingKind,
    pub note: String,
}

/// The four audit dimensions (OpenKB's semantic-lint taxonomy).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SemanticFindingKind {
    Contradiction,
    Gap,
    Stale,
    Redundant,
}

impl SemanticFindingKind {
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "contradiction" => Some(Self::Contradiction),
            "gap" => Some(Self::Gap),
            "stale" => Some(Self::Stale),
            "redundant" => Some(Self::Redundant),
            _ => None,
        }
    }
}

/// A wiki page reduced to its auditable digest (title + capped body).
struct PageDigest {
    name: String,
    content: String,
}

/// Audit the wiki under `wiki_dir`. `enabled` is the resolved
/// `[agentic.lint] semantic` gate — false returns immediately with no LLM
/// call and no filesystem access.
pub async fn scan_semantic(wiki_dir: &Path, enabled: bool) -> Vec<SemanticFinding> {
    if !enabled {
        return Vec::new();
    }
    let pages = collect_digests(wiki_dir);
    if pages.len() < 2 {
        return Vec::new();
    }

    let prompt = build_prompt(&pages);
    let known: HashSet<String> = pages.iter().map(|p| p.name.clone()).collect();

    let raw = match call_default_provider(&prompt).await {
        Ok(raw) => raw,
        Err(e) => {
            warn!(error = %e, "semantic lint: provider call failed — zero findings (fail-open)");
            return Vec::new();
        }
    };

    let findings = parse_findings(&raw, &known);
    info!(
        findings = findings.len(),
        pages = pages.len(),
        "semantic lint complete"
    );
    findings
}

/// Walk `.md` pages (sorted, capped) and reduce each to a digest.
fn collect_digests(wiki_dir: &Path) -> Vec<PageDigest> {
    let mut names: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(wiki_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                names.push(stem.to_string());
            }
        }
    }
    names.sort();
    names.truncate(MAX_PAGES);

    names
        .into_iter()
        .map(|name| {
            let path = wiki_dir.join(format!("{name}.md"));
            let content = std::fs::read_to_string(&path)
                .map(|c| {
                    let stripped = crate::distill::wiki_compile::strip_frontmatter(&c);
                    truncate_chars(&stripped, MAX_PAGE_CHARS)
                })
                .unwrap_or_default();
            PageDigest { name, content }
        })
        .collect()
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}\n[truncated]")
    }
}

fn build_prompt(pages: &[PageDigest]) -> String {
    let mut prompt = String::from(
        "You are auditing a personal knowledge wiki. Below are page digests. \
Identify cross-page CONTRADICTIONS, GAPS, STALE claims, and REDUNDANCY. \
Respond with ONLY a JSON array, each element {\"page\": <page name>, \
\"kind\": <contradiction|gap|stale|redundant>, \"note\": <one sentence>}. \
Use exact page names from the list. Empty array if nothing found.\n\n",
    );
    for page in pages {
        prompt.push_str(&format!("=== {} ===\n{}\n\n", page.name, page.content));
    }
    prompt
}

/// Route through the default provider (sensitivity-aware, local-first) on a
/// blocking thread — the established spawn_blocking guard: route()/call()
/// are sync and OllamaProvider constructs a nested tokio Runtime inside
/// them, which panics on an async worker thread.
async fn call_default_provider(prompt: &str) -> Result<String, String> {
    let config = zen_core::config::load_config().map_err(|e| e.to_string())?;
    let router = DefaultRouter::from_agentic(config);
    let requirements = TaskRequirements {
        max_tokens: Some(1024),
        sensitivity: zen_core::types::Sensitivity::Private,
        preferred_model: None,
        budget_limit: None,
    };
    let prompt = prompt.to_string();
    tokio::task::spawn_blocking(move || {
        router
            .route(&requirements)
            .and_then(|provider| router.call(provider, &prompt))
    })
    .await
    .map_err(|e| format!("semantic lint task join failed: {e}"))?
    .map_err(|e| e.to_string())
}

/// Parse the LLM reply into validated findings. Anything unparseable,
/// unknown-kind, or naming a page outside the audited set is dropped — a
/// hallucinated finding must never reach the report.
fn parse_findings(raw: &str, known: &HashSet<String>) -> Vec<SemanticFinding> {
    let json_start = raw.find('[');
    let json_end = raw.rfind(']');
    let (Some(start), Some(end)) = (json_start, json_end) else {
        warn!("semantic lint: reply has no JSON array — zero findings (fail-open)");
        return Vec::new();
    };
    let Ok(items) = serde_json::from_str::<Vec<serde_json::Value>>(&raw[start..=end]) else {
        warn!("semantic lint: reply is not a JSON array — zero findings (fail-open)");
        return Vec::new();
    };

    let mut out = Vec::new();
    for item in items {
        let (Some(page), Some(kind), Some(note)) = (
            item.get("page").and_then(|v| v.as_str()),
            item.get("kind")
                .and_then(|v| v.as_str())
                .and_then(SemanticFindingKind::parse),
            item.get("note").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        if !known.contains(page) || note.trim().is_empty() {
            continue;
        }
        out.push(SemanticFinding {
            page: page.to_string(),
            kind,
            note: note.trim().to_string(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding_json(page: &str, kind: &str, note: &str) -> String {
        format!(r#"{{"page": "{page}", "kind": "{kind}", "note": "{note}"}}"#)
    }

    #[test]
    fn parse_accepts_valid_fenced_reply() {
        let known: HashSet<String> = ["rust-guide", "deploy"]
            .into_iter()
            .map(String::from)
            .collect();
        let raw = format!(
            "Here are the findings:\n[{}, {}]\nDone.",
            finding_json("rust-guide", "stale", "references the 2024 toolchain"),
            finding_json(
                "deploy",
                "contradiction",
                "says blue-green, guide says canary"
            )
        );
        let findings = parse_findings(&raw, &known);
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].kind, SemanticFindingKind::Stale);
        assert_eq!(findings[1].page, "deploy");
    }

    #[test]
    fn parse_drops_hallucinated_pages_and_unknown_kinds() {
        let known: HashSet<String> = ["rust-guide".to_string()].into_iter().collect();
        let raw = format!(
            "[{}, {}, {}, {}]",
            finding_json("ghost-page", "gap", "names a page that does not exist"),
            finding_json("rust-guide", "weather", "invalid kind"),
            finding_json("rust-guide", "gap", ""),
            "not even an object"
        );
        assert!(parse_findings(&raw, &known).is_empty());
    }

    #[test]
    fn parse_fails_open_on_garbage() {
        let known: HashSet<String> = ["a".to_string()].into_iter().collect();
        assert!(parse_findings("no json at all", &known).is_empty());
        assert!(parse_findings("[broken", &known).is_empty());
    }

    #[tokio::test]
    async fn gate_closed_makes_no_filesystem_or_llm_calls() {
        // A nonexistent wiki dir must not be touched when the gate is shut.
        let findings = scan_semantic(Path::new("/nonexistent/wiki"), false).await;
        assert!(findings.is_empty());
    }

    #[tokio::test]
    async fn tiny_wiki_needs_no_llm_call() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("only.md"), "# one page\n").unwrap();
        let findings = scan_semantic(tmp.path(), true).await;
        assert!(findings.is_empty());
    }

    #[test]
    fn digests_are_capped_and_sorted() {
        let tmp = tempfile::TempDir::new().unwrap();
        for name in ["b", "a", "c"] {
            std::fs::write(
                tmp.path().join(format!("{name}.md")),
                "---\ntitle: x\n---\nbody",
            )
            .unwrap();
        }
        let pages = collect_digests(tmp.path());
        let names: Vec<&str> = pages.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
        assert!(pages[0].content.contains("body"));
        assert!(!pages[0].content.contains("title:"));
    }
}
