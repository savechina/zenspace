//! Seed knowledge library — embedded static content for initial knowledge base.
//!
//! Contains 27 markdown files: 9 mental models, 11 behavioral anti-patterns,
//! 7 virtue domains. Embedded at compile time via `include_dir!`.

use include_dir::{include_dir, Dir};
use std::path::Path;

static SEED_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/seeds");

pub const SEED_FILE_COUNT: usize = 27;

/// Copies all seed knowledge files to the target workspace directory.
///
/// Creates subdirectories as needed:
/// - `wiki/wisdom/models/` — 9 mental models
/// - `wiki/wisdom/anti-patterns/` — 11 behavioral anti-patterns
/// - `wiki/virtues/` — 7 virtue domains
///
/// Returns the number of files written. Existing files are overwritten.
pub fn copy_seeds_to(target: &Path) -> anyhow::Result<usize> {
    SEED_DIR
        .extract(target)
        .map_err(|e| anyhow::anyhow!("Failed to extract seeds to {}: {}", target.display(), e))?;
    Ok(count_files(&SEED_DIR))
}

/// Returns the relative paths of all seed files (for inspection without extraction).
pub fn seed_file_paths() -> Vec<String> {
    collect_paths(&SEED_DIR, "")
}

fn count_files(dir: &Dir) -> usize {
    dir.entries()
        .iter()
        .map(|e| match e {
            include_dir::DirEntry::File(_) => 1,
            include_dir::DirEntry::Dir(d) => count_files(d),
        })
        .sum()
}

fn collect_paths(dir: &Dir, prefix: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for entry in dir.entries() {
        let name = entry.path().file_name().unwrap().to_string_lossy();
        let full_path = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        match entry {
            include_dir::DirEntry::File(_) => paths.push(full_path),
            include_dir::DirEntry::Dir(d) => paths.extend(collect_paths(d, &full_path)),
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_seed_dir_has_expected_count() {
        let count = count_files(&SEED_DIR);
        assert_eq!(count, SEED_FILE_COUNT);
    }

    #[test]
    fn test_copy_seeds_to_temp_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let count = copy_seeds_to(tmp.path()).unwrap();
        assert_eq!(count, SEED_FILE_COUNT);
    }

    #[test]
    fn test_seed_file_paths_all_categories() {
        let paths = seed_file_paths();
        assert_eq!(paths.len(), SEED_FILE_COUNT);
        assert!(
            paths.iter().any(|p| p.contains("models/")),
            "Missing mental model seeds"
        );
        assert!(
            paths.iter().any(|p| p.contains("anti-patterns/")),
            "Missing anti-pattern seeds"
        );
        assert!(
            paths.iter().any(|p| p.contains("virtues/")),
            "Missing virtue domain seeds"
        );
    }

    #[test]
    fn test_copy_seeds_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        copy_seeds_to(tmp.path()).unwrap();
        let count = copy_seeds_to(tmp.path()).unwrap();
        assert_eq!(count, SEED_FILE_COUNT);
    }

    #[test]
    fn test_extracted_file_has_content() {
        let tmp = tempfile::TempDir::new().unwrap();
        copy_seeds_to(tmp.path()).unwrap();
        let sample = std::fs::read_to_string(
            tmp.path().join("wiki/wisdom/models/first-principles.md"),
        )
        .unwrap();
        assert!(sample.contains("type: mental-model"));
        assert!(sample.contains("# First Principles Thinking"));
    }
}
