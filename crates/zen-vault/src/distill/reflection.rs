//! Reflexion-style verbal feedback (Reflexion, NeurIPS 2023) for the
//! hypothesis loop: a rejection produces a written "why it failed / what to
//! try differently" record that the NEXT generation pass reads, so failures
//! become in-context guidance instead of being silently archived.
//!
//! The reflection text is derived deterministically from the rejection's own
//! falsifier — no extra LLM call — which keeps it testable and free.

use std::path::{Path, PathBuf};

use sha2::Digest;

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

/// W9.2 named reader — the single seam for the `wisdom/plans/<slug>.md`
/// filename. Returns EXACTLY the base slug [`record_plan_archive`] uses for
/// its page filename (before the write-time collision suffix), so the writer
/// and every reader (e.g. the morning-brief `done` count) cannot drift.
///
/// Resolution order: `plan_name` (when present and non-blank) →
/// `first_task_id` → `plan_id`, then the shared [`reflection_slug`]
/// normalization (lowercase, non-alphanumerics → `-`, collapsed, ≤48 chars).
///
/// Readers holding only a `workflow_plans` row pass `first_task_id = None`:
/// a nameless plan whose page was slugged from its first task id is then
/// (correctly, fail-closed) not matched — the row simply is not counted done.
pub fn plan_archive_slug(
    plan_name: Option<&str>,
    first_task_id: Option<&str>,
    plan_id: &str,
) -> String {
    let slug_source = plan_name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| first_task_id.unwrap_or(plan_id));
    reflection_slug(slug_source)
}

/// SC-010 (D21 §4): one completed task's one-line summary for the archive
/// table. Bounded at [`PLAN_RESULT_SUMMARY_CHARS`], newline-flattened and
/// sanitizer-cleaned — an agent-authored response must never inject
/// frontmatter/table syntax into the page.
pub const PLAN_RESULT_SUMMARY_CHARS: usize = 300;

/// Agent-writable page cap (the [`REWIND_MAX_BYTES`] precedent).
pub const PLAN_ARCHIVE_MAX_BYTES: usize = 256 * 1024;

/// One task row of a plan archive (SC-010).
pub struct PlanArchiveTask<'a> {
    pub id: &'a str,
    pub agent: &'a str,
    pub status: &'a str,
    /// Full task response; the writer derives the one-line summary.
    pub response: &'a str,
}

/// Inputs for a success-archive page (SC-010, D21 §4). `record_plan_archive`
/// renders frontmatter + task table + verdict + duration; the slug derives
/// from `plan_name` (else the first task id) with a short hash suffix on
/// collision, so two same-named plans never overwrite each other.
pub struct PlanArchive<'a> {
    pub plan_name: Option<&'a str>,
    pub plan_id: &'a str,
    pub tasks: &'a [PlanArchiveTask<'a>],
    pub plan_approved: bool,
    pub delivery_ready: bool,
    pub duration_ms: u128,
}

/// Write the success-twin page under `<plans_dir>/<slug>.md` — sibling of
/// [`record_rewind`] (atomic write, same slug hygiene, same 256KiB cap and
/// sanitizer discipline). The caller owns the mutual exclusivity with the
/// rewind path (any failed task ⇒ rewind only) and the ≥2-tasks/≥1-completed
/// gate.
pub fn record_plan_archive(plans_dir: &Path, archive: &PlanArchive<'_>) -> Result<PathBuf> {
    let sanitizer = InputSanitizer::new();
    // Only the agent-authored response text is untrusted prose — plan ids,
    // agent names and statuses are engine/registry-controlled values that
    // must stay sentinel-free for machine readers (e.g. W9.2).
    let clean_response = |text: &str| -> String {
        let one_line = text.replace(['\n', '\r'], " ");
        let trimmed = one_line.trim();
        let bounded: String = trimmed.chars().take(PLAN_RESULT_SUMMARY_CHARS).collect();
        sanitizer
            .sanitize(&bounded)
            .map(|c| c.sanitized)
            .unwrap_or_else(|_| bounded)
    };
    let clean = |text: &str| -> String { text.replace(['\n', '\r'], " ").trim().to_string() };
    let (completed, failed) =
        archive
            .tasks
            .iter()
            .fold((0usize, 0usize), |(c, f), t| match t.status {
                "completed" => (c + 1, f),
                "failed" => (c, f + 1),
                _ => (c, f),
            });
    let mut rows = String::new();
    for t in archive.tasks {
        rows.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            clean(t.id),
            clean(t.agent),
            clean(t.status),
            clean_response(t.response)
        ));
    }
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let body = format!(
        "---\ntype: plan\ndate: {date}\nplan_id: {}\ntasks_total: {}\ntasks_completed: {completed}\ntasks_failed: {failed}\ndelivery_ready: {}\n---\n\n# Plan {}\n\n| id | agent | status | summary |\n|---|---|---|---|\n{rows}\nverdict: plan_approved={} delivery_ready={}\nduration_ms: {}\n",
        clean(archive.plan_id),
        archive.tasks.len(),
        archive.delivery_ready,
        clean(archive.plan_name.unwrap_or(archive.plan_id)),
        archive.plan_approved,
        archive.delivery_ready,
        archive.duration_ms,
    );
    let body = if body.len() > PLAN_ARCHIVE_MAX_BYTES {
        // Char-boundary-safe truncation with the shared marker shape.
        let mut cut = PLAN_ARCHIVE_MAX_BYTES;
        while cut > 0 && !body.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}\n\n…[truncated]\n", &body[..cut])
    } else {
        body
    };
    let base_slug = plan_archive_slug(
        archive.plan_name,
        archive.tasks.first().map(|t| t.id),
        archive.plan_id,
    );
    std::fs::create_dir_all(plans_dir)
        .with_context(|| format!("create {}", plans_dir.display()))?;
    let mut target = plans_dir.join(format!("{}.md", base_slug));
    if target.exists() {
        // Collision: same slug, different plan — disambiguate with a short
        // content-keyed suffix instead of overwriting the older page. The
        // suffix is inherently write-time (it depends on what is already on
        // disk) and stays out of [`plan_archive_slug`].
        let digest = sha2::Sha256::digest(archive.plan_id.as_bytes());
        let suffix: String = digest
            .iter()
            .take(4)
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join("");
        target = plans_dir.join(format!("{}-{suffix}.md", base_slug));
    }
    zen_core::atomic_file::write_atomic(&target, body.as_bytes())
        .with_context(|| format!("write {}", target.display()))?;
    Ok(target)
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

    fn archive_tasks() -> Vec<PlanArchiveTask<'static>> {
        vec![
            PlanArchiveTask {
                id: "a",
                agent: "Explore",
                status: "completed",
                response: "found docs\nwith a second line",
            },
            PlanArchiveTask {
                id: "b",
                agent: "Librarian",
                status: "completed",
                response: "ok",
            },
        ]
    }

    #[test]
    fn plan_archive_writes_single_sanitized_page() {
        let dir = TempDir::new().unwrap();
        let tasks = archive_tasks();
        let archive = PlanArchive {
            plan_name: Some("Sweep Docs"),
            plan_id: "p1",
            tasks: &tasks,
            plan_approved: true,
            delivery_ready: true,
            duration_ms: 4200,
        };
        let path = record_plan_archive(dir.path(), &archive).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.starts_with("---\ntype: plan\n"), "{body}");
        assert!(body.contains("plan_id: p1"), "{body}");
        assert!(body.contains("tasks_completed: 2"), "{body}");
        assert!(body.contains("delivery_ready: true"), "{body}");
        // Newline-flattened one-line summary: the response's second line
        // must never break the table row.
        assert!(
            body.contains("[USER_CONTENT_START]found docs with a second line[USER_CONTENT_END]"),
            "sanitizer sentinels the agent-authored summary: {body}"
        );
        assert!(!body.contains("docs\nwith"), "{body}");
        assert!(
            body.contains("verdict: plan_approved=true delivery_ready=true"),
            "{body}"
        );
        assert!(body.contains("duration_ms: 4200"), "{body}");
    }

    #[test]
    fn plan_archive_summary_caps_at_char_bound() {
        let dir = TempDir::new().unwrap();
        let long_response = "s".repeat(PLAN_RESULT_SUMMARY_CHARS + 100);
        let tasks = vec![PlanArchiveTask {
            id: "a",
            agent: "Explore",
            status: "completed",
            response: long_response.as_str(),
        }];
        let archive = PlanArchive {
            plan_name: None,
            plan_id: "p1",
            tasks: &tasks,
            plan_approved: false,
            delivery_ready: false,
            duration_ms: 1,
        };
        let path = record_plan_archive(dir.path(), &archive).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let occurrences = body.matches('s').count();
        assert!(
            occurrences <= PLAN_RESULT_SUMMARY_CHARS + 40,
            "summary must be bounded, got {occurrences} chars"
        );
        assert!(body.contains("plan_id: p1"), "{body}");
    }

    #[test]
    fn plan_archive_slug_matches_written_filename() {
        let dir = TempDir::new().unwrap();
        let tasks = archive_tasks();
        let named = PlanArchive {
            plan_name: Some("Sweep Docs"),
            plan_id: "p1",
            tasks: &tasks,
            plan_approved: true,
            delivery_ready: true,
            duration_ms: 1,
        };
        let path = record_plan_archive(dir.path(), &named).unwrap();
        let stem = path.file_stem().unwrap().to_str().unwrap();
        assert_eq!(
            stem,
            plan_archive_slug(Some("Sweep Docs"), Some("a"), "p1"),
            "named plan: reader helper must reconstruct the writer's filename"
        );
        assert_eq!(stem, "sweep-docs");

        // Nameless: the writer falls back to the first task id; a reader
        // holding only the row (first_task_id = None) resolves to plan_id
        // and must NOT claim a match (fail-closed reader contract).
        let nameless = PlanArchive {
            plan_name: None,
            plan_id: "p2",
            tasks: &tasks,
            plan_approved: true,
            delivery_ready: true,
            duration_ms: 1,
        };
        let path = record_plan_archive(dir.path(), &nameless).unwrap();
        let stem = path.file_stem().unwrap().to_str().unwrap();
        assert_eq!(stem, plan_archive_slug(None, Some("a"), "p2"));
        assert_eq!(stem, "a");
        assert_ne!(
            stem,
            plan_archive_slug(None, None, "p2"),
            "row-only reader must not match a first-task-id slug"
        );

        // No tasks at all: both writer and reader land on plan_id.
        let empty: Vec<PlanArchiveTask<'static>> = Vec::new();
        let bare = PlanArchive {
            plan_name: None,
            plan_id: "p3",
            tasks: &empty,
            plan_approved: false,
            delivery_ready: false,
            duration_ms: 1,
        };
        let path = record_plan_archive(dir.path(), &bare).unwrap();
        assert_eq!(
            path.file_stem().unwrap().to_str().unwrap(),
            plan_archive_slug(None, None, "p3")
        );
    }

    #[test]
    fn plan_archive_collision_suffixes_instead_of_overwriting() {
        let dir = TempDir::new().unwrap();
        let tasks = archive_tasks();
        let first = PlanArchive {
            plan_name: Some("Same Name"),
            plan_id: "p-one",
            tasks: &tasks,
            plan_approved: true,
            delivery_ready: true,
            duration_ms: 1,
        };
        let second = PlanArchive {
            plan_name: Some("Same Name"),
            plan_id: "p-two",
            tasks: &tasks,
            plan_approved: false,
            delivery_ready: false,
            duration_ms: 2,
        };
        let p1 = record_plan_archive(dir.path(), &first).unwrap();
        let p2 = record_plan_archive(dir.path(), &second).unwrap();
        assert_ne!(p1, p2, "second plan must not overwrite the first page");
        let first_body = std::fs::read_to_string(&p1).unwrap();
        assert!(first_body.contains("plan_id: p-one"), "{first_body}");
        assert!(
            std::fs::read_to_string(&p2)
                .unwrap()
                .contains("plan_id: p-two")
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
