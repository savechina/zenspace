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
//! NEVER touched. All mutations go through the cycle's
//! [`TransactionScope`] so CAS rollback (FR-032) and crash replay (④)
//! cover them. Sources values are code-managed (E2) — absolute vault
//! paths, so existence is a plain `Path::exists`.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use tracing::info;

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
pub fn sweep_dead_sources(wiki_dir: &Path, track: &TransactionScope) -> Result<CascadeReport> {
    let mut report = CascadeReport::default();
    if !wiki_dir.is_dir() {
        return Ok(report);
    }

    // Pass 1: strip dead entries; collect fully-orphaned pages for deletion.
    let mut deleted: Vec<(String, String)> = Vec::new(); // (title, slug)
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
            let title = frontmatter_title(content).unwrap_or_else(|| path_title(path));
            let slug = super::wiki_compile::slugify(&title);
            if fs::remove_file(path).is_ok() {
                report.pages_deleted += 1;
                deleted.push((title, slug));
                return Some(String::new()); // signal: file gone.
            }
            return None;
        }

        let new_content = write_sources(content, &alive);
        report.entries_stripped += dead;
        Some(new_content)
    })
    .context("cascade sweep: walk wiki pages")?;

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

        let report = sweep_dead_sources(&wiki, &txn).unwrap();
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

        let report = sweep_dead_sources(&wiki, &txn).unwrap();
        assert_eq!(report.pages_deleted, 1);
        assert!(
            !page.exists(),
            "fully-orphaned machine page must be deleted"
        );

        let after = sweep_dead_sources(&wiki, &txn).unwrap();
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

        let report = sweep_dead_sources(&wiki, &txn).unwrap();
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

        let report = sweep_dead_sources(&wiki, &txn).unwrap();
        assert_eq!(report.pages_deleted, 1);
        assert_eq!(report.links_cleaned, 1, "only [[Victim]] counts");

        let content = fs::read_to_string(&holder).unwrap();
        assert!(content.contains("See Victim and [[Live Page]]."));
        assert!(!content.contains("[[Victim]]"));
    }
}
