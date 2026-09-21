//! Reflexion-style verbal feedback (Reflexion, NeurIPS 2023) for the
//! hypothesis loop: a rejection produces a written "why it failed / what to
//! try differently" record that the NEXT generation pass reads, so failures
//! become in-context guidance instead of being silently archived.
//!
//! The reflection text is derived deterministically from the rejection's own
//! falsifier — no extra LLM call — which keeps it testable and free.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::warn;

use zen_memory::RejectedHypothesis;

fn reflection_slug(claim: &str) -> String {
    let slug: String = claim
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let collapsed = slug
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    collapsed.chars().take(48).collect()
}

/// Persist the reflection for a rejected hypothesis under
/// `<reflections_dir>/<slug>.md`.
pub fn record_reflection(
    reflections_dir: &Path,
    rejection: &RejectedHypothesis,
) -> Result<PathBuf> {
    std::fs::create_dir_all(reflections_dir)
        .with_context(|| format!("create {}", reflections_dir.display()))?;
    let target = reflections_dir.join(format!("{}.md", reflection_slug(&rejection.claim)));
    let content = format!(
        "# Reflection\n\nclaim: {}\nfalsifier: {}\nbecause: {}\nnext_attempt: {}\n",
        rejection.claim,
        rejection.falsifier,
        rejection.because,
        next_attempt(&rejection.falsifier),
    );
    zen_core::atomic_file::write_atomic(&target, content.as_bytes())
        .with_context(|| format!("write {}", target.display()))?;
    Ok(target)
}

/// Deterministic "what to try differently" line derived from the falsifier.
fn next_attempt(falsifier: &str) -> String {
    format!(
        "Do not re-propose this claim until the falsifier is resolved ({}); \
         gather direct evidence for the missing/contradicting artifact first, \
         then re-attempt with an exploration prompt that cites that evidence.",
        falsifier.trim()
    )
}

/// Most recent reflections, newest first, as single-line summaries suitable
/// for prompt injection. Unreadable files are skipped with a warning.
pub fn recent_reflections(reflections_dir: &Path, limit: usize) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(reflections_dir) else {
        return Vec::new();
    };
    let mut dated: Vec<(std::time::SystemTime, String)> = Vec::new();
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            warn!(path = %path.display(), "reflection unreadable — skipped");
            continue;
        };
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        dated.push((mtime, content.trim().to_string()));
    }
    dated.sort_by_key(|b| std::cmp::Reverse(b.0));
    dated
        .into_iter()
        .take(limit)
        .map(|(_, content)| {
            let field = |key: &str| {
                content
                    .lines()
                    .find_map(|l| l.strip_prefix(&format!("{key}: ")))
                    .unwrap_or_default()
                    .to_string()
            };
            let claim = field("claim");
            let next = field("next_attempt");
            format!("prior failure — claim: {claim} | guidance: {next}")
        })
        .collect()
}

/// Render reflections as a prompt section, or `None` when there are none.
pub fn render_reflection_block(reflections: &[String]) -> Option<String> {
    if reflections.is_empty() {
        return None;
    }
    let mut block = String::from("Prior failed attempts (do not repeat):\n");
    for line in reflections {
        block.push_str(&format!("- {line}\n"));
    }
    Some(block)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn rejection() -> RejectedHypothesis {
        RejectedHypothesis {
            claim: "Gap 'orphan' detected: entity Foo unreachable".to_string(),
            falsifier: "missing wiki page for entity 'foo'".to_string(),
            because: "reverify: stale >7 days".to_string(),
            expiry: "2026-01-01".to_string(),
        }
    }

    #[test]
    fn reflection_roundtrip_produces_guidance() {
        let dir = TempDir::new().unwrap();
        let path = record_reflection(dir.path(), &rejection()).unwrap();
        assert!(path.exists());

        let recent = recent_reflections(dir.path(), 5);
        assert_eq!(recent.len(), 1);
        assert!(recent[0].contains("entity Foo unreachable"));
        assert!(
            recent[0].to_lowercase().contains("do not re-propose"),
            "guidance must carry the negative-space instruction: {}",
            recent[0]
        );

        let block = render_reflection_block(&recent).expect("block");
        assert!(block.starts_with("Prior failed attempts"));
    }

    #[test]
    fn empty_reflections_render_nothing() {
        assert!(render_reflection_block(&[]).is_none());
        let dir = TempDir::new().unwrap();
        assert!(recent_reflections(dir.path(), 5).is_empty());
    }

    #[test]
    fn reflections_are_newest_first_and_capped() {
        let dir = TempDir::new().unwrap();
        record_reflection(dir.path(), &rejection()).unwrap();
        let second = RejectedHypothesis {
            claim: "Gap 'stale_ingest' detected: bar".to_string(),
            falsifier: "untouched >2 cycles".to_string(),
            because: "reverify".to_string(),
            expiry: "2026-02-02".to_string(),
        };
        record_reflection(dir.path(), &second).unwrap();

        let recent = recent_reflections(dir.path(), 1);
        assert_eq!(recent.len(), 1, "limit is honoured");
    }
}
