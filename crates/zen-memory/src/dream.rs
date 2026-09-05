use std::collections::HashSet;
use std::fs;
use std::sync::Arc;

use chrono::NaiveDate;
use tracing::{debug, info, warn};

use zen_core::notion_graph::{NotionGraphProvider, SimpleNotion};
use zen_core::paths::ZenPaths;

use crate::quality_gate::{MemoryGrade, grade_session_signal};

// ─── ExtractedSignals — Typed signals from session conversations ─────────

/// Signals extracted from a session conversation, grouped by type.
/// Used by SessionJournaler (extraction) and MemoryCurator (routing).
#[derive(Debug, Clone, Default)]
pub struct ExtractedSignals {
    /// Past-tense durable facts: what happened, decisions made, things learned.
    pub facts: Vec<String>,
    /// Self-assessments of what went wrong or could be better.
    pub reflections: Vec<String>,
    /// Forward-looking promises: what the user committed to do.
    pub commitments: Vec<String>,
    /// Structured decision records (text, context, expected_value).
    pub decisions: Vec<String>,
    /// Corrections to prior errors: error + correct answer + cost.
    pub corrections: Vec<String>,
    /// Feedback signals: target, content, sentiment.
    pub feedback: Vec<String>,
    /// Candidate beliefs with confidence scores.
    pub beliefs: Vec<String>,
    /// Positive actions that should be continued — inversion thinking counterpart to reflections.
    pub continue_doing_candidates: Vec<String>,
    /// Preference triples `subject|||predicate|||object|||confidence` (FR-021 Pi point 2).
    pub preferences: Vec<String>,
}

impl ExtractedSignals {
    pub fn is_empty(&self) -> bool {
        self.facts.is_empty()
            && self.reflections.is_empty()
            && self.commitments.is_empty()
            && self.decisions.is_empty()
            && self.corrections.is_empty()
            && self.feedback.is_empty()
            && self.beliefs.is_empty()
            && self.continue_doing_candidates.is_empty()
            && self.preferences.is_empty()
    }

    pub fn total(&self) -> usize {
        self.facts.len()
            + self.reflections.len()
            + self.commitments.len()
            + self.decisions.len()
            + self.corrections.len()
            + self.feedback.len()
            + self.beliefs.len()
            + self.continue_doing_candidates.len()
            + self.preferences.len()
    }
}

// ─── ZenDream — Knowledge Consolidation Pipeline ────────────────────────

/// ZenDream — Nightly consolidation state.
///
/// Runs during a configurable window (default 2AM–4AM) to:
/// 1. Extract durable facts from journal entries (written by SessionJournaler)
/// 2. Update MEMORY.md with all new durable facts (personal record)
/// 3. Compress old subconscious logs
/// 4. Recompute notion relationships from wiki
///
/// All operations are offline-first — no network/LLM calls required.
pub struct ZenDream {
    notion_graph: Option<Arc<dyn NotionGraphProvider>>,
}

impl ZenDream {
    pub fn new(notion_graph: Option<Arc<dyn NotionGraphProvider>>) -> Self {
        Self { notion_graph }
    }

    pub async fn run_cycle(
        &self,
        zen_paths: &ZenPaths,
        date: NaiveDate,
    ) -> Result<DreamReport, DreamError> {
        info!("dream cycle started for {date}");

        let extracted = extract_facts_from_journal_entries(zen_paths, date)?;

        if extracted.is_empty() {
            debug!("no durable facts extracted for {date}, skipping memory update");
            return Ok(DreamReport::empty(date));
        }

        // T073: M2→M3 promotion gate (contracts/memory-grade.json) — only
        // signals whose InformationQualityGate::can_promote_to_m3() passes
        // leave the M2 journal for MEMORY.md / the notion graph. Blocked
        // signals stay in the M2 journal (never deleted), they just don't
        // promote this cycle.
        let (facts, gate_blocked) = gate_m2_to_m3_facts(&extracted);
        if gate_blocked > 0 {
            info!(
                blocked = gate_blocked,
                promoted = facts.len(),
                "M2→M3 gate: low-quality signals stay in M2 journal"
            );
        }

        info!(
            fact_count = facts.len(),
            "extracted {} durable fact(s) for {date}",
            facts.len()
        );

        let memory_updated = update_memory_from_facts(zen_paths, &facts, "Dream")?;

        if memory_updated {
            info!("MEMORY.md updated with {} new fact(s)", facts.len());
        }

        // FR-040: 3-line wake-up briefing artifact (fail-soft, best-effort).
        let wake_lines: Vec<String> = facts.iter().take(3).cloned().collect();
        if !wake_lines.is_empty()
            && let Err(e) =
                write_wake_up_brief(zen_paths, &date.format("%Y-%m-%d").to_string(), &wake_lines)
        {
            warn!(error = %e, "wake-up brief write failed; dream cycle unaffected");
        }

        let logs_compressed = compress_old_logs(zen_paths)?;
        let entities_recomputed =
            recompute_entities(zen_paths, self.notion_graph.as_deref()).await?;

        let (entities_decayed, entities_promoted, top_entities) =
            run_graph_maintenance(zen_paths, self.notion_graph.as_deref())
                .await
                .unwrap_or_else(|e| {
                    warn!(error = %e, "run_graph_maintenance failed, dream cycle continues without graph maintenance");
                    (0, 0, Vec::new())
                });

        let report = DreamReport {
            date,
            facts_extracted: facts.len(),
            memory_updated,
            knowledge_updated: false,
            wiki_pages_created: 0,
            logs_compressed,
            entities_recomputed,
            entities_decayed,
            entities_promoted,
            top_entities,
            gate_blocked,
        };

        info!(
            "dream cycle complete: facts={}, memory_updated={}, logs_compressed={}, entities_recomputed={}, decayed={}, promoted={}, top={} {:?}",
            report.facts_extracted,
            report.memory_updated,
            report.logs_compressed,
            report.entities_recomputed,
            report.entities_decayed,
            report.entities_promoted,
            report.top_entities.len(),
            report.top_entities,
        );

        Ok(report)
    }

    /// Backfill the knowledge graph by replaying journal entries from `from` to `to`.
    ///
    /// Phase 3 Task 7: First-run historical scanning. Each day is processed
    /// via `run_cycle`; failures are logged and skipped, not fatal.
    pub async fn backfill(
        &self,
        zen_paths: &ZenPaths,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Vec<DreamReport> {
        let mut reports = Vec::new();
        let mut date = from;

        while date <= to {
            match self.run_cycle(zen_paths, date).await {
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
        Self::new(None)
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
    pub entities_decayed: usize,
    pub entities_promoted: usize,
    pub top_entities: Vec<String>,
    /// Facts blocked by the M2→M3 InformationQualityGate this cycle (T073).
    pub gate_blocked: usize,
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
            entities_decayed: 0,
            entities_promoted: 0,
            top_entities: Vec::new(),
            gate_blocked: 0,
        }
    }
}

// ─── Error type ────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum DreamError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

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
fn extract_facts_from_journal_entries(
    zen_paths: &ZenPaths,
    date: NaiveDate,
) -> Result<Vec<String>, DreamError> {
    let journal_dir = zen_paths.journal_entries();

    if !journal_dir.exists() {
        debug!(
            "journal entries dir does not exist: {}",
            journal_dir.display()
        );
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
pub fn parse_facts_section(content: &str) -> Vec<String> {
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

/// Extract durable facts from a conversation string via keyword matching.
///
/// Heuristic: lines containing past-tense action verbs (completed, fixed, added, etc.).
/// Used as a LLM-free fallback by SessionJournaler when no LLM provider is available.
pub fn extract_durable_facts_from_entry(content: &str) -> Vec<String> {
    const ACTION_KEYWORDS: &[&str] = &[
        "completed",
        "fixed",
        "added",
        "removed",
        "resolved",
        "implemented",
        "created",
        "shipped",
        "deployed",
        "updated",
        "design",
        "build",
        "migrate",
        "refactor",
        "optimize",
        "test",
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

// ─── T073: M2→M3 promotion gate ────────────────────────────────────────

/// Split journal facts into promotable vs gate-blocked (T073).
///
/// Each fact is graded via `grade_session_signal` (single shared heuristic
/// with the SessionJournaler pre-filter); only signals passing
/// `InformationQualityGate::can_promote_to_m3()` may leave M2. The journal
/// entry itself is never modified — blocking means "does not promote".
fn gate_m2_to_m3_facts(facts: &[String]) -> (Vec<String>, usize) {
    let mut promoted = Vec::with_capacity(facts.len());
    let mut blocked = 0usize;
    for fact in facts {
        let (grade, gate) = grade_session_signal(fact, false);
        if grade == MemoryGrade::Unverified || !gate.can_promote_to_m3() {
            debug!(fact = %fact, reasons = ?gate.fail_reasons(), "M2→M3 gate blocked fact");
            blocked += 1;
        } else {
            promoted.push(fact.clone());
        }
    }
    (promoted, blocked)
}

// ─── Step 2: All Facts → MEMORY.md — Tasks 2 & 3 ────────────────────────

/// Per DESIGN.md §3.1, MEMORY.md must contain these sections.
/// If any are missing, they are initialized as empty headers.
const MEMORY_SECTION_HEADERS: &[&str] = &[
    "## Identity",
    "## Active Commitments",
    "## Stop-Doing Ledger",
    "## Continue-Doing Ledger",
    "## Active Mental Models",
    "## Recent Wisdom",
];

/// Ensure all DESIGN.md §3.1 required section headers exist in MEMORY.md.
/// Inserts missing headers before `## Recent Wisdom` so facts land in the right place.
fn ensure_memory_sections(content: &str) -> String {
    let mut result = content.to_string();
    for header in MEMORY_SECTION_HEADERS {
        if !result.contains(header) {
            if let Some(pos) = result.find("## Recent Wisdom") {
                result.insert_str(pos, &format!("{header}\n\n"));
            } else {
                result.push_str(&format!("\n{header}\n\n"));
            }
        }
    }
    result
}

/// NEW Step 2 (refactored): Update MEMORY.md from the shared facts Vec.
///
/// Replaces the old update_memory() which read entries again internally.
/// Now accepts pre-extracted facts as a bridge — ensures ALL durable facts
/// are stored in MEMORY.md, while **notion-aware facts also go to knowledge graph**.
/// Personal-only facts (no known notion names) go ONLY to MEMORY.md (not graph).
pub fn update_memory_from_facts(
    zen_paths: &ZenPaths,
    facts: &[String],
    source: &str,
) -> Result<bool, DreamError> {
    if facts.is_empty() {
        debug!("no facts provided for MEMORY.md update");
        return Ok(false);
    }

    let memory_path = zen_paths.identity().join("MEMORY.md");

    if !memory_path.exists() {
        let default_content = MEMORY_SECTION_HEADERS.join("\n\n");
        if let Some(parent) = memory_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&memory_path, &default_content)?;
        debug!("initialized MEMORY.md with all DESIGN.md §3.1 section headers");
    }

    let content = ensure_memory_sections(&fs::read_to_string(&memory_path)?);

    // Deduplicate facts before writing
    let unique_facts: Vec<String> = dedupe_facts(facts);
    if unique_facts.is_empty() {
        debug!("all facts were duplicates, skipping MEMORY.md write");
        return Ok(false);
    }

    // Route by source type (DESIGN.md §15.3 item 3):
    // - "Commitment" → "## Active Commitments" section
    // - "Reflection" → skip (ReflectionWorker handles Stop-Doing/Continue-Doing ledgers)
    // - Everything else → "## Recent Wisdom" (default)
    if source == "Reflection" {
        debug!("skipping MEMORY.md write for Reflection source — ReflectionWorker handles ledgers");
        return Ok(false);
    }
    let section_header = if source == "Commitment" {
        "## Active Commitments"
    } else {
        "## Recent Wisdom"
    };
    let now = chrono::Utc::now().format("%Y-%m-%d").to_string();

    let new_entries: Vec<String> = unique_facts
        .iter()
        .map(|fact| format!("- **[{source}]** {fact} ({now})"))
        .collect();

    let mut filtered_entries = Vec::new();
    for entry in &new_entries {
        if let Some(text) = entry.split("]** ").nth(1) {
            let text_without_date = text.strip_suffix(&format!(" ({now})")).unwrap_or(text);
            if content.contains(text_without_date) {
                debug!("fact already present in MEMORY.md, skipping: {text_without_date}");
                continue;
            }
        }
        filtered_entries.push(entry.clone());
    }

    if filtered_entries.is_empty() {
        debug!("all facts already present in MEMORY.md, skipping write");
        return Ok(false);
    }

    let new_content = if let Some(start) = content.find(section_header) {
        let after_header = &content[start + section_header.len()..];
        let header_line_end = after_header.find('\n').map(|i| i + 1).unwrap_or(0);
        let insert_pos = start + section_header.len() + header_line_end;

        let rest = &content[insert_pos..];

        let mut result = String::new();
        result.push_str(&content[..insert_pos]);
        for entry in &filtered_entries {
            result.push_str(entry);
            result.push('\n');
        }
        result.push_str(rest);
        result
    } else {
        let mut result = content;
        if !result.ends_with('\n') {
            result.push('\n');
        }
        result.push_str(&format!("\n{section_header}\n\n"));
        for entry in &filtered_entries {
            result.push_str(entry);
            result.push('\n');
        }
        result
    };

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let (new_content, merged_away) = enforce_memory_cap(&new_content, &today);
    if merged_away > 0 {
        info!(
            merged = merged_away,
            cap = MEMORY_LINES_CAP,
            "MEMORY.md bounded: older entries merged away (see git history)"
        );
    }

    let tmp_path = memory_path.with_extension("md.tmp");
    fs::write(&tmp_path, &new_content)?;
    fs::rename(&tmp_path, &memory_path)?;

    info!(
        "MEMORY.md updated with {} new fact(s)",
        filtered_entries.len()
    );
    Ok(true)
}

/// Deduplicate fact strings while preserving order.
fn dedupe_facts(facts: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    facts
        .iter()
        .filter(|f| seen.insert(f.to_lowercase()))
        .cloned()
        .collect()
}

// ─── T081: Bounded memory + negative space + wake-up + nudge ────────────

/// Hard cap on MEMORY.md length (FR-040: 200 lines, then merge+supersede).
pub const MEMORY_LINES_CAP: usize = 200;

/// Enforce the MEMORY.md line cap by merging, never by silent truncation.
///
/// File order is newest-first within each section (entries insert at the
/// section top), so keeping the first body lines in file order is
/// newest-biased. All `#` headers survive; dropped entries are counted and
/// summarized in one rollup line (full text recoverable from git history).
/// Returns the capped content plus the number of merged-away entries.
pub fn enforce_memory_cap(content: &str, date: &str) -> (String, usize) {
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() <= MEMORY_LINES_CAP {
        return (content.to_string(), 0);
    }
    let header_count = lines
        .iter()
        .filter(|l| l.trim_start().starts_with('#'))
        .count();
    // One slot is reserved for the rollup line; total output stays ≤ CAP.
    let body_budget = MEMORY_LINES_CAP.saturating_sub(header_count + 1);
    let mut kept: Vec<&str> = Vec::with_capacity(MEMORY_LINES_CAP);
    let mut kept_body = 0usize;
    let mut dropped = 0usize;
    for line in &lines {
        if line.trim_start().starts_with('#') {
            kept.push(line);
        } else if line.trim().is_empty() {
            // Structural blanks ride along only while the cap allows.
            if kept.len() + 1 < MEMORY_LINES_CAP {
                kept.push(line);
            }
        } else if kept_body < body_budget {
            kept.push(line);
            kept_body += 1;
        } else {
            dropped += 1;
        }
    }
    // Defensive trim: headers are uncapped, so trim oldest body lines from
    // the end (file order is newest-first) until the rollup slot fits.
    while kept.len() + 1 > MEMORY_LINES_CAP {
        match kept
            .iter()
            .rposition(|l| !l.trim_start().starts_with('#') && !l.trim().is_empty())
        {
            Some(idx) => {
                kept.remove(idx);
                dropped += 1;
            }
            None => break,
        }
    }
    let rollup = memory_merge_rollup(dropped, date);
    let mut result = kept.join("\n");
    result.push('\n');
    result.push_str(&rollup);
    result.push('\n');
    (result, dropped)
}

/// Rollup line recording a bounded-memory merge (never silent loss).
pub fn memory_merge_rollup(dropped: usize, date: &str) -> String {
    format!(
        "- _Merged {dropped} older entries on {date} (bounded memory, cap {MEMORY_LINES_CAP} — full text in git history)_"
    )
}

/// A falsified hypothesis kept as negative space (FR-040): records WHY a
/// claim was rejected so the loop never re-proposes it.
#[derive(Debug, Clone)]
pub struct RejectedHypothesis {
    pub claim: String,
    pub falsifier: String,
    pub because: String,
    pub expiry: String,
}

impl RejectedHypothesis {
    fn slug(&self) -> String {
        self.claim
            .to_lowercase()
            .chars()
            .filter_map(|c| {
                if c.is_alphanumeric() {
                    Some(c)
                } else if c.is_whitespace() || c == '-' {
                    Some('-')
                } else {
                    None
                }
            })
            .collect::<String>()
            .split('-')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-")
            .chars()
            .take(48)
            .collect()
    }

    /// Persist under `wiki/wisdom/rejected/<slug>.md` (atomic tmp+rename).
    pub fn record(&self, zen_paths: &ZenPaths) -> Result<std::path::PathBuf, DreamError> {
        let dir = zen_paths.wiki().join("wisdom").join("rejected");
        fs::create_dir_all(&dir)?;
        let target = dir.join(format!("{}.md", self.slug()));
        let content = format!(
            "---\nclaim: {:?}\nfalsifier: {:?}\nbecause: {:?}\nexpiry: {:?}\n---\n\n# Rejected: {}\n\nFalsified by {}. {}\n",
            self.claim,
            self.falsifier,
            self.because,
            self.expiry,
            self.claim,
            self.falsifier,
            self.because
        );
        let tmp = target.with_extension("md.tmp");
        fs::write(&tmp, &content)?;
        fs::rename(&tmp, &target)?;
        Ok(target)
    }
}

/// Write the 3-line wake-up briefing artifact (`logs/wake-up-<date>.md`).
pub fn write_wake_up_brief(
    zen_paths: &ZenPaths,
    date: &str,
    lines: &[String],
) -> Result<std::path::PathBuf, DreamError> {
    let target = zen_paths.logs().join(format!("wake-up-{date}.md"));
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut content = format!("# Wake-up — {date}\n\n");
    for line in lines.iter().take(3) {
        content.push_str(&format!("- {line}\n"));
    }
    fs::write(&target, &content)?;
    Ok(target)
}

/// Whether a memory nudge is due after `user_turns` user turns (every 10).
pub fn memory_nudge_due(user_turns: u64) -> bool {
    user_turns > 0 && user_turns.is_multiple_of(10)
}

/// Nudge text surfaced via logs + `logs/memory-nudges.jsonl` (auditable).
/// The TUI/log viewer renders it; it never enters the model token stream.
pub fn memory_nudge_text() -> &'static str {
    "Memory nudge: 10 turns since the last check — run a quick review of open commitments?"
}

// ─── Post-Pipeline Helpers ────────────────────────────────────────────

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

async fn recompute_entities(
    zen_paths: &ZenPaths,
    notion_graph: Option<&dyn NotionGraphProvider>,
) -> Result<usize, DreamError> {
    let Some(graph) = notion_graph else {
        debug!("recompute_entities: no notion graph provider, skipping");
        return Ok(0);
    };

    let entities_dir = zen_paths.vault().join("wiki/notions");

    if !entities_dir.exists() {
        debug!("recompute_entities: wiki/notions/technology/ does not exist, nothing to recompute");
        return Ok(0);
    }

    let mut upserted = 0usize;

    let entries = match std::fs::read_dir(&entities_dir) {
        Ok(e) => e,
        Err(e) => {
            warn!(
                error = %e,
                path = %entities_dir.display(),
                "recompute_entities: failed to read notions directory"
            );
            return Ok(0);
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !(path.is_file() && path.extension().is_some_and(|ext| ext == "md")) {
            continue;
        }

        match parse_entity_file(&path) {
            Ok((name, kind, aliases)) => {
                let md5_input = format!("{}:{}", name, kind);
                let notion = SimpleNotion {
                    id: format!("wiki-{}", md5_hex(&md5_input)),
                    name,
                    kind,
                    source: "wiki-recompute".to_string(),
                };

                if let Err(e) = graph.upsert_entity(&notion).await {
                    warn!(
                        file = %path.display(),
                        error = %e,
                        "recompute_entities: failed to upsert notion"
                    );
                    continue;
                }

                for alias in &aliases {
                    if let Err(e) = graph.insert_alias(alias, &notion.id).await {
                        warn!(alias = %alias, error = %e, "recompute_entities: failed to insert alias");
                    }
                }

                upserted += 1;
                debug!(file = %path.display(), name = %notion.name, "recompute_entities: upserted");
            }
            Err(e) => {
                warn!(file = %path.display(), error = %e, "recompute_entities: skipping malformed notion file");
            }
        }
    }

    info!(upserted, "recompute_entities: complete");
    Ok(upserted)
}

async fn run_graph_maintenance(
    _zen_paths: &ZenPaths,
    notion_graph: Option<&dyn NotionGraphProvider>,
) -> std::result::Result<(usize, usize, Vec<String>), DreamError> {
    let Some(graph) = notion_graph else {
        return Ok((0, 0, Vec::new()));
    };

    let decayed = graph
        .apply_confidence_decay(30.0)
        .await
        .map_err(|e| DreamError::KnowledgeGraphPersist(e.to_string()))?;
    debug!(
        decayed,
        "run_graph_maintenance: applied confidence decay (30-day half-life)"
    );

    let promoted = graph
        .auto_promote_entities(3)
        .await
        .map_err(|e| DreamError::KnowledgeGraphPersist(e.to_string()))?;
    debug!(
        promoted,
        "run_graph_maintenance: auto-promoted notions (access_count >= 3)"
    );

    let top_entities = graph
        .compute_importance(40, 0.85)
        .await
        .map(|scores| scores.iter().take(5).map(|s| s.notion_id.clone()).collect())
        .unwrap_or_else(|e| {
            warn!(error = %e, "run_graph_maintenance: PageRank computation failed, returning empty");
            Vec::new()
        });
    debug!(
        top = top_entities.len(),
        "run_graph_maintenance: computed PageRank importance"
    );

    Ok((decayed, promoted, top_entities))
}

fn parse_entity_file(path: &std::path::Path) -> Result<(String, String, Vec<String>), DreamError> {
    let content = fs::read_to_string(path).map_err(DreamError::Io)?;

    let mut in_frontmatter = false;
    let mut name: Option<String> = None;
    let mut kind_str: Option<String> = None;
    let mut aliases_str: Option<String> = None;

    for line in content.lines().take(20) {
        let trimmed = line.trim();
        if trimmed == "---" {
            in_frontmatter = !in_frontmatter;
            continue;
        }
        if !in_frontmatter {
            break;
        }
        if let Some(val) = trimmed.strip_prefix("name:") {
            name = Some(val.trim().trim_matches('"').to_string());
        } else if let Some(val) = trimmed.strip_prefix("kind:") {
            kind_str = Some(val.trim().trim_matches('"').to_string());
        } else if let Some(val) = trimmed.strip_prefix("aliases:") {
            aliases_str = Some(val.trim().trim_matches('"').to_string());
        }
    }

    let name = name.ok_or_else(|| {
        DreamError::WikiCompile(format!("missing 'name' in frontmatter: {}", path.display()))
    })?;
    let kind = kind_str.ok_or_else(|| {
        DreamError::WikiCompile(format!("missing 'kind' in frontmatter: {}", path.display()))
    })?;

    let aliases: Vec<String> = aliases_str
        .unwrap_or_default()
        .split(',')
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty())
        .collect();

    Ok((name, kind, aliases))
}

fn md5_hex(input: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
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
        fs::write(&memory_path, "# Memory\n\n## Recent Wisdom\n\n").unwrap();

        let facts = vec!["completed test feature".to_string()];
        let updated = update_memory_from_facts(&paths, &facts, "Session").unwrap();
        assert!(updated);

        let content = fs::read_to_string(&memory_path).unwrap();
        assert!(
            content.contains("## Recent Wisdom"),
            "should contain '## Recent Wisdom', got: {content}"
        );
        assert!(
            content.contains("- **[Session]** completed test feature"),
            "should contain session-tagged fact, got: {content}"
        );
    }

    #[test]
    fn test_different_sources_coexist() {
        let (_dir, paths) = setup_test_paths();
        let identity = paths.identity();
        fs::create_dir_all(&identity).unwrap();
        let memory_path = identity.join("MEMORY.md");
        fs::write(&memory_path, "# Memory\n\n## Recent Wisdom\n\n").unwrap();

        let dream_facts = vec!["completed dream task".to_string()];
        let session_facts = vec!["completed session task".to_string()];

        update_memory_from_facts(&paths, &dream_facts, "Dream").unwrap();
        update_memory_from_facts(&paths, &session_facts, "Session").unwrap();

        let content = fs::read_to_string(&memory_path).unwrap();
        assert!(
            content.contains("## Recent Wisdom"),
            "should contain Recent Wisdom section"
        );
        assert!(
            content.contains("- **[Dream]** completed dream task"),
            "should contain Dream-tagged fact"
        );
        assert!(
            content.contains("- **[Session]** completed session task"),
            "should contain Session-tagged fact"
        );
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

    #[test]
    fn test_gate_m2_to_m3_blocks_vague_promotes_specific() {
        let facts = vec![
            "ok".to_string(),
            "migrated the session store from json blobs to sqlite wal".to_string(),
        ];
        let (promoted, blocked) = gate_m2_to_m3_facts(&facts);
        assert_eq!(blocked, 1);
        assert_eq!(promoted.len(), 1);
        assert_eq!(promoted[0], facts[1]);
    }

    #[test]
    fn test_gate_m2_to_m3_never_reorders() {
        let facts = vec![
            "implemented the retry backoff for provider failures".to_string(),
            "fixed the flaky websocket reconnect test".to_string(),
            "added tracing spans around gateway dispatch".to_string(),
        ];
        let (promoted, blocked) = gate_m2_to_m3_facts(&facts);
        assert_eq!(blocked, 0);
        assert_eq!(promoted, facts);
    }

    #[tokio::test]
    async fn test_recompute_entities_empty_dir() {
        let (_dir, paths) = setup_test_paths();
        let entities_dir = paths.vault().join("wiki/notions");
        fs::create_dir_all(&entities_dir).unwrap();

        let count = recompute_entities(&paths, None).await.unwrap();
        assert_eq!(count, 0, "no provider should return 0");
    }

    #[tokio::test]
    async fn test_recompute_entities_valid_entity() {
        let (_dir, paths) = setup_test_paths();
        let entities_dir = paths.vault().join("wiki/notions");
        fs::create_dir_all(&entities_dir).unwrap();

        let entity_content = "---\nname: Rust\nkind: Technology\naliases: rust-lang, rustlang\n---\n\n# Rust\n\nA systems programming language.\n";
        fs::write(entities_dir.join("rust.md"), entity_content).unwrap();

        let count = recompute_entities(&paths, None).await.unwrap();
        assert_eq!(count, 0, "no provider should return 0");
    }

    #[tokio::test]
    async fn test_recompute_entities_malformed_file_skipped() {
        let (_dir, paths) = setup_test_paths();
        let entities_dir = paths.vault().join("wiki/notions");
        fs::create_dir_all(&entities_dir).unwrap();

        fs::write(
            entities_dir.join("bad.md"),
            "---\ntitle: no kind\n---\n\nBody\n",
        )
        .unwrap();

        let count = recompute_entities(&paths, None).await.unwrap();
        assert_eq!(count, 0, "malformed file should be skipped, returning 0");
    }

    // ── T081/T082: bounded memory (200-line cap → merge, never truncate) ──

    #[test]
    fn test_memory_cap_short_file_untouched() {
        let content = "# Memory\n\n## Recent Wisdom\n\n- a\n";
        let (capped, dropped) = enforce_memory_cap(content, "2026-09-03");
        assert_eq!(dropped, 0);
        assert_eq!(capped, content);
    }

    #[test]
    fn test_memory_cap_merges_newest_biased_with_rollup() {
        let mut content = String::from("# Memory\n\n## Recent Wisdom\n\n");
        for i in 0..300 {
            content.push_str(&format!("- fact {i}\n"));
        }
        let (capped, dropped) = enforce_memory_cap(&content, "2026-09-03");
        assert!(dropped > 0, "over-cap file must merge entries away");
        assert!(
            capped.lines().count() <= MEMORY_LINES_CAP,
            "total stays within cap (rollup included), got {}",
            capped.lines().count()
        );
        assert!(capped.contains("## Recent Wisdom"), "headers survive");
        assert!(capped.contains("fact 0"), "newest entries survive");
        assert!(!capped.contains("fact 299"), "oldest entries merged away");
        assert!(
            capped.contains(&memory_merge_rollup(dropped, "2026-09-03")),
            "merge recorded, never silent"
        );
    }

    #[test]
    fn test_rejected_hypothesis_keeps_falsifier() {
        let (_dir, paths) = setup_test_paths();
        let r = RejectedHypothesis {
            claim: "All meetings need agendas".to_string(),
            falsifier: "2026-08 standup log".to_string(),
            because: "3 standups without agendas still shipped".to_string(),
            expiry: "2026-12-01".to_string(),
        };
        let path = r.record(&paths).unwrap();
        assert!(path.is_file());
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("2026-08 standup log"), "falsifier kept");
        assert!(content.contains("expiry"), "expiry kept");
    }

    #[test]
    fn test_wake_up_brief_is_three_lines() {
        let (_dir, paths) = setup_test_paths();
        let path = write_wake_up_brief(
            &paths,
            "2026-09-03",
            &[
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string(),
            ],
        )
        .unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("# Wake-up — 2026-09-03"));
        assert!(content.contains("- c"));
        assert!(!content.contains("- d"), "capped at 3 lines");
    }

    #[test]
    fn test_memory_nudge_every_ten_turns() {
        assert!(!memory_nudge_due(0));
        assert!(!memory_nudge_due(9));
        assert!(memory_nudge_due(10));
        assert!(memory_nudge_due(20));
        assert!(!memory_nudge_due(21));
        assert!(!memory_nudge_text().is_empty());
    }
}
