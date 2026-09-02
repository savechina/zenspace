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
//! T044 adds the named dual-track extractors — [`CodeExtractor`]
//! (deterministic, index-only provenance pages, no vault copy) and
//! [`DocExtractor`] (semantic extraction with products limited to M2 Fact /
//! M3 Concept; Belief arises exclusively via FR-025) — plus frontmatter
//! stamping of `workspace_id` + `worker_type` on preserved raw copies.

use std::path::Path;

use anyhow::{Context, Result};
use tracing::{info, warn};

use zen_core::config::HostSourceContext;

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

/// True when the file's extension routes to a known track (Code/Paper/Note).
/// Host-source sweeps use this to avoid reading arbitrary host binaries.
pub fn is_routable_extension(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    CODE_EXTENSIONS.contains(&ext.as_str())
        || PAPER_EXTENSIONS.contains(&ext.as_str())
        || NOTE_EXTENSIONS.contains(&ext.as_str())
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
    code_extractor: CodeExtractor,
    doc_extractor: DocExtractor,
    notion_service: NotionService,
}

impl GraphRouter {
    /// Build a router over an injected [`NotionService`]. `NotionService::new()`
    /// matches the pipeline's field initialization (`pipeline.rs`).
    pub fn new(notion_service: NotionService) -> Self {
        Self {
            code_extractor: CodeExtractor::new(),
            doc_extractor: DocExtractor::new(),
            notion_service,
        }
    }

    /// Route one ingest source and join its notions into the graph.
    ///
    /// - `client` is required for the Join stage (`NotionService::upsert_entity`
    ///   is the identical call the distill pipeline makes).
    /// - `host` is the Phase 8 FR-033 host-source context (`worker_type`
    ///   override + governance metadata stamping + doc-product kind filter);
    ///   `None` routes by content classification only.
    /// - `provenance_root` — vault root for index-only code-track provenance
    ///   pages (`{provenance_root}/{para_target}/host-{hash}-{slug}.md`);
    ///   pages are written only when both `host` and the root are present
    ///   and the source declares a `para_target`.
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
        host: Option<&HostSourceContext>,
        provenance_root: Option<&Path>,
        cycle_id: &str,
    ) -> Result<RouterOutcome> {
        let worker_type = host.and_then(|h| h.worker_type).map(|k| k.as_str());
        let track = resolve_track(source, content, worker_type);

        let (notions, deterministic) = match track {
            SourceTrack::Code => {
                let notions = self.code_extractor.extract(source, content, host)?;
                if let (Some(host), Some(root)) = (host, provenance_root)
                    && let Some(para) = &host.para_target
                    && let Err(e) = self.code_extractor.write_provenance_page(
                        &root.join(para),
                        source,
                        content,
                        host,
                    )
                {
                    warn!(
                        cycle_id,
                        source = %source.display(),
                        error = %e,
                        "provenance page write failed (extraction continues)"
                    );
                }
                (notions, true)
            }
            SourceTrack::Paper | SourceTrack::DailyNote => {
                (self.doc_extractor.extract(source, content, host)?, false)
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

/// Stamp host-governance metadata (`workspace_id`, `worker_type`, host hash,
/// sensitivity) onto every extracted notion (FR-033 tag-for-filtering).
fn stamp_host_metadata(notions: &mut [Notion], host: &HostSourceContext) {
    for notion in notions.iter_mut() {
        if let Some(ws) = &host.workspace_id {
            notion
                .metadata
                .insert("workspace_id".to_string(), ws.clone());
        }
        if let Some(kind) = host.worker_type {
            notion
                .metadata
                .insert("worker_type".to_string(), kind.as_str().to_string());
        }
        notion
            .metadata
            .insert("host_hash".to_string(), host.host_hash.clone());
        notion
            .metadata
            .insert("sensitivity".to_string(), host.sensitivity.to_string());
        if let Some(tier) = &host.m_tier {
            notion.metadata.insert("m_tier".to_string(), tier.clone());
        }
    }
}

/// Inject `key: value` pairs into a document's YAML frontmatter, creating a
/// frontmatter block when the document has none. Existing keys win — only
/// absent keys are inserted (host originals are never modified; this is
/// applied to vault copies only).
fn stamp_frontmatter(content: &str, tags: &[(String, String)]) -> String {
    if tags.is_empty() {
        return content.to_string();
    }
    let mut stamped = String::new();
    if let Some(rest) = content.strip_prefix("---\n")
        && let Some(end) = rest.find("\n---")
    {
        let (existing, tail) = rest.split_at(end);
        stamped.push_str("---\n");
        stamped.push_str(existing);
        stamped.push('\n');
        for (k, v) in tags {
            if !existing.contains(&format!("{k}:")) {
                stamped.push_str(&format!("{k}: {v}\n"));
            }
        }
        // `tail` begins with the closing "\n---" fence — append verbatim.
        stamped.push_str(tail);
        return stamped;
    }
    stamped.push_str("---\n");
    for (k, v) in tags {
        stamped.push_str(&format!("{k}: {v}\n"));
    }
    stamped.push_str("---\n\n");
    stamped.push_str(content);
    stamped
}

/// Notion kinds permitted as doc-track products (FR-033: M2 Fact / M3
/// Concept ONLY — Belief arises exclusively via FR-025 Bayesian evidence,
/// never direct doc extraction). `Other` is the flat-fact stand-in for the
/// M2 tier; everything code-specific (Technology/Module/Decision/…) stays
/// on the code track or the full agentic pipeline.
fn is_doc_track_kind(kind: &NotionKind) -> bool {
    matches!(kind, NotionKind::Concept | NotionKind::Other)
}

/// SHA-256 hex checksum (first 16 chars) for provenance records.
fn content_checksum(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(content.as_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// FR-033 Code track (T044): deterministic, no-LLM extraction with
/// index-only provenance — the vault records *about* host files
/// (`host_path` + checksum), never copies their content.
pub struct CodeExtractor;

impl CodeExtractor {
    pub fn new() -> Self {
        Self
    }

    /// Deterministic extraction for one host source file: slug-validated
    /// package/module notions (no LLM, no FS writes).
    ///
    /// # Parameters
    /// - `source` — host file path (read in place; never copied).
    /// - `content` — file contents (unused by the current slug heuristics,
    ///   kept for interface parity / future AST parsing).
    /// - `host` — host-source context; stamps governance metadata when present.
    ///
    /// # Returns
    /// Slug-validated, name-deduplicated notions.
    ///
    /// # Errors
    /// Currently infallible; `Result` keeps extractor parity for Phase 8
    /// AST/git mining.
    pub fn extract(
        &self,
        source: &Path,
        _content: &str,
        host: Option<&HostSourceContext>,
    ) -> Result<Vec<Notion>> {
        let mut notions = self.deterministic_notions(source);
        if let Some(host) = host {
            stamp_host_metadata(&mut notions, host);
        }
        Ok(notions)
    }

    /// Write the index-only provenance page for one host source file under
    /// `para_dir` (the source's configured PARA bucket inside the vault).
    ///
    /// Scope logic: page name `host-{host_hash}-{slug}.md` is idempotent
    /// (rewrites in place); `host_path` + checksum give FTS-indexable
    /// provenance without GB-scale copies (FR-033 index-only).
    ///
    /// # Errors
    /// FS write failures bubble up; callers warn-and-continue.
    pub fn write_provenance_page(
        &self,
        para_dir: &Path,
        source: &Path,
        content: &str,
        host: &HostSourceContext,
    ) -> Result<std::path::PathBuf> {
        let stem = source
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unnamed");
        let slug = slugify(stem);
        let page_name = format!("host-{}-{slug}.md", host.host_hash);
        let page_path = para_dir.join(page_name);
        let frontmatter = format!(
            "type: reference\nworker_type: code\nhost_path: {}\nhost_hash: {}\nchecksum: {}\n",
            source.to_string_lossy(),
            host.host_hash,
            content_checksum(content)
        );
        let mut page = String::from("---\n");
        page.push_str(&frontmatter);
        if let Some(ws) = &host.workspace_id {
            page.push_str(&format!("workspace_id: {ws}\n"));
        }
        page.push_str(&format!("sensitivity: {}\n", host.sensitivity));
        if let Some(tier) = &host.m_tier {
            page.push_str(&format!("m_tier: {tier}\n"));
        }
        page.push_str("---\n\n");
        page.push_str(&format!("# {stem}\n\n"));
        page.push_str(
            "Indexed in place (FR-033 code track, index-only): source content is not copied into the vault.\n",
        );
        std::fs::create_dir_all(para_dir)
            .with_context(|| format!("create provenance dir: {}", para_dir.display()))?;
        std::fs::write(&page_path, page)
            .with_context(|| format!("write provenance page: {}", page_path.display()))?;
        Ok(page_path)
    }

    /// Package + module notions shared with the router's legacy path.
    fn deterministic_notions(&self, source: &Path) -> Vec<Notion> {
        let mut notions: Vec<Notion> = Vec::new();
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
}

impl Default for CodeExtractor {
    fn default() -> Self {
        Self::new()
    }
}

/// FR-033 Doc track (T044): semantic extraction with raw preservation.
///
/// Extraction delegates to the existing heuristic [`NotionExtractor`] (LLM
/// enrichment rides the distill pipeline's FR-003 stage); products are
/// filtered to M2 Fact / M3 Concept. [`DocExtractor::ensure_raw_copy`]
/// preserves the original read-only under `vault/raw/{host_hash}/` with
/// `workspace_id` + `worker_type` frontmatter stamps.
pub struct DocExtractor {
    extractor: NotionExtractor,
}

impl DocExtractor {
    pub fn new() -> Self {
        Self {
            extractor: NotionExtractor::new(),
        }
    }

    /// Semantic extraction for one host doc source, products limited to
    /// M2 Fact / M3 Concept ([`is_doc_track_kind`]).
    ///
    /// # Parameters
    /// - `source` — file being extracted (vault raw copy or host path).
    /// - `content` — document contents.
    /// - `host` — host context; stamps governance metadata when present.
    ///
    /// # Returns
    /// Filtered, stamped notions.
    ///
    /// # Errors
    /// Propagates [`NotionExtractor`] failures.
    pub fn extract(
        &self,
        source: &Path,
        content: &str,
        host: Option<&HostSourceContext>,
    ) -> Result<Vec<Notion>> {
        let note = Note {
            id: uuid::Uuid::now_v7().to_string(),
            source: "graph_router".to_string(),
            content: content.to_string(),
            file_path: Some(source.to_path_buf()),
            ..Note::default()
        };
        let extracted = self.extractor.extract(&note)?;
        // The M2 Fact / M3 Concept product limit is a HOST-governance rule —
        // unhosted callers keep the full agentic extraction set.
        let mut notions: Vec<Notion> = if host.is_some() {
            extracted
                .into_iter()
                .filter(|n| is_doc_track_kind(&n.kind))
                .collect()
        } else {
            extracted
        };
        if let Some(host) = host {
            stamp_host_metadata(&mut notions, host);
        }
        Ok(notions)
    }

    /// Preserve one host file read-only under `raw_root/{host_hash}/`,
    /// stamping `worker_type` + `workspace_id` + `source_path` provenance
    /// frontmatter into the VAULT COPY (the host original is untouched).
    /// Idempotent: an existing same-name copy is left as-is.
    ///
    /// # Errors
    /// FS create/write failures bubble up; callers warn-and-continue.
    pub fn ensure_raw_copy(
        &self,
        raw_root: &Path,
        host: &HostSourceContext,
        source: &Path,
        content: &str,
    ) -> Result<std::path::PathBuf> {
        let dir = raw_root.join(&host.host_hash);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create raw host dir: {}", dir.display()))?;
        let file_name = source
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_else(|| std::ffi::OsString::from("unnamed.md"));
        let dest = dir.join(&file_name);
        if dest.exists() {
            return Ok(dest);
        }
        let mut tags: Vec<(String, String)> = vec![(
            "worker_type".to_string(),
            host.worker_type
                .map(|k| k.as_str().to_string())
                .unwrap_or_else(|| "doc".to_string()),
        )];
        if let Some(ws) = &host.workspace_id {
            tags.push(("workspace_id".to_string(), ws.clone()));
        }
        tags.push((
            "source_path".to_string(),
            source.to_string_lossy().into_owned(),
        ));
        tags.push(("sensitivity".to_string(), host.sensitivity.to_string()));
        let stamped = stamp_frontmatter(content, &tags);
        std::fs::write(&dest, stamped)
            .with_context(|| format!("write raw copy: {}", dest.display()))?;
        Ok(dest)
    }
}

impl Default for DocExtractor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zen_core::config::{HostRawPolicy, HostWorkerKind};

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
            .route_and_join(&client, Path::new("logo.png"), "", None, None, "cycle-test")
            .await
            .unwrap();

        assert_eq!(outcome.track, SourceTrack::Unknown);
        assert_eq!(outcome.notions_extracted, 0);
        assert!(!outcome.joined);
    }

    fn host_context(worker: HostWorkerKind, policy: HostRawPolicy) -> HostSourceContext {
        HostSourceContext {
            host_path: "/tmp/host-demo".into(),
            host_hash: "ab12cd34".into(),
            worker_type: Some(worker),
            raw_policy: policy,
            sensitivity: zen_core::types::Sensitivity::Private,
            allow_cloud: false,
            para_target: Some("resources".into()),
            m_tier: Some("M3".into()),
            workspace_id: Some("ws-demo".into()),
        }
    }

    #[test]
    fn doc_extractor_limits_products_to_m2_fact_m3_concept() {
        let extractor = DocExtractor::new();
        let content = "rust and sqlite notes\n\n## Design Patterns\n\nmore text here";
        let all = extractor
            .extract(Path::new("notes.md"), content, None)
            .unwrap();
        assert!(
            all.iter().any(|n| n.kind == NotionKind::Technology),
            "unfiltered extraction should include Technology kinds"
        );

        let host = host_context(HostWorkerKind::Doc, HostRawPolicy::Copy);
        let filtered = extractor
            .extract(Path::new("notes.md"), content, Some(&host))
            .unwrap();
        assert!(
            filtered.iter().all(|n| is_doc_track_kind(&n.kind)),
            "doc products must be M2 Fact / M3 Concept only, got {:?}",
            filtered.iter().map(|n| n.kind.clone()).collect::<Vec<_>>()
        );
        assert!(
            !filtered.is_empty(),
            "content should still yield at least one Concept/Fact product"
        );
        let doc = filtered.first().unwrap();
        assert_eq!(
            doc.metadata.get("worker_type").map(String::as_str),
            Some("doc")
        );
        assert_eq!(
            doc.metadata.get("workspace_id").map(String::as_str),
            Some("ws-demo")
        );
        assert_eq!(
            doc.metadata.get("host_hash").map(String::as_str),
            Some("ab12cd34")
        );
        assert_eq!(
            doc.metadata.get("sensitivity").map(String::as_str),
            Some("Private")
        );
    }

    #[test]
    fn code_extractor_stamps_host_metadata_without_llm() {
        let extractor = CodeExtractor::new();
        let host = host_context(HostWorkerKind::Code, HostRawPolicy::IndexOnly);
        let notions = extractor
            .extract(
                Path::new("/repos/zenspace/src/graph_router.rs"),
                "fn main() {}",
                Some(&host),
            )
            .unwrap();
        assert!(notions.len() >= 2);
        for notion in &notions {
            assert_eq!(
                notion.metadata.get("worker_type").map(String::as_str),
                Some("code")
            );
            assert_eq!(
                notion.metadata.get("workspace_id").map(String::as_str),
                Some("ws-demo")
            );
            assert_eq!(
                notion.metadata.get("m_tier").map(String::as_str),
                Some("M3")
            );
        }
    }

    #[test]
    fn code_extractor_provenance_page_is_index_only() {
        let dir = tempfile::tempdir().unwrap();
        let para_dir = dir.path().join("resources");
        let extractor = CodeExtractor::new();
        let host = host_context(HostWorkerKind::Code, HostRawPolicy::IndexOnly);

        let page = extractor
            .write_provenance_page(
                &para_dir,
                Path::new("/repos/demo/main.rs"),
                "fn main(){}",
                &host,
            )
            .unwrap();

        assert_eq!(
            page.file_name().unwrap().to_str().unwrap(),
            "host-ab12cd34-main.md"
        );
        let body = std::fs::read_to_string(&page).unwrap();
        assert!(body.contains("host_path: /repos/demo/main.rs"));
        assert!(body.contains("worker_type: code"));
        assert!(body.contains("checksum: "));
        assert!(body.contains("workspace_id: ws-demo"));
        // Index-only: page is tiny provenance, never the source content.
        assert!(!body.contains("fn main"));
    }

    #[test]
    fn doc_extractor_raw_copy_is_stamped_and_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let raw_root = dir.path().join("raw");
        let host = host_context(HostWorkerKind::Doc, HostRawPolicy::Copy);
        let extractor = DocExtractor::new();
        let source = Path::new("/tmp/host-demo/report.md");

        let copy = extractor
            .ensure_raw_copy(&raw_root, &host, source, "# Report\n\nplain body")
            .unwrap();
        assert_eq!(copy.parent().unwrap(), raw_root.join("ab12cd34"));
        let first = std::fs::read_to_string(&copy).unwrap();
        assert!(first.starts_with("---\nworker_type: doc\n"));
        assert!(first.contains("workspace_id: ws-demo"));
        assert!(first.contains("source_path: /tmp/host-demo/report.md"));
        assert!(first.contains("# Report"));

        // Idempotent: existing copy is never overwritten.
        std::fs::write(&copy, "mutated").unwrap();
        extractor
            .ensure_raw_copy(&raw_root, &host, source, "# Report\n\nplain body")
            .unwrap();
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), "mutated");
    }

    #[test]
    fn stamp_frontmatter_merges_without_clobbering() {
        let tags = vec![
            ("worker_type".to_string(), "doc".to_string()),
            ("workspace_id".to_string(), "ws-1".to_string()),
        ];
        let plain = stamp_frontmatter("# Note\nbody", &tags);
        assert!(
            plain.starts_with("---\nworker_type: doc\nworkspace_id: ws-1\n---\n\n# Note\nbody")
        );

        let existing = "---\ntitle: kept\n---\n\nbody";
        let merged = stamp_frontmatter(existing, &tags);
        assert!(merged.starts_with("---\ntitle: kept\nworker_type: doc\n"));
        assert!(merged.ends_with("\n---\n\nbody"));
        assert!(!merged.contains("title: kept\ntitle:"));
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
