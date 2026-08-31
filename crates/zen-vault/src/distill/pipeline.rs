use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;
use rig_compose::context::InvestigationContext;
use rig_compose::registry::{KernelError, ToolRegistry};
use rig_compose::skill::Skill;
use rig_compose::workflow::Workflow;
use tracing::{info, warn};

use super::checkpoint::Checkpoint;
use super::checkpoint::CheckpointManager;
use super::contradiction::ContradictionDetector;
use super::notion_extraction::NotionExtractor;
use super::recovery::RecoveryManager;
use super::stages::LlmDistillStage;
use super::transaction::TransactionScope;
use super::transaction::VersionSnapshot;
use super::types::{GapKind, GapRecord, LoopBudget, VerificationNode};
use super::wiki_compile::WikiCompiler;
use crate::graph_router::validate_slug;
use crate::graph_verify::PlaceholderRegistry;
use crate::notion::service::NotionService;

use crate::note::{Note, parse_frontmatter};
use crate::tindy::checksum::ChangeDetector;
use crate::wiki::WikiPage;

/// Summary of a single distillation pipeline run.
///
/// Produced by [`DistillationPipeline::run`] and [`DistillationPipeline::run_scoped`]
/// to report per-cycle metrics to the worker and scheduler.
#[derive(Debug, Clone)]
pub struct DistillationReport {
    /// Inbox `.md` files loaded and processed this cycle.
    pub notes_processed: usize,
    /// Entities extracted from notes via NotionExtractor.
    pub entities_extracted: usize,
    /// Notions durably upserted into the DB graph via NotionService (T003).
    pub entities_persisted: usize,
    /// New wiki pages compiled by WikiCompiler this cycle.
    pub wiki_pages_created: usize,
    /// Wiki merge plans executed (T015, FR-016).
    pub merged_count: usize,
    /// Contradictions detected between note content and existing wiki pages.
    pub contradictions_found: usize,
    /// Raw notes archived to `vault/archive/<yyyy-mm>/` (T005; was wiki-moves).
    pub migrated_files: Vec<(PathBuf, PathBuf)>,
}

/// Return type from `run_scoped` carrying the distillation report plus
/// ORAV verification data and pending pool count (T030/T033, FR-029/FR-032).
pub struct ScopedRunOutcome {
    /// Core distillation metrics.
    pub report: DistillationReport,
    /// Per-source ORAV verification attempts (T030, FR-029).
    pub verifications: Vec<VerificationNode>,
    /// Notes deferred to pending pool due to budget or ORAV failure (T033).
    pub pending_count: usize,
    /// Gaps emitted during this cycle (ORAV failures, budget deferrals).
    pub gaps: Vec<GapRecord>,
    /// Page creates downgraded to updates via PlaceholderRegistry (T032, FR-031).
    pub placeholder_downgrades: usize,
    /// True when CAS drift rolled back the cycle's tracked writes (T033, FR-032).
    pub cas_rolled_back: bool,
    /// Paths that drifted outside the cycle's VersionSnapshot (T033, FR-032).
    pub cas_drifted: Vec<String>,
    /// Inbox sources archived this cycle whose removal was deferred to the
    /// CAS commit point (FR-032). Empty when CAS was inactive.
    pub deferred_sources: Vec<PathBuf>,
}

/// Scan content for known entity names and wrap them in `[[wikilinks]]`
/// if they appear as plain text and are not already linked.
///
/// Uses `WikiPage::extract_wikilinks()` to identify existing links and
/// avoid double-linking.
pub fn auto_link_wikilinks(content: &str, known_entities: &[String]) -> String {
    let existing_links = WikiPage::extract_wikilinks(content);
    let mut result = content.to_string();

    // Sort entities by length (longest first) to avoid partial matches
    let mut sorted_entities: Vec<&String> = known_entities.iter().collect();
    sorted_entities.sort_by_key(|b| std::cmp::Reverse(b.len()));

    for entity in sorted_entities {
        // Skip if already linked
        if existing_links.iter().any(|l| l == entity) {
            continue;
        }

        // Only wrap if the entity appears as plain text (not inside [[...]] or `...`)
        let entity_lower = entity.to_lowercase();
        let mut new_result = String::with_capacity(result.len());
        let mut i = 0;
        let bytes = result.as_bytes();
        let result_lower = result.to_lowercase();

        while i < bytes.len() {
            // Check if we're inside a wikilink or backtick
            if bytes[i] == b'[' && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                // Skip until ]]
                new_result.push_str(&result[i..i + 2]);
                i += 2;
                while i < bytes.len() - 1 {
                    if bytes[i] == b']' && bytes[i + 1] == b']' {
                        new_result.push_str("]]");
                        i += 2;
                        break;
                    }
                    new_result.push(bytes[i] as char);
                    i += 1;
                }
                continue;
            }
            if bytes[i] == b'`' {
                // Skip until closing backtick
                new_result.push(bytes[i] as char);
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'`' {
                        new_result.push(bytes[i] as char);
                        i += 1;
                        break;
                    }
                    new_result.push(bytes[i] as char);
                    i += 1;
                }
                continue;
            }

            // Check if entity starts at this position
            let remaining_lower = &result_lower[i..];
            if remaining_lower.starts_with(&entity_lower) {
                // Ensure word boundary
                let end = i + entity.len();
                let before_ok = i == 0 || !result.as_bytes()[i - 1].is_ascii_alphanumeric();
                let after_ok =
                    end >= result.len() || !result.as_bytes()[end].is_ascii_alphanumeric();

                if before_ok && after_ok {
                    new_result.push_str(&format!("[[{entity}]]"));
                    i = end;
                    continue;
                }
            }

            new_result.push(bytes[i] as char);
            i += 1;
        }

        result = new_result;
    }

    result
}

/// T004: deterministic content normalization (FR-003) —
/// CRLF → LF, trailing-whitespace strip, 2+ blank lines collapsed to 1,
/// outer blank-line trim. Encoding is forced to UTF-8 at read time
/// (`load_notes` uses `from_utf8_lossy`).
pub fn normalize_content(content: &str) -> String {
    let lf = content.replace("\r\n", "\n").replace('\r', "\n");
    let no_trailing_ws: String = lf
        .lines()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n");
    let mut collapsed: Vec<&str> = Vec::with_capacity(no_trailing_ws.len());
    let mut blank_run = 0usize;
    for line in no_trailing_ws.lines() {
        if line.is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        collapsed.push(line);
    }
    while collapsed.first().is_some_and(|l| l.is_empty()) {
        collapsed.remove(0);
    }
    while collapsed.last().is_some_and(|l| l.is_empty()) {
        collapsed.pop();
    }
    let mut out = collapsed.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Dynamic context pruning (FR-029): trim per-note historical reasoning
/// while retaining the canonical entity schema (entity names + kinds).
///
/// **Retention rules:**
/// - Always retained: entity names/kinds (lines containing `entity:`, `kind:`,
///   `[[...]]` wikilinks, YAML frontmatter keys), and the first `retain_lines`
///   content lines of each note.
/// - Trimmed: remaining content lines beyond `retain_lines` — these carry
///   per-note historical reasoning that causes context drift across cycles.
///
/// This prevents the compile/LLM stages from accumulating unbounded
/// per-source history while preserving the structural entity schema
/// that drives wiki consistency.
pub fn prune_context(notes: &[Note], entity_names: &[String], retain_lines: usize) -> Vec<Note> {
    let retain_set: HashSet<&str> = entity_names.iter().map(|s| s.as_str()).collect();

    notes
        .iter()
        .map(|note| {
            let mut pruned = note.clone();
            let mut kept = Vec::new();
            let mut line_count = 0usize;

            for line in note.content.lines() {
                let trimmed = line.trim();

                // Always retain: frontmatter, entity schema, wikilinks
                if trimmed == "---"
                    || trimmed.starts_with("entity:")
                    || trimmed.starts_with("kind:")
                    || trimmed.starts_with("tags:")
                    || trimmed.starts_with("source:")
                    || trimmed.starts_with("id:")
                    || trimmed.starts_with("sensitivity:")
                    || (trimmed.starts_with("[[") && trimmed.ends_with("]]"))
                {
                    kept.push(line);
                    continue;
                }

                // Check if line references a known entity name
                if retain_set.iter().any(|name| line.contains(*name)) {
                    kept.push(line);
                    continue;
                }

                // Content lines: retain up to retain_lines
                if !trimmed.is_empty() && line_count < retain_lines {
                    kept.push(line);
                    line_count += 1;
                }
            }

            pruned.content = kept.join("\n");
            if !pruned.content.ends_with('\n') && !pruned.content.is_empty() {
                pruned.content.push('\n');
            }
            pruned
        })
        .collect()
}

/// Insert provenance key/value lines into a note's YAML frontmatter
/// (before the closing `---`). Content without frontmatter gets one prepended.
pub fn append_provenance(raw: &str, pairs: &[(&str, String)]) -> String {
    let mut lines: Vec<String> = raw.lines().map(|l| l.to_string()).collect();
    let has_frontmatter = lines.first().map(|l| l.trim() == "---").unwrap_or(false);
    if has_frontmatter {
        let close = lines
            .iter()
            .skip(1)
            .position(|l| l.trim() == "---")
            .map(|p| p + 1);
        let insert_at = close.unwrap_or(lines.len());
        for (i, (k, v)) in pairs.iter().enumerate() {
            lines.insert(insert_at + i, format!("{k}: \"{v}\""));
        }
    } else {
        let mut fm: Vec<String> = vec!["---".into()];
        for (k, v) in pairs {
            fm.push(format!("{k}: \"{v}\""));
        }
        fm.push("---".into());
        fm.extend(lines);
        lines = fm;
    }
    let mut out = lines.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// T005: archive processed inbox notes to `vault/archive/<yyyy-mm>/` with
/// provenance frontmatter (FR-006/007). Returns (source, dest) pairs.
///
/// With `defer_source_removal` (FR-032 CAS) the inbox source is left in place
/// as rollback insurance; `run_scoped` removes it after a clean conditional
/// commit. Replaces the old wiki-tree move: raw notes leave the inbox but
/// never enter the wiki domain dirs; the inbox is empty after a Completed cycle.
fn archive_processed_notes(
    notes: &[Note],
    archive_dir: &Path,
    cycle_id: &str,
    track: &TransactionScope,
    defer_source_removal: bool,
) -> Vec<(PathBuf, PathBuf)> {
    let mut archived = Vec::new();

    for note in notes {
        let source = match &note.file_path {
            Some(p) if p.exists() => p.clone(),
            _ => continue,
        };

        let now = chrono::Utc::now();
        let month_dir = archive_dir.join(now.format("%Y-%m").to_string());
        if let Err(e) = std::fs::create_dir_all(&month_dir) {
            tracing::warn!(
                source = %source.display(),
                error = %e,
                "Failed to create archive month directory, leaving note in inbox"
            );
            continue;
        }

        let filename = source.file_name().unwrap_or_default();
        let mut dest = month_dir.join(filename);
        if dest.exists() {
            let stem = source.file_stem().unwrap_or_default();
            let ext = source.extension().unwrap_or_default();
            dest = month_dir.join(format!(
                "{}_{}.{}",
                stem.to_string_lossy(),
                now.format("%Y%m%d%H%M%S"),
                ext.to_string_lossy()
            ));
        }

        let raw = match std::fs::read(&source) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
            Err(e) => {
                tracing::warn!(source = %source.display(), error = %e, "Failed to read note for archive");
                continue;
            }
        };
        let checksum = ChangeDetector::compute_checksum(&raw);
        let provenance = append_provenance(
            &raw,
            &[
                ("source_path", source.to_string_lossy().to_string()),
                ("archived_at", now.to_rfc3339()),
                ("cycle_id", cycle_id.to_string()),
                ("original_created_at", note.created_at.to_rfc3339()),
                ("checksum", checksum),
                ("merged_into", String::new()),
            ],
        );

        let write_result = if defer_source_removal {
            std::fs::write(&dest, provenance)
        } else {
            std::fs::write(&dest, provenance).and_then(|_| std::fs::remove_file(&source))
        };
        match write_result {
            Ok(()) => {
                if let Err(e) = track.track_path(&dest) {
                    tracing::warn!(dest = %dest.display(), error = %e, "Failed to track archived file");
                }
                info!(
                    source = %source.display(),
                    dest = %dest.display(),
                    "Archived processed inbox note"
                );
                archived.push((source, dest));
            }
            Err(e) => {
                tracing::warn!(
                    source = %source.display(),
                    error = %e,
                    "Failed to archive inbox note, leaving in inbox"
                );
            }
        }
    }

    archived
}

/// Orchestrates the full distillation pipeline: budget gate, normalize,
/// notion extraction, wiki compilation, contradiction detection, merge,
/// ORAV self-correction, and archival.
pub struct DistillationPipeline {
    extractor: NotionExtractor,
    compiler: WikiCompiler,
    detector: ContradictionDetector,
    notion_service: NotionService,
    /// Optional LLM model for FR-003 enrichment. `None` → heuristic only.
    llm_model: Option<String>,
    /// FR-032 OCC/CAS gate: snapshot the wiki dir before stages and commit
    /// via `commit_conditional`, rolling back tracked writes on drift.
    /// `false` restores plain `txn.commit()` (source removal not deferred).
    cas_commit: bool,
}

impl DistillationPipeline {
    /// Create a new pipeline with default (heuristic-only) configuration.
    pub fn new() -> Self {
        Self {
            extractor: NotionExtractor::new(),
            compiler: WikiCompiler::new(),
            detector: ContradictionDetector::new(),
            notion_service: NotionService::new(),
            llm_model: None,
            cas_commit: true,
        }
    }

    /// Set the LLM model for FR-003 enrichment. `Some("openai:gpt-4o")` enables
    /// the LLM path; `None` (default) disables it.
    pub fn with_llm_model(mut self, model: String) -> Self {
        self.llm_model = Some(model);
        self
    }

    /// Enable/disable FR-032 OCC/CAS conditional commits. `true` (default)
    /// snapshots the wiki dir each `run_scoped` cycle and rolls back every
    /// tracked write on drift; `false` keeps today's plain-commit behavior.
    /// The manual `run()` path overrides this from
    /// `LoopConfig::cas_commit_or_default()`.
    pub fn with_cas_commit(mut self, cas_commit: bool) -> Self {
        self.cas_commit = cas_commit;
        self
    }

    /// Production entry (sync callers `await` this): derives archive/logs
    /// dirs from `ZenPaths` and opens the workspace DB lazily.
    pub async fn run(&self, inbox_dir: &Path, wiki_dir: &Path) -> Result<DistillationReport> {
        use zen_core::paths::ZenPaths;
        let vault_root = wiki_dir
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let archive_dir = vault_root.join("archive");
        let zen_paths = ZenPaths::detect();
        let logs_dir = zen_paths
            .as_ref()
            .map(|p| p.logs().to_path_buf())
            .unwrap_or_else(|_| vault_root.join("logs"));
        // Canonical DB — always <data>/state.db, the same file
        // ZenLoopWorker, reindex, and FTS open. Opening a different path
        // here would fork the entity graph by entry point (the worker's
        // "same code path as `zen wiki distill`" contract, T008).
        let db = match &zen_paths {
            Ok(p) => zen_repo::SqliteClient::open_lazy(&p.data().join("state.db"))
                .await
                .ok(),
            Err(_) => None,
        };
        // FR-032: the manual path resolves its CAS gate from config (same knob
        // the worker exposes), defaulting to enabled when config is unreadable.
        let cas_commit = match zen_core::config::load_config() {
            Ok(cfg) => cfg.agentic.loop_cfg.cas_commit_or_default(),
            Err(_) => true,
        };
        let outcome = self
            .run_scoped_inner(
                inbox_dir,
                wiki_dir,
                &archive_dir,
                &logs_dir,
                db.as_ref(),
                None,
                cas_commit,
            )
            .await?;
        Ok(outcome.report)
    }

    /// Isolated-dirs entry (worker + tests): all stage dirs injected, DB optional.
    ///
    /// Stage chain (worker contract stage 3):
    /// T007 checkpoint gate → load → T004 normalize → extract →
    /// T003 NotionService persist → auto-link → compile → contradictions →
    /// T005 archive, with T006 TransactionScope around mutations.
    ///
    /// The CAS gate follows the pipeline's `cas_commit` field (`true` by
    /// default, [`Self::with_cas_commit`]); the manual [`Self::run`] path
    /// resolves it from `LoopConfig::cas_commit_or_default()` instead.
    pub async fn run_scoped(
        &self,
        inbox_dir: &Path,
        wiki_dir: &Path,
        archive_dir: &Path,
        logs_dir: &Path,
        db: Option<&zen_repo::SqliteClient>,
        budget: Option<&mut LoopBudget>,
    ) -> Result<ScopedRunOutcome> {
        self.run_scoped_inner(
            inbox_dir,
            wiki_dir,
            archive_dir,
            logs_dir,
            db,
            budget,
            self.cas_commit,
        )
        .await
    }

    /// `run_scoped` body with the CAS gate injected by the caller.
    #[allow(clippy::too_many_arguments)]
    async fn run_scoped_inner(
        &self,
        inbox_dir: &Path,
        wiki_dir: &Path,
        archive_dir: &Path,
        logs_dir: &Path,
        db: Option<&zen_repo::SqliteClient>,
        budget: Option<&mut LoopBudget>,
        cas_commit: bool,
    ) -> Result<ScopedRunOutcome> {
        // T007: pre-cycle gate — recover-or-restart on a prior crashed cycle.
        let recovery = RecoveryManager::new(logs_dir);
        if let Some(cp) = recovery.check_incomplete()? {
            info!(
                status = %cp.status,
                "Prior cycle checkpoint found — clearing for idempotent restart"
            );
            recovery.recover()?;
        }
        let cycle_id = uuid::Uuid::now_v7().to_string();
        let checkpoints = CheckpointManager::new(logs_dir);
        checkpoints.write_checkpoint(&Checkpoint {
            status: "started".to_string(),
            started_at: chrono::Utc::now().to_rfc3339(),
            notes_count: 0,
        })?;

        let txn = TransactionScope::new(&format!("distill-{cycle_id}"));
        txn.begin()?;

        // FR-031b: consult the placeholder registry so page creates whose
        // slugs were reserved during Agent planning are recorded this cycle.
        // Missing file → empty registry; corrupt file → fresh (non-fatal).
        let placeholders_path = logs_dir.join("placeholders.json");
        let mut registry = match PlaceholderRegistry::load(&placeholders_path) {
            Ok(reg) => reg,
            Err(e) => {
                warn!(
                    error = %e,
                    "Failed to load placeholder registry — continuing with a fresh one"
                );
                PlaceholderRegistry::new()
            }
        };
        let mut registry_dirty = false;

        // FR-032 (T033): capture the wiki working set BEFORE any stage runs so
        // the cycle can commit conditionally. Empty inventory (or a missing
        // wiki dir) → no snapshot → CAS skipped for this cycle.
        let snapshot = if cas_commit && wiki_dir.is_dir() {
            let wiki_paths: Vec<PathBuf> = crate::graph_verify::wiki_page_inventory(wiki_dir)
                .into_iter()
                .map(|(_, rel)| wiki_dir.join(rel))
                .collect();
            if wiki_paths.is_empty() {
                None
            } else {
                match VersionSnapshot::capture(&wiki_paths) {
                    Ok(snap) => Some(snap),
                    Err(e) => {
                        warn!(
                            error = %e,
                            "Failed to capture wiki version snapshot — CAS skipped this cycle"
                        );
                        None
                    }
                }
            }
        } else {
            None
        };

        let run = self.run_stages(
            inbox_dir,
            wiki_dir,
            archive_dir,
            db,
            &txn,
            &cycle_id,
            budget,
            snapshot.is_some(),
            &mut registry,
            &mut registry_dirty,
        );
        match run.await {
            Ok(mut outcome) => {
                // FR-032 (T033): self-write-aware OCC. The snapshot covers
                // pre-existing wiki pages; this cycle's own rewrites (merges,
                // link rewrites, regenerated index/log) are txn-tracked and
                // expected — only drift on paths we did NOT write counts as
                // external interference and forces a rollback.
                let committed = if let Some(snapshot) = &snapshot {
                    let tracked: std::collections::HashSet<PathBuf> =
                        txn.tracked_paths().into_iter().collect();
                    match snapshot.verify() {
                        Ok(drift) => {
                            let external: Vec<PathBuf> =
                                drift.into_iter().filter(|p| !tracked.contains(p)).collect();
                            if external.is_empty() {
                                // Clean cycle — reap the inbox sources whose
                                // removal was deferred past the drift window.
                                for src in &outcome.deferred_sources {
                                    if let Err(e) = std::fs::remove_file(src) {
                                        warn!(
                                            source = %src.display(),
                                            error = %e,
                                            "Failed to remove deferred inbox source"
                                        );
                                    }
                                }
                                txn.commit()?;
                                true
                            } else {
                                // Rollback deletes the tracked outputs (wiki
                                // pages + archive dests); inbox sources are
                                // intact because removal was deferred — zero
                                // data loss, the next cycle reprocesses them.
                                warn!(
                                    drifted = ?external,
                                    "CAS drift detected — cycle rolled back, checkpoint skipped"
                                );
                                if let Err(rb) = txn.rollback() {
                                    tracing::warn!(
                                        error = %rb,
                                        "Transaction rollback itself failed"
                                    );
                                }
                                outcome.cas_rolled_back = true;
                                outcome.cas_drifted =
                                    external.iter().map(|p| p.display().to_string()).collect();
                                false
                            }
                        }
                        Err(e) => {
                            if let Err(rb) = txn.rollback() {
                                tracing::warn!(error = %rb, "Transaction rollback itself failed");
                            }
                            return Err(e.context("CAS snapshot verification failed"));
                        }
                    }
                } else {
                    txn.commit()?;
                    true
                };
                if committed {
                    checkpoints.write_checkpoint(&Checkpoint {
                        status: "completed".to_string(),
                        started_at: chrono::Utc::now().to_rfc3339(),
                        notes_count: outcome.report.notes_processed,
                    })?;
                }
                if registry_dirty && let Err(e) = registry.save(&placeholders_path) {
                    warn!(error = %e, "Failed to save placeholder registry");
                }
                Ok(outcome)
            }
            Err(e) => {
                if let Err(rb) = txn.rollback() {
                    tracing::warn!(error = %rb, "Transaction rollback itself failed");
                }
                Err(e)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_stages(
        &self,
        inbox_dir: &Path,
        wiki_dir: &Path,
        archive_dir: &Path,
        db: Option<&zen_repo::SqliteClient>,
        txn: &TransactionScope,
        cycle_id: &str,
        mut budget: Option<&mut LoopBudget>,
        defer_source_removal: bool,
        registry: &mut PlaceholderRegistry,
        registry_dirty: &mut bool,
    ) -> Result<ScopedRunOutcome> {
        let mut notes = self.load_notes(inbox_dir)?;
        // FR-012: configurable skip by extension from LoopConfig
        if let Ok(cfg) = zen_core::config::load_config() {
            let skip = cfg.agentic.loop_cfg.skip_extensions_or_default();
            if !skip.is_empty() {
                let before = notes.len();
                notes.retain(|n| {
                    let ext = n
                        .file_path
                        .as_ref()
                        .and_then(|p| p.extension())
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    !skip.iter().any(|s| s.to_ascii_lowercase() == ext)
                });
                if notes.len() != before {
                    info!(
                        skipped = before - notes.len(),
                        "FR-012 skip_extensions filtered notes"
                    );
                }
            }
        }

        // T033 (FR-032): per-source budget enforcement — consume a step per
        // source processed; over-budget sources move to vault/archive/pending/
        // instead of the normal pipeline, incrementing report.pending_count.
        let mut pending_notes: Vec<PathBuf> = Vec::new();
        if let Some(ref mut b) = budget {
            let mut retained = Vec::new();
            for note in notes {
                if !b.consume_step() {
                    warn!(
                        note = %note.id,
                        pending = pending_notes.len() + 1,
                        "loop: note deferred — over budget (T033 pending pool)"
                    );
                    if let Some(ref p) = note.file_path {
                        pending_notes.push(p.clone());
                    }
                    continue;
                }
                retained.push(note);
            }
            notes = retained;
        }

        // Budget-deferred notes physically move to the pending pool now —
        // without this they would silently stay in the inbox and
        // report.pending_count would claim deferrals that never happened.
        if !pending_notes.is_empty() {
            let pending_dir = archive_dir.join("pending");
            if let Err(e) = std::fs::create_dir_all(&pending_dir) {
                warn!(error = %e, "Failed to create pending pool dir");
            } else {
                for src in &pending_notes {
                    let dest = match src.file_name() {
                        Some(name) => pending_dir.join(name),
                        None => continue,
                    };
                    match std::fs::rename(src, &dest) {
                        Ok(()) => {
                            txn.track_path(&dest).ok();
                        }
                        Err(e) => {
                            warn!(
                                source = %src.display(),
                                error = %e,
                                "Failed to move over-budget note to pending pool"
                            );
                        }
                    }
                }
            }
        }

        let notes_processed = notes.len();
        info!(
            notes_processed,
            pending = pending_notes.len(),
            "Loaded notes from inbox, starting consolidation pipeline"
        );

        let mut notions = self.extractor.extract_batch(&notes)?;
        let entities_extracted = notions.len();
        info!(entities_extracted, "Notion extraction complete");

        // T003: persist extracted notions into the DB graph (Principle XII).
        // DB mutations share the TransactionScope lifetime with FS mutations;
        // rollback deletes FS files while upsert idempotency keeps DB safe for reprocess.
        let mut entities_persisted = 0usize;
        if let Some(client) = db {
            for notion in &notions {
                match self.notion_service.upsert_entity(client, notion).await {
                    Ok(()) => entities_persisted += 1,
                    Err(e) => tracing::warn!(
                        notion = %notion.name,
                        error = %e,
                        "Failed to persist notion, continuing"
                    ),
                }
            }
            info!(entities_persisted, "Notion persistence complete");
        } else {
            tracing::debug!("No DB client — skipping notion persistence (test mode)");
        }

        // FR-029 (T030): Dynamic Context Pruning — trim per-note historical
        // reasoning while retaining canonical entity schema (entity names + kinds).
        let entity_names: Vec<String> = notions.iter().map(|n| n.name.clone()).collect();
        let pruned_notes = prune_context(&notes, &entity_names, 10);

        let normalized: Vec<Note> = pruned_notes
            .iter()
            .map(|note| {
                let mut n = note.clone();
                n.content = normalize_content(&note.content);
                n
            })
            .collect();

        // FR-003: LLM enrichment hook — if model configured, attempt LLM-augmented
        // notion extraction bounded by LoopBudget; otherwise heuristic path only.
        let mut llm_stage = LlmDistillStage::new(LoopBudget::default(), self.llm_model.clone());
        match llm_stage.distill_with_fallback(&normalized) {
            Ok((llm_notions, tokens_used)) => {
                if !llm_notions.is_empty() {
                    let before = notions.len();
                    // Merge LLM notions, dedup by name.
                    for notion in llm_notions {
                        if !notions.iter().any(|n| n.name == notion.name) {
                            notions.push(notion);
                        }
                    }
                    let added = notions.len().saturating_sub(before);
                    info!(
                        added,
                        tokens_used, "FR-003: LLM enrichment merged additional notions"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "FR-003: LLM distill failed, falling back to heuristic-only"
                );
            }
        }

        let entity_names: Vec<String> = notions.iter().map(|n| n.name.clone()).collect();
        let linked_notes: Vec<Note> = normalized
            .into_iter()
            .map(|mut note| {
                note.content = auto_link_wikilinks(&note.content, &entity_names);
                note
            })
            .collect();

        // FR-031b: pages already on disk before compile are updates; only NEW
        // page paths consult the placeholder registry for create-downgrades.
        let pre_compile_pages: HashSet<PathBuf> =
            crate::graph_verify::wiki_page_inventory(wiki_dir)
                .into_iter()
                .map(|(_, rel)| wiki_dir.join(rel))
                .collect();

        let pages = self.compiler.compile(&linked_notes, wiki_dir)?;
        // The compiler also regenerates `log.md` and (when pages exist)
        // `index.md` — track them so CAS self-write filtering sees them and
        // rollback can clean them up like any other cycle output.
        txn.track_path(&wiki_dir.join("log.md")).ok();
        txn.track_path(&wiki_dir.join("index.md")).ok();
        let wiki_pages_created = pages.len();
        let mut placeholder_downgrades = 0usize;
        for page in &pages {
            // Track the FULL path — rollback (incl. CAS drift rollback) can
            // only clean up files it can resolve; page.path is wiki-relative.
            let full_path = wiki_dir.join(&page.path);
            txn.track_path(&full_path)?;
            // FR-031b: slug = page file stem (slugify convention). A slug
            // reserved in the registry downgrades this create to an update —
            // the page is still written (single-writer create-as-update) and
            // the slot advances to Merged.
            let slug = full_path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if !pre_compile_pages.contains(&full_path) && registry.downgrade_to_update(&slug) {
                placeholder_downgrades += 1;
                info!(slug = %slug, "FR-031b: placeholder create downgraded to update");
            }
            if registry.lookup(&slug).is_some() {
                registry.merge(&slug);
                *registry_dirty = true;
            }
        }
        if wiki_pages_created > 0 {
            info!(wiki_pages_created, "Wiki pages compiled and written");
        }

        // T015: merge execution — cluster + fold duplicates (FR-016).
        let merged_count = self.execute_merge_plans(wiki_dir, archive_dir, txn, cycle_id)?;

        let contradictions = self.detector.detect(&notes)?;
        let contradictions_found = contradictions.len();
        if contradictions_found > 0 {
            self.detector
                .log_contradictions(&contradictions, wiki_dir)?;
            info!(contradictions_found, "Contradictions detected and logged");
        } else {
            info!("No contradictions found");
        }

        // ── T030 (FR-029): ORAV per-source verification ────────────────
        //
        // Observe → Reason → Act → Verify cycle applied per source note.
        // On Verify failure: retry IN-PLACE up to 2 times (drop the
        // offending slug/claim, regenerate the compile for that source)
        // before falling back to archiving with a GapRecord.
        //
        // GapKind choice: QuarantinedNote — the note itself is the unit
        // being quarantined (not an LLM output failure), so QuarantinedNote
        // is semantically correct. LlmFailure is reserved for cases where
        // the LLM call itself fails, not for output validation failures.
        let wiki_inventory = crate::graph_verify::wiki_page_inventory(wiki_dir);
        let mut verifications: Vec<VerificationNode> = Vec::new();
        let mut orav_failed: HashSet<PathBuf> = HashSet::new();

        for note in linked_notes.iter() {
            let (vnodes, failed) = self.run_orav_for_source(
                note,
                &entity_names,
                wiki_dir,
                &wiki_inventory,
                txn,
                cycle_id,
            );
            verifications.extend(vnodes);
            if failed && let Some(ref p) = note.file_path {
                orav_failed.insert(p.clone());
            }
        }

        // Move ORAV-failed notes to pending pool (they were already processed
        // by the pipeline above; the pending pool signals "needs re-processing
        // next cycle after the offending content is remediated").
        for failed_path in &orav_failed {
            let pending_dir = archive_dir.join("pending");
            if let Err(e) = std::fs::create_dir_all(&pending_dir) {
                warn!(error = %e, "Failed to create pending pool dir");
                continue;
            }
            let file_name = failed_path
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_default();
            let dest = pending_dir.join(&file_name);
            match std::fs::rename(failed_path, &dest) {
                Ok(()) => {
                    pending_notes.push(failed_path.clone());
                    txn.track_path(&dest).ok();
                    info!(
                        source = %failed_path.display(),
                        dest = %dest.display(),
                        "ORAV verification failed — note moved to pending pool"
                    );
                }
                Err(e) => {
                    warn!(
                        source = %failed_path.display(),
                        error = %e,
                        "Failed to move ORAV-failed note to pending pool"
                    );
                }
            }
        }

        // Archive notes that passed ORAV and weren't budget-deferred.
        let notes_to_archive: Vec<Note> = linked_notes
            .into_iter()
            .filter(|n| {
                n.file_path
                    .as_ref()
                    .map(|p| !orav_failed.contains(p))
                    .unwrap_or(true)
            })
            .collect();
        let archived = archive_processed_notes(
            &notes_to_archive,
            archive_dir,
            cycle_id,
            txn,
            defer_source_removal,
        );
        // FR-032: under CAS the inbox sources stay in place as rollback
        // insurance; run_scoped removes them only after a clean commit.
        let deferred_sources = if defer_source_removal {
            archived.iter().map(|(src, _)| src.clone()).collect()
        } else {
            Vec::new()
        };

        // Collect ORAV gap records for the worker's gap persistence layer.
        let mut gaps: Vec<GapRecord> = Vec::new();
        for v in &verifications {
            if let Some(ref gap_id) = v.gap_ref {
                gaps.push(
                    GapRecord::new(
                        GapKind::QuarantinedNote,
                        cycle_id,
                        format!(
                            "ORAV verification failed: slug_legality={}, contradiction={}, gap_ref={}",
                            v.slug_legality, v.contradiction_detected, gap_id
                        ),
                    )
                    .with_path(gap_id.clone()),
                );
            }
        }

        Ok(ScopedRunOutcome {
            report: DistillationReport {
                notes_processed,
                entities_extracted,
                entities_persisted,
                merged_count,
                wiki_pages_created,
                contradictions_found,
                migrated_files: archived,
            },
            verifications,
            pending_count: pending_notes.len(),
            gaps,
            placeholder_downgrades,
            cas_rolled_back: false,
            cas_drifted: Vec::new(),
            deferred_sources,
        })
    }

    /// T030 (FR-029): ORAV per-source verification loop.
    ///
    /// Observe → Reason → Act → Verify for a single source note:
    /// - **Observe**: note content + extracted entity names
    /// - **Reason**: diff (which entities/pages are new vs already present)
    /// - **Act**: normal pipeline already compiled this source; verify the output
    /// - **Verify**: validate every wikilink/slug via `validate_slug`;
    ///   contradiction detection currently compares claims WITHIN the note
    ///   only — cross-page comparison against the pre-cycle wiki snapshot
    ///   is not wired yet (`_wiki_inventory` is the Phase-8 hook), so the
    ///   contradiction leg only catches self-contradictory notes today.
    ///
    /// On Verify failure: retry IN-PLACE up to 2 times (drop the offending
    /// slug/claim, re-link, re-compile for that source). After 2 retries,
    /// fall back to moving the note to the pending pool with a GapRecord
    /// (GapKind::QuarantinedNote — the note is the unit being quarantined,
    /// not an LLM output failure).
    fn run_orav_for_source(
        &self,
        note: &Note,
        entity_names: &[String],
        _wiki_dir: &Path,
        _wiki_inventory: &[(String, String)],
        _txn: &TransactionScope,
        cycle_id: &str,
    ) -> (Vec<VerificationNode>, bool) {
        let mut verifications = Vec::new();
        let mut content = note.content.clone();
        let max_retries = 2u32;

        for attempt in 0..=max_retries {
            // ── Verify: slug legality ──────────────────────────────────
            // Wikilinks carry natural-case titles ([[Rust]]); pages live on
            // disk under their kebab-case slugs, so legality is judged on the
            // normalized form.
            let wikilinks = WikiPage::extract_wikilinks(&content);
            let normalized_slug =
                |link: &str| validate_slug(&link.to_lowercase().replace(' ', "-"));
            let slug_legality = wikilinks.iter().all(|link| normalized_slug(link));

            // ── Verify: contradiction detection ────────────────────────
            // Check proposed content against existing wiki pages (pre-cycle
            // snapshot stored in wiki_inventory).
            let contradiction_detected = {
                let temp_note = Note {
                    content: content.clone(),
                    ..note.clone()
                };
                match self.detector.detect(&[temp_note]) {
                    Ok(contradictions) => !contradictions.is_empty(),
                    Err(_) => false,
                }
            };

            let will_retry = (slug_legality && !contradiction_detected) || attempt >= max_retries;
            let gap_ref = if !slug_legality || contradiction_detected {
                if attempt >= max_retries {
                    Some(format!("orav-{cycle_id}-{}", note.id))
                } else {
                    None
                }
            } else {
                None
            };

            verifications.push(VerificationNode {
                slug_legality,
                fact_consistency: !contradiction_detected,
                contradiction_detected,
                will_retry: !will_retry,
                gap_ref,
            });

            // Success: all slugs valid and no contradictions
            if slug_legality && !contradiction_detected {
                return (verifications, false);
            }

            if attempt < max_retries {
                // ── Retry: drop offending slugs and re-link ────────────
                for link in &wikilinks {
                    if !normalized_slug(link) {
                        content = content
                            .replace(&format!("[[{link}]]"), &format!("{{{{bad_slug:{link}}}}}"));
                    }
                }
                // Re-link with valid entity names only, preserving the strips
                // above (re-linking from the original content would resurrect
                // the offending links).
                let valid_entities: Vec<String> = entity_names
                    .iter()
                    .filter(|e| normalized_slug(e))
                    .cloned()
                    .collect();
                content = auto_link_wikilinks(&content, &valid_entities);
                info!(
                    note = %note.id,
                    attempt = attempt + 1,
                    "ORAV: retrying after dropping offending slugs"
                );
            }
        }

        // All retries exhausted — note will be moved to pending pool by caller
        (verifications, true)
    }

    /// T015 (FR-016): cluster compiled wiki pages and execute merge plans.
    ///
    /// PureDuplicate (≥0.98): target keeps its content; Merge: target absorbs
    /// non-overlapping source content. Both: sources archived with
    /// `merged_into`, target gains `merged_from`/`supersede_of`/`merged_at`
    /// frontmatter, and other pages' `[[source]]` wikilinks are rewritten to
    /// `[[target]]` (OVP2 bi-temporal pattern, data-model §5). Merge never
    /// deletes — originals live in the archive.
    fn execute_merge_plans(
        &self,
        wiki_dir: &Path,
        archive_dir: &Path,
        txn: &TransactionScope,
        cycle_id: &str,
    ) -> Result<usize> {
        use super::merge::{MergeStrategy, build_merge_plans};

        let inventory = crate::graph_verify::wiki_page_inventory(wiki_dir);
        if inventory.len() < 2 {
            return Ok(0);
        }

        let (threshold, pure_dup) = match zen_core::config::load_config() {
            Ok(cfg) => (
                cfg.agentic.loop_cfg.merge_threshold_or_default(),
                cfg.agentic.loop_cfg.merge_pure_duplicate_or_default(),
            ),
            Err(_) => (0.82, 0.98),
        };

        let pages: Vec<crate::wiki::WikiPage> = inventory
            .iter()
            .filter_map(|(name, rel)| {
                let raw = std::fs::read_to_string(wiki_dir.join(rel)).ok()?;
                Some(crate::wiki::WikiPage {
                    title: name.clone(),
                    path: wiki_dir.join(rel),
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                    tags: vec![],
                    wikilinks: crate::wiki::WikiPage::extract_wikilinks(&raw),
                    para: None,
                    okf_type: None,
                    content: raw,
                })
            })
            .collect();

        let (plans, alias_gaps) = build_merge_plans(&pages, threshold, pure_dup, cycle_id);
        if !alias_gaps.is_empty() {
            for gap in &alias_gaps {
                info!(kind = ?gap.kind, "merge: alias collision detected");
            }
        }

        let mut merged = 0usize;
        let now = chrono::Utc::now();
        for plan in plans {
            if plan.strategy == MergeStrategy::Skip {
                continue;
            }
            let target_path = plan.target_page.clone();
            let target_stem = target_path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let mut target_content = std::fs::read_to_string(&target_path).unwrap_or_default();

            let mut absorbed = Vec::new();
            for source in &plan.source_pages {
                if source == &target_path || !source.exists() {
                    continue;
                }
                let source_stem = source
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                let source_content = std::fs::read_to_string(source).unwrap_or_default();

                // Mechanical content absorption: append source body to target
                // when it carries unique lines (deterministic; LLM assist via
                // `merge_llm_model` is not yet wired — tracked by T046).
                if plan.strategy == MergeStrategy::Merge {
                    let unique: Vec<&str> = source_content
                        .lines()
                        .filter(|line| {
                            !line.trim().is_empty() && !target_content.contains(line.trim())
                        })
                        .collect();
                    if !unique.is_empty() {
                        target_content.push_str(&format!(
                            "\n\n## From [[{source_stem}]]\n\n{}\n",
                            unique.join("\n")
                        ));
                    }
                }

                // Archive the source (never delete — data-model §5) with
                // merged_into provenance.
                let month_dir = archive_dir.join(now.format("%Y-%m").to_string());
                std::fs::create_dir_all(&month_dir).ok();
                let file_name = source
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| format!("{}.md", source_stem));
                let archived = month_dir.join(file_name);
                let provenance = append_provenance(
                    &source_content,
                    &[
                        ("source_path", source.to_string_lossy().to_string()),
                        ("archived_at", now.to_rfc3339()),
                        ("cycle_id", cycle_id.to_string()),
                        ("merged_into", target_stem.clone()),
                    ],
                );
                if std::fs::write(&archived, provenance).is_ok()
                    && std::fs::remove_file(source).is_ok()
                {
                    txn.track_path(&archived)?;
                    txn.track_path(source)?;
                    absorbed.push(source_stem);
                }
            }

            if absorbed.is_empty() {
                continue;
            }

            // Rewrite wikilinks in remaining wiki pages: [[absorbed]] → [[target]].
            for page in &pages {
                if !page.path.exists() || page.path == target_path {
                    continue;
                }
                let content = match std::fs::read_to_string(&page.path) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let mut rewritten = content.clone();
                for stem in &absorbed {
                    rewritten =
                        rewritten.replace(&format!("[[{stem}]]"), &format!("[[{target_stem}]]"));
                }
                if rewritten != content {
                    std::fs::write(&page.path, &rewritten).ok();
                    txn.track_path(&page.path)?;
                }
            }

            // Target provenance (mem0 ADD/UPDATE discipline).
            let list: Vec<String> = absorbed.iter().map(|s| format!("\"{s}\"")).collect();
            target_content = append_provenance(
                &target_content,
                &[
                    ("merged_from", format!("[{}]", list.join(", "))),
                    ("supersede_of", format!("[{}]", list.join(", "))),
                    ("merged_at", now.to_rfc3339()),
                    ("cycle_id", cycle_id.to_string()),
                ],
            );
            std::fs::write(&target_path, &target_content)?;
            txn.track_path(&target_path)?;
            merged += 1;
        }

        if merged > 0 {
            info!(merged, "merge: plans executed");
        }
        Ok(merged)
    }

    /// Load all .md notes from the inbox directory.
    ///
    /// Reads bytes and decodes lossily to UTF-8 (T004 encoding normalization).
    fn load_notes(&self, inbox_dir: &Path) -> Result<Vec<Note>> {
        let mut notes = Vec::new();

        if !inbox_dir.is_dir() {
            info!(
                inbox_dir = %inbox_dir.display(),
                "Inbox directory does not exist, skipping"
            );
            return Ok(notes);
        }

        let mut entries: Vec<_> = std::fs::read_dir(inbox_dir)?
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .map(|ext| ext == "md")
                    .unwrap_or(false)
            })
            .collect();

        // Sort by filename for deterministic ordering
        entries.sort_by_key(|e| e.path());

        for entry in entries {
            let path = entry.path();
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    info!(
                        path = %path.display(),
                        error = %e,
                        "Failed to read note file, skipping"
                    );
                    continue;
                }
            };
            let content = String::from_utf8_lossy(&bytes).to_string();
            match parse_frontmatter(&content) {
                Ok(mut note) => {
                    note.file_path = Some(path.clone());
                    notes.push(note);
                }
                Err(e) => {
                    info!(
                        path = %path.display(),
                        error = %e,
                        "Failed to parse frontmatter, skipping note"
                    );
                }
            }
        }

        Ok(notes)
    }
}

impl Default for DistillationPipeline {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Workflow for DistillationPipeline {
    type Input = DistillationPipelineInput;
    type Output = DistillationReport;

    fn name(&self) -> &str {
        "zen-consolidation-pipeline"
    }

    async fn run(&self, input: Self::Input) -> Result<Self::Output, KernelError> {
        let inbox_dir = &input.inbox_dir;
        let wiki_dir = &input.wiki_dir;
        let dry_run = input.dry_run;

        let notes = self
            .load_notes(inbox_dir)
            .map_err(|e| KernelError::ToolFailed(e.to_string()))?;
        let notes_processed = notes.len();
        info!(
            notes_processed,
            "Loaded notes from inbox, starting consolidation pipeline"
        );

        if notes.is_empty() {
            info!("No notes to process, returning empty report");
            return Ok(DistillationReport {
                notes_processed: 0,
                entities_extracted: 0,
                entities_persisted: 0,
                merged_count: 0,
                wiki_pages_created: 0,
                contradictions_found: 0,
                migrated_files: Vec::new(),
            });
        }

        let _ctx = InvestigationContext::new("consolidation", "zen-vault")
            .with_block(uuid::Uuid::now_v7());

        let tools = ToolRegistry::new();

        let notes_json = serde_json::json!({
            "notes": notes.iter().map(|n| {
                serde_json::json!({
                    "id": n.id,
                    "content": n.content,
                    "tags": n.tags,
                    "source": n.source,
                    "sensitivity": format!("{:?}", n.sensitivity),
                })
            }).collect::<Vec<_>>(),
            "wiki_dir": wiki_dir.to_string_lossy(),
        });

        let ctx_with_notes = InvestigationContext::new("consolidation", "zen-vault")
            .with_block(uuid::Uuid::now_v7());

        let mut ctx = ctx_with_notes;
        ctx.evidence.push(rig_compose::context::Evidence {
            recorded_at: std::time::SystemTime::now(),
            source_skill: "consolidation-setup".to_string(),
            label: "consolidation".to_string(),
            detail: serde_json::json!({
                "notes": notes_json["notes"],
                "wiki_dir": wiki_dir.to_string_lossy(),
            }),
        });

        if dry_run {
            info!("Dry-run mode enabled, executing skills without writing to disk");
        }

        let outcome1 = self.extractor.execute(&mut ctx, &tools).await?;
        let entities_extracted = if outcome1.confidence_delta > 0.0 {
            info!("Notion extraction skill completed successfully");
            notes_processed.max(1)
        } else {
            0
        };

        let mut ctx = InvestigationContext::new("consolidation-wiki", "zen-vault")
            .with_block(uuid::Uuid::now_v7());

        ctx.evidence.push(rig_compose::context::Evidence {
            recorded_at: std::time::SystemTime::now(),
            source_skill: "wiki-setup".to_string(),
            label: "wiki-compilation".to_string(),
            detail: serde_json::json!({
                "notes": notes_json["notes"],
                "wiki_dir": wiki_dir.to_string_lossy(),
            }),
        });

        let outcome2 = self.compiler.execute(&mut ctx, &tools).await?;
        let wiki_pages_created = if outcome2.confidence_delta > 0.0 {
            info!("Wiki compilation skill completed successfully");
            notes_processed
        } else {
            0
        };

        let contradictions_found = if notes_processed > 0 && !dry_run {
            let contradictions = self
                .detector
                .detect(&notes)
                .map_err(|e| KernelError::SkillFailed(e.to_string()))?;
            let count = contradictions.len();
            if count > 0 {
                let reports_dir = wiki_dir.join("reports");
                std::fs::create_dir_all(&reports_dir).map_err(|e| {
                    KernelError::SkillFailed(format!("failed to create reports dir: {e}"))
                })?;
                self.detector
                    .log_contradictions(&contradictions, &reports_dir)
                    .map_err(|e| KernelError::SkillFailed(e.to_string()))?;
                info!(count, "Contradictions detected and logged");
            } else {
                info!("No contradictions found");
            }
            count
        } else {
            let mut ctx = InvestigationContext::new("consolidation-contradiction", "zen-vault")
                .with_block(uuid::Uuid::now_v7());

            ctx.evidence.push(rig_compose::context::Evidence {
                recorded_at: std::time::SystemTime::now(),
                source_skill: "contradiction-setup".to_string(),
                label: "contradiction-detection".to_string(),
                detail: serde_json::json!({
                    "notes": notes_json["notes"],
                    "wiki_dir": wiki_dir.to_string_lossy(),
                }),
            });

            let outcome3 = self.detector.execute(&mut ctx, &tools).await?;
            info!(
                contradiction_delta = outcome3.confidence_delta,
                "Contradiction detection skill completed"
            );
            0
        };

        let migrated = if !dry_run {
            let archive_dir = wiki_dir
                .parent()
                .map(|p| p.join("archive"))
                .unwrap_or_else(|| wiki_dir.join("archive"));
            let txn = TransactionScope::new("workflow-archive");
            txn.begin()
                .map_err(|e| KernelError::ToolFailed(e.to_string()))?;
            let cycle_id = uuid::Uuid::now_v7().to_string();
            let archived = archive_processed_notes(&notes, &archive_dir, &cycle_id, &txn, false);
            txn.commit()
                .map_err(|e| KernelError::ToolFailed(e.to_string()))?;
            archived
        } else {
            Vec::new()
        };

        Ok(DistillationReport {
            notes_processed,
            entities_extracted,
            entities_persisted: 0,
            merged_count: 0,
            wiki_pages_created,
            contradictions_found,
            migrated_files: migrated,
        })
    }
}

/// Input bundle for the rig-compose [`Workflow`] implementation.
#[derive(Debug, Clone)]
pub struct DistillationPipelineInput {
    /// Directory containing inbox `.md` notes to process.
    pub inbox_dir: PathBuf,
    /// Target directory for compiled wiki pages.
    pub wiki_dir: PathBuf,
    /// When `true`, pipeline runs read-only — no FS/DB mutations.
    pub dry_run: bool,
}

impl DistillationPipelineInput {
    /// Create an input with `dry_run: false`.
    pub fn new(inbox_dir: PathBuf, wiki_dir: PathBuf) -> Self {
        Self {
            inbox_dir,
            wiki_dir,
            dry_run: false,
        }
    }

    /// Enable dry-run mode: pipeline executes without writing to disk.
    pub fn with_dry_run(mut self) -> Self {
        self.dry_run = true;
        self
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::distill::types::PlaceholderStatus;

    fn create_test_note(id: &str, content: &str) -> String {
        format!(
            r#"---
id: "{}"
source: "test"
sensitivity: private
created_at: "2026-05-23T15:00:00+00:00"
updated_at: "2026-05-23T15:00:00+00:00"
---

{}"#,
            id, content
        )
    }

    /// T007/T006-isolated runner: tmp inbox/wiki/archive/logs, no DB.
    async fn run_isolated(
        pipeline: &DistillationPipeline,
        inbox: &Path,
        wiki: &Path,
    ) -> Result<DistillationReport> {
        Ok(run_isolated_outcome(pipeline, inbox, wiki).await?.report)
    }

    /// Like `run_isolated` but returns the full [`ScopedRunOutcome`] so tests
    /// can assert CAS/placeholder fields.
    async fn run_isolated_outcome(
        pipeline: &DistillationPipeline,
        inbox: &Path,
        wiki: &Path,
    ) -> Result<ScopedRunOutcome> {
        let root = wiki.parent().unwrap_or(wiki);
        let archive = root.join("archive");
        let logs = root.join("logs");
        pipeline
            .run_scoped(inbox, wiki, &archive, &logs, None, None)
            .await
    }

    #[tokio::test]
    async fn test_pipeline_empty_inbox() {
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        fs::create_dir(&inbox_dir).unwrap();
        let wiki_dir = tmp.path().join("wiki");

        let pipeline = DistillationPipeline::new();
        let report = run_isolated(&pipeline, &inbox_dir, &wiki_dir)
            .await
            .unwrap();

        assert_eq!(report.notes_processed, 0);
        assert_eq!(report.entities_extracted, 0);
        assert_eq!(report.entities_persisted, 0);
        assert_eq!(report.wiki_pages_created, 0);
        assert_eq!(report.contradictions_found, 0);
    }

    #[tokio::test]
    async fn test_pipeline_nonexistent_inbox() {
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        let wiki_dir = tmp.path().join("wiki");

        let pipeline = DistillationPipeline::new();
        let report = run_isolated(&pipeline, &inbox_dir, &wiki_dir)
            .await
            .unwrap();

        assert_eq!(report.notes_processed, 0);
        assert_eq!(report.entities_extracted, 0);
        assert_eq!(report.entities_persisted, 0);
        assert_eq!(report.wiki_pages_created, 0);
        assert_eq!(report.contradictions_found, 0);
    }

    #[tokio::test]
    async fn test_pipeline_with_single_note() {
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        fs::create_dir(&inbox_dir).unwrap();
        let wiki_dir = tmp.path().join("wiki");

        let note_content = create_test_note(
            "note-1",
            "# Rust Project\n\nI love using Rust and Tokio for async programming.",
        );
        fs::write(inbox_dir.join("note1.md"), &note_content).unwrap();

        let pipeline = DistillationPipeline::new();
        let report = run_isolated(&pipeline, &inbox_dir, &wiki_dir)
            .await
            .unwrap();

        assert_eq!(report.notes_processed, 1);
        assert!(
            report.entities_extracted > 0,
            "Should extract Rust/Tokio notions"
        );
        assert_eq!(report.wiki_pages_created, 1, "Should create one wiki page");
        assert_eq!(report.contradictions_found, 0);
    }

    #[tokio::test]
    async fn test_pipeline_filters_non_md_files() {
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        fs::create_dir(&inbox_dir).unwrap();
        let wiki_dir = tmp.path().join("wiki");

        let note_content = create_test_note("note-1", "# Hello\n\nSome content about Python.");
        fs::write(inbox_dir.join("note1.md"), &note_content).unwrap();
        fs::write(inbox_dir.join("readme.txt"), "not a note").unwrap();
        fs::write(inbox_dir.join("data.json"), "{}").unwrap();

        let pipeline = DistillationPipeline::new();
        let report = run_isolated(&pipeline, &inbox_dir, &wiki_dir)
            .await
            .unwrap();

        assert_eq!(report.notes_processed, 1);
    }

    #[tokio::test]
    async fn test_pipeline_with_multiple_notes() {
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        fs::create_dir(&inbox_dir).unwrap();
        let wiki_dir = tmp.path().join("wiki");

        let note1 = create_test_note(
            "note-1",
            "# Systems Programming\n\nRust is great for performance-critical code.",
        );
        let note2 = create_test_note(
            "note-2",
            "# Data Science\n\nPython and its ecosystem for machine learning.",
        );
        fs::write(inbox_dir.join("01-rust.md"), &note1).unwrap();
        fs::write(inbox_dir.join("02-python.md"), &note2).unwrap();

        let pipeline = DistillationPipeline::new();
        let report = run_isolated(&pipeline, &inbox_dir, &wiki_dir)
            .await
            .unwrap();

        assert_eq!(report.notes_processed, 2);
        // Entities are deduplicated across notes
        assert!(report.entities_extracted > 0);
        assert!(
            report.entities_extracted >= 2,
            "Should find at least Python and Rust"
        );
    }

    #[tokio::test]
    async fn test_pipeline_skips_malformed_notes() {
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        fs::create_dir(&inbox_dir).unwrap();
        let wiki_dir = tmp.path().join("wiki");

        // Valid note
        let note1 = create_test_note("note-1", "# Hello\n\nSome content.");
        fs::write(inbox_dir.join("01-good.md"), &note1).unwrap();

        // Malformed frontmatter (missing closing ---)
        fs::write(inbox_dir.join("02-bad.md"), "---\nid: \"note-2\"\n\nbody").unwrap();

        let pipeline = DistillationPipeline::new();
        let report = run_isolated(&pipeline, &inbox_dir, &wiki_dir)
            .await
            .unwrap();

        assert_eq!(report.notes_processed, 1, "Should skip the malformed note");
    }

    #[tokio::test]
    async fn test_pipeline_creates_wiki_dir_if_missing() {
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        fs::create_dir(&inbox_dir).unwrap();
        // wiki_dir is intentionally NOT created
        let wiki_dir = tmp.path().join("wiki");

        let note = create_test_note("note-1", "# Hello");
        fs::write(inbox_dir.join("note.md"), &note).unwrap();

        let pipeline = DistillationPipeline::new();
        let result = run_isolated(&pipeline, &inbox_dir, &wiki_dir).await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_report_debug_derive() {
        let report = DistillationReport {
            notes_processed: 5,
            entities_extracted: 3,
            entities_persisted: 3,
            merged_count: 1,
            wiki_pages_created: 2,
            contradictions_found: 1,
            migrated_files: Vec::new(),
        };
        let debug_str = format!("{:?}", report);
        assert!(debug_str.contains("notes_processed"));
        assert!(debug_str.contains("5"));
    }

    #[test]
    fn test_report_clone() {
        let report = DistillationReport {
            notes_processed: 1,
            entities_extracted: 0,
            entities_persisted: 0,
            merged_count: 0,
            wiki_pages_created: 0,
            contradictions_found: 0,
            migrated_files: Vec::new(),
        };
        let cloned = report.clone();
        assert_eq!(report.notes_processed, cloned.notes_processed);
    }

    #[tokio::test]
    async fn test_pipeline_default() {
        let pipeline = DistillationPipeline::default();
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        let wiki_dir = tmp.path().join("wiki");

        let result = run_isolated(&pipeline, &inbox_dir, &wiki_dir).await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_auto_link_wikilinks_wraps_entity() {
        let content = "I love using Rust for systems programming.";
        let entities = vec!["Rust".to_string()];
        let result = auto_link_wikilinks(content, &entities);
        assert_eq!(result, "I love using [[Rust]] for systems programming.");
    }

    #[test]
    fn test_auto_link_wikilinks_skips_already_linked() {
        let content = "I love using [[Rust]] for systems programming.";
        let entities = vec!["Rust".to_string()];
        let result = auto_link_wikilinks(content, &entities);
        assert_eq!(result, "I love using [[Rust]] for systems programming.");
    }

    #[test]
    fn test_auto_link_wikilinks_multiple_entities() {
        let content = "Rust and Tokio are great for async programming.";
        let entities = vec!["Rust".to_string(), "Tokio".to_string()];
        let result = auto_link_wikilinks(content, &entities);
        assert_eq!(
            result,
            "[[Rust]] and [[Tokio]] are great for async programming."
        );
    }

    #[test]
    fn test_auto_link_wikilinks_longest_first() {
        let content = "PostgreSQL and SQL are databases.";
        let entities = vec!["SQL".to_string(), "PostgreSQL".to_string()];
        let result = auto_link_wikilinks(content, &entities);
        assert_eq!(result, "[[PostgreSQL]] and [[SQL]] are databases.");
    }

    #[test]
    fn test_auto_link_wikilinks_word_boundary() {
        let content = "Rustic is not Rust.";
        let entities = vec!["Rust".to_string()];
        let result = auto_link_wikilinks(content, &entities);
        assert_eq!(result, "Rustic is not [[Rust]].");
    }

    #[test]
    fn test_auto_link_wikilinks_no_entities() {
        let content = "No entities here.";
        let entities = vec![];
        let result = auto_link_wikilinks(content, &entities);
        assert_eq!(result, "No entities here.");
    }

    #[test]
    fn test_auto_link_wikilinks_empty_content() {
        let content = "";
        let entities = vec!["Rust".to_string()];
        let result = auto_link_wikilinks(content, &entities);
        assert_eq!(result, "");
    }

    #[test]
    fn test_auto_link_wikilinks_preserves_backticks() {
        let content = "Use `Rust` for programming.";
        let entities = vec!["Rust".to_string()];
        let result = auto_link_wikilinks(content, &entities);
        assert_eq!(result, "Use `Rust` for programming.");
    }

    fn archive_test_txn(logs: &Path) -> TransactionScope {
        let txn = TransactionScope::new("archive-test");
        std::fs::create_dir_all(logs).unwrap();
        txn.begin().unwrap();
        txn
    }

    #[test]
    fn test_archive_processed_notes_writes_month_dir_and_provenance() {
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        let archive_dir = tmp.path().join("archive");
        let logs_dir = tmp.path().join("logs");
        fs::create_dir_all(&inbox_dir).unwrap();

        let content = create_test_note("note-1", "# Work note");
        let source = inbox_dir.join("work-note.md");
        fs::write(&source, &content).unwrap();

        let notes = vec![Note {
            id: "note-1".to_string(),
            domain: vec![crate::note::Domain::Work],
            file_path: Some(source.clone()),
            ..Note::default()
        }];

        let txn = archive_test_txn(&logs_dir);
        let archived = archive_processed_notes(&notes, &archive_dir, "cycle-1", &txn, false);

        assert_eq!(archived.len(), 1);
        let (src, dst) = &archived[0];
        assert_eq!(src, &source);
        assert!(
            dst.starts_with(&archive_dir),
            "dest must live under archive dir"
        );
        assert!(!source.exists(), "inbox file should be gone");
        assert!(dst.exists(), "archived file should exist");

        let archived_content = fs::read_to_string(dst).unwrap();
        assert!(archived_content.contains("source_path:"));
        assert!(archived_content.contains("archived_at:"));
        assert!(archived_content.contains("cycle_id: \"cycle-1\""));
        assert!(archived_content.contains("original_created_at:"));
        assert!(archived_content.contains("checksum:"));
        assert!(archived_content.contains("merged_into:"));
    }

    #[test]
    fn test_archive_processed_notes_inbox_empty_after() {
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        let archive_dir = tmp.path().join("archive");
        let logs_dir = tmp.path().join("logs");
        fs::create_dir_all(&inbox_dir).unwrap();

        let c1 = create_test_note("n1", "# Note one");
        let c2 = create_test_note("n2", "# Note two");
        let s1 = inbox_dir.join("note1.md");
        let s2 = inbox_dir.join("note2.md");
        fs::write(&s1, &c1).unwrap();
        fs::write(&s2, &c2).unwrap();

        let notes = vec![
            Note {
                id: "n1".to_string(),
                domain: vec![crate::note::Domain::Personal],
                file_path: Some(s1.clone()),
                ..Note::default()
            },
            Note {
                id: "n2".to_string(),
                domain: vec![crate::note::Domain::Learning],
                file_path: Some(s2.clone()),
                ..Note::default()
            },
        ];

        let txn = archive_test_txn(&logs_dir);
        let archived = archive_processed_notes(&notes, &archive_dir, "cycle-2", &txn, false);

        assert_eq!(archived.len(), 2);
        assert!(!s1.exists());
        assert!(!s2.exists());

        let inbox_entries: Vec<_> = fs::read_dir(&inbox_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            inbox_entries.is_empty(),
            "inbox should be empty after archive"
        );
    }

    #[test]
    fn test_archive_avoids_overwrite_with_timestamp() {
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        let archive_dir = tmp.path().join("archive");
        let logs_dir = tmp.path().join("logs");
        let month_dir = archive_dir.join(chrono::Utc::now().format("%Y-%m").to_string());
        fs::create_dir_all(&inbox_dir).unwrap();
        fs::create_dir_all(&month_dir).unwrap();

        let content = create_test_note("note-3", "# Another work note");
        let source = inbox_dir.join("duplicate.md");
        fs::write(&source, &content).unwrap();
        fs::write(month_dir.join("duplicate.md"), "existing").unwrap();

        let notes = vec![Note {
            id: "note-3".to_string(),
            domain: vec![crate::note::Domain::Work],
            file_path: Some(source.clone()),
            ..Note::default()
        }];

        let txn = archive_test_txn(&logs_dir);
        let archived = archive_processed_notes(&notes, &archive_dir, "cycle-3", &txn, false);

        assert_eq!(archived.len(), 1);
        let (_, dst) = &archived[0];
        assert!(month_dir.join("duplicate.md").exists());
        let dst_name = dst.file_name().unwrap().to_string_lossy();
        assert!(
            dst_name.starts_with("duplicate_"),
            "expected timestamp suffix: {dst_name}"
        );
        assert!(dst.exists());
    }

    #[test]
    fn test_normalize_content_collapses_and_trims() {
        let raw = "\r\n# Title   \r\n\r\n\r\n\r\nBody line.  \n\n\n\n";
        let out = normalize_content(raw);
        assert!(!out.contains('\r'), "CRLF must become LF");
        assert!(
            !out.contains("   \n"),
            "trailing whitespace must be stripped"
        );
        assert!(out.starts_with("# Title"), "leading blanks trimmed");
        assert!(out.ends_with("Body line.\n"), "trailing blanks trimmed");
        assert!(!out.contains("\n\n\n"), "3+ blank lines collapsed to 2");
    }

    #[test]
    fn test_normalize_content_keeps_single_blank_lines() {
        let out = normalize_content("# T\n\nBody.\n");
        assert_eq!(out, "# T\n\nBody.\n");
    }

    #[test]
    fn test_append_provenance_into_existing_frontmatter() {
        let raw = "---\nid: \"n1\"\n---\n\nbody";
        let out = append_provenance(raw, &[("cycle_id", "c9".to_string())]);
        assert!(out.starts_with("---\n"));
        assert!(out.contains("id: \"n1\""));
        assert!(out.contains("cycle_id: \"c9\""));
        assert!(out.contains("---\n\nbody"));
    }

    #[test]
    fn test_append_provenance_prepends_when_missing() {
        let out = append_provenance("no frontmatter here", &[("cycle_id", "c9".into())]);
        assert!(out.starts_with("---\ncycle_id: \"c9\"\n---\n"));
    }

    // ── FR-032 CAS commit + FR-031b placeholder consult ────────────────

    const RUST_NOTE: &str = "# Rust Project\n\nI love using Rust and Tokio for async programming.";

    /// FR-032 happy path: with CAS on (default) the inbox source removal is
    /// deferred past `commit_conditional` — a clean commit reaps it and the
    /// archive dest + compiled wiki page survive.
    #[tokio::test]
    async fn test_cas_clean_commit_removes_deferred_source() {
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        fs::create_dir(&inbox_dir).unwrap();
        let wiki_dir = tmp.path().join("wiki");
        fs::create_dir(&wiki_dir).unwrap();
        // Non-empty wiki inventory so the VersionSnapshot is captured.
        fs::write(wiki_dir.join("seed.md"), "# Seed\n\nAnchor page for CAS.").unwrap();

        let source = inbox_dir.join("note1.md");
        fs::write(&source, create_test_note("note-1", RUST_NOTE)).unwrap();

        let pipeline = DistillationPipeline::new();
        let outcome = run_isolated_outcome(&pipeline, &inbox_dir, &wiki_dir)
            .await
            .unwrap();

        assert!(!outcome.cas_rolled_back);
        assert!(outcome.cas_drifted.is_empty());
        assert_eq!(outcome.report.notes_processed, 1);

        // Deferral did not break the flow: source removed only AFTER commit.
        assert!(!source.exists(), "inbox source must be removed post-commit");
        let dest = &outcome.report.migrated_files[0].1;
        assert!(dest.exists(), "archive dest must exist");
        assert!(
            wiki_dir.join("notions/technology/rust-project.md").exists(),
            "compiled page must survive the conditional commit"
        );
    }

    /// FR-032 regression guard: CAS off restores today's behavior — source is
    /// removed during the archive stage, no deferral, no rollback.
    #[tokio::test]
    async fn test_cas_off_removes_source_during_archive_stage() {
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        fs::create_dir(&inbox_dir).unwrap();
        let wiki_dir = tmp.path().join("wiki");
        fs::create_dir(&wiki_dir).unwrap();
        fs::write(wiki_dir.join("seed.md"), "# Seed").unwrap();

        let source = inbox_dir.join("note1.md");
        fs::write(&source, create_test_note("note-1", RUST_NOTE)).unwrap();

        let pipeline = DistillationPipeline::new().with_cas_commit(false);
        let outcome = run_isolated_outcome(&pipeline, &inbox_dir, &wiki_dir)
            .await
            .unwrap();

        assert!(!outcome.cas_rolled_back);
        assert!(outcome.deferred_sources.is_empty());
        assert!(!source.exists(), "CAS off: source removed in-stage");
        assert!(outcome.report.migrated_files[0].1.exists());
    }

    /// FR-031b: a slug reserved in placeholders.json downgrades the page
    /// create to an update (page still written), and the saved registry
    /// shows the slot advanced to Merged.
    #[tokio::test]
    async fn test_placeholder_downgrade_recorded_and_merged() {
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        fs::create_dir(&inbox_dir).unwrap();
        let wiki_dir = tmp.path().join("wiki");

        fs::write(
            inbox_dir.join("note1.md"),
            create_test_note("note-1", RUST_NOTE),
        )
        .unwrap();

        // Pre-declare the slug this note compiles to ("rust-project": title
        // "Rust Project" → slugify → notions/technology/rust-project.md).
        let logs_dir = tmp.path().join("logs");
        fs::create_dir_all(&logs_dir).unwrap();
        let placeholders_path = logs_dir.join("placeholders.json");
        let mut reg = PlaceholderRegistry::new();
        reg.declare("rust-project", "agent-a");
        reg.save(&placeholders_path).unwrap();

        let pipeline = DistillationPipeline::new();
        let outcome = run_isolated_outcome(&pipeline, &inbox_dir, &wiki_dir)
            .await
            .unwrap();

        assert!(
            outcome.placeholder_downgrades >= 1,
            "reserved slug create must be recorded as a downgrade"
        );
        assert!(
            wiki_dir.join("notions/technology/rust-project.md").exists(),
            "page still created (create-as-update semantics)"
        );

        let reloaded = PlaceholderRegistry::load(&placeholders_path).unwrap();
        assert_eq!(
            reloaded.lookup("rust-project").unwrap().status,
            PlaceholderStatus::Merged,
            "saved registry must show the slot merged"
        );
    }

    /// FR-032 self-write awareness: the compiler regenerates `index.md` and
    /// `log.md` every cycle, so from cycle 2 on those churn files sit inside
    /// the snapshot scope. Txn-tracked self-writes must NOT count as drift —
    /// otherwise every writing cycle rolls back (livelock).
    #[tokio::test]
    async fn test_cas_self_writes_do_not_roll_back() {
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        fs::create_dir(&inbox_dir).unwrap();
        let wiki_dir = tmp.path().join("wiki");
        fs::create_dir(&wiki_dir).unwrap();
        fs::write(wiki_dir.join("seed.md"), "# Seed").unwrap();
        fs::write(wiki_dir.join("index.md"), "# Index\n\n- stale entry\n").unwrap();
        fs::write(wiki_dir.join("log.md"), "# Log\n\nold-cycle entry\n").unwrap();

        let source = inbox_dir.join("note1.md");
        fs::write(&source, create_test_note("note-1", RUST_NOTE)).unwrap();

        let pipeline = DistillationPipeline::new();
        let outcome = run_isolated_outcome(&pipeline, &inbox_dir, &wiki_dir)
            .await
            .unwrap();

        assert!(
            !outcome.cas_rolled_back,
            "self-writes (index/log regeneration) must not trigger CAS rollback"
        );
        assert!(outcome.cas_drifted.is_empty());
        assert!(!source.exists(), "clean commit reaps the deferred source");
    }

    /// FR-032 drift: an external edit inside the snapshot window forces
    /// `commit_conditional` to roll back every tracked output while the
    /// drifted (non-tracked) wiki file stays intact.
    #[test]
    fn test_cas_drift_rolls_back_tracked_outputs() {
        let tmp = tempdir().unwrap();
        let wiki = tmp.path().join("wiki");
        fs::create_dir_all(&wiki).unwrap();
        let wiki_page = wiki.join("rust.md");
        fs::write(&wiki_page, "# Rust\n\noriginal").unwrap();

        let txn = TransactionScope::new("cas-drift-pipeline-test");
        txn.begin().unwrap();
        // Tracked cycle output (archive dest analogue).
        let tracked_output = tmp.path().join("archive").join("dest.md");
        fs::create_dir_all(tracked_output.parent().unwrap()).unwrap();
        fs::write(&tracked_output, "compiled").unwrap();
        txn.track_path(&tracked_output).unwrap();

        let snapshot = VersionSnapshot::capture(std::slice::from_ref(&wiki_page)).unwrap();
        // External modification between capture and commit → drift.
        fs::write(&wiki_page, "# Rust\n\nexternal edit").unwrap();

        match txn.commit_conditional(&snapshot).unwrap() {
            crate::distill::transaction::CasCommitOutcome::RolledBack { drifted } => {
                assert_eq!(drifted, vec![wiki_page.clone()]);
            }
            crate::distill::transaction::CasCommitOutcome::Committed => {
                panic!("external edit must force rollback")
            }
        }
        assert!(!tracked_output.exists(), "rollback deletes tracked outputs");
        assert!(wiki_page.exists(), "drifted wiki file stays intact");
    }
}
