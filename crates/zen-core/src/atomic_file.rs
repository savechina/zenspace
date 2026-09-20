//! Atomic file replacement for artefacts that must never be observed
//! half-written.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Sibling temp path used during replacement.
fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Write `content` to `path` without ever exposing a partial file.
///
/// Writes a sibling temp file, fsyncs it, then renames over the target. A
/// reader therefore sees either the old bytes or the new bytes, never a
/// truncated mix — which matters for the small JSON artefacts that several
/// processes read (a half-written registry parses as corrupt and is discarded).
/// The rename is atomic within one filesystem; the parent directory is assumed
/// to exist.
pub fn write_atomic(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let tmp = tmp_path(path);
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(content)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}
