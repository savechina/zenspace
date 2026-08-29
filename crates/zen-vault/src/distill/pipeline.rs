use std::path::Path;
use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;
use rig_compose::context::InvestigationContext;
use rig_compose::registry::{KernelError, ToolRegistry};
use rig_compose::skill::Skill;
use rig_compose::workflow::Workflow;
use tracing::info;

use super::checkpoint::Checkpoint;
use super::contradiction::ContradictionDetector;
use super::checkpoint::CheckpointManager;
use super::notion_extraction::NotionExtractor;
use super::recovery::RecoveryManager;
use super::transaction::TransactionScope;
use super::wiki_compile::WikiCompiler;
use crate::notion::service::NotionService;

use crate::note::{Note, parse_frontmatter};
use crate::tindy::checksum::ChangeDetector;
use crate::wiki::WikiPage;

#[derive(Debug, Clone)]
pub struct DistillationReport {
    pub notes_processed: usize,
    pub entities_extracted: usize,
    /// Notions durably upserted into the DB graph via NotionService (T003).
    pub entities_persisted: usize,
    pub wiki_pages_created: usize,
    pub contradictions_found: usize,
    /// Raw notes archived to `vault/archive/<yyyy-mm>/` (T005; was wiki-moves).
    pub migrated_files: Vec<(PathBuf, PathBuf)>,
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
/// Replaces the old wiki-tree move: raw notes leave the inbox but never
/// enter the wiki domain dirs; the inbox is empty after a Completed cycle.
fn archive_processed_notes(
    notes: &[Note],
    archive_dir: &Path,
    cycle_id: &str,
    track: &TransactionScope,
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
                (
                    "original_created_at",
                    note.created_at.to_rfc3339(),
                ),
                ("checksum", checksum),
                ("merged_into", String::new()),
            ],
        );

        match std::fs::write(&dest, provenance)
            .and_then(|_| std::fs::remove_file(&source))
        {
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

pub struct DistillationPipeline {
    extractor: NotionExtractor,
    compiler: WikiCompiler,
    detector: ContradictionDetector,
    notion_service: NotionService,
}

impl DistillationPipeline {
    pub fn new() -> Self {
        Self {
            extractor: NotionExtractor::new(),
            compiler: WikiCompiler::new(),
            detector: ContradictionDetector::new(),
            notion_service: NotionService::new(),
        }
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
        let logs_dir = ZenPaths::detect()
            .map(|p| p.logs().to_path_buf())
            .unwrap_or_else(|_| vault_root.join("logs"));
        let db = zen_repo::SqliteClient::open_lazy(&logs_dir.join("state.db"))
            .await
            .ok();
        self.run_scoped(inbox_dir, wiki_dir, &archive_dir, &logs_dir, db.as_ref())
            .await
    }

    /// Isolated-dirs entry (worker + tests): all stage dirs injected, DB optional.
    ///
    /// Stage chain (worker contract stage 3):
    /// T007 checkpoint gate → load → T004 normalize → extract →
    /// T003 NotionService persist → auto-link → compile → contradictions →
    /// T005 archive, with T006 TransactionScope around mutations.
    pub async fn run_scoped(
        &self,
        inbox_dir: &Path,
        wiki_dir: &Path,
        archive_dir: &Path,
        logs_dir: &Path,
        db: Option<&zen_repo::SqliteClient>,
    ) -> Result<DistillationReport> {
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
        let run = self.run_stages(inbox_dir, wiki_dir, archive_dir, db, &txn, &cycle_id);
        match run.await {
            Ok(report) => {
                txn.commit()?;
                checkpoints.write_checkpoint(&Checkpoint {
                    status: "completed".to_string(),
                    started_at: chrono::Utc::now().to_rfc3339(),
                    notes_count: report.notes_processed,
                })?;
                Ok(report)
            }
            Err(e) => {
                if let Err(rb) = txn.rollback() {
                    tracing::warn!(error = %rb, "Transaction rollback itself failed");
                }
                Err(e)
            }
        }
    }

    async fn run_stages(
        &self,
        inbox_dir: &Path,
        wiki_dir: &Path,
        archive_dir: &Path,
        db: Option<&zen_repo::SqliteClient>,
        txn: &TransactionScope,
        cycle_id: &str,
    ) -> Result<DistillationReport> {
        let notes = self.load_notes(inbox_dir)?;
        let notes_processed = notes.len();
        info!(
            notes_processed,
            "Loaded notes from inbox, starting consolidation pipeline"
        );

        let notions = self.extractor.extract_batch(&notes)?;
        let entities_extracted = notions.len();
        info!(entities_extracted, "Notion extraction complete");

        // T003: persist extracted notions into the DB graph (Principle XII).
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

        let normalized: Vec<Note> = notes
            .iter()
            .map(|note| {
                let mut n = note.clone();
                n.content = normalize_content(&note.content);
                n
            })
            .collect();

        let entity_names: Vec<String> = notions.iter().map(|n| n.name.clone()).collect();
        let linked_notes: Vec<Note> = normalized
            .into_iter()
            .map(|mut note| {
                note.content = auto_link_wikilinks(&note.content, &entity_names);
                note
            })
            .collect();

        let pages = self.compiler.compile(&linked_notes, wiki_dir)?;
        let wiki_pages_created = pages.len();
        for page in &pages {
            if let Some(path) = page_file_path(page) {
                txn.track_path(&path)?;
            }
        }
        if wiki_pages_created > 0 {
            info!(wiki_pages_created, "Wiki pages compiled and written");
        }

        let contradictions = self.detector.detect(&notes)?;
        let contradictions_found = contradictions.len();
        if contradictions_found > 0 {
            self.detector
                .log_contradictions(&contradictions, wiki_dir)?;
            info!(contradictions_found, "Contradictions detected and logged");
        } else {
            info!("No contradictions found");
        }

        let archived = archive_processed_notes(&notes, archive_dir, cycle_id, txn);

        Ok(DistillationReport {
            notes_processed,
            entities_extracted,
            entities_persisted,
            wiki_pages_created,
            contradictions_found,
            migrated_files: archived,
        })
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

fn page_file_path(page: &WikiPage) -> Option<PathBuf> {
    Some(page.path.clone())
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
            let archived = archive_processed_notes(&notes, &archive_dir, &cycle_id, &txn);
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
            wiki_pages_created,
            contradictions_found,
            migrated_files: migrated,
        })
    }
}

#[derive(Debug, Clone)]
pub struct DistillationPipelineInput {
    pub inbox_dir: PathBuf,
    pub wiki_dir: PathBuf,
    pub dry_run: bool,
}

impl DistillationPipelineInput {
    pub fn new(inbox_dir: PathBuf, wiki_dir: PathBuf) -> Self {
        Self {
            inbox_dir,
            wiki_dir,
            dry_run: false,
        }
    }

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
        let root = wiki.parent().unwrap_or(wiki);
        let archive = root.join("archive");
        let logs = root.join("logs");
        pipeline
            .run_scoped(inbox, wiki, &archive, &logs, None)
            .await
    }

    #[tokio::test]
    async fn test_pipeline_empty_inbox() {
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        fs::create_dir(&inbox_dir).unwrap();
        let wiki_dir = tmp.path().join("wiki");

        let pipeline = DistillationPipeline::new();
        let report = run_isolated(&pipeline, &inbox_dir, &wiki_dir).await.unwrap();

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
        let report = run_isolated(&pipeline, &inbox_dir, &wiki_dir).await.unwrap();

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
        let report = run_isolated(&pipeline, &inbox_dir, &wiki_dir).await.unwrap();

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
        let report = run_isolated(&pipeline, &inbox_dir, &wiki_dir).await.unwrap();

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
        let report = run_isolated(&pipeline, &inbox_dir, &wiki_dir).await.unwrap();

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
        let report = run_isolated(&pipeline, &inbox_dir, &wiki_dir).await.unwrap();

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
        let archived = archive_processed_notes(&notes, &archive_dir, "cycle-1", &txn);

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
        let archived = archive_processed_notes(&notes, &archive_dir, "cycle-2", &txn);

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
        let archived = archive_processed_notes(&notes, &archive_dir, "cycle-3", &txn);

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
        assert!(!out.contains("   \n"), "trailing whitespace must be stripped");
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
}
