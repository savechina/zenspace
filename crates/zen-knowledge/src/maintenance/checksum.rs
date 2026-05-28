use std::fs;
use std::path::Path;

use anyhow::Result;
use sha2::{Digest, Sha256};
use tracing::info;

// ── Standalone functions (T084 API) ─────────────────────────────────

/// Compute a SHA-256 checksum of a file's contents and return it as a
/// lowercase hex string.
pub fn compute_file_checksum(file_path: &Path) -> Result<String> {
    let bytes = fs::read(file_path)?;
    let hash = Sha256::digest(&bytes);
    let hex: String = format!("{:x}", hash);
    Ok(hex)
}

/// Check whether the file at `file_path` differs from the previously
/// recorded `stored_checksum`. Returns `true` when the file is new or
/// modified (checksums differ).
pub fn needs_reindex(file_path: &Path, stored_checksum: &str) -> Result<bool> {
    let current = compute_file_checksum(file_path)?;
    let needs = current != stored_checksum;
    if needs {
        info!(
            "Change detected for {}: stored={:.8}… current={:.8}…",
            file_path.display(),
            stored_checksum,
            current
        );
    }
    Ok(needs)
}

/// Recompute the checksum for `file_path` and return the new hex string.
///
/// Callers can use this value to update their `notes_metadata.checksum`
/// column after a successful reindex.
pub fn update_checksum(file_path: &Path) -> Result<String> {
    let hash = compute_file_checksum(file_path)?;
    info!("Updated checksum for {}: {:.8}…", file_path.display(), hash);
    Ok(hash)
}

// ── Legacy struct (kept for backward compatibility) ────────────────

/// Checksum-based change detector backed by SHA-256 file hashing.
#[derive(Default)]
pub struct ChangeDetector;

impl ChangeDetector {
    pub fn new() -> Self {
        ChangeDetector
    }

    /// Compute a SHA-256 hex string for raw content (in-memory).
    ///
    /// Prefer [`compute_file_checksum`] for on-disk files.
    pub fn compute_checksum(content: &str) -> String {
        let hash = Sha256::digest(content.as_bytes());
        format!("{:x}", hash)
    }

    /// Return `true` when `current_checksum` and `stored_checksum` differ.
    ///
    /// Both values are expected to be lowercase hex strings produced by
    /// [`compute_checksum`] or [`compute_file_checksum`].
    pub fn needs_reindex(&self, current_checksum: &str, stored_checksum: &str) -> bool {
        let needs = current_checksum != stored_checksum;
        if needs {
            info!("Change detected: checksums differ");
        }
        needs
    }
}
