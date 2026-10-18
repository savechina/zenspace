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

use zen_core::sanitize::InputSanitizer;
use zen_memory::RejectedHypothesis;

/// Per-file size cap for rewind-memory reads (`256 KiB`, the T093/T214
/// wake-up-brief / identity-file precedent). An agent-writable file must
/// never be read unbounded into a prompt.
const REWIND_MAX_BYTES: u64 = 256 * 1024;

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

/// Write `body` to `<dir>/<slug>.md` atomically. Overwriting the same slug is
/// idempotent — the shared persistence core for reflections and rewinds.
fn write_markdown(dir: &Path, slug_source: &str, body: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let target = dir.join(format!("{}.md", reflection_slug(slug_source)));
    zen_core::atomic_file::write_atomic(&target, body.as_bytes())
        .with_context(|| format!("write {}", target.display()))?;
    Ok(target)
}

/// Persist the reflection for a rejected hypothesis under
/// `<reflections_dir>/<slug>.md`.
pub fn record_reflection(
    reflections_dir: &Path,
    rejection: &RejectedHypothesis,
) -> Result<PathBuf> {
    let content = format!(
        "# Reflection\n\nclaim: {}\nfalsifier: {}\nbecause: {}\nnext_attempt: {}\n",
        rejection.claim,
        rejection.falsifier,
        rejection.because,
        next_attempt(&rejection.falsifier),
    );
    write_markdown(reflections_dir, &rejection.claim, &content)
}

/// W2.2 (D12): persist an agent-authored "rewind memory" at
/// `<rewinds_dir>/<slug>.md` — the state-plus-guidance record a rollback
/// boundary leaves behind so the next attempt does not repeat the failed
/// path (AgentRewind, arXiv:2608.14380). The design's boundaries are the
/// `TransactionScope` rollback (cascade/pipeline), the CAS
/// `commit_conditional` drift rollback, and the `plan.execute` failure path
/// ([`recent_rewinds`] is the read side). Same shape/fields as a reflection
/// so the shared reader lifts both. `claim` is the slug source.
pub fn record_rewind(
    rewinds_dir: &Path,
    claim: &str,
    falsifier: &str,
    because: &str,
) -> Result<PathBuf> {
    let content = format!(
        "# Rewind\n\nclaim: {claim}\nfalsifier: {falsifier}\nbecause: {because}\nnext_attempt: {}\n",
        next_attempt(falsifier),
    );
    write_markdown(rewinds_dir, claim, &content)
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

/// Newest-first `(claim, next_attempt)` summaries from a directory of
/// markdown memories. `max_bytes` enforces a per-file read cap and
/// `sanitize` applies the [`InputSanitizer`] — the wake-up-brief read-site
/// hygiene (T093/T214). Unreadable or oversized files are skipped.
fn newest_summaries(
    dir: &Path,
    limit: usize,
    max_bytes: Option<u64>,
    sanitize: bool,
) -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let sanitizer = sanitize.then(InputSanitizer::new);
    let mut dated: Vec<(std::time::SystemTime, String)> = Vec::new();
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        if let Some(cap) = max_bytes
            && entry.metadata().map(|m| m.len()).unwrap_or(0) > cap
        {
            warn!(path = %path.display(), "memory file exceeds size cap — skipped");
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            warn!(path = %path.display(), "memory file unreadable — skipped");
            continue;
        };
        let content = match &sanitizer {
            Some(s) => s.strip_dangerous_patterns(&content),
            None => content,
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
            (field("claim"), field("next_attempt"))
        })
        .collect()
}

/// Most recent reflections, newest first, as single-line summaries suitable
/// for prompt injection. Unreadable files are skipped with a warning.
pub fn recent_reflections(reflections_dir: &Path, limit: usize) -> Vec<String> {
    newest_summaries(reflections_dir, limit, None, false)
        .into_iter()
        .map(|(claim, next)| format!("prior failure — claim: {claim} | guidance: {next}"))
        .collect()
}

/// W2.2 (D12): most recent rewind memories, newest-first single-line
/// summaries for the next cycle's prompt injection. Carries the
/// wake-up-brief read-site hygiene — a 256 KiB per-file cap and the
/// [`InputSanitizer`] before the content can reach a prompt.
pub fn recent_rewinds(rewinds_dir: &Path, limit: usize) -> Vec<String> {
    newest_summaries(rewinds_dir, limit, Some(REWIND_MAX_BYTES), true)
        .into_iter()
        .map(|(claim, next)| format!("prior rollback — claim: {claim} | guidance: {next}"))
        .collect()
}

/// Render lines under `header` as a prompt block, or `None` when empty.
fn render_block(header: &str, lines: &[String]) -> Option<String> {
    if lines.is_empty() {
        return None;
    }
    let mut block = String::from(header);
    for line in lines {
        block.push_str(&format!("- {line}\n"));
    }
    Some(block)
}

/// Render reflections as a prompt section, or `None` when there are none.
pub fn render_reflection_block(reflections: &[String]) -> Option<String> {
    render_block("Prior failed attempts (do not repeat):\n", reflections)
}

/// W2.2 (D12): render rewind memories as a prompt section, or `None` when
/// there are none.
pub fn render_rewind_block(rewinds: &[String]) -> Option<String> {
    render_block(
        "Prior rollbacks (state restored — do not repeat):\n",
        rewinds,
    )
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

    #[test]
    fn rewind_roundtrip_produces_guidance() {
        let dir = TempDir::new().unwrap();
        record_rewind(
            dir.path(),
            "plan `nightly` task `compile` failed",
            "sandbox denied write",
            "plan.execute task failure",
        )
        .unwrap();

        let rewinds = recent_rewinds(dir.path(), 5);
        assert_eq!(rewinds.len(), 1);
        assert!(rewinds[0].contains("nightly"), "{}", rewinds[0]);
        assert!(
            rewinds[0].to_lowercase().contains("do not re-propose"),
            "guidance must carry the negative-space instruction: {}",
            rewinds[0]
        );
        let block = render_rewind_block(&rewinds).expect("block");
        assert!(block.starts_with("Prior rollbacks"), "{block}");
    }

    #[test]
    fn rewind_same_claim_is_idempotent_and_newest_wins() {
        let dir = TempDir::new().unwrap();
        let first = record_rewind(dir.path(), "plan `p` failed", "old falsifier", "first").unwrap();
        let second =
            record_rewind(dir.path(), "plan `p` failed", "new falsifier", "second").unwrap();
        assert_eq!(first, second, "same claim slug ⇒ same target path");
        let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(entries.len(), 1, "overwrite, never duplicate");
        assert!(
            recent_rewinds(dir.path(), 5)[0].contains("new falsifier"),
            "newest content wins"
        );
    }

    #[test]
    fn oversized_rewind_is_skipped_by_cap() {
        let dir = TempDir::new().unwrap();
        let oversized = format!(
            "# Rewind\n\nclaim: huge\nfalsifier: {}\n",
            "x".repeat((REWIND_MAX_BYTES + 1) as usize)
        );
        std::fs::write(dir.path().join("huge.md"), oversized).unwrap();
        assert!(
            recent_rewinds(dir.path(), 5).is_empty(),
            "a file above the cap must never reach the reader"
        );
    }
}
