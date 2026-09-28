//! E5 (compile-hygiene): compiled-wiki page versioning.
//!
//! The distill pipeline's recompile overwrites wiki pages in place. The
//! CAS snapshot (FR-032) protects against *external* drift only — the
//! pipeline itself clobbering a hand-tuned page left no trace. This store
//! captures the prior bytes of every page the pipeline is about to
//! modify, under `vault/iterations/`.
//!
//! The store lives OUTSIDE the wiki dir on purpose: `wiki_page_inventory`,
//! the compile whitelist, the lint walk and the CAS `VersionSnapshot` all
//! enumerate the wiki dir — iteration files inside it would poison every
//! one of those consumers (iteration snapshots would become whitelisted
//! link targets, lint subjects, and phantom CAS drift).
//!
//! Reuse discipline (no new mechanics beyond the capture itself):
//! - iteration writes go through [`zen_core::atomic_file::write_atomic`]
//! - callers `txn.track_path()` every captured file, so the existing CAS
//!   rollback and ④ crash replay clean up partial captures for free
//! - retention prunes the store via the existing [`crate::distill::retention`]
//!   `Action::DeleteOlder` policy engine (90 days, aligned with the journal)
//! - diffs via `diffy` (the same crate the `fs.edit` tool uses)
//!
//! Layout: `vault/iterations/<wiki-relative-dir>/<page-stem>/<unix-millis>.md`
//! mirroring the wiki structure, plus a `<unix-millis>.diff` sidecar
//! (prior version → the content the pipeline was about to write).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;

/// Versioned page-iteration store rooted at `vault/iterations/`.
pub struct PageIterations {
    root: PathBuf,
}

/// One captured prior version of a wiki page.
#[derive(Debug, Clone)]
pub struct CapturedIteration {
    /// Page path relative to the wiki dir (e.g. `coding/rust-guide.md`).
    pub page_rel: PathBuf,
    /// Stored prior version (restorable byte-exact).
    pub md_path: PathBuf,
    /// Unified diff (prior → next), when the store root is writable.
    pub diff_path: Option<PathBuf>,
}

/// A stored version of a page, as listed for the rollback surface.
#[derive(Debug, Clone)]
pub struct IterationVersion {
    /// Page path relative to the wiki dir.
    pub page_rel: PathBuf,
    /// Unix millis the version was captured at (also the file stem).
    pub millis: i64,
    /// Stored prior version file.
    pub md_path: PathBuf,
    /// Diff sidecar, when present.
    pub diff_path: Option<PathBuf>,
}

impl PageIterations {
    /// Create a store rooted at `root` (typically `vault/iterations`).
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Store root (for retention policy wiring and tests).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Capture the current bytes of `page_path` as one iteration, before
    /// the pipeline overwrites it with `next_content`.
    ///
    /// Returns `Ok(None)` — no capture — when the page does not exist yet
    /// (a create, nothing to protect), when the overwrite is byte-identical
    /// (nothing would change), or when the prior content is not readable
    /// UTF-8 (fail-open: a capture problem must never fail the cycle).
    ///
    /// # Errors
    /// Only when the capture itself cannot be persisted (mkdir/write
    /// failure). Callers track the returned files in the cycle txn so a
    /// CAS rollback or crash replay removes partial captures.
    pub fn capture(
        &self,
        wiki_dir: &Path,
        page_path: &Path,
        next_content: &str,
    ) -> Result<Option<CapturedIteration>> {
        let page_rel = match page_path.strip_prefix(wiki_dir) {
            Ok(rel) => rel.to_path_buf(),
            Err(_) => return Ok(None),
        };
        let prior = match std::fs::read_to_string(page_path) {
            Ok(c) => c,
            // Missing page = create; unreadable = fail-open (see doc).
            Err(_) => return Ok(None),
        };
        if prior == next_content {
            return Ok(None);
        }

        let version_dir = version_dir_for(&self.root, &page_rel);
        std::fs::create_dir_all(&version_dir)
            .with_context(|| format!("create iteration dir: {}", version_dir.display()))?;

        let mut millis = Utc::now().timestamp_millis();
        let md_path = loop {
            let candidate = version_dir.join(format!("{millis}.md"));
            if !candidate.exists() {
                break candidate;
            }
            millis += 1;
        };

        zen_core::atomic_file::write_atomic(&md_path, prior.as_bytes())
            .with_context(|| format!("write iteration: {}", md_path.display()))?;

        let patch = diffy::create_patch(&prior, next_content).to_string();
        let diff_path = if patch.is_empty() {
            None
        } else {
            let diff_path = md_path.with_extension("diff");
            match zen_core::atomic_file::write_atomic(&diff_path, patch.as_bytes()) {
                Ok(()) => Some(diff_path),
                // The .md capture is the restore source; the diff is a
                // review aid only — never fail the capture for it.
                Err(_) => None,
            }
        };

        Ok(Some(CapturedIteration {
            page_rel,
            md_path,
            diff_path,
        }))
    }

    /// Restore a stored version back onto its wiki page. The CURRENT page
    /// content is captured first, so a rollback is itself reversible.
    ///
    /// Returns `Ok(None)` when the page does not currently exist (nothing
    /// to protect) — the restore still proceeds.
    ///
    /// # Errors
    /// When the stored version is unreadable or the page write fails.
    pub fn restore(
        &self,
        wiki_dir: &Path,
        version: &IterationVersion,
    ) -> Result<Option<CapturedIteration>> {
        let restored = std::fs::read_to_string(&version.md_path)
            .with_context(|| format!("read iteration: {}", version.md_path.display()))?;
        let page_path = wiki_dir.join(&version.page_rel);
        let captured = self.capture(wiki_dir, &page_path, &restored)?;
        zen_core::atomic_file::write_atomic(&page_path, restored.as_bytes())
            .with_context(|| format!("restore page: {}", page_path.display()))?;
        Ok(captured)
    }

    /// All pages that have at least one stored iteration, as wiki-relative
    /// paths. Empty when the store does not exist yet.
    pub fn list_pages(&self) -> Vec<PathBuf> {
        let mut pages = Vec::new();
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "md")
                    && let Some(version_dir) = path.parent()
                    && let Ok(rel) = version_dir.strip_prefix(&self.root)
                {
                    let page = rel.with_extension("md");
                    if !pages.contains(&page) {
                        pages.push(page);
                    }
                }
            }
        }
        pages.sort();
        pages
    }

    /// Stored versions of one page, newest first. Empty when none.
    pub fn versions(&self, wiki_dir: &Path, page_path: &Path) -> Vec<IterationVersion> {
        let Ok(page_rel) = page_path.strip_prefix(wiki_dir) else {
            return Vec::new();
        };
        let version_dir = version_dir_for(&self.root, page_rel);
        let Ok(entries) = std::fs::read_dir(&version_dir) else {
            return Vec::new();
        };
        let mut versions: Vec<IterationVersion> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "md"))
            .filter_map(|md_path| {
                let millis = md_path.file_stem()?.to_str()?.parse::<i64>().ok()?;
                let diff_path = {
                    let candidate = md_path.with_extension("diff");
                    candidate.exists().then_some(candidate)
                };
                Some(IterationVersion {
                    page_rel: page_rel.to_path_buf(),
                    millis,
                    md_path,
                    diff_path,
                })
            })
            .collect();
        versions.sort_by_key(|v| std::cmp::Reverse(v.millis));
        versions
    }

    /// Resolve a page name (title, file stem, or wiki-relative path) to a
    /// wiki page that has iterations, using the same NFC normalization as
    /// the compile whitelist. Returns the absolute page path.
    pub fn resolve_page(&self, wiki_dir: &Path, name: &str) -> Option<PathBuf> {
        use zen_repo::normalize_alias;
        let wanted = normalize_alias(name);
        let wanted_spaced = normalize_alias(&name.replace('-', " "));
        self.list_pages()
            .into_iter()
            .find(|rel| {
                let stem = rel.file_stem().map(|s| s.to_string_lossy().to_string());
                let stem = match stem {
                    Some(s) => s,
                    None => return false,
                };
                let stem_norm = normalize_alias(&stem);
                let rel_norm = normalize_alias(&rel.to_string_lossy());
                stem_norm == wanted
                    || rel_norm == wanted
                    || normalize_alias(&stem.replace('-', " ")) == wanted
                    || stem_norm == wanted_spaced
            })
            .map(|rel| wiki_dir.join(rel))
    }
}

/// `root/<page-dir>/<stem>/<millis>.{md,diff}` — mirrors the wiki layout.
fn version_dir_for(root: &Path, page_rel: &Path) -> PathBuf {
    let stem = page_rel
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let parent = page_rel.parent().unwrap_or(Path::new(""));
    root.join(parent).join(stem)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, PageIterations, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let wiki = tmp.path().join("wiki");
        let store = PageIterations::new(tmp.path().join("iterations"));
        std::fs::create_dir_all(wiki.join("coding")).unwrap();
        (tmp, store, wiki)
    }

    #[test]
    fn capture_writes_prior_bytes_and_diff() {
        let (_tmp, store, wiki) = setup();
        let page = wiki.join("coding/rust-guide.md");
        std::fs::write(&page, "# v1\nhand-tuned\n").unwrap();

        let captured = store
            .capture(&wiki, &page, "# v2\npipeline\n")
            .unwrap()
            .expect("overwrite must be captured");
        assert_eq!(captured.page_rel, PathBuf::from("coding/rust-guide.md"));
        let stored = std::fs::read_to_string(&captured.md_path).unwrap();
        assert_eq!(stored, "# v1\nhand-tuned\n");
        let diff = std::fs::read_to_string(captured.diff_path.as_ref().unwrap()).unwrap();
        assert!(diff.contains("-hand-tuned"));
        assert!(diff.contains("+pipeline"));
    }

    #[test]
    fn capture_skips_missing_page_and_identical_overwrite() {
        let (_tmp, store, wiki) = setup();
        let page = wiki.join("coding/new.md");
        assert!(store.capture(&wiki, &page, "# new\n").unwrap().is_none());

        std::fs::write(&page, "# same\n").unwrap();
        assert!(store.capture(&wiki, &page, "# same\n").unwrap().is_none());
    }

    #[test]
    fn versions_newest_first_and_restore_is_reversible() {
        let (_tmp, store, wiki) = setup();
        let page = wiki.join("coding/rust-guide.md");
        std::fs::write(&page, "# v1\n").unwrap();
        store.capture(&wiki, &page, "# v2\n").unwrap().unwrap();
        std::fs::write(&page, "# v2\n").unwrap();
        store.capture(&wiki, &page, "# v3\n").unwrap().unwrap();

        let versions = store.versions(&wiki, &page);
        assert_eq!(versions.len(), 2);
        assert!(versions[0].millis >= versions[1].millis, "newest first");

        // Restore the oldest version; the v2 content must be captured
        // first so the rollback itself can be undone.
        let undone = store.restore(&wiki, &versions[1]).unwrap();
        assert!(undone.is_some(), "restore captures the clobbered version");
        assert_eq!(std::fs::read_to_string(&page).unwrap(), "# v1\n");
        assert_eq!(store.versions(&wiki, &page).len(), 3);
    }

    #[test]
    fn list_pages_and_resolve_page() {
        let (_tmp, store, wiki) = setup();
        let page = wiki.join("coding/rust-guide.md");
        std::fs::write(&page, "# v1\n").unwrap();
        store.capture(&wiki, &page, "# v2\n").unwrap().unwrap();

        assert_eq!(
            store.list_pages(),
            vec![PathBuf::from("coding/rust-guide.md")]
        );
        // Title-style, stem-style and spaced lookups all resolve.
        assert!(store.resolve_page(&wiki, "rust-guide").is_some());
        assert!(store.resolve_page(&wiki, "Rust Guide").is_some());
        assert!(store.resolve_page(&wiki, "coding/rust-guide.md").is_some());
        assert!(store.resolve_page(&wiki, "no-such-page").is_none());
    }
}
