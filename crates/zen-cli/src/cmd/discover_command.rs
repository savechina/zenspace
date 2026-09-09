//! Manual Hybrid C surface for the self-learning pipeline (PD-04/PD-06).
//!
//! Hypothesis generation and promotion staging live inside the zen-loop
//! cycle (stages 5b/5c/5d); this surface operates the gates: `run` triggers
//! one zen-loop cycle on demand, `queue` / `confirm` / `reject` operate the
//! promotion queue staged by stage 5d, and `report` prints live
//! [`DiscoverMetrics`] numbers.

use clap::Subcommand;
use colored::Colorize;

use zen_agents::scheduler::{PromotionWorker, WorkerContext, ZenLoopWorker, ZenWorker};
use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;
use zen_vault::distill::{
    CliContestant, Contestant, NaiveBaseline, ZenDistill, aggregate, load_reports, run_arena,
};

#[derive(Subcommand)]
pub enum DiscoverCommands {
    /// Run one zen-loop cycle now (distill → verify → hypotheses → staging)
    Run,
    /// Stage validated hypotheses into the promotion queue (same as stage 5d)
    Stage,
    /// List staged promotion proposals awaiting confirmation
    Queue,
    /// Confirm a staged promotion by id (Hybrid C gate)
    Confirm {
        /// Promotion id (e.g. `promo-orphan-cache`)
        id: String,
    },
    /// Reject a staged promotion by id (drops it with an audit line)
    Reject {
        /// Promotion id (e.g. `promo-orphan-cache`)
        id: String,
    },
    /// Show RSI health metrics aggregated from cycle reports
    Report,
    /// Run the adversarial arena: same corpus, every contestant, mechanical judge
    Arena {
        /// Extra external contestant as `name;program;arg...` (stdin/stdout JSON protocol)
        #[arg(long)]
        external: Option<String>,
        /// Cycle id for the report filename (default `arena-<yyyymmdd>`)
        #[arg(long)]
        cycle: Option<String>,
        /// Dump the shared corpus gaps as JSON to <path> (for external runners) and exit
        #[arg(long)]
        dump_corpus: Option<String>,
    },
}

pub async fn execute_command(cmd: &DiscoverCommands) -> Result<(), ZenError> {
    let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
    let map_err = |e: anyhow::Error| ZenError::Message(e.to_string());

    match cmd {
        DiscoverCommands::Run => {
            let worker = ZenLoopWorker::new();
            let report = worker
                .execute(&WorkerContext::new(chrono::Utc::now()))
                .await
                .map_err(map_err)?;
            println!(
                "{} zen-loop cycle complete: {} notes processed in {}ms",
                "discover".green().bold(),
                report.fact_count,
                report.duration_ms,
            );
        }
        DiscoverCommands::Stage => {
            let worker = PromotionWorker::with_paths();
            let report = worker
                .execute(&WorkerContext::new(chrono::Utc::now()))
                .await
                .map_err(map_err)?;
            println!(
                "{} {} promotion(s) staged",
                "promotion".green().bold(),
                report.fact_count,
            );
        }
        DiscoverCommands::Queue => {
            let worker = PromotionWorker::with_paths();
            let pending = worker.pending(&paths.logs()).map_err(map_err)?;
            if pending.is_empty() {
                println!("{}", "promotion queue is empty".dimmed());
                return Ok(());
            }
            for item in &pending {
                println!(
                    "{} {:?} — {}",
                    item.id.cyan().bold(),
                    item.target,
                    item.summary,
                );
            }
        }
        DiscoverCommands::Confirm { id } => {
            let worker = PromotionWorker::with_paths();
            let summary = worker
                .pending(&paths.logs())
                .map_err(map_err)?
                .iter()
                .find(|item| item.id == *id)
                .map(|item| item.summary.clone());
            let target = worker
                .confirm(&paths.logs(), &paths.skills(), &paths.wiki(), id)
                .map_err(map_err)?;
            println!("{} {id} → {target:?}", "confirmed".green().bold());
            if let Some(summary) = summary {
                println!("  {}", summary.dimmed());
            }
        }
        DiscoverCommands::Reject { id } => {
            let worker = PromotionWorker::with_paths();
            if worker.reject(&paths.logs(), id).map_err(map_err)? {
                println!("{} {id}", "rejected".yellow().bold());
            } else {
                return Err(ZenError::Message(format!(
                    "no staged promotion item with id '{id}' (see `zen discover queue`)"
                )));
            }
        }
        DiscoverCommands::Report => {
            let history = load_reports(&paths.logs())
                .map_err(|e| ZenError::Message(format!("discover metrics I/O error: {e}")))?;
            let metrics = aggregate(&history);
            println!(
                "{}",
                serde_json::to_string_pretty(&metrics)
                    .map_err(|e| ZenError::Message(e.to_string()))?
            );
        }
        DiscoverCommands::Arena {
            external,
            cycle,
            dump_corpus,
        } => {
            if let Some(path) = dump_corpus {
                let cases = zen_vault::distill::corpus();
                let json = serde_json::to_string_pretty(
                    &cases
                        .iter()
                        .map(|case| {
                            serde_json::json!({
                                "case_id": case.id,
                                "description": case.description,
                                "gaps": case.gaps,
                                "expected_opps": case.expected_opps,
                                "expected_slugs": case.expected_slugs,
                            })
                        })
                        .collect::<Vec<_>>(),
                )
                .map_err(|e| ZenError::Message(e.to_string()))?;
                std::fs::write(path, json).map_err(|e| ZenError::Message(e.to_string()))?;
                println!("corpus dumped ({} cases)", cases.len());
                return Ok(());
            }
            let zen = ZenDistill;
            let naive = NaiveBaseline;
            let owned: Vec<&dyn Contestant> = vec![&zen, &naive];
            let external_owned;
            let mut contestants: Vec<&dyn Contestant> = owned;
            if let Some(spec) = external {
                let mut parts = spec.split(';');
                let name = parts.next().unwrap_or("external").to_string();
                let program = parts.next().unwrap_or(&name).to_string();
                let args: Vec<String> = parts.map(str::to_string).collect();
                external_owned = CliContestant {
                    name,
                    program,
                    args,
                    timeout_secs: 300,
                };
                contestants.push(&external_owned);
            }
            let cycle_id = cycle
                .clone()
                .unwrap_or_else(|| format!("arena-{}", chrono::Utc::now().format("%Y%m%d")));
            let report = run_arena(&contestants, &paths.logs(), &cycle_id).map_err(map_err)?;
            for case in &report.cases {
                let mark = if case.winner == zen_vault::distill::INCUMBENT {
                    "✓".green()
                } else {
                    "✗".red()
                };
                println!("{mark} {:22} winner: {}", case.case_id, case.winner.bold());
            }
            println!(
                "zen-distill {}/{} cases (regression gate; losses recorded in the report only)",
                report.zen_wins, report.total_cases,
            );
        }
    }
    Ok(())
}
