use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tracing::warn;
use walkdir::WalkDir;

mod fusion;
mod service;
mod tier2;
mod tier3;
mod tier4;
mod tier5;
pub mod tier_selector;

pub use fusion::{FusedResult, RRF_K, RankedList, reciprocal_rank_fusion};
pub use service::SearchService;
pub use tier_selector::TierSelector;
pub use tier2::{Tier2Search, Tier2SearchTool};
pub use tier3::{Tier3Search, Tier3SearchTool};
pub use tier4::{GraphResult, Tier4Search, Tier4SearchTool};
pub use tier5::Tier5Search;

/// Result of a search operation (shared across tiers).
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub file: PathBuf,
    pub line: u32,
    pub content: String,
}

/// Wall-clock budget for a tier-1 scan (rg execution or in-process walk) —
/// prevents a hung search (e.g. over a network mount) from blocking the
/// caller indefinitely.
const TIER1_TIMEOUT: Duration = Duration::from_secs(10);

/// Explicit override for the ripgrep binary location, checked before PATH
/// and well-known install locations (same operational pattern as ort's
/// `ORT_DYLIB_PATH`).
const RG_PATH_ENV: &str = "ZEN_RG_PATH";

/// Well-known install locations probed when `rg` is not on the process
/// `PATH`. Launchd/agent daemons get a minimal PATH that regularly omits
/// Homebrew and cargo bin dirs even though ripgrep IS installed — the
/// production "failed to spawn ripgrep" incident (2026-10-16) was exactly
/// this: rg lived at /opt/homebrew/bin/rg, unreachable from the daemon env.
const RG_WELL_KNOWN: &[&str] = &["/opt/homebrew/bin/rg", "/usr/local/bin/rg"];

fn resolve_rg_binary_uncached() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os(RG_PATH_ENV) {
        let p = PathBuf::from(explicit);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(found) = which::which("rg") {
        return Some(found);
    }
    let cargo_rg = std::env::var_os("HOME").map(|h| {
        let mut p = PathBuf::from(h);
        p.push(".cargo");
        p.push("bin");
        p.push("rg");
        p
    });
    RG_WELL_KNOWN
        .iter()
        .map(PathBuf::from)
        .chain(cargo_rg)
        .find(|p| p.is_file())
}

fn resolve_rg_binary() -> Option<PathBuf> {
    static RESOLVED: OnceLock<Option<PathBuf>> = OnceLock::new();
    RESOLVED.get_or_init(resolve_rg_binary_uncached).clone()
}

/// Tier 1 search: exact keyword search — ripgrep when a binary can be
/// resolved (PATH, `ZEN_RG_PATH`, or well-known install locations), with an
/// environment-independent in-process scan as fallback.
///
/// Target latency: <50ms (FR-007) on the rg path; the fallback trades speed
/// for availability so the tier never disappears from daemon environments
/// that lack rg entirely.
pub struct Tier1Search;

impl Tier1Search {
    /// Search for an exact (case-sensitive, literal) keyword match.
    ///
    /// Returns matching lines across all files under `base_dir`, capped at
    /// `limit` TOTAL results.
    pub fn search(query: &str, base_dir: &Path, limit: usize) -> Result<Vec<SearchResult>> {
        if query.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        match resolve_rg_binary() {
            Some(rg) => search_via_rg(&rg, query, base_dir, limit),
            None => search_in_process(query, base_dir, limit),
        }
    }
}

/// Run ripgrep by ABSOLUTE path (immune to daemon PATH) and parse its
/// `path:line:content` output. A spawn failure falls back to the in-process
/// scan — the tier must not vanish because of the environment.
fn search_via_rg(
    rg: &Path,
    query: &str,
    base_dir: &Path,
    limit: usize,
) -> Result<Vec<SearchResult>> {
    let mut cmd = Command::new(rg);
    cmd.arg("--line-number")
        .arg("--no-heading")
        .arg("--color=never")
        .arg("--fixed-strings")
        .arg("--max-count")
        .arg(limit.to_string())
        .arg(query)
        .arg(
            base_dir
                .to_str()
                .context("base_dir contains invalid UTF-8")?,
        );

    let child = match cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            warn!(
                error = %e,
                rg = %rg.display(),
                "failed to spawn rg — falling back to in-process scan"
            );
            return search_in_process(query, base_dir, limit);
        }
    };

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });

    let output = rx
        .recv_timeout(TIER1_TIMEOUT)
        .context("ripgrep timed out")?
        .context("failed to execute ripgrep")?;

    if !output.status.success() && output.stdout.is_empty() {
        // ripgrep returns exit code 1 when no matches found — that's fine
        if output.status.code() == Some(1) {
            return Ok(Vec::new());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("ripgrep failed: {stderr}");
    }

    let stdout = String::from_utf8(output.stdout)?;
    // rg's --max-count caps per FILE; enforce the global limit post-hoc.
    let mut results: Vec<SearchResult> = stdout.lines().filter_map(parse_rg_line).collect();
    results.truncate(limit);
    Ok(results)
}

/// Parse a single ripgrep output line: `filepath:line_number:content`
fn parse_rg_line(line: &str) -> Option<SearchResult> {
    let (file_part, rest) = line.split_once(':')?;
    let (line_part, content) = rest.split_once(':')?;

    let file = PathBuf::from(file_part);
    let line: u32 = line_part.parse().ok()?;

    Some(SearchResult {
        file,
        line,
        content: content.to_string(),
    })
}

/// Environment-independent fallback: walkdir + literal substring match
/// (case-sensitive, hidden entries and binary files skipped — rg's
/// observable defaults), bounded by [`TIER1_TIMEOUT`] and capped at `limit`
/// total results.
fn search_in_process(query: &str, base_dir: &Path, limit: usize) -> Result<Vec<SearchResult>> {
    let deadline = Instant::now() + TIER1_TIMEOUT;
    let mut results = Vec::new();

    let walker = WalkDir::new(base_dir)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !is_hidden(e));

    for entry in walker {
        if Instant::now() >= deadline {
            anyhow::bail!("tier1 search timed out after {TIER1_TIMEOUT:?}");
        }
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let bytes = match std::fs::read(entry.path()) {
            Ok(b) => b,
            Err(_) => continue,
        };
        if bytes.contains(&0u8) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        for (idx, line) in text.lines().enumerate() {
            if line.contains(query) {
                results.push(SearchResult {
                    file: entry.path().to_path_buf(),
                    line: idx as u32 + 1,
                    content: line.to_string(),
                });
                if results.len() >= limit {
                    return Ok(results);
                }
            }
        }
    }

    Ok(results)
}

/// Hidden (dot-prefixed) entries are skipped — ripgrep's default behaviour.
fn is_hidden(entry: &walkdir::DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .is_some_and(|name| name.starts_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_tier1_search_finds_matches() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.txt");
        fs::write(&file_path, "hello world\nfoo bar\nhello again\n").unwrap();

        let results = Tier1Search::search("hello", tmp.path(), 50).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results[0].content.contains("hello world"));
        assert_eq!(results[0].line, 1);
        assert!(results[1].content.contains("hello again"));
        assert_eq!(results[1].line, 3);
    }

    #[test]
    fn test_tier1_search_no_matches_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.txt");
        fs::write(&file_path, "nothing here\n").unwrap();

        let results = Tier1Search::search("nonexistent", tmp.path(), 50).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_tier1_requires_no_external_binary() {
        // In-process scan: a query must succeed even when the process PATH is
        // stripped of rg entirely (the daemon-environment failure mode).
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.md"), "needle in vault\n").unwrap();

        // SAFETY: nextest isolates each test in its own process.
        let restored = std::env::var_os("PATH");
        unsafe { std::env::set_var("PATH", "") };
        let result = Tier1Search::search("needle", tmp.path(), 10);
        if let Some(p) = restored {
            unsafe { std::env::set_var("PATH", p) };
        } else {
            unsafe { std::env::remove_var("PATH") };
        }
        let results = result.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].line, 1);
    }

    #[test]
    fn test_tier1_skips_hidden_and_binary_and_caps_total() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.md"), "match\nmatch\nmatch\n").unwrap();
        fs::write(tmp.path().join(".hidden.md"), "match\n").unwrap();
        fs::create_dir(tmp.path().join(".git")).unwrap();
        fs::write(tmp.path().join(".git").join("cfg"), "match\n").unwrap();
        fs::write(tmp.path().join("bin.dat"), "match\0binary\n").unwrap();

        // Pin the in-process walker directly (search() may take the rg path).
        let all = search_in_process("match", tmp.path(), 50).unwrap();
        assert_eq!(all.len(), 3, "only a.md's three lines, got {all:?}");

        let capped = search_in_process("match", tmp.path(), 2).unwrap();
        assert_eq!(capped.len(), 2, "limit caps TOTAL results");

        // The public entry must agree regardless of which engine ran.
        let via_search = Tier1Search::search("match", tmp.path(), 50).unwrap();
        assert_eq!(via_search.len(), 3, "got {via_search:?}");
    }

    #[test]
    fn test_rg_and_walker_agree_when_rg_resolvable() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.md"), "needle one\nplain\nneedle two\n").unwrap();
        fs::create_dir(tmp.path().join("sub")).unwrap();
        fs::write(tmp.path().join("sub").join("b.md"), "needle three\n").unwrap();

        let Some(rg) = resolve_rg_binary() else {
            eprintln!("Skipping test: rg not resolvable");
            return;
        };
        let via_rg = search_via_rg(&rg, "needle", tmp.path(), 50).unwrap();
        let via_walker = search_in_process("needle", tmp.path(), 50).unwrap();
        assert_eq!(
            via_rg.len(),
            via_walker.len(),
            "rg={via_rg:?} walker={via_walker:?}"
        );
        // rg's parallel walker emits files in non-deterministic order —
        // compare as a sorted multiset.
        let mut rg_lines: Vec<(String, u32)> = via_rg
            .iter()
            .map(|r| {
                (
                    r.file.file_name().unwrap().to_string_lossy().into_owned(),
                    r.line,
                )
            })
            .collect();
        rg_lines.sort();
        let mut walker_lines: Vec<(String, u32)> = via_walker
            .iter()
            .map(|r| {
                (
                    r.file.file_name().unwrap().to_string_lossy().into_owned(),
                    r.line,
                )
            })
            .collect();
        walker_lines.sort();
        assert_eq!(rg_lines, walker_lines);
    }

    #[test]
    fn test_rg_resolution_env_override_wins() {
        // SAFETY: nextest isolates each test in its own process; the uncached
        // resolver reads env fresh by design.
        unsafe { std::env::set_var(RG_PATH_ENV, "/bin/cat") };
        let resolved = resolve_rg_binary_uncached();
        unsafe { std::env::remove_var(RG_PATH_ENV) };
        assert_eq!(resolved, Some(PathBuf::from("/bin/cat")));
    }

    #[test]
    fn test_parse_rg_line_valid() {
        let line = "src/main.rs:42:    let x = 1;";
        let result = parse_rg_line(line).unwrap();
        assert_eq!(result.file, PathBuf::from("src/main.rs"));
        assert_eq!(result.line, 42);
        assert_eq!(result.content, "    let x = 1;");
    }

    #[test]
    fn test_parse_rg_line_content_with_colons() {
        let line = "file.rs:10:key: value: extra";
        let result = parse_rg_line(line).unwrap();
        assert_eq!(result.file, PathBuf::from("file.rs"));
        assert_eq!(result.line, 10);
        assert_eq!(result.content, "key: value: extra");
    }

    #[test]
    fn test_domain_filter() {
        use super::service::filter_by_domain;
        let tmp = TempDir::new().unwrap();

        let work_file = tmp.path().join("work_note.md");
        fs::write(
            &work_file,
            "---\nid: \"w1\"\ntags: []\nsource: test\nsensitivity: private\ncreated_at: \"2026-01-01T00:00:00Z\"\nupdated_at: \"2026-01-01T00:00:00Z\"\ndomain: [work]\n---\nWork content",
        ).unwrap();

        let personal_file = tmp.path().join("personal_note.md");
        fs::write(
            &personal_file,
            "---\nid: \"p1\"\ntags: []\nsource: test\nsensitivity: private\ncreated_at: \"2026-01-01T00:00:00Z\"\nupdated_at: \"2026-01-01T00:00:00Z\"\ndomain: [personal]\n---\nPersonal content",
        ).unwrap();

        let results = vec![
            SearchResult {
                file: work_file.clone(),
                line: 1,
                content: "work".to_string(),
            },
            SearchResult {
                file: personal_file.clone(),
                line: 1,
                content: "personal".to_string(),
            },
        ];

        let filtered = filter_by_domain(results, "work").unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].file, work_file);

        let results2 = vec![
            SearchResult {
                file: work_file,
                line: 1,
                content: "work".to_string(),
            },
            SearchResult {
                file: personal_file,
                line: 1,
                content: "personal".to_string(),
            },
        ];
        let filtered2 = filter_by_domain(results2, "personal").unwrap();
        assert_eq!(filtered2.len(), 1);
    }
}
