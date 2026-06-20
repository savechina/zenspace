use crate::journal::Journal;
use std::collections::{HashMap, HashSet};
use std::fs;

use chrono::NaiveDate;
use tracing::{debug, error, info, warn};

use zen_core::paths::ZenPaths;
use zen_vault::{
    Entity, EntityData, EntityService, EntityType, RelationType, Relationship, WikiCompiler,
};

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

        let facts = extract_durable_facts(zen_paths, date)?;
        
        if facts.is_empty() {
            debug!("no durable facts extracted for {date}, skipping memory and knowledge updates");
            return Ok(DreamReport::empty(date));
        }

        info!(fact_count = facts.len(), "extracted {} durable fact(s) for {date}", facts.len());

        let memory_updated = update_memory_from_facts(zen_paths, &facts)?;

        if memory_updated {
            info!("MEMORY.md updated with {} new fact(s)", facts.len());
        }

        let (knowledge_updated, wiki_pages_created) = update_knowledge(zen_paths, &facts);
        
        if knowledge_updated {
            info!(wiki_pages = wiki_pages_created, "Knowledge graph updated for {date}");
        }

        let report = DreamReport {
            date,
            facts_extracted: facts.len(),
            memory_updated,
            logs_compressed: compress_old_logs(zen_paths)?,
            entities_recomputed: recompute_entities(zen_paths)?,
            knowledge_updated,
            wiki_pages_created,
        };

        info!(
            "dream cycle complete: facts={}, memory_updated={}, knowledge_updated={}, wiki_pages={}, logs_compressed={}, entities_recomputed={}",
            report.facts_extracted,
            report.memory_updated,
            report.knowledge_updated,
            report.wiki_pages_created,
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

/// Step 1 (refactored): Consolidate daily log → durable facts.
///
/// Reads the daily log for `date`, extracts structured facts from entries,
/// returns the full Vec<String> as a **shared bridge** — consumed by both
/// MEMORY.md and knowledge-graph update_knowledge().
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
pub fn update_memory_from_facts(zen_paths: &ZenPaths, facts: &[String]) -> Result<bool, DreamError> {
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
    let section_marker = format!("## Dream Facts — {now}");

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

    fs::write(&memory_path, format!("{content}{update}"))?;

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
    "Rust", "Python", "TypeScript", "JavaScript",
    "CLIP", "LLM", "GPT", "OpenAI",
    "Anthropic", "DeepSeek", "Gemini",
    "Tokio", "async", "sqlite", "SQLite",
    "FTS5", "vector", "embedding",
];

/// Promote entity-aware facts to graph.db + generate wiki pages via WikiCompiler.
/// Personal-only facts (no known entity match) are skipped — they go to MEMORY.md only.
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
        let entity = Entity::new(name.clone(), EntityType::Technology, "dream-cycle");
        if let Err(e) = svc.upsert_entity(&graph_db, &entity) {
            error!(entity = %name, error = %e, "failed to upsert entity to graph.db");
            continue;
        }
        entity_ids.insert(name.clone(), entity.id.clone());
        debug!(entity = %name, fact_count = fact_list.len(), "upserted entity to graph.db");

        let entity_text = format!("{name}: {}", fact_list.join(" "));
        if let Err(e) = svc.store_entity_embedding(&vec_db, &entity.id, &entity_text) {
            debug!(entity = %name, error = %e, "failed to store entity embedding (non-fatal)");
        }

        entity_index.insert(name.clone(), entity_data_list.len());
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
/// TODO(phase-1): implement actual compression (archive logs older than
/// `retention_days`, move to `subconscious/archive/`). Returns `false`
/// (no-op) until then.
fn compress_old_logs(_zen_paths: &ZenPaths) -> Result<bool, DreamError> {
    debug!("compress_old_logs: stubbed, no compression performed");
    Ok(false)
}

/// Recompute entity relationships by scanning the wiki directory.
///
/// TODO(phase-1): implement graph rebuild from `wiki/entities/*.md`
/// frontmatter + cross-references. Returns `0` (no-op) until then.
fn recompute_entities(_zen_paths: &ZenPaths) -> Result<usize, DreamError> {
    debug!("recompute_entities: stubbed, no entities recomputed");
    Ok(0)
}
