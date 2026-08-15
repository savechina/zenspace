//! Tempfile lifecycle management (FR-040).
//!
//! Prevents `.bak` / `.tmp` files from accumulating when processes crash
//! mid-operation (`fs.edit`, the wiki writer, the scheduler). Two parts:
//!
//! 1. [`TempfileDropGuard`] — a RAII guard that removes a file when dropped,
//!    covering panics, early returns, and task cancellation.
//! 2. [`boot_time_sweep`] — walks the workspace for stale tempfile artifacts
//!    left behind by crashed operations and removes them.
//!
//! All cleanup here is best-effort: nothing in this module panics on error.

use std::path::{Path, PathBuf};

use tracing::warn;

/// File extensions treated as stale tempfile artifacts by [`boot_time_sweep`].
///
/// Matches the patterns emitted by `zen_plugin::tools::fs_edit` (`.bak` backup
/// and `.tmp` write target) and `zen_vault::wiki::writer` (`.tmp` write
/// target). Both tools append the marker as the *last* path extension (e.g.
/// `foo.md.bak`), so only the final extension is compared.
const STALE_TEMPFILE_EXTENSIONS: &[&str] = &["bak", "tmp"];

/// RAII guard that removes a file when dropped.
///
/// Create the guard immediately after creating a temp file, then call
/// [`disarm`](Self::disarm) once the temp file has been successfully
/// renamed/committed. If the enclosing function returns early, panics, or the
/// hosting task is cancelled, `Drop` runs and the stale temp file is removed.
///
/// Removal is best-effort: errors are logged but never propagated and the guard
/// never panics. The expected "file already gone" case (`NotFound`, e.g. when
/// the caller renamed the file without disarming) is silent.
pub struct TempfileDropGuard {
    path: PathBuf,
    armed: bool,
}

impl TempfileDropGuard {
    /// Create a guard for the given path. The file will be deleted when this
    /// guard is dropped, unless [`disarm`](Self::disarm) is called first.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            armed: true,
        }
    }

    /// Disarm the guard — the file will NOT be deleted on drop. Call this
    /// after the operation succeeds and the temp file has been renamed/committed.
    pub fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempfileDropGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            // NotFound is the expected case when the caller already renamed the
            // file without disarming — stay silent to avoid log noise.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                warn!(
                    path = %self.path.display(),
                    err = %e,
                    "failed to remove temp file during cleanup"
                );
            }
        }
    }
}

/// Sweep the workspace for stale tempfile artifacts from crashed operations.
///
/// Removes files whose final extension matches a known tool pattern:
/// - `*.bak` — `fs.edit` backup files
/// - `*.tmp` — `fs.edit` / wiki writer temp write files
///
/// Each removal is logged at `warn` level. The walk is recursive and
/// best-effort: unreadable subdirectories are skipped, individual removal
/// failures are logged but do not abort the sweep, and symlinks are never
/// followed (so the walk stays inside `workspace_root` and cannot cycle).
pub fn boot_time_sweep(workspace_root: &Path) {
    sweep_dir(workspace_root);
}

fn sweep_dir(dir: &Path) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            warn!(
                dir = %dir.display(),
                err = %e,
                "boot_time_sweep: cannot read directory, skipping"
            );
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!(
                    dir = %dir.display(),
                    err = %e,
                    "boot_time_sweep: cannot read directory entry, skipping"
                );
                continue;
            }
        };

        // `file_type` reads `d_type` without following the entry's symlink, so
        // symlinked directories are not recursed into.
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(e) => {
                warn!(
                    path = %entry.path().display(),
                    err = %e,
                    "boot_time_sweep: cannot stat entry, skipping"
                );
                continue;
            }
        };

        if file_type.is_symlink() {
            continue;
        }

        let path = entry.path();

        if file_type.is_dir() {
            sweep_dir(&path);
        } else if file_type.is_file() && is_stale_tempfile(&path) {
            warn!(
                path = %path.display(),
                "boot_time_sweep: removing stale tempfile artifact"
            );
            if let Err(e) = std::fs::remove_file(&path) {
                warn!(
                    path = %path.display(),
                    err = %e,
                    "boot_time_sweep: failed to remove stale tempfile"
                );
            }
        }
    }
}

/// Returns `true` if `path`'s final extension is a known stale-tempfile marker.
fn is_stale_tempfile(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| STALE_TEMPFILE_EXTENSIONS.contains(&ext))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn drop_guard_removes_file_when_armed() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("note.md.tmp");
        fs::write(&file, b"wip").unwrap();

        assert!(file.exists(), "sanity: file exists before guard drops");

        {
            let _guard = TempfileDropGuard::new(&file);
        }

        assert!(!file.exists(), "armed guard must remove file on drop");
    }

    #[test]
    fn drop_guard_keeps_file_after_disarm() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("note.md.tmp");
        fs::write(&file, b"wip").unwrap();

        {
            let mut guard = TempfileDropGuard::new(&file);
            guard.disarm();
        }

        assert!(file.exists(), "disarmed guard must NOT remove file");
    }

    #[test]
    fn drop_guard_does_not_panic_when_file_already_gone() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("missing.tmp");
        let _guard = TempfileDropGuard::new(&file);
    }

    #[test]
    fn drop_guard_accepts_string_and_pathbuf() {
        let _a = TempfileDropGuard::new("foo.tmp");
        let _b = TempfileDropGuard::new(PathBuf::from("bar.tmp"));
    }

    #[test]
    fn boot_time_sweep_removes_bak_and_tmp_files_recursively() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let bak = root.join("note1.md.bak");
        let tmp = root.join("note2.md.tmp");
        let nested_bak = root.join("sub").join("deep.bak");
        let nested_tmp = root.join("sub").join("nested").join("deep.tmp");
        fs::create_dir_all(root.join("sub").join("nested")).unwrap();
        fs::write(&bak, b"x").unwrap();
        fs::write(&tmp, b"x").unwrap();
        fs::write(&nested_bak, b"x").unwrap();
        fs::write(&nested_tmp, b"x").unwrap();

        let md = root.join("note3.md");
        let nested_md = root.join("sub").join("keep.md");
        fs::write(&md, b"x").unwrap();
        fs::write(&nested_md, b"x").unwrap();

        boot_time_sweep(root);

        assert!(!bak.exists(), "sweep must remove *.bak at root");
        assert!(!tmp.exists(), "sweep must remove *.tmp at root");
        assert!(!nested_bak.exists(), "sweep must recurse one level deep");
        assert!(!nested_tmp.exists(), "sweep must recurse arbitrarily deep");
        assert!(md.exists(), "sweep must leave regular .md alone");
        assert!(
            nested_md.exists(),
            "sweep must leave nested regular files alone"
        );
    }

    #[test]
    fn boot_time_sweep_leaves_non_temp_files_alone() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let keepers = ["readme.md", "data.json", "backup.txt", ".hidden"];
        for name in &keepers {
            fs::write(root.join(name), b"x").unwrap();
        }

        boot_time_sweep(root);

        for name in &keepers {
            assert!(root.join(name).exists(), "sweep must not touch {name}");
        }
    }

    #[test]
    fn boot_time_sweep_does_not_follow_symlinked_dirs() {
        let dir = tempdir().unwrap();
        // `root` is a subdirectory of the tempdir so files in the tempdir
        // itself are genuinely *outside* the swept tree.
        let root = dir.path().join("workspace");
        fs::create_dir_all(&root).unwrap();

        // Target file outside the swept subtree, exposed inside via a symlink.
        // The sweep must not follow the symlink, so this target must survive.
        let outside = dir.path().join("outside.md.tmp");
        fs::write(&outside, b"x").unwrap();

        let real_sub = root.join("real");
        fs::create_dir_all(&real_sub).unwrap();
        fs::write(real_sub.join("inside.md.bak"), b"x").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink(&outside, root.join("link.md.tmp")).unwrap();
        }

        boot_time_sweep(&root);

        assert!(
            !real_sub.join("inside.md.bak").exists(),
            "sweep must remove real nested tempfile"
        );
        assert!(
            outside.exists(),
            "sweep must not follow symlinks to files outside the tree"
        );
    }

    #[test]
    fn boot_time_sweep_handles_missing_root_gracefully() {
        // A non-existent root must not panic — just log and return.
        boot_time_sweep(Path::new("/nonexistent/zenspace-sweep-test-12345"));
    }

    #[test]
    fn stale_tempfile_predicate_matches_known_extensions() {
        assert!(is_stale_tempfile(Path::new("a.bak")));
        assert!(is_stale_tempfile(Path::new("a.md.bak")));
        assert!(is_stale_tempfile(Path::new("a.tmp")));
        assert!(is_stale_tempfile(Path::new("a.md.tmp")));
    }

    #[test]
    fn stale_tempfile_predicate_rejects_unrelated_paths() {
        assert!(!is_stale_tempfile(Path::new("a.md")));
        assert!(!is_stale_tempfile(Path::new("a.txt")));
        assert!(!is_stale_tempfile(Path::new("backup")));
        assert!(!is_stale_tempfile(Path::new(".hidden")));
        assert!(!is_stale_tempfile(Path::new("")));
    }
}
