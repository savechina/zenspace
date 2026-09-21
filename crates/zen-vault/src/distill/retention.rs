//! Bounded-retention policy engine — caps the append-only filesystem homes
//! (review finding D2).
//!
//! Scope logic (Constitution XV):
//! - Functionality: applies the code-defined policy table (JSONL rotation,
//!   age-based deletes, keep-newest caps, report-only quarantine) to a
//!   [`ZenPaths`] tree and returns a serde-serializable [`RetentionReport`].
//! - User impact: without it every loop sidecar grows unbounded; with it the
//!   daily `RetentionWorker` (zen-agents scheduler) keeps each home within
//!   its window. Quarantined notes are user data — observed and reported,
//!   NEVER deleted under any config.
//! - Default: `[agentic.retention] enabled=true, dry_run=false`.
//! - Interaction: `dry_run=true` computes the identical report but mutates
//!   nothing; `enabled=false` makes [`apply_policies`] a no-op returning an
//!   empty report.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use tracing::warn;
use zen_core::config::RetentionConfig;
use zen_core::paths::ZenPaths;

const MIB: u64 = 1024 * 1024;

/// Per-home outcome of one sweep.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeReport {
    pub removed_files: u64,
    pub rotated: u64,
    pub bytes_freed: u64,
    /// Report-only observation (quarantine): files counted, never deleted.
    pub observed_files: u64,
    /// Report-only observation (quarantine): total bytes on disk.
    pub observed_bytes: u64,
}

/// Whole-sweep outcome: one entry per policy home (deterministic ordering).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RetentionReport {
    pub homes: BTreeMap<String, HomeReport>,
}

impl RetentionReport {
    pub fn removed(&self) -> u64 {
        self.homes.values().map(|h| h.removed_files).sum()
    }

    pub fn rotated(&self) -> u64 {
        self.homes.values().map(|h| h.rotated).sum()
    }

    pub fn bytes_freed(&self) -> u64 {
        self.homes.values().map(|h| h.bytes_freed).sum()
    }
}

enum Action {
    Rotate {
        max_bytes: u64,
        keep: u32,
    },
    DeleteOlder {
        /// Single-`*` glob on the file name; `None` matches every file.
        pattern: Option<&'static str>,
        days: i64,
        recursive: bool,
    },
    KeepNewest {
        pattern: &'static str,
        keep: usize,
    },
    ReportOnly,
}

struct Policy {
    home: &'static str,
    /// File path for [`Action::Rotate`], directory path otherwise.
    root: PathBuf,
    action: Action,
}

/// The policy table — code-defined defaults (no per-dir TOML overrides).
fn policies(paths: &ZenPaths) -> Vec<Policy> {
    let logs = paths.logs();
    let memory = paths.memory();
    let vault = paths.vault();
    vec![
        Policy {
            home: "logs/audit.jsonl",
            root: logs.join("audit.jsonl"),
            action: Action::Rotate {
                max_bytes: 10 * MIB,
                keep: 3,
            },
        },
        Policy {
            home: "logs/loop-gaps.jsonl",
            root: logs.join("loop-gaps.jsonl"),
            action: Action::Rotate {
                max_bytes: 5 * MIB,
                keep: 2,
            },
        },
        Policy {
            home: "logs/discovery-tree.jsonl",
            root: logs.join("discovery-tree.jsonl"),
            action: Action::Rotate {
                max_bytes: 5 * MIB,
                keep: 2,
            },
        },
        Policy {
            home: "logs/outbox",
            root: logs.join("outbox"),
            action: Action::DeleteOlder {
                pattern: Some("*.json"),
                days: 30,
                recursive: false,
            },
        },
        Policy {
            home: "logs/wake-up",
            root: logs.clone(),
            action: Action::DeleteOlder {
                pattern: Some("wake-up-*.md"),
                days: 14,
                recursive: false,
            },
        },
        Policy {
            home: "logs/archive",
            root: logs.join("archive"),
            action: Action::DeleteOlder {
                pattern: Some("*.md"),
                days: 365,
                recursive: false,
            },
        },
        Policy {
            home: "memories/journal",
            root: paths.journal_entries(),
            action: Action::DeleteOlder {
                pattern: None,
                days: 90,
                recursive: true,
            },
        },
        Policy {
            home: "memories/research-suggestions",
            root: memory.join("research-suggestions"),
            action: Action::DeleteOlder {
                pattern: Some("*.md"),
                days: 30,
                recursive: false,
            },
        },
        Policy {
            home: "memories/demoted-beliefs",
            root: memory.join("demoted-beliefs"),
            action: Action::KeepNewest {
                pattern: "*.md",
                keep: 200,
            },
        },
        Policy {
            // MemoryCurator routes virtue logs under vault/memories/virtue_logs
            // (route_typed_signals uses paths.vault()) — the growing home.
            home: "memories/virtue_logs",
            root: vault.join("memories/virtue_logs"),
            action: Action::DeleteOlder {
                pattern: None,
                days: 365,
                recursive: true,
            },
        },
        Policy {
            // ExpressWorker publishes weekly-review/blog drafts here.
            home: "vault/output",
            root: vault.join("output"),
            action: Action::DeleteOlder {
                pattern: Some("*.md"),
                days: 365,
                recursive: false,
            },
        },
        Policy {
            home: "vault/wiki/wisdom/suggestions",
            root: paths.wiki().join("wisdom/suggestions"),
            action: Action::DeleteOlder {
                pattern: Some("*.md"),
                days: 180,
                recursive: false,
            },
        },
        Policy {
            home: "vault/archive/quarantine",
            root: paths.archive().join("quarantine"),
            action: Action::ReportOnly,
        },
    ]
}

/// Apply the whole policy table to one `ZenPaths` tree.
///
/// Pure core of the retention subsystem: no config loading, no audit writes,
/// no clock reads — `now` is injected so tests are deterministic. Per-home
/// fail-open: an unreadable directory warns and never aborts the sweep.
/// `enabled=false` returns an empty report without touching the filesystem.
pub fn apply_policies(
    paths: &ZenPaths,
    cfg: &RetentionConfig,
    now: DateTime<Utc>,
) -> RetentionReport {
    let mut report = RetentionReport::default();
    if !cfg.enabled_or_default() {
        return report;
    }
    let dry_run = cfg.dry_run_or_default();
    for policy in policies(paths) {
        let home = run_policy(&policy, now, dry_run);
        report.homes.insert(policy.home.to_string(), home);
    }
    report
}

fn run_policy(policy: &Policy, now: DateTime<Utc>, dry_run: bool) -> HomeReport {
    match &policy.action {
        Action::Rotate { max_bytes, keep } => rotate(&policy.root, *max_bytes, *keep, dry_run),
        Action::DeleteOlder {
            pattern,
            days,
            recursive,
        } => delete_older(&policy.root, *pattern, *days, *recursive, now, dry_run),
        Action::KeepNewest { pattern, keep } => keep_newest(&policy.root, pattern, *keep, dry_run),
        Action::ReportOnly => report_only(&policy.root),
    }
}

/// Sibling path of a rotated log: `audit.jsonl` + n → `audit.jsonl.n`.
fn sibling(file: &Path, n: u32) -> PathBuf {
    let mut name = file.as_os_str().to_os_string();
    name.push(format!(".{n}"));
    PathBuf::from(name)
}

/// Rotate `file` when it has reached `max_bytes`, keeping `keep` numbered
/// siblings.
///
/// Crash safety: the cascade drops the oldest sibling first, then renames
/// `.keep-1 → .keep … .1 → .2`, and only LAST renames the live file to `.1`
/// and recreates an empty live file. A crash mid-cascade at worst loses one
/// rotated sibling — the live file is either still in place or already
/// safely at `.1`, never lost.
fn rotate(file: &Path, max_bytes: u64, keep: u32, dry_run: bool) -> HomeReport {
    let mut home = HomeReport::default();
    let size = match fs::metadata(file) {
        Ok(m) => m.len(),
        Err(_) => return home,
    };
    if size < max_bytes {
        return home;
    }
    home.rotated = 1;

    let oldest = sibling(file, keep);
    let oldest_len = fs::metadata(&oldest).map(|m| m.len()).unwrap_or(0);
    if dry_run {
        home.bytes_freed = oldest_len;
        return home;
    }

    if oldest_len > 0 || oldest.exists() {
        match fs::remove_file(&oldest) {
            Ok(()) => home.bytes_freed += oldest_len,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => warn!(
                path = %oldest.display(), error = %e,
                "retention: rotate drop-oldest failed (continuing)"
            ),
        }
    }
    for i in (1..keep).rev() {
        let from = sibling(file, i);
        if from.exists()
            && let Err(e) = fs::rename(&from, sibling(file, i + 1))
        {
            warn!(
                path = %from.display(), error = %e,
                "retention: rotate cascade rename failed (continuing)"
            );
        }
    }
    if let Err(e) = fs::rename(file, sibling(file, 1)) {
        warn!(
            path = %file.display(), error = %e,
            "retention: live-file rename failed; next sweep retries"
        );
        return home;
    }
    if let Err(e) = fs::File::create(file) {
        warn!(
            path = %file.display(), error = %e,
            "retention: could not recreate empty live file (appenders recreate on next write)"
        );
    }
    home
}

fn delete_older(
    dir: &Path,
    pattern: Option<&str>,
    days: i64,
    recursive: bool,
    now: DateTime<Utc>,
    dry_run: bool,
) -> HomeReport {
    let mut home = HomeReport::default();
    let cutoff = system_time_of(now - Duration::days(days));
    visit_files(dir, recursive, &mut |path| {
        if let Some(pat) = pattern {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if !matches_glob(name, pat) {
                return;
            }
        }
        let (mtime, len) = match stat_file(path) {
            Some(s) => s,
            None => return,
        };
        // Boundary: strictly older than `days` is deleted; exactly-N-days-old
        // (mtime == cutoff) is kept.
        if mtime >= cutoff {
            return;
        }
        if dry_run {
            home.removed_files += 1;
            home.bytes_freed += len;
            return;
        }
        match fs::remove_file(path) {
            Ok(()) => {
                home.removed_files += 1;
                home.bytes_freed += len;
            }
            Err(e) => warn!(
                path = %path.display(), error = %e,
                "retention: delete failed (file kept, continuing)"
            ),
        }
    });
    home
}

fn keep_newest(dir: &Path, pattern: &str, keep: usize, dry_run: bool) -> HomeReport {
    let mut home = HomeReport::default();
    let mut files: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    visit_files(dir, false, &mut |path| {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if !matches_glob(name, pattern) {
            return;
        }
        if let Some((mtime, len)) = stat_file(path) {
            files.push((mtime, len, path.to_path_buf()));
        }
    });
    if files.len() <= keep {
        return home;
    }
    // Newest first; deterministic path tie-break so equal-mtime files never
    // flip between runs.
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.2.cmp(&b.2)));
    for (_mtime, len, path) in files.iter().skip(keep) {
        if dry_run {
            home.removed_files += 1;
            home.bytes_freed += len;
            continue;
        }
        match fs::remove_file(path) {
            Ok(()) => {
                home.removed_files += 1;
                home.bytes_freed += len;
            }
            Err(e) => warn!(
                path = %path.display(), error = %e,
                "retention: keep-newest delete failed (file kept, continuing)"
            ),
        }
    }
    home
}

fn report_only(dir: &Path) -> HomeReport {
    let mut home = HomeReport::default();
    visit_files(dir, true, &mut |path| match fs::metadata(path) {
        Ok(m) => {
            home.observed_files += 1;
            home.observed_bytes += m.len();
        }
        Err(e) => warn!(
            path = %path.display(), error = %e,
            "retention: report-only stat failed (entry skipped)"
        ),
    });
    home
}

/// Walk `dir` invoking `visit` for every regular file (symlinks are never
/// followed). A missing directory is normal (the home simply never produced
/// files); any other read failure warns and skips — fail-open, one unreadable
/// home never aborts the sweep.
fn visit_files(dir: &Path, recursive: bool, visit: &mut dyn FnMut(&Path)) {
    if !dir.exists() {
        return;
    }
    if recursive {
        for entry in walkdir::WalkDir::new(dir).min_depth(1) {
            match entry {
                Ok(e) if e.file_type().is_file() => visit(e.path()),
                Ok(_) => {}
                Err(e) => warn!(
                    dir = %dir.display(), error = %e,
                    "retention: recursive walk entry skipped (fail-open)"
                ),
            }
        }
        return;
    }
    match fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries {
                match entry {
                    Ok(e) => {
                        let path = e.path();
                        if path.is_file() {
                            visit(&path);
                        }
                    }
                    Err(e) => warn!(
                        dir = %dir.display(), error = %e,
                        "retention: dir entry unreadable (skipped, fail-open)"
                    ),
                }
            }
        }
        Err(e) => warn!(
            dir = %dir.display(), error = %e,
            "retention: unreadable dir (home skipped, fail-open)"
        ),
    }
}

fn stat_file(path: &Path) -> Option<(SystemTime, u64)> {
    match fs::metadata(path) {
        Ok(m) => match m.modified() {
            Ok(mtime) => Some((mtime, m.len())),
            Err(e) => {
                warn!(
                    path = %path.display(), error = %e,
                    "retention: mtime unreadable (file kept, fail-open)"
                );
                None
            }
        },
        Err(e) => {
            warn!(
                path = %path.display(), error = %e,
                "retention: metadata unreadable (file kept, fail-open)"
            );
            None
        }
    }
}

fn system_time_of(t: DateTime<Utc>) -> SystemTime {
    SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(t.timestamp().max(0) as u64)
}

/// Minimal single-`*` glob — every pattern in the policy table has at most
/// one star (`*.json`, `wake-up-*.md`), so no regex engine or new dependency
/// is warranted. Patterns with more than one star are NOT supported.
fn matches_glob(name: &str, pattern: &str) -> bool {
    match pattern.split_once('*') {
        None => name == pattern,
        Some((prefix, suffix)) => {
            name.starts_with(prefix)
                && name.ends_with(suffix)
                && name.len() >= prefix.len() + suffix.len()
        }
    }
}
