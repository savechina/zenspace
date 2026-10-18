use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use zen_core::atomic_file::write_atomic;
use zen_core::config::MemoryStrengthConfig;
use zen_core::constants::MEMVID_STORE_FILE;
use zen_core::jsonl::append_jsonl_line;
use zen_core::paths::ZenPaths;
use zen_memory::memvid_index::extract_anchor_date;
use zen_memory::strength::retention_strength;
use zen_memory::{MemvidIndexer, ZenMemvidStore};
use zen_vault::distill::{card_id_from_path, read_reward};

use super::super::{WorkerContext, WorkerReport, ZenWorker};

/// Report file name under `logs/` (Phase 31 T210).
pub const MEMORY_STRENGTH_REPORT_FILE: &str = "memory-strength-report.json";

/// `logs/memory-strength-report.json` — the nightly lowest-strength report.
pub fn memory_strength_report_path(logs_dir: &Path) -> PathBuf {
    logs_dir.join(MEMORY_STRENGTH_REPORT_FILE)
}

/// One item of the nightly lowest-strength report (design F1: report-only
/// eviction CANDIDATE list — nothing ever deletes on this signal, the
/// retention quarantine precedent).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStrengthItem {
    /// Sidecar card id, or the memory-root-relative journal path.
    pub key: String,
    /// M-tier when the item came from a journal file (`"m2"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// Retention strength R ∈ (0,1] at `generated_at`.
    pub strength: f64,
    /// Reinforcement events (access_count + downstream_citations).
    pub events: u32,
    /// Content anchor (RFC3339).
    pub anchor: String,
    /// Last reinforcement (RFC3339), when a sidecar recorded one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event: Option<String>,
}

/// The nightly lowest-strength report (bottom-N by R, ascending).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStrengthReport {
    pub generated_at: String,
    pub half_life_days: f64,
    pub count: usize,
    pub items: Vec<MemoryStrengthItem>,
}

/// Fail-open reader for `zen discover report` (T210): missing ⇒ `None`
/// (section shows "not recorded"); corrupt ⇒ warn + `None` (the report is
/// advisory, never load-bearing — the replay-section precedent).
pub fn read_memory_strength_report(logs_dir: &Path) -> Option<MemoryStrengthReport> {
    let path = memory_strength_report_path(logs_dir);
    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return None,
    };
    match serde_json::from_str::<MemoryStrengthReport>(&content) {
        Ok(report) => Some(report),
        Err(e) => {
            warn!(
                path = %path.display(),
                error = %e,
                "corrupt memory-strength report, treating as not recorded"
            );
            None
        }
    }
}

/// u64 sidecar counters → the u32 `events` input of `retention_strength`.
fn to_events(access_count: u64, downstream_citations: u64) -> u32 {
    u32::try_from(access_count.saturating_add(downstream_citations)).unwrap_or(u32::MAX)
}

fn file_mtime_utc(path: &Path) -> Option<DateTime<Utc>> {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .map(DateTime::<Utc>::from)
}

/// Build the strength items, FILE-BASED per design F1 (reward sidecars +
/// journal anchors — NO memvid store scan; frames inherit strength at search
/// time via their uri/source_path joins). Unsorted; the writer sorts and
/// truncates.
///
/// Journal files are enumerated FIRST and consume their sidecar (joined by
/// `card_id_from_path("journal/<file>.md")` — the same derivation the T209
/// consumers and T211 use), so one file never appears twice. A standalone
/// sidecar (key space that cannot be reversed to a path — the `_` flattening
/// in `card_id_from_path` is lossy) anchors on `last_reward_at`, else its
/// file mtime; an item with neither anchor is skipped (honest omission over a
/// fabricated age).
pub fn build_strength_items(
    memory_root: &Path,
    reward_dir: &Path,
    half_life_days: f64,
    now: DateTime<Utc>,
) -> Vec<MemoryStrengthItem> {
    let mut items = Vec::new();
    let mut consumed_sidecars = std::collections::HashSet::new();

    let journal_dir = memory_root.join("journal");
    if let Ok(entries) = fs::read_dir(&journal_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let rel = format!("journal/{file_name}");
            let stem = file_name.trim_end_matches(".md");
            let anchor = extract_anchor_date(&format!("journal-{stem}"))
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map(|dt| dt.and_utc())
                .or_else(|| file_mtime_utc(&path));
            let Some(anchor) = anchor else { continue };

            let card_id = card_id_from_path(&rel);
            consumed_sidecars.insert(card_id.clone());
            let reward = read_reward(reward_dir, &card_id);
            items.push(MemoryStrengthItem {
                key: rel,
                tier: Some("m2".to_string()),
                strength: retention_strength(
                    anchor,
                    reward.last_reward_at,
                    to_events(reward.access_count, reward.downstream_citations),
                    now,
                    half_life_days,
                ),
                events: to_events(reward.access_count, reward.downstream_citations),
                anchor: anchor.to_rfc3339(),
                last_event: reward.last_reward_at.map(|t| t.to_rfc3339()),
            });
        }
    }

    if let Ok(entries) = fs::read_dir(reward_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(card_id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if consumed_sidecars.contains(card_id) {
                continue;
            }
            // read_reward is fail-open on corrupt files (warn + defaults).
            let reward = read_reward(reward_dir, card_id);
            let anchor = reward.last_reward_at.or_else(|| file_mtime_utc(&path));
            let Some(anchor) = anchor else { continue };
            items.push(MemoryStrengthItem {
                key: card_id.to_string(),
                tier: None,
                strength: retention_strength(
                    anchor,
                    reward.last_reward_at,
                    to_events(reward.access_count, reward.downstream_citations),
                    now,
                    half_life_days,
                ),
                events: to_events(reward.access_count, reward.downstream_citations),
                anchor: anchor.to_rfc3339(),
                last_event: reward.last_reward_at.map(|t| t.to_rfc3339()),
            });
        }
    }

    items
}

/// Compute, sort (ascending R, ties by key), truncate to
/// `eviction_report_size`, atomically write the report, and emit ONE
/// `loop.memory.strength` audit line when it is non-empty (flood discipline).
/// REPORT-ONLY: no deletion or eviction action exists anywhere on this path.
pub fn write_strength_report(
    paths: &ZenPaths,
    cfg: &MemoryStrengthConfig,
    now: DateTime<Utc>,
) -> Result<MemoryStrengthReport> {
    let half_life_days = cfg.half_life_days_or_default();
    let memory_root = paths.memory();
    let reward_dir = memory_root.join(".reward");

    let mut items = build_strength_items(&memory_root, &reward_dir, half_life_days, now);
    items.sort_by(|a, b| {
        a.strength
            .total_cmp(&b.strength)
            .then_with(|| a.key.cmp(&b.key))
    });
    items.truncate(cfg.eviction_report_size_or_default());

    let report = MemoryStrengthReport {
        generated_at: now.to_rfc3339(),
        half_life_days,
        count: items.len(),
        items,
    };

    let logs = paths.logs();
    fs::create_dir_all(&logs).with_context(|| format!("create logs dir: {}", logs.display()))?;
    let body = serde_json::to_vec_pretty(&report).context("serialize memory-strength report")?;
    write_atomic(&memory_strength_report_path(&logs), &body)
        .context("write memory-strength report")?;

    if report.count > 0 {
        let audit = serde_json::json!({
            "kind": "loop.memory.strength",
            "ts": now.to_rfc3339(),
            "count": report.count,
            "lowest": report.items[0].strength,
            "half_life_days": report.half_life_days,
        });
        let audit_path = logs.join("audit.jsonl");
        if let Err(e) = append_jsonl_line(&audit_path, &audit) {
            warn!(
                error = %e, path = %audit_path.display(),
                "memory-strength audit append failed (report file kept)"
            );
        }
    }
    Ok(report)
}

pub struct MemvidIndexerWorker {
    scheduled: Option<&'static str>,
}

impl Default for MemvidIndexerWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl MemvidIndexerWorker {
    pub fn new() -> Self {
        Self { scheduled: None }
    }

    pub fn with_schedule(mut self, expr: &str) -> Self {
        self.scheduled = Some(Box::leak(expr.to_string().into_boxed_str()));
        self
    }
}

#[async_trait::async_trait]
impl ZenWorker for MemvidIndexerWorker {
    fn id(&self) -> &'static str {
        "memvid-indexer"
    }

    fn description(&self) -> &'static str {
        "Nightly knowledge indexer: ingest journal, wiki, wisdom into memvid store"
    }

    fn schedule(&self) -> &'static str {
        self.scheduled.unwrap_or("0 0 1 * * *")
    }

    async fn execute(&self, ctx: &WorkerContext) -> Result<WorkerReport> {
        let start = std::time::Instant::now();
        let paths = ZenPaths::detect()?;

        // T210: the memory-strength subsystem rides this worker's config.
        // Fail-open: a config error keeps indexing alive with the subsystem
        // OFF (defaults gate closed) — the worker must not die for an
        // advisory report.
        let strength_cfg = match zen_core::config::load_config() {
            Ok(config) => config.agentic.memory_strength.clone(),
            Err(e) => {
                warn!(
                    error = %e,
                    "memvid-indexer: config load failed — memory-strength subsystem stays off"
                );
                MemoryStrengthConfig::default()
            }
        };

        let store_path = paths.memory().join(MEMVID_STORE_FILE);
        fs::create_dir_all(store_path.parent().unwrap_or(&store_path)).with_context(|| {
            format!(
                "failed to create memvid store dir: {}",
                store_path.display()
            )
        })?;

        let mut store =
            ZenMemvidStore::new(store_path).with_context(|| "failed to open memvid store")?;

        // Path Spec v2 (T195/W1): roots come from ZenPaths — no workspace_root
        // dependency, and no silent skip when it is absent (a launchd daemon
        // runs with cwd=/ and never resolves one).
        // Provenance metadata (T208/T210): frames carry {source_path, tier}
        // only when the strength subsystem is enabled — the wave-1 indexer
        // default stays OFF and this worker is the sole production flipper.
        // Incremental, not full (T219 / review R1): the store is append-only
        // with memvid-core dedup deliberately OFF (dedup(true) would block the
        // T210/T211 provenance backfill on re-index), so a nightly `index_all`
        // re-appended every M2/M3/M4 frame plus a fresh MemoryCard per chunk.
        // `index_incremental` skips files whose mtime matches the checksum
        // sidecar (`memories/.index-checksums.json`) and falls back to the
        // full index when the sidecar is missing or unreadable. Report numbers
        // are therefore per-run ADDITIONS — a quiet night honestly reports 0.
        let indexer = MemvidIndexer::new(paths.memory(), paths.wiki())
            .with_provenance_metadata(strength_cfg.enabled_or_default());
        let report = indexer
            .index_incremental(&mut store)
            .with_context(|| "memvid indexing failed")?;

        info!(
            files = report.files_scanned,
            chunks = report.chunks_indexed,
            errors = report.errors.len(),
            "memvid indexing complete"
        );

        // T210 nightly strength report (report-only, fail-open: a failure
        // here never fails the worker once indexing succeeded).
        if strength_cfg.enabled_or_default() {
            match write_strength_report(&paths, &strength_cfg, ctx.now) {
                Ok(strength_report) => info!(
                    count = strength_report.count,
                    "memory-strength report written"
                ),
                Err(e) => warn!(
                    error = %e,
                    "memory-strength report failed (indexing result kept)"
                ),
            }
        }

        Ok(WorkerReport {
            worker_id: self.id().to_string(),
            success: true,
            fact_count: report.chunks_indexed,
            duration_ms: start.elapsed().as_millis() as u64,
            llm_cost_usd: 0.0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_worker_id() {
        let worker = MemvidIndexerWorker::new();
        assert_eq!(worker.id(), "memvid-indexer");
    }

    #[test]
    fn test_worker_schedule() {
        let worker = MemvidIndexerWorker::new();
        assert_eq!(worker.schedule(), "0 0 1 * * *");
    }

    #[test]
    fn test_worker_description() {
        let worker = MemvidIndexerWorker::new();
        assert!(worker.description().contains("indexer"));
    }

    #[test]
    fn test_with_schedule() {
        let worker = MemvidIndexerWorker::new().with_schedule("0 30 2 * * *");
        assert_eq!(worker.schedule(), "0 30 2 * * *");
    }

    fn reward(
        access: u64,
        citations: u64,
        last: Option<DateTime<Utc>>,
    ) -> zen_vault::distill::MemoryReward {
        zen_vault::distill::MemoryReward {
            access_count: access,
            downstream_citations: citations,
            correction_count: 0,
            last_reward_at: last,
        }
    }

    #[test]
    fn journal_items_anchor_on_filename_date_and_join_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        let memory_root = dir.path().join("memories");
        let journal = memory_root.join("journal");
        let reward_dir = memory_root.join(".reward");
        fs::create_dir_all(&journal).unwrap();
        fs::create_dir_all(&reward_dir).unwrap();
        fs::write(journal.join("2020-01-01.md"), "# old\n").unwrap();
        // Same key derivation the T209 consumers use (memory-root-relative).
        zen_vault::distill::write_reward(
            &reward_dir,
            &card_id_from_path("journal/2020-01-01.md"),
            &reward(7, 0, None),
        )
        .unwrap();

        let now = Utc::now();
        let items = build_strength_items(&memory_root, &reward_dir, 30.0, now);
        assert_eq!(items.len(), 1, "sidecar consumed by its journal file");
        let item = &items[0];
        assert_eq!(item.key, "journal/2020-01-01.md");
        assert_eq!(item.tier.as_deref(), Some("m2"));
        assert_eq!(item.events, 7);
        // 7 events ⇒ stability 4×HL; a ~6-year-old anchor still decays hard.
        assert!(item.strength < 0.1, "got {}", item.strength);
        let reinforced = item.strength;

        // Without the sidecar the same anchor decays FASTER (events=0).
        fs::remove_dir_all(&reward_dir).unwrap();
        let bare = build_strength_items(&memory_root, &reward_dir, 30.0, now);
        assert_eq!(bare.len(), 1);
        assert_eq!(bare[0].events, 0);
        assert!(bare[0].strength < reinforced);
    }

    #[test]
    fn standalone_sidecars_anchor_on_last_reward_or_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let memory_root = dir.path().join("memories");
        let reward_dir = memory_root.join(".reward");
        fs::create_dir_all(&reward_dir).unwrap();
        let old = now_minus_days(120);
        zen_vault::distill::write_reward(
            &reward_dir,
            "vault_wiki_old_note",
            &reward(0, 0, Some(old)),
        )
        .unwrap();

        let now = Utc::now();
        let items = build_strength_items(&memory_root, &reward_dir, 30.0, now);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].key, "vault_wiki_old_note");
        assert!(items[0].tier.is_none());
        // 120 days = 4 half-lives ⇒ ≈ 0.0625.
        assert!(
            (items[0].strength - 0.0625).abs() < 0.01,
            "got {}",
            items[0].strength
        );
        assert_eq!(
            items[0].last_event.as_deref(),
            Some(old.to_rfc3339().as_str())
        );
    }

    #[test]
    fn corrupt_sidecar_fails_open_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let memory_root = dir.path().join("memories");
        let reward_dir = memory_root.join(".reward");
        fs::create_dir_all(&reward_dir).unwrap();
        fs::write(reward_dir.join("broken.json"), "{ not json").unwrap();

        let items = build_strength_items(&memory_root, &reward_dir, 30.0, Utc::now());
        // read_reward defaults (no last_reward_at) ⇒ mtime anchor ⇒ fresh ⇒ ≈1.
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].events, 0);
        assert!(items[0].strength > 0.99, "got {}", items[0].strength);
    }

    #[test]
    fn report_is_sorted_ascending_truncated_and_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let paths = ZenPaths::for_testing(root.clone());
        let journal = paths.memory().join("journal");
        fs::create_dir_all(&journal).unwrap();
        for day in 1..=5 {
            fs::write(
                journal.join(format!("2020-01-0{day}.md")),
                format!("# d{day}\n"),
            )
            .unwrap();
        }
        let cfg = MemoryStrengthConfig {
            enabled: Some(true),
            eviction_report_size: Some(2),
            ..MemoryStrengthConfig::default()
        };
        let now = Utc::now();
        let report = write_strength_report(&paths, &cfg, now).unwrap();

        assert_eq!(report.count, 2, "truncated to eviction_report_size");
        assert_eq!(report.items.len(), 2);
        assert!(report.items[0].strength <= report.items[1].strength);
        assert_eq!(report.half_life_days, 30.0);

        // Reader roundtrip + audit flood discipline (count > 0 ⇒ one line).
        let loaded = read_memory_strength_report(&paths.logs()).unwrap();
        assert_eq!(loaded.count, 2);
        assert_eq!(loaded.items[0].key, report.items[0].key);
        let audit = fs::read_to_string(paths.logs().join("audit.jsonl")).unwrap();
        let lines: Vec<&str> = audit.lines().collect();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("loop.memory.strength"));
        assert!(lines[0].contains("\"lowest\""));
    }

    #[test]
    fn empty_inputs_write_report_but_no_audit_line() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ZenPaths::for_testing(dir.path().to_path_buf());
        let cfg = MemoryStrengthConfig {
            enabled: Some(true),
            ..MemoryStrengthConfig::default()
        };
        let report = write_strength_report(&paths, &cfg, Utc::now()).unwrap();
        assert_eq!(report.count, 0);
        assert!(memory_strength_report_path(&paths.logs()).exists());
        assert!(
            !paths.logs().join("audit.jsonl").exists(),
            "count=0 must not emit the audit line (flood discipline)"
        );
    }

    #[test]
    fn reader_missing_and_corrupt_are_none() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().to_path_buf();
        assert!(read_memory_strength_report(&logs).is_none());
        fs::create_dir_all(&logs).unwrap();
        fs::write(memory_strength_report_path(&logs), "{ broken").unwrap();
        assert!(read_memory_strength_report(&logs).is_none());
    }

    fn now_minus_days(days: i64) -> DateTime<Utc> {
        Utc::now() - chrono::TimeDelta::try_days(days).unwrap()
    }
}
