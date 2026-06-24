use crate::journal::Journal;
use std::collections::{HashMap, HashSet};
use std::fs;

use chrono::NaiveDate;
use tracing::{debug, error, info, warn};

use zen_core::paths::ZenPaths;
use zen_vault::{
    Entity, EntityData, EntityService, EntityType, RelationType, Relationship, WikiCompiler,
};

// ─── ExtractedSignals — Typed signals from session conversations ─────────

/// Signals extracted from a session conversation, grouped by type.
/// Used by SessionJournaler (extraction) and JournalWorker (routing).
#[derive(Debug, Clone, Default)]
pub struct ExtractedSignals {
    /// Past-tense durable facts: what happened, decisions made, things learned.
    pub facts: Vec<String>,
    /// Self-assessments of what went wrong or could be better (思危).
    pub reflections: Vec<String>,
    /// Forward-looking promises: what the user committed to do (思变).
    pub commitments: Vec<String>,
}

impl ExtractedSignals {
    pub fn is_empty(&self) -> bool {
        self.facts.is_empty() && self.reflections.is_empty() && self.commitments.is_empty()
    }

    pub fn total(&self) -> usize {
        self.facts.len() + self.reflections.len() + self.commitments.len()
    }
}

// ─── ZenDream — Knowledge Consolidation Pipeline ────────────────────────

/// ZenDream — Nightly consolidation state.
///
/// Runs during a configurable window (default 2AM–4AM) to:
/// 1. Consolidate daily logs → extract durable facts
/// 2. Update MEMORY.md with all new durable facts (personal record)
/// 3. **Promote entity-aware facts to knowledge graph** — detect tech/concept entities, write to graph.db + vec.db
/// 4. Compress old subconscious logs
/// 5. Recompute entity relationships from wiki
///
/// All operations are offline-first — no network/LLM calls required.
pub struct ZenDream;

impl ZenDream {
    /// Create a new ZenDream instance.
    pub fn new() -> Self {
        Self
    }

    /// Execute the full dream cycle for a given date.
    ///
    /// Refactored: fact extraction is the single shared bridge for both
    /// MEMORY.md and knowledge-graph update_knowledge().
    pub fn run_cycle(&self, zen_paths: &ZenPaths, date: NaiveDate) -> Result<DreamReport, DreamError> {
        info!("dream cycle started for {date}");

        let facts = extract_facts_from_journal_entries(zen_paths, date)?;

        if facts.is_empty() {
            debug!("no durable facts extracted for {date}, skipping memory update");
            return Ok(DreamReport::empty(date));
        }

        info!(fact_count = facts.len(), "extracted {} durable fact(s) for {date}", facts.len());

        // Phase B: entity promotion + wiki compilation moved to WikiCompilerWorker.
        // DreamWorker now only handles MEMORY.md update + maintenance.

        let memory_updated = update_memory_from_facts(zen_paths, &facts, "Dream")?;

        if memory_updated {
            info!("MEMORY.md updated with {} new fact(s)", facts.len());
        }

        let report = DreamReport {
            date,
            facts_extracted: facts.len(),
            memory_updated,
            logs_compressed: compress_old_logs(zen_paths)?,
            entities_recomputed: recompute_entities(zen_paths)?,
            knowledge_updated: false,  // Phase B: moved to WikiCompilerWorker
            wiki_pages_created: 0,     // Phase B: moved to WikiCompilerWorker
        };

        info!(
            "dream cycle complete: facts={}, memory_updated={}, logs_compressed={}, entities_recomputed={}",
            report.facts_extracted,
            report.memory_updated,
            report.logs_compressed,
            report.entities_recomputed
        );

        Ok(report)
    }

    /// Backfill the knowledge graph by replaying daily logs from `from` to `to`.
    ///
    /// Phase 3 Task 7: First-run historical scanning. Each day is processed
    /// via `run_cycle`; failures are logged and skipped, not fatal.
    pub fn backfill(
        &self,
        zen_paths: &ZenPaths,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Vec<DreamReport> {
        let mut reports = Vec::new();
        let mut date = from;

        while date <= to {
            match self.run_cycle(zen_paths, date) {
                Ok(report) => {
                    info!(date = %date, facts = report.facts_extracted, "backfill cycle ok");
                    reports.push(report);
                }
                Err(e) => {
                    warn!(date = %date, error = %e, "backfill cycle failed, skipping");
                }
            }
            date = date.succ_opt().unwrap_or(date);
        }

        let total_facts: usize = reports.iter().map(|r| r.facts_extracted).sum();
        info!(
            days_processed = reports.len(),
            total_facts,
            "backfill complete: {} days, {} facts",
            reports.len(),
            total_facts
        );

        reports
    }
}

impl Default for ZenDream {
    fn default() -> Self {
        Self::new()
    }
}

// ─── DreamReport — Consolidation outcome summary ────────────────────────

/// Result summary from a dream cycle run.
#[derive(Debug, Default)]
pub struct DreamReport {
    pub date: NaiveDate,
    pub facts_extracted: usize,
    pub memory_updated: bool,
    pub knowledge_updated: bool,
    pub wiki_pages_created: usize,
    pub logs_compressed: bool,
    pub entities_recomputed: usize,
}

impl DreamReport {
    fn empty(date: NaiveDate) -> Self {
        Self {
            date,
            facts_extracted: 0,
            memory_updated: false,
            knowledge_updated: false,
            wiki_pages_created: 0,
            logs_compressed: false,
            entities_recomputed: 0,
        }
    }
}

// ─── Error type ────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum DreamError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("failed to read daily log: {0}")]
    DailyLogRead(String),

    #[error("failed to update MEMORY.md: {0}")]
    MemoryUpdate(String),

    #[error("failed to persist knowledge graph: {0}")]
    KnowledgeGraphPersist(String),

    #[error("failed to update vec.db embeddings: {0}")]
    EmbeddingStore(String),

    #[error("failed to compile wiki page: {0}")]
    WikiCompile(String),
}

// ─── Core Pipeline: Unified Fact Extraction Tasks 1-3 ──────────────────────

/// Extract durable facts from journal entries for a given date.
/// Reads `memories/journal/{date}-*.md` files written by SessionJournaler.
/// Parses the `## Facts` section from each entry.
fn extract_facts_from_journal_entries(zen_paths: &ZenPaths, date: NaiveDate) -> Result<Vec<String>, DreamError> {
    let journal_dir = zen_paths.journal_entries();

    if !journal_dir.exists() {
        debug!("journal entries dir does not exist: {}", journal_dir.display());
        return Ok(Vec::new());
    }

    let date_prefix = date.format("%Y-%m-%d").to_string();
    let mut facts = Vec::new();

    // Scan for files matching {date}-*.md
    let entries = match std::fs::read_dir(&journal_dir) {
        Ok(e) => e,
        Err(_) => return Ok(Vec::new()),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };

        // Match files like "2026-06-24-abc123.md"
        if !name.starts_with(&date_prefix) || !name.ends_with(".md") {
            continue;
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Parse ## Facts section
        let entry_facts = parse_facts_section(&content);
        if !entry_facts.is_empty() {
            debug!(file = %name, count = entry_facts.len(), "extracted facts from journal entry");
            facts.extend(entry_facts);
        }
    }

    Ok(facts)
}

/// Parse bullet-list facts from a `## Facts` section in markdown.
pub(crate) fn parse_facts_section(content: &str) -> Vec<String> {
    let mut facts = Vec::new();
    let mut in_facts_section = false;

    for line in content.lines() {
        if line.starts_with("## ") {
            in_facts_section = line.trim_start_matches("## ").trim() == "Facts";
            continue;
        }
        if in_facts_section {
            let trimmed = line.trim();
            if trimmed.starts_with("- ") {
                let fact = trimmed.trim_start_matches("- ").trim();
                if !fact.is_empty() && !fact.starts_with("_(no") {
                    facts.push(fact.to_string());
                }
            }
        }
    }
    facts
}

/// Step 1 (refactored): Consolidate daily log → durable facts.
///
/// Reads the daily log for `date`, extracts structured facts from entries,
/// returns the full Vec<String> as a **shared bridge** — consumed by both
/// MEMORY.md and knowledge-graph update_knowledge().
#[allow(dead_code)]
fn extract_durable_facts(zen_paths: &ZenPaths, date: NaiveDate) -> Result<Vec<String>, DreamError> {
    let entries = Journal::read_entries(zen_paths, date)
        .map_err(|e| DreamError::DailyLogRead(e.to_string()))?;

    if entries.is_empty() {
        return Ok(Vec::new());
    }

    let mut facts = Vec::new();
    
    for entry in &entries {
        let entry_facts = extract_durable_facts_from_entry(&entry.content);
        if !entry_facts.is_empty() {
            debug!(
                "extracted {} durable fact(s) from entry at {}",
                entry_facts.len(),
                entry.timestamp
            );
            facts.extend(entry_facts);
        }
    }

    Ok(facts)
}

/// Extract durable facts from a single journal entry content string.
///
/// Heuristic: lines that look like completed actions (past tense verbs).
pub fn extract_durable_facts_from_entry(content: &str) -> Vec<String> {
    const ACTION_KEYWORDS: &[&str] = &[
        "completed", "fixed", "added", "removed", "resolved",
        "implemented", "created", "shipped", "deployed", "updated",
        "design", "build", "migrate", "refactor", "optimize", "test",
    ];

    let mut facts = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let lower = trimmed.to_lowercase();
        if ACTION_KEYWORDS.iter().any(|kw| lower.contains(kw)) {
            facts.push(trimmed.to_string());
        }
    }

    facts
}

// ─── Step 2: All Facts → MEMORY.md — Tasks 2 & 3 ────────────────────────

/// NEW Step 2 (refactored): Update MEMORY.md from the shared facts Vec.
///
/// Replaces the old update_memory() which read entries again internally.
/// Now accepts pre-extracted facts as a bridge — ensures ALL durable facts
/// are stored in MEMORY.md, while **entity-aware facts also go to knowledge graph**.
/// Personal-only facts (no known entity names) go ONLY to MEMORY.md (not graph).
pub fn update_memory_from_facts(zen_paths: &ZenPaths, facts: &[String], source: &str) -> Result<bool, DreamError> {
    if facts.is_empty() {
        debug!("no facts provided for MEMORY.md update");
        return Ok(false);
    }

    let memory_path = zen_paths.identity().join("MEMORY.md");

    if !memory_path.exists() {
        debug!("MEMORY.md not found, skipping update");
        return Ok(false);
    }

    // Deduplicate facts before writing
    let unique_facts: Vec<String> = dedupe_facts(facts);
    
    if unique_facts.is_empty() {
        debug!("all facts were duplicates, skipping MEMORY.md write");
        return Ok(false);
    }

    let now = chrono::Utc::now().date_naive();
    let section_marker = format!("## {source} Facts — {now}");

    let content = fs::read_to_string(&memory_path)?;

    if content.contains(&section_marker) {
        debug!("dream facts for {now} already present in MEMORY.md");
        return Ok(false);
    }

    let mut update = String::new();
    update.push_str(&format!("\n{section_marker}\n\n"));
    for fact in &unique_facts {
        update.push_str(&format!("- {fact}\n"));
    }

    let new_content = format!("{content}{update}");
    let tmp_path = memory_path.with_extension("md.tmp");
    fs::write(&tmp_path, &new_content)?;
    fs::rename(&tmp_path, &memory_path)?;

    info!("MEMORY.md updated with {} new fact(s)", unique_facts.len());
    Ok(true)
}

/// Deduplicate fact strings while preserving order.
fn dedupe_facts(facts: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    facts.iter()
        .filter(|f| seen.insert(f.to_lowercase()))
        .cloned()
        .collect()
}

// ─── Step 3: Knowledge Graph Writes + Wiki Generation ───────────────────

const TECH_KEYWORDS: &[&str] = &[
    "rust", "python", "typescript", "javascript",
    "clip", "llm", "gpt", "openai",
    "anthropic", "deepseek", "gemini",
    "tokio", "async", "sqlite",
    "fts5", "vector", "embedding",
];

/// Promote entity-aware facts to graph.db + generate wiki pages via WikiCompiler.
/// Personal-only facts (no known entity match) are skipped — they go to MEMORY.md only.
#[allow(dead_code)]
fn update_knowledge(zen_paths: &ZenPaths, facts: &[String]) -> (bool, usize) {
    let graph_db = zen_paths.db().join("graph.db");
    let vec_db = zen_paths.db().join("vec.db");
    let wiki_dir = zen_paths.vault().join("wiki");
    let svc = EntityService::new();

    let mut known: HashSet<String> = svc
        .load_known_entity_names(&graph_db)
        .unwrap_or_default();
    for kw in TECH_KEYWORDS {
        known.insert((*kw).to_string());
    }

    if known.is_empty() && facts.is_empty() {
        debug!("no entities nor facts to process, skipping update_knowledge");
        return (false, 0);
    }

    let mut entity_facts: HashMap<String, Vec<String>> = HashMap::new();
    let mut personal_only = 0usize;

    for fact in facts {
        match find_entity_match(fact, &known) {
            Some(entity) => entity_facts.entry(entity).or_default().push(fact.clone()),
            None => personal_only += 1,
        }
    }

    if personal_only > 0 {
        debug!(count = personal_only, "personal-only facts skip knowledge graph");
    }

    if entity_facts.is_empty() {
        return (false, 0);
    }

    let mut entity_data_list: Vec<EntityData> = Vec::new();
    let mut entity_ids: HashMap<String, String> = HashMap::new();
    let mut entity_index: HashMap<String, usize> = HashMap::new();

    for (name, fact_list) in &entity_facts {
        let canonical = name.to_lowercase();
        let entity = Entity::new(canonical.clone(), EntityType::Technology, "dream-cycle");
        if let Err(e) = svc.upsert_entity(&graph_db, &entity) {
            error!(entity = %canonical, error = %e, "failed to upsert entity to graph.db");
            continue;
        }
        entity_ids.insert(canonical.clone(), entity.id.clone());
        debug!(entity = %canonical, fact_count = fact_list.len(), "upserted entity to graph.db");

        let entity_text = format!("{name}: {}", fact_list.join(" "));
        if let Err(e) = svc.store_entity_embedding(&vec_db, &entity.id, &entity_text) {
            debug!(entity = %name, error = %e, "failed to store entity embedding (non-fatal)");
        }

        entity_index.insert(canonical.clone(), entity_data_list.len());
        entity_data_list.push(EntityData {
            entity,
            facts: fact_list.clone(),
            relationships: Vec::new(),
        });
    }

    for fact in facts {
        let mentioned: Vec<String> = entity_ids.keys()
            .filter(|e| fact.to_lowercase().contains(&e.to_lowercase()))
            .cloned()
            .collect();

        if mentioned.len() >= 2 {
            let source = &mentioned[0];
            let target = &mentioned[1];
            let rel = Relationship::new(
                entity_ids[source].clone(),
                entity_ids[target].clone(),
                RelationType::RelatedTo,
                "dream-cycle",
            );
            if let Err(e) = svc.insert_relationship(&graph_db, &rel) {
                debug!(error = %e, "failed to insert relationship (non-fatal)");
            }
            if let Some(&idx) = entity_index.get(source) {
                entity_data_list[idx].relationships
                    .push((target.clone(), RelationType::RelatedTo));
            }
        }
    }

    let wiki_pages = match WikiCompiler::new().compile_from_entities(&entity_data_list, &wiki_dir) {
        Ok(n) => n,
        Err(e) => {
            error!(error = %e, "WikiCompiler::compile_from_entities failed");
            0
        }
    };

    (true, wiki_pages)
}

fn find_entity_match(fact: &str, entities: &HashSet<String>) -> Option<String> {
    let lower = fact.to_lowercase();
    for entity in entities {
        if lower.contains(&entity.to_lowercase()) {
            return Some(entity.clone());
        }
    }
    None
}

// ─── Post-Pipeline Helpers (deferred — not Phase 0 scope) ───────────────

/// Compress old subconscious logs beyond the retention window.
///
/// When `logs/subconscious.md` exceeds ~100KB or 500 lines, archives the
/// oldest portion to `logs/archive/subconscious-{YYYY-MM}.md` and keeps
/// only the most recent `MAX_LINES` lines in the live file.
fn compress_old_logs(zen_paths: &ZenPaths) -> Result<bool, DreamError> {
    let log_path = zen_paths.logs().join("subconscious.md");

    if !log_path.exists() {
        return Ok(false);
    }

    let content = fs::read_to_string(&log_path)?;
    const MAX_LINES: usize = 500;
    const ARCHIVE_THRESHOLD_BYTES: usize = 100_000; // ~100KB

    // Skip if file is small enough
    if content.len() < ARCHIVE_THRESHOLD_BYTES {
        return Ok(false);
    }

    let lines: Vec<&str> = content.lines().collect();
    if lines.len() <= MAX_LINES {
        return Ok(false);
    }

    // Split: archive everything before the last MAX_LINES
    let split_point = lines.len() - MAX_LINES;
    let archive_content: String = lines[..split_point].join("\n");
    let remaining_content: String = lines[split_point..].join("\n");

    // Write archive
    let archive_dir = zen_paths.logs().join("archive");
    fs::create_dir_all(&archive_dir)?;

    let now = chrono::Utc::now();
    let archive_name = format!("subconscious-{}.md", now.format("%Y-%m"));
    let archive_path = archive_dir.join(&archive_name);

    // If archive file for this month exists, append; else create
    if archive_path.exists() {
        let existing = fs::read_to_string(&archive_path)?;
        fs::write(&archive_path, format!("{existing}\n\n{archive_content}"))?;
    } else {
        fs::write(&archive_path, &archive_content)?;
    }

    // Write remaining content atomically
    let tmp_path = log_path.with_extension("md.tmp");
    fs::write(&tmp_path, &remaining_content)?;
    fs::rename(&tmp_path, &log_path)?;

    info!(
        "compress_old_logs: archived {} lines to {}, kept {} lines",
        split_point,
        archive_path.display(),
        MAX_LINES
    );

    Ok(true)
}

/// Recompute entity relationships by scanning the wiki directory.
///
/// TODO(phase-1): implement graph rebuild from `wiki/entities/*.md`
/// frontmatter + cross-references. Returns `0` (no-op) until then.
fn recompute_entities(_zen_paths: &ZenPaths) -> Result<usize, DreamError> {
    debug!("recompute_entities: stubbed, no entities recomputed");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_test_paths() -> (TempDir, ZenPaths) {
        let dir = TempDir::new().unwrap();
        let paths = ZenPaths::for_testing(dir.path().to_path_buf());
        (dir, paths)
    }

    #[test]
    fn test_section_marker_includes_source() {
        let (_dir, paths) = setup_test_paths();
        let identity = paths.identity();
        fs::create_dir_all(&identity).unwrap();
        let memory_path = identity.join("MEMORY.md");
        fs::write(&memory_path, "# Memory\n").unwrap();

        let facts = vec!["completed test feature".to_string()];
        let updated = update_memory_from_facts(&paths, &facts, "Session").unwrap();
        assert!(updated);

        let content = fs::read_to_string(&memory_path).unwrap();
        assert!(
            content.contains("## Session Facts"),
            "should contain '## Session Facts', got: {content}"
        );
        assert!(
            !content.contains("## Dream Facts"),
            "should NOT contain '## Dream Facts'"
        );
    }

    #[test]
    fn test_different_sources_coexist() {
        let (_dir, paths) = setup_test_paths();
        let identity = paths.identity();
        fs::create_dir_all(&identity).unwrap();
        let memory_path = identity.join("MEMORY.md");
        fs::write(&memory_path, "# Memory\n").unwrap();

        let dream_facts = vec!["completed dream task".to_string()];
        let session_facts = vec!["completed session task".to_string()];

        update_memory_from_facts(&paths, &dream_facts, "Dream").unwrap();
        update_memory_from_facts(&paths, &session_facts, "Session").unwrap();

        let content = fs::read_to_string(&memory_path).unwrap();
        assert!(
            content.contains("## Dream Facts"),
            "should contain Dream Facts section"
        );
        assert!(
            content.contains("## Session Facts"),
            "should contain Session Facts section"
        );
        assert!(content.contains("completed dream task"));
        assert!(content.contains("completed session task"));
    }

    #[test]
    fn test_compress_small_file_noop() {
        let (_dir, paths) = setup_test_paths();
        let logs_dir = paths.logs();
        fs::create_dir_all(&logs_dir).unwrap();
        let log_path = logs_dir.join("subconscious.md");
        fs::write(&log_path, "small content\n").unwrap();

        let result = compress_old_logs(&paths).unwrap();
        assert!(!result, "small file should return false");

        let archive_dir = logs_dir.join("archive");
        assert!(!archive_dir.exists(), "archive dir should not be created");
    }

    #[test]
    fn test_compress_large_file_archives() {
        let (_dir, paths) = setup_test_paths();
        let logs_dir = paths.logs();
        fs::create_dir_all(&logs_dir).unwrap();
        let log_path = logs_dir.join("subconscious.md");

        let mut content = String::new();
        for i in 0..600 {
            content.push_str(&format!("line {i}: {:<160}\n", "x"));
        }
        fs::write(&log_path, &content).unwrap();

        let result = compress_old_logs(&paths).unwrap();
        assert!(result, "large file should return true");

        let remaining = fs::read_to_string(&log_path).unwrap();
        let remaining_lines: Vec<&str> = remaining.lines().collect();
        assert_eq!(remaining_lines.len(), 500, "should keep exactly 500 lines");

        let archive_dir = logs_dir.join("archive");
        assert!(archive_dir.exists(), "archive dir should exist");

        let entries: Vec<_> = fs::read_dir(&archive_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1, "should have exactly one archive file");

        let archive_content = fs::read_to_string(entries[0].path()).unwrap();
        let archive_lines: Vec<&str> = archive_content.lines().collect();
        assert_eq!(archive_lines.len(), 100, "archive should have 100 lines");
    }

    #[test]
    fn test_parse_facts_section() {
        let content = "---\nsession_id: test\ndate: 2026-06-24\n---\n\n# Session Journal\n\n## Facts\n\n- completed auth module\n- fixed login bug\n- deployed v2\n\n## Other\n\n- not a fact\n";
        let facts = parse_facts_section(content);
        assert_eq!(facts.len(), 3);
        assert!(facts.contains(&"completed auth module".to_string()));
        assert!(facts.contains(&"fixed login bug".to_string()));
        assert!(facts.contains(&"deployed v2".to_string()));
    }

    #[test]
    fn test_parse_facts_section_empty_or_placeholder() {
        let content1 = "## Facts\n\n_(no facts recorded)_\n";
        let facts1 = parse_facts_section(content1);
        assert!(facts1.is_empty(), "placeholder facts should be filtered");

        let content2 = "## Facts\n\n## Other\n\n- something\n";
        let facts2 = parse_facts_section(content2);
        assert!(facts2.is_empty(), "no facts section means no facts");
    }
}
