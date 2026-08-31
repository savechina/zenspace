//! Graph heterogeneous-topology Router & Join nodes (005-agentic-loop, T031, FR-030).
//!
//! FR-030 splits graph engineering into three node kinds; this module is the
//! **Router & Join** layer:
//!
//! - **Deterministic Nodes** (Code track): slug validation of file/package
//!   names plus frontmatter-aware note plumbing — hard-coded functions, no
//!   LLM. Heuristics are intentionally lightweight and stdlib-only; full AST
//!   parsing and git-commit mining arrive in Phase 8 (T044 CodeExtractor).
//! - **Agentic Nodes** (Paper / DailyNote tracks): semantic extraction
//!   delegated to the existing heuristic [`NotionExtractor`]; LLM enrichment
//!   rides on the distill pipeline's FR-003 stage, not here.
//! - **Join**: every extracted notion is upserted through [`NotionService`]
//!   with the exact same call the distillation pipeline uses
//!   (`pipeline.rs` stage "T003 NotionService persist") — no duplicated
//!   join logic.
//!
//! `worker_type` is the Phase 8 host-source override hook
//! (`zen-core/config.rs` `HostSource::worker_type`, FR-033):
//! `Some("code")` forces the deterministic Code track; `Some("doc")` forces
//! an agentic track (Paper heuristics first, falling back to DailyNote).
//! Host sources themselves are wired in Phase 8 — this router only honors
//! the override.

use std::path::Path;

use anyhow::Result;
use tracing::{info, warn};

use crate::distill::NotionExtractor;
use crate::note::Note;
use crate::notion::{Notion, NotionKind, NotionService};

/// Source-content track for Router & Join dispatch (FR-030).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceTrack {
    /// Source code → deterministic track (slug validation, name-derived
    /// entities only — no LLM).
    Code,
    /// Papers / long-form research docs → agentic track.
    Paper,
    /// Inbox markdown notes → agentic track (default for notes).
    DailyNote,
    /// Content type not recognized; router skips extraction.
    Unknown,
}

/// File extensions routed to the deterministic Code track.
const CODE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "pyi", "go", "java", "c", "h", "cpp",
    "hpp", "cc", "cxx", "swift", "kt", "kts", "rb", "php", "cs", "scala", "zig", "lua", "sh",
];

/// File extensions routed to the Paper track.
const PAPER_EXTENSIONS: &[&str] = &["pdf", "tex", "latex", "bib"];

/// File extensions treated as notes (DailyNote unless Paper heuristics hit).
const NOTE_EXTENSIONS: &[&str] = &["md", "markdown", "txt"];

/// Ancestor directory names that are layout noise, not a package name
/// (e.g. `repo/src/main.rs` — the package is `repo`, not `src`).
const COMMON_SOURCE_DIRS: &[&str] = &[
    "src", "lib", "source", "app", "cmd", "internal", "tests", "test", "examples", "bin", "target",
];

/// Classify an ingest source into a [`SourceTrack`] (deterministic,
/// stdlib-only heuristics per FR-030; AST-level classification is Phase 8).
///
/// Rules, in order:
/// 1. Code extension → [`SourceTrack::Code`]
/// 2. Paper extension (`.pdf`/`.tex`/…) → [`SourceTrack::Paper`]
/// 3. Note extension (`.md`/`.txt`) with an `# Abstract` heading →
///    [`SourceTrack::Paper`]; otherwise → [`SourceTrack::DailyNote`]
/// 4. Anything else → [`SourceTrack::Unknown`]
pub fn classify_source(path: &Path, content: &str) -> SourceTrack {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    if CODE_EXTENSIONS.contains(&ext.as_str()) {
        return SourceTrack::Code;
    }
    if PAPER_EXTENSIONS.contains(&ext.as_str()) {
        return SourceTrack::Paper;
    }
    if NOTE_EXTENSIONS.contains(&ext.as_str()) {
        if has_abstract_heading(content) {
            return SourceTrack::Paper;
        }
        return SourceTrack::DailyNote;
    }
    SourceTrack::Unknown
}

/// Paper heuristic: a `#`-prefixed heading whose text is exactly `abstract`
/// (case-insensitive). Matches `# Abstract` and `## abstract`; deliberately
/// does not match body sentences like "Abstract: ...".
fn has_abstract_heading(content: &str) -> bool {
    content.lines().any(|line| {
        let trimmed = line.trim();
        match trimmed.strip_prefix('#') {
            Some(rest) => rest
                .trim_start_matches('#')
                .trim()
                .eq_ignore_ascii_case("abstract"),
            None => false,
        }
    })
}

/// Deterministic-node slug legality check (shared FR-029/FR-030 convention):
/// non-empty lowercase kebab-case `[a-z0-9-]`, no leading/trailing hyphen,
/// no double hyphen, at least one alphanumeric character.
pub fn validate_slug(slug: &str) -> bool {
    if slug.is_empty() || slug.starts_with('-') || slug.ends_with('-') || slug.contains("--") {
        return false;
    }
    slug.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && slug.chars().any(|c| c.is_ascii_alphanumeric())
}

/// Deterministically derive a slug from a raw name: lowercase, map every
/// non-alphanumeric character to `-`, collapse runs, trim edges.
fn slugify(raw: &str) -> String {
    let mut slug: String = raw
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    slug.trim_matches('-').to_string()
}

/// Resolve the effective track: the `worker_type` override (FR-033,
/// `code|doc`) forces the lane; `None` (or any other value) falls through
/// to content classification.
pub fn resolve_track(source: &Path, content: &str, worker_type: Option<&str>) -> SourceTrack {
    match worker_type {
        Some("code") => SourceTrack::Code,
        Some("doc") => match classify_source(source, content) {
            // Doc workers always run an agentic lane; paper heuristics
            // refine to Paper, everything else lands on DailyNote.
            SourceTrack::Paper => SourceTrack::Paper,
            _ => SourceTrack::DailyNote,
        },
        _ => classify_source(source, content),
    }
}

/// Result of one Router & Join dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouterOutcome {
    /// Track the source was dispatched to.
    pub track: SourceTrack,
    /// Number of notions extracted by the specialized track.
    pub notions_extracted: usize,
    /// True when at least one notion was joined into the main graph.
    pub joined: bool,
    /// True when the deterministic (no-LLM) track produced the notions.
    pub deterministic: bool,
}

/// Router & Join node (FR-030): dispatches ingest content by type to a
/// specialized extractor, then joins all extracted notions into the main
/// wiki graph via the same [`NotionService`] calls the distill pipeline uses.
pub struct GraphRouter {
    extractor: NotionExtractor,
    notion_service: NotionService,
}

impl GraphRouter {
    /// Build a router over an injected [`NotionService`]. `NotionService::new()`
    /// matches the pipeline's field initialization (`pipeline.rs`).
    pub fn new(notion_service: NotionService) -> Self {
        Self {
            extractor: NotionExtractor::new(),
            notion_service,
        }
    }

    /// Route one ingest source and join its notions into the graph.
    ///
    /// - `client` is required for the Join stage (`NotionService::upsert_entity`
    ///   is the identical call the distill pipeline makes).
    /// - `worker_type` is the Phase 8 host-source override (`code`/`doc`).
    /// - `cycle_id` is used purely for log correlation.
    ///
    /// Errors only on extractor failure; per-notion join errors are logged
    /// and skipped (warn-and-continue, matching the pipeline's persistence
    /// stage). Unknown tracks skip extraction entirely.
    pub async fn route_and_join(
        &self,
        client: &zen_repo::SqliteClient,
        source: &Path,
        content: &str,
        worker_type: Option<&str>,
        cycle_id: &str,
    ) -> Result<RouterOutcome> {
        let track = resolve_track(source, content, worker_type);

        let (notions, deterministic) = match track {
            SourceTrack::Code => (self.extract_code_notions(source), true),
            SourceTrack::Paper | SourceTrack::DailyNote => {
                (self.extract_agentic_notions(source, content)?, false)
            }
            SourceTrack::Unknown => {
                warn!(
                    cycle_id,
                    source = %source.display(),
                    "Unknown source track — skipping extraction"
                );
                return Ok(RouterOutcome {
                    track,
                    notions_extracted: 0,
                    joined: false,
                    deterministic: false,
                });
            }
        };

        // Join stage — same NotionService upsert the distill pipeline uses;
        // idempotent on re-route, warn-and-continue per notion.
        let mut joined_count = 0usize;
        for notion in &notions {
            match self.notion_service.upsert_entity(client, notion).await {
                Ok(()) => joined_count += 1,
                Err(e) => warn!(
                    cycle_id,
                    notion = %notion.name,
                    error = %e,
                    "Router join failed for notion, continuing"
                ),
            }
        }

        info!(
            cycle_id,
            track = ?track,
            source = %source.display(),
            extracted = notions.len(),
            joined = joined_count,
            deterministic,
            "Router & Join complete"
        );

        Ok(RouterOutcome {
            track,
            notions_extracted: notions.len(),
            joined: joined_count > 0,
            deterministic,
        })
    }

    /// Deterministic Code track (no LLM): entities derived from the file
    /// stem and the nearest non-layout ancestor directory (package name)
    /// only. Each candidate is slug-validated before joining; metadata
    /// records the track for downstream filtering.
    fn extract_code_notions(&self, source: &Path) -> Vec<Notion> {
        let mut notions: Vec<Notion> = Vec::new();

        // Package name: nearest ancestor dir that is not layout noise.
        let package = source.ancestors().skip(1).find_map(|dir| {
            let name = dir.file_name()?.to_str()?;
            if COMMON_SOURCE_DIRS.contains(&name.to_ascii_lowercase().as_str()) {
                None
            } else {
                Some(name.to_string())
            }
        });
        if let Some(package) = package {
            push_deterministic(&mut notions, &package, NotionKind::Technology, source);
        }

        if let Some(stem) = source.file_stem().and_then(|s| s.to_str()) {
            push_deterministic(&mut notions, stem, NotionKind::Module, source);
        }
        notions
    }

    /// Agentic Paper/DailyNote track: semantic extraction delegated to the
    /// existing heuristic [`NotionExtractor`] (LLM enrichment happens in the
    /// distill pipeline's FR-003 stage, not here).
    fn extract_agentic_notions(&self, source: &Path, content: &str) -> Result<Vec<Notion>> {
        let note = Note {
            id: uuid::Uuid::now_v7().to_string(),
            source: "graph_router".to_string(),
            content: content.to_string(),
            file_path: Some(source.to_path_buf()),
            ..Note::default()
        };
        self.extractor.extract(&note)
    }
}

/// Append a slug-validated, name-deduplicated deterministic notion.
fn push_deterministic(notions: &mut Vec<Notion>, raw: &str, kind: NotionKind, source: &Path) {
    let slug = slugify(raw);
    if !validate_slug(&slug) || notions.iter().any(|n| n.name == slug) {
        return;
    }
    let mut notion = Notion::new(slug.clone(), kind, source.to_string_lossy());
    notion
        .metadata
        .insert("track".to_string(), "code".to_string());
    notion
        .metadata
        .insert("deterministic".to_string(), "true".to_string());
    notions.push(notion);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_code_extensions() {
        for ext in ["rs", "ts", "py", "go", "swift", "cpp"] {
            assert_eq!(
                classify_source(Path::new(&format!("foo.{ext}")), ""),
                SourceTrack::Code,
                "extension .{ext} must route to Code"
            );
        }
    }

    #[test]
    fn classify_paper_by_extension_and_abstract_heading() {
        assert_eq!(
            classify_source(Path::new("paper.pdf"), ""),
            SourceTrack::Paper
        );
        assert_eq!(
            classify_source(Path::new("paper.tex"), ""),
            SourceTrack::Paper
        );
        assert_eq!(
            classify_source(Path::new("note.md"), "# Abstract\nbody"),
            SourceTrack::Paper
        );
        assert_eq!(
            classify_source(Path::new("note.md"), "## abstract\nbody"),
            SourceTrack::Paper
        );
        // Body sentence is not a heading — stays a daily note.
        assert_eq!(
            classify_source(Path::new("note.md"), "Abstract: we did stuff"),
            SourceTrack::DailyNote
        );
    }

    #[test]
    fn classify_daily_note_and_unknown() {
        assert_eq!(
            classify_source(Path::new("2026-08-29.md"), "# Journal"),
            SourceTrack::DailyNote
        );
        assert_eq!(
            classify_source(Path::new("todo.txt"), "milk"),
            SourceTrack::DailyNote
        );
        assert_eq!(
            classify_source(Path::new("image.png"), ""),
            SourceTrack::Unknown
        );
        assert_eq!(
            classify_source(Path::new("noext"), "x"),
            SourceTrack::Unknown
        );
    }

    #[test]
    fn slug_validation_rules() {
        assert!(validate_slug("rust-async"));
        assert!(validate_slug("graph-router-2"));
        assert!(!validate_slug(""));
        assert!(!validate_slug("Rust")); // uppercase
        assert!(!validate_slug("rust async")); // space
        assert!(!validate_slug("-rust"));
        assert!(!validate_slug("rust-"));
        assert!(!validate_slug("rust--async"));
        assert!(!validate_slug("---"));
    }

    #[test]
    fn worker_type_override_forces_lane() {
        // code override forces Code even for markdown.
        assert_eq!(
            resolve_track(Path::new("note.md"), "# Journal", Some("code")),
            SourceTrack::Code
        );
        // doc override forces an agentic lane even for source files.
        assert_eq!(
            resolve_track(Path::new("main.rs"), "fn main(){}", Some("doc")),
            SourceTrack::DailyNote
        );
        // doc override preserves Paper heuristics.
        assert_eq!(
            resolve_track(Path::new("paper.tex"), "", Some("doc")),
            SourceTrack::Paper
        );
        // Unknown worker_type values fall through to classification.
        assert_eq!(
            resolve_track(Path::new("main.rs"), "", Some("bogus")),
            SourceTrack::Code
        );
        assert_eq!(
            resolve_track(Path::new("main.rs"), "", None),
            SourceTrack::Code
        );
    }

    #[tokio::test]
    async fn route_code_track_is_deterministic_and_joins() {
        let dir = tempfile::tempdir().unwrap();
        let client = zen_repo::SqliteClient::open(&dir.path().join("state.db"))
            .await
            .unwrap();
        let router = GraphRouter::new(NotionService::new());

        let outcome = router
            .route_and_join(
                &client,
                Path::new("repos/zenspace/src/graph_router.rs"),
                "fn main() {}",
                None,
                "cycle-test",
            )
            .await
            .unwrap();

        assert_eq!(outcome.track, SourceTrack::Code);
        assert!(outcome.deterministic);
        assert!(outcome.joined);
        // Package (zenspace) + module (graph-router) entities.
        assert!(outcome.notions_extracted >= 2);

        let names = router_names(&client).await;
        assert!(names.contains(&"zenspace".to_string()));
        assert!(names.contains(&"graph-router".to_string()));
    }

    #[tokio::test]
    async fn route_daily_note_track_is_agentic() {
        let dir = tempfile::tempdir().unwrap();
        let client = zen_repo::SqliteClient::open(&dir.path().join("state.db"))
            .await
            .unwrap();
        let router = GraphRouter::new(NotionService::new());

        let outcome = router
            .route_and_join(
                &client,
                Path::new("2026-08-29.md"),
                "# Rust study\nToday I learned rust and sqlite.",
                None,
                "cycle-test",
            )
            .await
            .unwrap();

        assert_eq!(outcome.track, SourceTrack::DailyNote);
        assert!(!outcome.deterministic);
        // Known-tech keyword extraction must have joined something.
        assert!(outcome.joined);
    }

    #[tokio::test]
    async fn route_unknown_track_skips() {
        let dir = tempfile::tempdir().unwrap();
        let client = zen_repo::SqliteClient::open(&dir.path().join("state.db"))
            .await
            .unwrap();
        let router = GraphRouter::new(NotionService::new());

        let outcome = router
            .route_and_join(&client, Path::new("logo.png"), "", None, "cycle-test")
            .await
            .unwrap();

        assert_eq!(outcome.track, SourceTrack::Unknown);
        assert_eq!(outcome.notions_extracted, 0);
        assert!(!outcome.joined);
    }

    /// Helper: all entity names currently in the graph.
    async fn router_names(client: &zen_repo::SqliteClient) -> Vec<String> {
        zen_repo::NotionsRepo::new(client)
            .load_all_entities()
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect()
    }
}
