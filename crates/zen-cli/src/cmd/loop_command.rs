//! `zen wiki loop` — knowledge-processing loop CLI (005-agentic-loop).
//!
//! Scope logic (Constitution XV):
//! - Functionality: manual trigger + observability surface for ZenLoopWorker.
//! - User impact: `run` executes one full cycle regardless of enabled state;
//!   enable/disable only gates the cron registration (next tick).
//! - Default: enabled=true from config; run is always allowed.
//! - Interaction: same worker code path as the scheduler tick (contracts/cli.md).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};

use clap::Subcommand;
use zen_agents::scheduler::{ZenLoopWorker, ZenScheduler, gaps_path, last_report_path};
use zen_core::errors::ZenError;
use zen_core::jsonl::read_jsonl_lines;
use zen_core::paths::ZenPaths;
use zen_vault::distill::{CycleOutcome, LoopCycleReport};

/// Subcommands for `zen wiki loop` — manual trigger and observability
/// surface for the ZenLoopWorker knowledge-processing loop.
///
/// Scope logic (Constitution XV):
/// - Functionality: manual trigger + observability surface for ZenLoopWorker.
/// - User impact: `run` executes one full cycle regardless of enabled state;
///   enable/disable only gates the cron registration (next tick).
/// - Default: enabled=true from config; run is always allowed.
/// - Interaction: same worker code path as the scheduler tick (contracts/cli.md).
#[derive(Subcommand)]
pub enum LoopCommands {
    /// Execute one full processing cycle immediately.
    ///
    /// Runs the same code path as the cron tick: ingest sweep, distill,
    /// wisdom hooks, graph verify, reindex, report + audit. The cycle
    /// executes even when the loop is disabled via `disable`.
    Run {
        /// Compute the cycle but perform no mutations (report only).
        ///
        /// Scope logic:
        /// - Functionality: read-only cycle — distill and reindex stages
        ///   are skipped; ingest sweep and gap detection still run.
        /// - User impact: no files are moved, created, or archived; the
        ///   report shows what *would* happen.
        /// - Default: false (full mutations).
        #[arg(long)]
        dry_run: bool,

        /// Machine-readable LoopCycleReport.
        ///
        /// Scope logic:
        /// - Functionality: outputs the full `LoopCycleReport` as JSON
        ///   instead of the human-readable summary.
        /// - User impact: suitable for scripting, piping, or dashboard
        ///   ingestion; fields include cycle_id, outcome, notes_processed,
        ///   entities_persisted, pages_created, archived_count, gaps.
        /// - Default: false (human-readable output).
        /// - Interaction: --json can be combined with --dry-run.
        #[arg(long)]
        json: bool,
    },

    /// Show loop state: last cycle report, schedule, open gap counts.
    ///
    /// Scope logic:
    /// - Functionality: reads `loop-last-report.json` and counts gaps
    ///   by kind from `loop-gaps.jsonl`; does not trigger a cycle. The
    ///   wall time of that read path is measured (SC-005) and reported
    ///   as `status_read_ms`.
    /// - User impact: shows enabled state, cron schedule, last cycle
    ///   outcome, and open gap counts per kind.
    /// - Default: human-readable table output with a subtle
    ///   `(read in Nms)` header suffix.
    /// - Interaction: --json outputs a single JSON object with
    ///   `last_cycle`, `schedule`, `enabled`, `open_gaps`, and
    ///   `status_read_ms` fields.
    Status {
        /// Machine-readable status object.
        ///
        /// Scope logic:
        /// - Functionality: outputs a JSON object with last_cycle
        ///   (LoopCycleReport or null), schedule (cron string), enabled
        ///   (bool), open_gaps (map of kind to count), and status_read_ms
        ///   (wall time of the status read path in milliseconds, rounded
        ///   up, always ≥ 1 — a measured quantity, never a claim).
        /// - User impact: suitable for scripting or monitoring dashboards.
        /// - Default: false (human-readable output).
        #[arg(long)]
        json: bool,
    },

    /// List detected gap records (most recent first).
    ///
    /// Scope logic:
    /// - Functionality: reads `loop-gaps.jsonl` and displays records in
    ///   reverse-chronological order; optional kind filter narrows results.
    ///   Also surfaces pending user questions from the Discovery Loop's
    ///   `refinement-queue.json` (T187) — count + list appended after the
    ///   gap records in human output, `user_questions` key in JSON output.
    /// - User impact: shows gap kind, detail, and subject path for each
    ///   record; useful for diagnosing stale inbox files, quarantined
    ///   notes, decision blocks, or belief lifecycle events. Pending user
    ///   questions are hypotheses awaiting human judgment.
    /// - Default: all gap kinds, human-readable format.
    Gaps {
        /// Filter by gap kind (e.g. orphan_entity).
        ///
        /// Scope logic:
        /// - Functionality: retains only records whose `kind` field
        ///   matches the given value (exact string match).
        /// - User impact: narrows output to a specific concern, e.g.
        ///   `--kind decision_blocked` shows only quarantined decisions.
        /// - Default: no filter (all kinds shown).
        /// - Values: decision_blocked, commitment_overdue,
        ///   self_cognition_blocked, anti_talk_suspect, quarantined_note,
        ///   wiki_page_without_entities, orphan_entity,
        ///   duplicate_entity_alias.
        #[arg(long)]
        kind: Option<String>,

        /// Machine-readable output.
        ///
        /// Scope logic:
        /// - Functionality: outputs a JSON object
        ///   `{"gaps": [<filtered GapRecord>...], "user_questions": [<string>...]}`
        ///   instead of the human-readable `[kind] detail (path)` format
        ///   (T187: the top level changed from a bare array to this object).
        /// - User impact: suitable for scripting or piping to jq
        ///   (`jq '.gaps'` for the legacy record array).
        /// - Default: false (human-readable output).
        #[arg(long)]
        json: bool,
    },

    /// Enable the loop worker (fires on the configured cron).
    ///
    /// Scope logic:
    /// - Functionality: writes `enabled = true` under `[agentic.loop]`
    ///   in the workspace config file via text-level upsert.
    /// - User impact: the scheduler registers the worker on the next
    ///   tick; manual `run` is unaffected (always works).
    /// - Default: enabled from config.
    /// - Interaction: takes effect next tick, not immediately; `run`
    ///   works regardless of this setting.
    Enable,

    /// Disable the loop worker (manual `run` still works).
    ///
    /// Scope logic:
    /// - Functionality: writes `enabled = false` under `[agentic.loop]`
    ///   in the workspace config file via text-level upsert.
    /// - User impact: the scheduler deregisters the worker on the next
    ///   tick; manual `run` is unaffected (always works).
    /// - Default: enabled from config.
    /// - Interaction: takes effect next tick, not immediately; `run`
    ///   works regardless of this setting.
    Disable,
}

/// Dispatch a `LoopCommands` variant to the appropriate handler.
///
/// # Parameters
/// - `operation` — the parsed CLI subcommand (run, status, gaps, enable, disable).
///
/// # Returns
/// `Ok(())` on success; `Err(ZenError)` on cycle failure or I/O errors.
///
/// # Errors
/// `run` propagates cycle failures from `LoopCycleReport::last_error`.
/// `status` and `gaps` fail on path detection or file read errors.
/// `enable`/`disable` fail on config file write errors.
pub async fn execute_command(operation: &LoopCommands) -> Result<(), ZenError> {
    match operation {
        LoopCommands::Run { dry_run, json } => run_cycle(*dry_run, *json).await,
        LoopCommands::Status { json } => show_status(*json),
        LoopCommands::Gaps { kind, json } => show_gaps(kind.as_deref(), *json),
        LoopCommands::Enable => set_enabled(true),
        LoopCommands::Disable => set_enabled(false),
    }
}

async fn run_cycle(dry_run: bool, json: bool) -> Result<(), ZenError> {
    let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
    let config = zen_core::config::load_config().map_err(|e| ZenError::Message(e.to_string()))?;
    let loop_cfg = &config.agentic.loop_cfg;

    let mut scheduler = ZenScheduler::new().with_timezone(config.cron.timezone_or_default());
    let worker = ZenLoopWorker::new()
        .with_schedule(loop_cfg.interval_or_default())
        .with_dry_run(dry_run);
    scheduler
        .register(worker)
        .map_err(|e| ZenError::Message(e.to_string()))?;

    if dry_run {
        println!("Dry-run: no mutations will be performed");
    }

    let report = scheduler
        .trigger("zen-loop")
        .await
        .map_err(|e| ZenError::Message(e.to_string()))?;

    let cycle = read_last_report(&paths.logs())?;
    if json {
        let out = cycle.unwrap_or(LoopCycleReport {
            cycle_id: format!("trigger-{}", report.worker_id),
            ..LoopCycleReport::default()
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&out).map_err(|e| ZenError::Message(e.to_string()))?
        );
    } else if let Some(cycle) = cycle {
        println!("Loop cycle report ({}):", cycle.cycle_id);
        println!(
            "  Outcome:            {}",
            cycle.outcome.map(|o| o.as_str()).unwrap_or("unknown")
        );
        println!("  Notes processed:    {}", cycle.notes_processed);
        println!("  Entities persisted: {}", cycle.entities_persisted);
        println!("  Wiki pages created: {}", cycle.pages_created);
        println!("  Archived:           {}", cycle.archived_count);
        println!("  Gaps:               {}", cycle.gaps.len());
        println!("  Worker duration:    {}ms", report.duration_ms);
    } else {
        println!(
            "Cycle executed ({}ms) — no report persisted",
            report.duration_ms
        );
    }

    match read_last_report(&paths.logs()) {
        Ok(Some(c)) if c.outcome == Some(CycleOutcome::Failed) => Err(ZenError::Message(
            c.last_error.unwrap_or_else(|| "cycle failed".into()),
        )),
        _ => Ok(()),
    }
}

/// The measured status-read snapshot (SC-005): everything `status` renders,
/// plus the wall time the read path took.
struct StatusSnapshot {
    cycle: Option<LoopCycleReport>,
    open_gaps: BTreeMap<String, u64>,
    /// Wall time of the report+gaps reads in milliseconds, rounded up and
    /// floored at 1 — an `Instant` delta is never negative and a completed
    /// read is never 0ms of real time; reporting 0 would read as unmeasured.
    status_read_ms: u128,
}

/// Read the loop status files under `<logs>` and measure the read path.
///
/// Pure over the directory (no config, no global paths) so it is testable
/// against a tempdir; errors only propagate from a *corrupt* persisted
/// report — absent files degrade to `None`/empty like the rest of the
/// status surface.
fn collect_status(logs: &std::path::Path) -> Result<StatusSnapshot, ZenError> {
    let started = std::time::Instant::now();
    let cycle = read_last_report(logs)?;
    let open_gaps = count_open_gaps(&gaps_path_from(logs));
    let status_read_ms = started.elapsed().as_micros().div_ceil(1000).max(1);
    Ok(StatusSnapshot {
        cycle,
        open_gaps,
        status_read_ms,
    })
}

/// Assemble the `--json` status payload from a snapshot plus config values.
fn status_payload(snapshot: &StatusSnapshot, schedule: &str, enabled: bool) -> serde_json::Value {
    serde_json::json!({
        "last_cycle": snapshot.cycle,
        "schedule": schedule,
        "enabled": enabled,
        "open_gaps": snapshot.open_gaps,
        "status_read_ms": snapshot.status_read_ms,
    })
}

fn show_status(json: bool) -> Result<(), ZenError> {
    let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
    let config = zen_core::config::load_config().map_err(|e| ZenError::Message(e.to_string()))?;
    let loop_cfg = &config.agentic.loop_cfg;
    let logs = paths.logs();

    let snapshot = collect_status(&logs)?;

    if json {
        let payload = status_payload(
            &snapshot,
            loop_cfg.interval_or_default(),
            loop_cfg.enabled_or_default(),
        );
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|e| ZenError::Message(e.to_string()))?
        );
        return Ok(());
    }

    println!("Zen loop status (read in {}ms):", snapshot.status_read_ms);
    println!("  Enabled:  {}", loop_cfg.enabled_or_default());
    println!("  Schedule: {}", loop_cfg.interval_or_default());
    match snapshot.cycle {
        Some(c) => {
            println!(
                "  Last cycle: {} ({})",
                c.cycle_id,
                c.started_at.map(|t| t.to_rfc3339()).unwrap_or_default()
            );
            println!(
                "  Outcome: {} · notes {} · pages {} · archived {} · gaps {}",
                c.outcome.map(|o| o.as_str()).unwrap_or("unknown"),
                c.notes_processed,
                c.pages_created,
                c.archived_count,
                c.gaps.len()
            );
        }
        None => println!("  Last cycle: none yet"),
    }
    println!("  Open gaps by kind:");
    for (kind, count) in &snapshot.open_gaps {
        println!("    {kind}: {count}");
    }
    Ok(())
}

fn show_gaps(kind: Option<&str>, json: bool) -> Result<(), ZenError> {
    let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
    let gaps_file = gaps_path_from(&paths.logs());
    let mut records = read_jsonl_lines(&gaps_file).map_err(|e| ZenError::Message(e.to_string()))?;
    records.reverse(); // most recent first

    if let Some(filter) = kind {
        records.retain(|r| {
            r.get("kind")
                .and_then(|k| k.as_str())
                .map(|k| k == filter)
                .unwrap_or(false)
        });
    }

    let user_questions = read_pending_user_questions(&paths.logs());

    if json {
        let payload = serde_json::json!({
            "gaps": records,
            "user_questions": user_questions,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|e| ZenError::Message(e.to_string()))?
        );
        return Ok(());
    }

    if records.is_empty() {
        println!("No gaps detected.");
    } else {
        for r in records {
            let k = r.get("kind").and_then(|k| k.as_str()).unwrap_or("?");
            let detail = r.get("detail").and_then(|d| d.as_str()).unwrap_or("");
            let path = r.get("subject_path").and_then(|p| p.as_str()).unwrap_or("");
            println!(
                "[{k}] {detail}{}",
                if path.is_empty() {
                    String::new()
                } else {
                    format!(" ({path})")
                }
            );
        }
    }

    println!("\nPending user questions ({}):", user_questions.len());
    for q in &user_questions {
        println!("  - {q}");
    }
    Ok(())
}

/// Read pending user questions from the Discovery Loop refinement queue
/// (`<logs>/refinement-queue.json`, `user_questions` key — T187).
///
/// # Parameters
/// - `logs` — resolved logs directory (e.g. `ZenPaths::logs()`).
///
/// # Returns
/// The `user_questions` string array written by the zen-loop stage-5c
/// refinement pass; empty when the file is absent, unreadable, corrupt, or
/// lacks the key (fail-open — the writer is explicitly non-fatal, so the
/// reader must never turn a bad queue into a CLI error).
fn read_pending_user_questions(logs: &std::path::Path) -> Vec<String> {
    let path = logs.join("refinement-queue.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    value
        .get("user_questions")
        .and_then(|q| q.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|q| q.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn set_enabled(enable: bool) -> Result<(), ZenError> {
    let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
    let config_path = paths.config_file();
    let raw = std::fs::read_to_string(&config_path).unwrap_or_default();
    let updated = upsert_loop_enabled(&raw, enable);
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ZenError::Message(e.to_string()))?;
    }
    std::fs::write(&config_path, updated)
        .map_err(|e| ZenError::Message(format!("write {}: {e}", config_path.display())))?;

    println!(
        "zen wiki loop {} — persisted to {} (takes effect next tick; `run` works regardless)",
        if enable { "enabled" } else { "disabled" },
        config_path.display()
    );
    Ok(())
}

/// Text-level `[agentic.loop] enabled = <bool>` upsert.
///
/// (toml 1.1.4 round-trip is broken: its writer emits `[agentic.loop]`
/// headers its own parser rejects, so string surgery preserves both the
/// user's comments and correctness.)
fn upsert_loop_enabled(raw: &str, enable: bool) -> String {
    let wanted = format!("enabled = {}", enable);
    let mut section: Option<(usize, usize)> = None;
    for (idx, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed == "[agentic.loop]" {
            section = Some((idx, idx));
            continue;
        }
        if section.is_some() {
            if trimmed.starts_with('[') {
                break; // next section begins
            }
            if trimmed.starts_with("enabled") {
                section = Some((section.unwrap().0, idx));
            }
        }
    }

    match section {
        Some((start, enabled_line)) if enabled_line > start => {
            let mut lines: Vec<String> = raw.lines().map(|l| l.to_string()).collect();
            lines[enabled_line] = wanted;
            lines.join("\n") + "\n"
        }
        Some((start, _)) => {
            let mut lines: Vec<String> = raw.lines().map(|l| l.to_string()).collect();
            lines.insert(start + 1, wanted);
            lines.join("\n") + "\n"
        }
        None => {
            let mut out = raw.to_string();
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("\n[agentic.loop]\n");
            out.push_str(&wanted);
            out.push('\n');
            out
        }
    }
}

fn gaps_path_from(logs: &std::path::Path) -> std::path::PathBuf {
    gaps_path(logs)
}

fn read_last_report(logs: &std::path::Path) -> Result<Option<LoopCycleReport>, ZenError> {
    let path = last_report_path(logs);
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| ZenError::Message(format!("read {}: {e}", path.display())))?;
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|e| ZenError::Message(format!("parse {}: {e}", path.display())))
}

fn count_open_gaps(gaps_file: &std::path::Path) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    let Ok(file) = std::fs::File::open(gaps_file) else {
        return counts;
    };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line)
            && let Some(kind) = v.get("kind").and_then(|k| k.as_str())
        {
            *counts.entry(kind.to_string()).or_insert(0) += 1;
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t187_pending_user_questions_are_read_from_refinement_queue() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(
            logs.join("refinement-queue.json"),
            r#"{
                "cycle_id": "c-1",
                "generated_at": "2026-09-20T00:00:00Z",
                "occupied_cells": 2,
                "fetch_prompts": ["Re-read evidence files [raw/foo.md]"],
                "user_questions": [
                    "Is hypothesis 'orphan-foo' still relevant? Current status: Exploring. Detail: ...",
                    "Is hypothesis 'duplicate-bar' still relevant? Current status: Hypothesis. Detail: ..."
                ]
            }"#,
        )
        .unwrap();

        let questions = read_pending_user_questions(&logs);
        assert_eq!(questions.len(), 2);
        assert!(questions[0].contains("orphan-foo"));
        assert!(questions[1].contains("duplicate-bar"));
    }

    #[test]
    fn sc005_status_read_ms_is_measured_and_present_in_json_payload() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(
            last_report_path(&logs),
            r#"{"cycle_id":"c-42","notes_processed":3,"archived_count":2}"#,
        )
        .unwrap();
        std::fs::write(
            gaps_path(&logs),
            "{\"kind\":\"orphan_entity\",\"detail\":\"a\"}\n\
             {\"kind\":\"orphan_entity\",\"detail\":\"b\"}\n\
             {\"kind\":\"quarantined_note\",\"detail\":\"c\"}\n",
        )
        .unwrap();

        let snapshot = collect_status(&logs).expect("status read must succeed");
        assert!(
            snapshot.status_read_ms >= 1,
            "a completed read must report ≥1ms, got {}",
            snapshot.status_read_ms
        );
        assert_eq!(snapshot.cycle.as_ref().unwrap().cycle_id, "c-42");
        assert_eq!(snapshot.open_gaps["orphan_entity"], 2);
        assert_eq!(snapshot.open_gaps["quarantined_note"], 1);

        let payload = status_payload(&snapshot, "0 */5 * * * *", true);
        assert!(
            payload["status_read_ms"].as_u64().unwrap() >= 1,
            "JSON payload must carry the measured field: {payload}"
        );
        assert_eq!(payload["last_cycle"]["cycle_id"], "c-42");
        assert_eq!(payload["schedule"], "0 */5 * * * *");
        assert_eq!(payload["enabled"], true);
        assert_eq!(payload["open_gaps"]["orphan_entity"], 2);
    }

    #[test]
    fn sc005_status_read_degrades_to_empty_without_files() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot = collect_status(dir.path()).expect("absent files are not an error");
        assert!(snapshot.cycle.is_none());
        assert!(snapshot.open_gaps.is_empty());
        assert!(snapshot.status_read_ms >= 1);
        let payload = status_payload(&snapshot, "* * * * *", false);
        assert!(payload.get("status_read_ms").is_some());
        assert!(payload["last_cycle"].is_null());
    }

    #[test]
    fn t187_missing_or_corrupt_queue_yields_no_questions() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_pending_user_questions(dir.path()).is_empty());

        let logs = dir.path().join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("refinement-queue.json"), "not json").unwrap();
        assert!(read_pending_user_questions(&logs).is_empty());

        std::fs::write(logs.join("refinement-queue.json"), r#"{"cycle_id":"c"}"#).unwrap();
        assert!(read_pending_user_questions(&logs).is_empty());
    }
}
