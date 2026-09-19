use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use tracing::{info, warn};

/// Ingests files from a raw directory into the notes workspace.
pub struct SourceIngester;

impl SourceIngester {
    pub fn new() -> Self {
        Self
    }

    /// Ingest files from `raw_dir`, copying markdown files into the notes
    /// inbox under `workspace/inbox/`. Non-markdown files are skipped with a
    /// warning.
    pub fn ingest(&self, raw_dir: &Path) -> Result<usize> {
        let mut count = 0;

        if !raw_dir.is_dir() {
            return Ok(0);
        }

        let inbox = raw_dir
            .parent()
            .map(|vault| vault.join("inbox"))
            .unwrap_or_else(|| raw_dir.to_path_buf());
        fs::create_dir_all(&inbox)
            .with_context(|| format!("create inbox dir: {}", inbox.display()))?;

        for entry in fs::read_dir(raw_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("txt") {
                let dest = inbox.join(
                    path.file_name()
                        .unwrap_or_else(|| std::ffi::OsStr::new("unknown")),
                );
                fs::copy(&path, &dest)
                    .with_context(|| format!("copy {} -> {}", path.display(), dest.display()))?;
                count += 1;
            } else {
                warn!(path = %path.display(), ext, "skipping non-ingestible file");
            }
        }

        info!(count, "files ingested from raw/ into inbox/");
        Ok(count)
    }

    /// Promote FR-033 host staging into the notes inbox (T043).
    ///
    /// Walks `inbox/_incoming/{host_hash}/`, moving every pending markdown/
    /// text file to `inbox/{host_hash}_{filename}` (hash prefix disambiguates
    /// same-named files from different hosts and from raw/ ingest copies).
    /// Promoted files are recorded under `_incoming/{host_hash}/promoted/` so
    /// a file is promoted exactly once even though the sweep re-stages nothing
    /// twice — the staging tree is both the sweep's seen-set and the
    /// promotion ledger.
    ///
    /// # Parameters
    /// - `inbox` — the workspace notes inbox (`vault/inbox/`).
    ///
    /// # Returns
    /// Map of `host_hash` → number of files promoted (for per-source audit).
    ///
    /// # Errors
    /// Returns `Ok` even when individual files fail — every per-file failure
    /// (write, ledger, rename, remove) is skipped warn-and-continue so one bad
    /// file never blocks the sweep or discards the other sources' promoted
    /// counts (T159). The only error path is an unreadable `_incoming` root,
    /// which returns an empty map.
    pub fn promote_incoming(&self, inbox: &Path) -> Result<HashMap<String, usize>> {
        let mut promoted: HashMap<String, usize> = HashMap::new();
        let incoming_root = inbox.join("_incoming");
        if !incoming_root.is_dir() {
            return Ok(promoted);
        }
        // T158: whole-file ingest ceiling — files above it are quarantined
        // (marker under `_incoming/{hash}/quarantined/`) rather than read into
        // memory, so a multi-GB staged file cannot OOM the daemon.
        let ceiling = zen_core::config::load_config()
            .map(|cfg| cfg.agentic.loop_cfg.max_ingest_bytes_or_default())
            .unwrap_or(64 * 1024 * 1024);

        let Ok(hash_dirs) = fs::read_dir(&incoming_root) else {
            warn!(
                path = %incoming_root.display(),
                "promote: cannot read _incoming staging root — skipped"
            );
            return Ok(promoted);
        };
        for hash_dir in hash_dirs.flatten() {
            let hash_path = hash_dir.path();
            if !hash_path.is_dir() {
                continue;
            }
            let Some(host_hash) = hash_path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let promoted_dir = hash_path.join("promoted");
            let quarantined_dir = hash_path.join("quarantined");
            let Ok(entries) = fs::read_dir(&hash_path) else {
                warn!(
                    path = %hash_path.display(),
                    "promote: cannot read staging dir — skipped"
                );
                continue;
            };
            for entry in entries.flatten() {
                let staged = entry.path();
                if !staged.is_file() {
                    continue;
                }
                let ext = staged.extension().and_then(|e| e.to_str()).unwrap_or("");
                if !(ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("txt")) {
                    continue;
                }
                let Some(file_name) = staged.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let is_txt = ext.eq_ignore_ascii_case("txt");
                // .txt files are converted to frontmatter-wrapped .md at promote
                // time so the downstream distill inbox scan (ext == "md" only)
                // picks them up as notes.
                let dest_name = if is_txt {
                    let stem = staged
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("note");
                    format!("{host_hash}_{stem}.md")
                } else {
                    format!("{host_hash}_{file_name}")
                };
                let dest = inbox.join(&dest_name);
                let ledger_path = promoted_dir.join(file_name);
                // T159: defer only when the promotion is COMMITTED (dest AND
                // ledger both present — the note is awaiting distill). A dest
                // without a ledger is an incomplete promotion from a prior
                // partial write: re-promote it rather than stall forever.
                if dest.exists() && ledger_path.exists() {
                    warn!(file = %dest_name, "incoming promotion target exists, deferring");
                    continue;
                }
                fs::create_dir_all(&promoted_dir).ok();
                // T158: stat-and-skip above the ceiling; the quarantine marker
                // is the sweep's seen-set entry so the file is not re-staged
                // and re-warned every cycle.
                if let Ok(meta) = fs::metadata(&staged)
                    && meta.len() > ceiling
                {
                    fs::create_dir_all(&quarantined_dir).ok();
                    let marker = quarantined_dir.join(file_name);
                    if !marker.exists() {
                        fs::write(&marker, b"").ok();
                    }
                    warn!(
                        file = %file_name,
                        size = meta.len(),
                        ceiling,
                        "promote: file exceeds ingest size ceiling — quarantined"
                    );
                    continue;
                }
                if is_txt {
                    // Read content; non-UTF-8 files are skipped (warn, no
                    // ledger entry, no move — consistent with skip semantics).
                    let content = match fs::read_to_string(&staged) {
                        Ok(c) => c,
                        Err(e) => {
                            warn!(
                                file = %file_name,
                                error = %e,
                                "skipping non-UTF-8 txt file during promote"
                            );
                            continue;
                        }
                    };
                    // T159: durable destination FIRST, ledger after — a
                    // partial write leaves dest-without-ledger, which the next
                    // cycle re-promotes instead of stalling forever.
                    let now = Utc::now().to_rfc3339();
                    let id = uuid::Uuid::now_v7().to_string();
                    let md_content = format!(
                        "---\nid: \"{id}\"\ntags: [\"host-import\"]\nsource: \"host:{host_hash}\"\nsource_id: null\nsensitivity: private\ncreated_at: \"{now}\"\nupdated_at: \"{now}\"\ndomain: []\nproject: null\n---\n\n{content}"
                    );
                    if let Err(e) = fs::write(&dest, md_content) {
                        warn!(
                            file = %file_name,
                            error = %e,
                            "promote: write converted note failed — skipped"
                        );
                        continue;
                    }
                    if let Err(e) = fs::copy(&staged, &ledger_path) {
                        warn!(
                            file = %file_name,
                            error = %e,
                            "promote: ledger record failed — skipped (will retry)"
                        );
                        continue;
                    }
                    if let Err(e) = fs::remove_file(&staged) {
                        warn!(
                            file = %file_name,
                            error = %e,
                            "promote: remove staged txt failed — promotion committed"
                        );
                    }
                } else {
                    // md: rename staged → dest FIRST (durable), then ledger.
                    if let Err(e) = fs::rename(&staged, &dest) {
                        warn!(
                            file = %file_name,
                            error = %e,
                            "promote: rename failed — skipped"
                        );
                        continue;
                    }
                    if let Err(e) = fs::copy(&dest, &ledger_path) {
                        warn!(
                            file = %file_name,
                            error = %e,
                            "promote: ledger record failed — promotion committed"
                        );
                    }
                }
                *promoted.entry(host_hash.to_string()).or_insert(0) += 1;
            }
        }

        let total: usize = promoted.values().sum();
        if total > 0 {
            info!(total, "host staging promoted into inbox/");
        }
        Ok(promoted)
    }
}

impl Default for SourceIngester {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::note::parse_frontmatter;
    use zen_core::types::Sensitivity;

    #[test]
    fn promote_incoming_moves_md_and_converts_txt_to_md() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        let staging = inbox.join("_incoming").join("ab12cd34");
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("report.md"), "# Report").unwrap();
        fs::write(staging.join("data.txt"), "raw text").unwrap();
        fs::write(staging.join("skip.bin"), "\x00\x01").unwrap();

        let promoted = SourceIngester::new().promote_incoming(&inbox).unwrap();
        assert_eq!(promoted.get("ab12cd34"), Some(&2));

        assert!(inbox.join("ab12cd34_report.md").is_file());
        assert_eq!(
            fs::read_to_string(inbox.join("ab12cd34_report.md")).unwrap(),
            "# Report"
        );

        assert!(!inbox.join("ab12cd34_data.txt").exists());
        let converted = fs::read_to_string(inbox.join("ab12cd34_data.md")).unwrap();
        assert!(converted.starts_with("---\n"));
        assert!(converted.contains("tags: [\"host-import\"]"));
        assert!(converted.contains("source: \"host:ab12cd34\""));
        let parsed = parse_frontmatter(&converted).unwrap();
        assert_eq!(parsed.content, "raw text");
        assert_eq!(parsed.tags, vec!["host-import".to_string()]);
        assert_eq!(parsed.source, "host:ab12cd34");
        // uuid v7 starts with "01"
        assert!(parsed.id.starts_with("01"));

        assert!(!inbox.join("ab12cd34_skip.bin").exists());
        assert!(!staging.join("report.md").exists());
        assert!(!staging.join("data.txt").exists());
        assert_eq!(
            fs::read_to_string(staging.join("promoted").join("report.md")).unwrap(),
            "# Report"
        );
        assert_eq!(
            fs::read_to_string(staging.join("promoted").join("data.txt")).unwrap(),
            "raw text"
        );

        // Second run: nothing left pending — no double promotion.
        let again = SourceIngester::new().promote_incoming(&inbox).unwrap();
        assert!(again.is_empty());
    }

    #[test]
    fn promote_incoming_txt_frontmatter_parses_correctly() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        let staging = inbox.join("_incoming").join("deadbeef");
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("note.txt"), "Hello world").unwrap();

        SourceIngester::new().promote_incoming(&inbox).unwrap();

        let md = fs::read_to_string(inbox.join("deadbeef_note.md")).unwrap();
        let note = parse_frontmatter(&md).unwrap();
        assert_eq!(note.content, "Hello world");
        assert!(!note.id.is_empty());
        assert_eq!(note.sensitivity, Sensitivity::Private);
        assert!(note.domain.is_empty());
        assert!(note.project.is_none());
    }

    #[test]
    fn promote_incoming_defers_when_inbox_target_exists() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        let staging = inbox.join("_incoming").join("ef987654");
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("note.md"), "v2").unwrap();
        fs::write(inbox.join("ef987654_note.md"), "v1 still pending distill").unwrap();
        // A COMMITTED promotion awaiting distill has both dest AND ledger —
        // only then does the promote defer (T159).
        fs::create_dir_all(staging.join("promoted")).unwrap();
        fs::write(staging.join("promoted").join("note.md"), "v1").unwrap();

        let promoted = SourceIngester::new().promote_incoming(&inbox).unwrap();
        assert!(promoted.is_empty());
        assert!(staging.join("note.md").is_file());
        assert_eq!(
            fs::read_to_string(inbox.join("ef987654_note.md")).unwrap(),
            "v1 still pending distill"
        );
    }

    #[test]
    fn promote_incoming_txt_defers_when_converted_target_exists() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        let staging = inbox.join("_incoming").join("cafe0001");
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("note.txt"), "v2 text").unwrap();
        fs::write(inbox.join("cafe0001_note.md"), "v1 still pending").unwrap();
        // Committed promotion awaiting distill: dest AND ledger both present.
        fs::create_dir_all(staging.join("promoted")).unwrap();
        fs::write(staging.join("promoted").join("note.txt"), "v1 text").unwrap();

        let promoted = SourceIngester::new().promote_incoming(&inbox).unwrap();
        assert!(promoted.is_empty());
        assert!(staging.join("note.txt").is_file());
    }

    #[test]
    fn promote_incoming_non_utf8_txt_skipped_no_panic() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        let staging = inbox.join("_incoming").join("bad00001");
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("binary.txt"), b"\xff\xfe\x00\x01invalid utf8").unwrap();

        let promoted = SourceIngester::new().promote_incoming(&inbox).unwrap();
        assert!(promoted.is_empty());
        assert!(!inbox.join("bad00001_binary.md").exists());
        assert!(staging.join("binary.txt").is_file());
        assert!(!staging.join("promoted").join("binary.txt").exists());
    }

    #[test]
    fn promote_incoming_noop_without_staging_dir() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        let promoted = SourceIngester::new().promote_incoming(&inbox).unwrap();
        assert!(promoted.is_empty());
    }

    #[test]
    fn promote_incoming_skips_and_quarantines_oversized_files() {
        // SAFETY: test-only env mutation; ZEN_LOOP_MAX_INGEST_BYTES is read by
        // no sibling test in this binary and is removed at the end of the test.
        unsafe { std::env::set_var("ZEN_LOOP_MAX_INGEST_BYTES", "1024") };
        zen_core::config::invalidate_config_cache();
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        let staging = inbox.join("_incoming").join("ab12cd34");
        fs::create_dir_all(&staging).unwrap();
        // 2 MiB > the clamped 1 MiB minimum ceiling — must be stat-and-skipped
        // (never read into memory) and quarantined with a marker the sweep's
        // seen-set honours, not promoted.
        let big = staging.join("huge.txt");
        fs::write(&big, vec![b'x'; 2 * 1024 * 1024]).unwrap();

        let promoted = SourceIngester::new().promote_incoming(&inbox).unwrap();
        assert!(promoted.is_empty());
        assert!(!inbox.join("ab12cd34_huge.md").exists());
        assert!(staging.join("huge.txt").is_file());
        assert!(staging.join("quarantined").join("huge.txt").exists());

        unsafe { std::env::remove_var("ZEN_LOOP_MAX_INGEST_BYTES") };
        zen_core::config::invalidate_config_cache();
    }

    #[test]
    fn promote_incoming_recovers_partial_write_dest_without_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        let staging = inbox.join("_incoming").join("ab12cd34");
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("note.txt"), "fresh content").unwrap();
        // Simulate a prior partial write: dest exists (stale/partial) but the
        // ledger record is absent — the promotion was never committed. The
        // next cycle must re-promote, never stall.
        fs::write(inbox.join("ab12cd34_note.md"), "partial garbage").unwrap();

        let promoted = SourceIngester::new().promote_incoming(&inbox).unwrap();
        assert_eq!(promoted.get("ab12cd34"), Some(&1));
        let md = fs::read_to_string(inbox.join("ab12cd34_note.md")).unwrap();
        assert!(md.contains("fresh content"), "dest must be re-promoted");
        assert!(!md.contains("partial garbage"));
        assert!(
            staging.join("promoted").join("note.txt").exists(),
            "ledger must now be present"
        );
        assert!(!staging.join("note.txt").exists(), "staged file consumed");
    }
}
