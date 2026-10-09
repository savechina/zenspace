//! Delete-cascade cleanup for compiled wiki pages (compile-hygiene E3).
//!
//! OpenKB's `page_ops` counterpart: when a source note leaves the vault,
//! the provenance it left on compiled pages must not dangle. Two entry
//! points, both cycle-internal (no new CLI — 2026-08-28 surface trim):
//!
//! - [`redirect_page_source`] — the archive hook. After a note moves from
//!   the inbox to its durable archive dest, pages citing the inbox path
//!   are re-pointed at the dest, keeping provenance alive.
//! - [`sweep_dead_sources`] — cycle-start cleanup. Source entries whose
//!   path no longer exists anywhere are stripped; a page whose provenance
//!   is fully dead (machine-rendered, `sources:` key present, list empties)
//!   is deleted as an orphan, and inbound `[[links]]` to it are rewritten
//!   to plain text.
//!
//! Safety contract: pages without a `sources:` key are hand-written and
//! NEVER touched. Rewrites go through the cycle's [`TransactionScope`] so
//! CAS rollback (FR-032) and crash replay (④) cover them.
//!
//! Orphan DELETES are deliberately *not* txn-tracked —
//! [`TransactionScope::rollback`] removes tracked paths rather than
//! restoring them, so a tracked delete could never be undone. A delete is
//! made recoverable instead by capturing the page's prior bytes into the
//! [`PageIterations`](crate::wiki::PageIterations) store first (recoverable
//! via `zen wiki rollback`) and by requiring two consecutive sweeps, so a
//! transiently unreachable vault cannot mass-delete the wiki. Sources
//! values are code-managed (E2) — absolute vault paths, so existence is a
//! plain `Path::exists`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{info, warn};

use super::transaction::TransactionScope;

/// Outcome of one [`sweep_dead_sources`] pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CascadeReport {
    /// Pages inspected (had a `sources:` key or were candidates).
    pub pages_scanned: usize,
    /// Dead source entries stripped from surviving pages.
    pub entries_stripped: usize,
    /// Pages deleted because their provenance was fully dead.
    pub pages_deleted: usize,
    /// Fully-orphaned pages captured but NOT yet deleted — they need one
    /// more consecutive sweep before removal (see `sweep_dead_sources`).
    pub orphans_pending: usize,
    /// Inbound `[[links]]` rewritten to plain text after a page deletion.
    pub links_cleaned: usize,
}

/// Stems of generated files the cascade never touches (same exemption as
/// the lint's OKF/E2 scans — `index.md` has no frontmatter by OKF §6,
/// `log.md` is the compiler journal).
const CASCADE_EXEMPT_STEMS: &[&str] = &["index", "log"];

/// Re-point pages citing `from` (the inbox path) at `to` (the durable
/// archive dest). Called right after [`super::pipeline`] archives a batch;
/// rewrites are txn-tracked, so a CAS rollback restores the old sources
/// list together with the rolled-back archive dest.
pub fn redirect_page_source(
    wiki_dir: &Path,
    from: &Path,
    to: &Path,
    track: &TransactionScope,
) -> Result<usize> {
    let from_str = from.to_string_lossy().to_string();
    let to_str = to.to_string_lossy().to_string();
    let redirected = std::cell::Cell::new(0usize);

    for_each_page(wiki_dir, &mut |path, content| {
        let sources = parse_sources(content)?;
        if !sources.iter().any(|s| s == &from_str) {
            return None;
        }
        let updated: Vec<String> = sources
            .iter()
            .map(|s| {
                if s == &from_str {
                    to_str.clone()
                } else {
                    s.clone()
                }
            })
            .collect();
        let new_content = write_sources(content, &updated);
        track.track_path(path).ok();
        redirected.set(redirected.get() + 1);
        Some(new_content)
    })
    .context("cascade redirect: walk wiki pages")?;
    Ok(redirected.into_inner())
}

/// Cycle-start sweep: strip dead source entries, delete fully-orphaned
/// machine-rendered pages, clean inbound links to deleted pages.
pub fn sweep_dead_sources(
    wiki_dir: &Path,
    track: &TransactionScope,
    iterations: Option<&crate::wiki::PageIterations>,
) -> Result<CascadeReport> {
    let mut report = CascadeReport::default();
    if !wiki_dir.is_dir() {
        return Ok(report);
    }

    // Pass 1: strip dead entries; collect fully-orphaned pages for deletion.
    let mut deleted: Vec<(String, String)> = Vec::new(); // (title, slug)
    let mut captured_paths: Vec<PathBuf> = Vec::new();
    for_each_page(wiki_dir, &mut |path, content| {
        report.pages_scanned += 1;
        let Some(sources) = parse_sources(content) else {
            return None; // hand-written page — protected.
        };
        if sources.is_empty() {
            return None; // provenance-less compile — keep, never delete.
        }
        let alive: Vec<String> = sources
            .iter()
            .filter(|s| Path::new(s.as_str()).exists())
            .cloned()
            .collect();
        let dead = sources.len() - alive.len();
        if dead == 0 {
            return None;
        }

        if alive.is_empty() {
            // Fully-orphaned machine page: OpenKB's orphan deletion.
            //
            // Two guards, both load-bearing — a bare delete here is
            // unrecoverable and can be catastrophic:
            //
            // 1. REVERSIBILITY. `TransactionScope::rollback` *deletes* every
            //    tracked path; it has no backup, so tracking a deleted page
            //    cannot restore it. Prior bytes must be captured into the
            //    iteration store BEFORE removal, which makes the delete
            //    recoverable through `zen wiki rollback`.
            // 2. TWO-SWEEP GRACE. A page must be observed fully-orphaned on
            //    two consecutive sweeps: the first only captures, the second
            //    deletes. `sources` are absolute vault paths, so a transiently
            //    unreachable vault (unmount, relocated ZEN_HOME, restored
            //    backup) makes *every* source read as dead. Without this
            //    grace that condition mass-deletes the compiled wiki.
            // With no store there is no way to record the observation, so
            // the grace cannot apply — fall back to deleting immediately
            // (legacy behaviour) rather than deferring forever. Production
            // always passes a store; `None` exists for callers that have
            // none.
            let seen_before = iterations
                .map(|store| !store.versions(wiki_dir, path).is_empty())
                .unwrap_or(true);
            let title = frontmatter_title(content).unwrap_or_else(|| path_title(path));
            let slug = super::wiki_compile::slugify(&title);

            if seen_before {
                if fs::remove_file(path).is_ok() {
                    report.pages_deleted += 1;
                    deleted.push((title, slug));
                    return Some(String::new()); // signal: file gone.
                }
                return None;
            }

            if let Some(store) = iterations {
                match store.capture(wiki_dir, path, "") {
                    Ok(Some(captured)) => {
                        captured_paths.push(captured.md_path);
                        if let Some(diff) = &captured.diff_path {
                            captured_paths.push(diff.clone());
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        warn!(
                            path = %path.display(),
                            error = %e,
                            "orphan capture failed — deferring delete to a later sweep"
                        );
                        return None;
                    }
                }
            }
            report.orphans_pending += 1;
            info!(
                path = %path.display(),
                "fully-orphaned page captured; delete deferred to the next sweep"
            );
            return None;
        }

        let new_content = write_sources(content, &alive);
        report.entries_stripped += dead;
        Some(new_content)
    })
    .context("cascade sweep: walk wiki pages")?;

    for path in &captured_paths {
        track.track_path(path)?;
    }

    if deleted.is_empty() {
        return Ok(report);
    }

    // Pass 2: clean inbound [[links]] to deleted pages (keep inner text).
    for_each_page(wiki_dir, &mut |path, content| {
        let links = crate::wiki::WikiPage::extract_wikilinks(content);
        let mut new_content = content.to_string();
        let mut changed = false;
        for target in links {
            let normalized = zen_repo::normalize_alias(&target);
            if deleted.iter().any(|(title, slug)| {
                normalized == zen_repo::normalize_alias(title) || normalized == *slug
            }) {
                new_content = new_content.replace(&format!("[[{target}]]"), &target);
                changed = true;
                report.links_cleaned += 1;
            }
        }
        if changed {
            track.track_path(path).ok();
            return Some(new_content);
        }
        None
    })
    .context("cascade sweep: link cleanup")?;

    info!(
        pages_scanned = report.pages_scanned,
        entries_stripped = report.entries_stripped,
        pages_deleted = report.pages_deleted,
        links_cleaned = report.links_cleaned,
        "cascade sweep complete"
    );
    Ok(report)
}

/// Visit every `.md` page under `wiki_dir` (recursively, generated stems
/// exempted). The callback returns `Some(new_content)` to rewrite the file
/// (tracked), `Some(String::new())` when the callback already deleted it,
/// or `None` to leave it untouched.
fn for_each_page(wiki_dir: &Path, f: &mut dyn FnMut(&Path, &str) -> Option<String>) -> Result<()> {
    let mut stack = vec![wiki_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if CASCADE_EXEMPT_STEMS.iter().any(|s| *s == stem) {
                continue;
            }
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            if let Some(new_content) = f(&path, &content) {
                if new_content.is_empty() {
                    continue; // callback deleted the file itself.
                }
                fs::write(&path, new_content)
                    .with_context(|| format!("cascade rewrite: {}", path.display()))?;
            }
        }
    }
    Ok(())
}

/// Parse the `sources: ["a", "b"]` frontmatter line — the exact inverse of
/// [`write_sources`]'s serialization. `None` when the page has no sources
/// key (hand-written marker).
fn parse_sources(content: &str) -> Option<Vec<String>> {
    let mut in_frontmatter = false;
    for line in content.lines() {
        let trimmed = line.trim_end();
        if trimmed == "---" {
            if in_frontmatter {
                break;
            }
            in_frontmatter = true;
            continue;
        }
        if !in_frontmatter {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("sources:") {
            let list = rest.trim();
            let inner = list
                .strip_prefix('[')
                .and_then(|l| l.strip_suffix(']'))
                .unwrap_or(list);
            let items: Vec<String> = inner
                .split("\", \"")
                .map(|item| item.trim().trim_matches('"').to_string())
                .filter(|item| !item.is_empty())
                .collect();
            return Some(items);
        }
    }
    None
}

/// Rewrite the `sources:` frontmatter line with the new list (the key
/// always exists when this is called — the caller parsed it first).
fn write_sources(content: &str, sources: &[String]) -> String {
    let serialized = sources
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let mut out = String::with_capacity(content.len());
    let mut in_frontmatter = false;
    let mut replaced = false;
    for line in content.lines() {
        let trimmed = line.trim_end();
        if trimmed == "---" {
            in_frontmatter = !in_frontmatter;
            out.push_str(trimmed);
            out.push('\n');
            continue;
        }
        if in_frontmatter && trimmed.starts_with("sources:") {
            out.push_str(&format!("sources: [{serialized}]\n"));
            replaced = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if replaced { out } else { content.to_string() }
}

/// The page's `title:` frontmatter value, if any.
fn frontmatter_title(content: &str) -> Option<String> {
    let mut in_frontmatter = false;
    for line in content.lines() {
        let trimmed = line.trim_end();
        if trimmed == "---" {
            if in_frontmatter {
                break;
            }
            in_frontmatter = true;
            continue;
        }
        if !in_frontmatter {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("title:") {
            return Some(rest.trim().trim_matches('"').to_string());
        }
    }
    None
}

/// Filename stem (the slug half of a deleted page's identity).
fn path_title(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn setup() -> (tempfile::TempDir, PathBuf, PathBuf, TransactionScope) {
        let dir = tempfile::tempdir().unwrap();
        let wiki = dir.path().join("wiki");
        let logs = dir.path().join("logs");
        fs::create_dir_all(&wiki).unwrap();
        fs::create_dir_all(&logs).unwrap();
        let txn = TransactionScope::new_for_logs(&logs, "cascade-test");
        txn.begin().unwrap();
        (dir, wiki, logs, txn)
    }

    fn write_page(wiki: &Path, name: &str, content: &str) -> PathBuf {
        let path = wiki.join(format!("{name}.md"));
        fs::write(&path, content).unwrap();
        path
    }

    const RENDERED: &str = "---\ntitle: \"Compiled\"\ncreated_at: \"2026-09-28\"\nupdated_at: \"2026-09-28\"\nwikipedia: []\nsources: [\"{src}\"]\n---\n\nCompiled body.\n";

    #[test]
    fn redirect_rewrites_sources_to_archive_dest() {
        let (_dir, wiki, _logs, txn) = setup();
        let inbox_note = wiki.parent().unwrap().join("inbox").join("n.md");
        let archive_note = wiki.parent().unwrap().join("archive/2026-09").join("n.md");
        let page = write_page(
            &wiki,
            "compiled",
            &RENDERED.replace("{src}", inbox_note.to_string_lossy().as_ref()),
        );

        let n = redirect_page_source(&wiki, &inbox_note, &archive_note, &txn).unwrap();
        assert_eq!(n, 1);
        let content = fs::read_to_string(&page).unwrap();
        assert!(
            content.contains(&archive_note.to_string_lossy().to_string()),
            "sources must cite the archive dest: {content}"
        );
        assert!(!content.contains("inbox"));
    }

    #[test]
    fn sweep_strips_dead_entry_and_keeps_partially_alive_page() {
        let (_dir, wiki, _logs, txn) = setup();
        let alive = wiki
            .parent()
            .unwrap()
            .join("archive/2026-09")
            .join("alive.md");
        fs::create_dir_all(alive.parent().unwrap()).unwrap();
        fs::write(&alive, "source").unwrap();
        let dead = wiki
            .parent()
            .unwrap()
            .join("archive/2025-01")
            .join("dead.md");
        let page = write_page(
            &wiki,
            "mixed",
            &format!(
                "---\ntitle: \"Mixed\"\ncreated_at: \"2026-09-28\"\nupdated_at: \"2026-09-28\"\nsources: [\"{}\", \"{}\"]\n---\n\nBody.\n",
                dead.display(),
                alive.display()
            ),
        );

        let report = sweep_dead_sources(&wiki, &txn, None).unwrap();
        assert_eq!(report.entries_stripped, 1);
        assert_eq!(report.pages_deleted, 0);
        let content = fs::read_to_string(&page).unwrap();
        assert!(content.contains(&alive.to_string_lossy().to_string()));
        assert!(!content.contains("2025-01"), "dead entry must be gone");
    }

    #[test]
    fn acceptance_dead_source_reference_count_reaches_zero() {
        let (_dir, wiki, _logs, txn) = setup();
        // Source exists at compile time, is deleted later (retention/user).
        let doomed = wiki
            .parent()
            .unwrap()
            .join("archive/2025-01")
            .join("doomed.md");
        let page = write_page(
            &wiki,
            "orphan",
            &format!(
                "---\ntitle: \"Orphan\"\ncreated_at: \"2026-09-28\"\nupdated_at: \"2026-09-28\"\nsources: [\"{}\"]\n---\n\nBody.\n",
                doomed.display()
            ),
        );
        let content_before = fs::read_to_string(&page).unwrap();
        assert!(content_before.contains(&doomed.to_string_lossy().to_string()));

        let report = sweep_dead_sources(&wiki, &txn, None).unwrap();
        assert_eq!(report.pages_deleted, 1);
        assert!(
            !page.exists(),
            "fully-orphaned machine page must be deleted"
        );

        let after = sweep_dead_sources(&wiki, &txn, None).unwrap();
        assert_eq!(after.pages_scanned, 0);
        let remaining: Vec<PathBuf> = fs::read_dir(&wiki)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect();
        assert!(
            !remaining.iter().any(|p| p == &page),
            "reference count to the deleted source must be zero"
        );
    }

    #[test]
    fn sweep_never_touches_hand_written_pages() {
        let (_dir, wiki, _logs, txn) = setup();
        let page = write_page(
            &wiki,
            "manual",
            "# My own note\n\nNo frontmatter, mentions [[Deleted Page]] freely.",
        );

        let report = sweep_dead_sources(&wiki, &txn, None).unwrap();
        assert_eq!(report.pages_deleted, 0);
        assert_eq!(
            fs::read_to_string(&page).unwrap(),
            "# My own note\n\nNo frontmatter, mentions [[Deleted Page]] freely."
        );
    }

    #[test]
    fn sweep_cleans_inbound_links_to_deleted_pages() {
        let (_dir, wiki, _logs, txn) = setup();
        let doomed = wiki
            .parent()
            .unwrap()
            .join("archive/2025-01")
            .join("doomed.md");
        write_page(
            &wiki,
            "victim",
            &format!(
                "---\ntitle: \"Victim\"\ncreated_at: \"2026-09-28\"\nupdated_at: \"2026-09-28\"\nsources: [\"{}\"]\n---\n\nGone.\n",
                doomed.display()
            ),
        );
        let live_src = wiki.parent().unwrap().join("live/source.md");
        fs::create_dir_all(live_src.parent().unwrap()).unwrap();
        fs::write(&live_src, "s").unwrap();
        let holder = write_page(
            &wiki,
            "holder",
            &format!(
                "---\ntitle: \"Holder\"\ncreated_at: \"2026-09-28\"\nupdated_at: \"2026-09-28\"\nsources: [\"{}\"]\n---\n\nSee [[Victim]] and [[Live Page]].\n",
                live_src.display()
            ),
        );

        let report = sweep_dead_sources(&wiki, &txn, None).unwrap();
        assert_eq!(report.pages_deleted, 1);
        assert_eq!(report.links_cleaned, 1, "only [[Victim]] counts");

        let content = fs::read_to_string(&holder).unwrap();
        assert!(content.contains("See Victim and [[Live Page]]."));
        assert!(!content.contains("[[Victim]]"));
    }

    /// A page whose sources are all unreachable must NOT be deleted on the
    /// first sweep. This is the mass-deletion guard: `sources` are absolute
    /// vault paths, so an unmounted or relocated vault makes every source
    /// read as dead.
    #[test]
    fn first_orphan_sweep_captures_without_deleting() {
        let (_dir, wiki, _logs, txn) = setup();
        let missing = wiki.parent().unwrap().join("archive/gone.md");
        let store = crate::wiki::PageIterations::new(wiki.parent().unwrap().join("iterations"));
        let page = write_page(
            &wiki,
            "orphan",
            &format!(
                "---\ntitle: \"Orphan\"\ncreated_at: \"2026-09-28\"\nupdated_at: \"2026-09-28\"\nsources: [\"{}\"]\n---\n\nBody.\n",
                missing.display()
            ),
        );

        let report = sweep_dead_sources(&wiki, &txn, Some(&store)).unwrap();
        assert_eq!(report.pages_deleted, 0, "first sweep must not delete");
        assert_eq!(report.orphans_pending, 1);
        assert!(page.exists(), "page must survive the first sweep");

        let captured = fs::read_to_string(&page).unwrap();
        assert!(captured.contains("Body."), "content must be intact");

        let second = sweep_dead_sources(&wiki, &txn, Some(&store)).unwrap();
        assert_eq!(second.pages_deleted, 1, "second sweep deletes");
        assert_eq!(second.orphans_pending, 0);
        assert!(!page.exists(), "page is gone after the grace elapsed");
    }

    /// The captured bytes must be restorable — this is what makes the
    /// delete recoverable at all, since txn rollback only deletes.
    #[test]
    fn orphan_delete_is_recoverable_from_the_iteration_store() {
        let (_dir, wiki, _logs, txn) = setup();
        let missing = wiki.parent().unwrap().join("archive/gone.md");
        let store = crate::wiki::PageIterations::new(wiki.parent().unwrap().join("iterations"));
        let page = write_page(
            &wiki,
            "orphan",
            &format!(
                "---\ntitle: \"Orphan\"\ncreated_at: \"2026-09-28\"\nupdated_at: \"2026-09-28\"\nsources: [\"{}\"]\n---\n\nPrecious body.\n",
                missing.display()
            ),
        );

        sweep_dead_sources(&wiki, &txn, Some(&store)).unwrap();
        sweep_dead_sources(&wiki, &txn, Some(&store)).unwrap();
        assert!(!page.exists());

        let versions = store.versions(&wiki, &page);
        assert!(!versions.is_empty(), "prior bytes must be recoverable");
        store.restore(&wiki, &versions[0]).unwrap();
        assert!(
            fs::read_to_string(&page)
                .unwrap()
                .contains("Precious body.")
        );
    }

    /// A vault that recovers between sweeps must leave the page alone — the
    /// pending capture is inert, never a deletion ticket.
    #[test]
    fn a_recovered_source_cancels_the_pending_delete() {
        let (_dir, wiki, _logs, txn) = setup();
        let src = wiki.parent().unwrap().join("archive/live.md");
        let store = crate::wiki::PageIterations::new(wiki.parent().unwrap().join("iterations"));
        let page = write_page(
            &wiki,
            "orphan",
            &format!(
                "---\ntitle: \"Orphan\"\ncreated_at: \"2026-09-28\"\nupdated_at: \"2026-09-28\"\nsources: [\"{}\"]\n---\n\nBody.\n",
                src.display()
            ),
        );

        // Source does not exist yet → first sweep only captures.
        assert_eq!(
            sweep_dead_sources(&wiki, &txn, Some(&store))
                .unwrap()
                .orphans_pending,
            1
        );

        fs::create_dir_all(src.parent().unwrap()).unwrap();
        fs::write(&src, "back").unwrap();

        let second = sweep_dead_sources(&wiki, &txn, Some(&store)).unwrap();
        assert_eq!(second.pages_deleted, 0);
        assert_eq!(second.orphans_pending, 0);
        assert!(page.exists(), "a live source must cancel the delete");
    }
}
