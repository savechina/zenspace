//! Cross-process-safe persistence for the FR-031 placeholder registry (T180).
//!
//! Two production writers save `<logs>/placeholders.json`: the distill
//! pipeline ([`crate::distill::pipeline`], end-of-cycle registry save) and the
//! zen-loop worker ([`crate::graph_verify::PlaceholderRegistry`] declarations
//! at stage 5b). A whole-file `save()` from either side is last-write-wins:
//! the loser's declarations are silently dropped. This module replaces that
//! with a **read-merge-write under an exclusive cross-process flock**.
//!
//! # Protocol
//!
//! 1. Take an exclusive lock on `<logs_dir>/placeholders.lock`
//!    (`std::fs::File::lock`, mirroring `zen-repo`'s migration lock: advisory,
//!    kernel-released if the holder dies, lock file persists).
//! 2. Load the current `<logs_dir>/placeholders.json`. Missing file ⇒ empty
//!    registry; corrupt/unreadable file ⇒ empty registry with a warning
//!    (fail-open, matching both call sites' existing load behaviour).
//! 3. Merge the caller's in-memory registry into the on-disk one (rule below).
//! 4. Persist the merged registry atomically
//!    ([`PlaceholderRegistry::save`] → `zen_core::atomic_file::write_atomic`:
//!    tmp → fsync → rename, so readers never observe a half-written file).
//! 5. Drop the guard, releasing the lock.
//!
//! # Merge rule (deterministic, reproducible from this doc alone)
//!
//! Union of slots keyed by placeholder slug. For a slug present on both sides:
//!
//! - **`status`**: the MORE-PROGRESSIONED status wins, ranked
//!   `Placeholder (0) < Claimed (1) < Merged (2)`. Equal rank ⇒ the incoming
//!   (caller's) status wins. A stale in-memory copy can therefore never
//!   regress a slot another writer already advanced.
//! - **`claimed_by`** (the only non-status field besides the slug key): the
//!   incoming entry wins unconditionally.
//! - Slugs present on only one side are carried over unchanged.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::warn;

use crate::distill::types::{GraphPlaceholder, PlaceholderStatus};
use crate::graph_verify::PlaceholderRegistry;

/// Registry file name under the logs directory.
pub const PLACEHOLDERS_FILE: &str = "placeholders.json";

/// Lock file name under the logs directory (persists between runs; the flock
/// itself is advisory and released by the kernel when the holder exits).
pub const PLACEHOLDERS_LOCK: &str = "placeholders.lock";

/// Exclusive-flock guard: unlocks on drop so the critical section is exactly
/// the read-merge-write scope (and a panic unwinding through it still
/// releases, as does process death via the kernel).
struct LockGuard {
    file: std::fs::File,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Block until the exclusive lock on `<logs_dir>/placeholders.lock` is held.
///
/// # Errors
/// Returns an error when the logs directory or lock file cannot be created/opened
/// or the flock syscall fails.
fn acquire_lock(logs_dir: &Path) -> Result<LockGuard> {
    std::fs::create_dir_all(logs_dir)
        .with_context(|| format!("creating logs dir {}", logs_dir.display()))?;
    let path = logs_dir.join(PLACEHOLDERS_LOCK);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening placeholder lock {}", path.display()))?;
    file.lock()
        .with_context(|| format!("locking {}", path.display()))?;
    Ok(LockGuard { file })
}

/// Progression rank used by the merge rule: higher = more progressed.
fn status_rank(status: PlaceholderStatus) -> u8 {
    match status {
        PlaceholderStatus::Placeholder => 0,
        PlaceholderStatus::Claimed => 1,
        PlaceholderStatus::Merged => 2,
    }
}

/// Resolve one colliding slug per the documented merge rule.
fn merge_slot(disk: &GraphPlaceholder, incoming: &GraphPlaceholder) -> GraphPlaceholder {
    let status = if status_rank(incoming.status) >= status_rank(disk.status) {
        incoming.status
    } else {
        disk.status
    };
    GraphPlaceholder {
        slug: incoming.slug.clone(),
        status,
        claimed_by: incoming.claimed_by.clone(),
    }
}

/// Pure merge of two registries (no I/O): union keyed by slug, collisions
/// resolved per the [module-level merge rule](self#merge-rule-deterministic-reproducible-from-this-doc-alone).
///
/// # Examples
///
/// ```
/// use zen_vault::distill::placeholders_store::merge_placeholders;
/// use zen_vault::distill::types::PlaceholderStatus;
/// use zen_vault::graph_verify::PlaceholderRegistry;
///
/// let mut disk = PlaceholderRegistry::new();
/// disk.declare("foreign", "agent-x");
/// let mut ours = PlaceholderRegistry::new();
/// ours.declare("mine", "agent-y");
///
/// let merged = merge_placeholders(&disk, &ours);
/// assert_eq!(merged.lookup("foreign").unwrap().status, PlaceholderStatus::Placeholder);
/// assert_eq!(merged.lookup("mine").unwrap().status, PlaceholderStatus::Placeholder);
/// ```
pub fn merge_placeholders(
    disk: &PlaceholderRegistry,
    ours: &PlaceholderRegistry,
) -> PlaceholderRegistry {
    let mut slots: HashMap<String, GraphPlaceholder> = disk.slots().clone();
    for (slug, incoming) in ours.slots() {
        match slots.get(slug) {
            Some(existing) => {
                let merged = merge_slot(existing, incoming);
                slots.insert(slug.clone(), merged);
            }
            None => {
                slots.insert(slug.clone(), incoming.clone());
            }
        }
    }
    PlaceholderRegistry::from_slots(slots)
}

/// Read-merge-write `placeholders.json` under the cross-process lock (T180).
///
/// Replaces a bare [`PlaceholderRegistry::save`] at every production call
/// site so concurrent writers (distill pipeline, zen-loop worker) can no
/// longer drop each other's declarations. Returns the merged registry that
/// was persisted, so callers can observe foreign slots that survived.
///
/// # Errors
/// Returns an error when the lock cannot be taken or the atomic write fails.
/// A missing or corrupt on-disk registry is NOT an error: it is treated as
/// empty (with a warning for the corrupt case), matching the fail-open load
/// behaviour of both call sites.
///
/// # Examples
///
/// ```no_run
/// use zen_vault::distill::placeholders_store::merge_save_placeholders;
/// use zen_vault::graph_verify::PlaceholderRegistry;
///
/// let mut ours = PlaceholderRegistry::new();
/// ours.declare("rust-async", "zen-loop");
/// let merged = merge_save_placeholders(std::path::Path::new("/tmp/zen-logs"), &ours)?;
/// assert!(merged.lookup("rust-async").is_some());
/// # Ok::<(), anyhow::Error>(())
/// ```
pub fn merge_save_placeholders(
    logs_dir: &Path,
    ours: &PlaceholderRegistry,
) -> Result<PlaceholderRegistry> {
    let _lock = acquire_lock(logs_dir)?;
    let path = registry_path(logs_dir);
    let disk = match PlaceholderRegistry::load(&path) {
        Ok(registry) => registry,
        Err(e) => {
            warn!(
                error = %e,
                path = %path.display(),
                "placeholder registry unreadable during merge-save — treating as empty"
            );
            PlaceholderRegistry::new()
        }
    };
    let merged = merge_placeholders(&disk, ours);
    merged
        .save(&path)
        .with_context(|| format!("merge-saving placeholder registry {}", path.display()))?;
    Ok(merged)
}

/// Absolute path of the registry file for a logs directory.
pub fn registry_path(logs_dir: &Path) -> PathBuf {
    logs_dir.join(PLACEHOLDERS_FILE)
}
