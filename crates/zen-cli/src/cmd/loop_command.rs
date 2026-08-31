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

#[derive(Subcommand)]
pub enum LoopCommands {
    /// Execute one full processing cycle immediately
    Run {
        /// Compute the cycle but perform no mutations (report only)
        #[arg(long)]
        dry_run: bool,
        /// Machine-readable LoopCycleReport
        #[arg(long)]
        json: bool,
    },
    /// Show loop state: last cycle report, schedule, open gap counts
    Status {
        /// Machine-readable status object
        #[arg(long)]
        json: bool,
    },
    /// List detected gap records (most recent first)
    Gaps {
        /// Filter by gap kind (e.g. orphan_entity)
        #[arg(long)]
        kind: Option<String>,
        /// Machine-readable GapRecord array
        #[arg(long)]
        json: bool,
    },
    /// Enable the loop worker (fires on the configured cron)
    Enable,
    /// Disable the loop worker (manual `run` still works)
    Disable,
}

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

    let mut scheduler = ZenScheduler::new();
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

    let cycle = read_last_report(&paths)?;
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

    match read_last_report(&paths) {
        Ok(Some(c)) if c.outcome == Some(CycleOutcome::Failed) => Err(ZenError::Message(
            c.last_error.unwrap_or_else(|| "cycle failed".into()),
        )),
        _ => Ok(()),
    }
}

fn show_status(json: bool) -> Result<(), ZenError> {
    let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
    let config = zen_core::config::load_config().map_err(|e| ZenError::Message(e.to_string()))?;
    let loop_cfg = &config.agentic.loop_cfg;
    let logs = paths.logs();

    let cycle = read_last_report(&paths)?;
    let open_gaps = count_open_gaps(&gaps_path_from(&logs));

    if json {
        let payload = serde_json::json!({
            "last_cycle": cycle,
            "schedule": loop_cfg.interval_or_default(),
            "enabled": loop_cfg.enabled_or_default(),
            "open_gaps": open_gaps,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|e| ZenError::Message(e.to_string()))?
        );
        return Ok(());
    }

    println!("Zen loop status:");
    println!("  Enabled:  {}", loop_cfg.enabled_or_default());
    println!("  Schedule: {}", loop_cfg.interval_or_default());
    match cycle {
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
    for (kind, count) in &open_gaps {
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

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&records).map_err(|e| ZenError::Message(e.to_string()))?
        );
        return Ok(());
    }

    if records.is_empty() {
        println!("No gaps detected.");
        return Ok(());
    }
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
    Ok(())
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

fn read_last_report(paths: &ZenPaths) -> Result<Option<LoopCycleReport>, ZenError> {
    let path = last_report_path(&paths.logs());
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
