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
use zen_agents::workflow_induction::{
    ConfirmOutcome, RejectOutcome, confirm_workflow, read_drafts, read_last_run, reject_workflow,
    shape_sha8,
};
use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;
use zen_vault::distill::{
    CalibrationTarget, CliContestant, Contestant, NaiveBaseline, ZenDistill, aggregate,
    aggregate_orchestration, analyze, append_label, calibrate, compute_baselines,
    compute_first_attempt_success, evaluate, label_status, latest_report, load_dataset,
    load_labels, load_reports, not_evaluated, run_arena, save_report, unlabeled, write_thresholds,
};

#[derive(Subcommand)]
pub enum DiscoverCommands {
    /// Run one zen-loop cycle now (distill → verify → hypotheses → staging)
    Run,
    /// Stage validated hypotheses into the promotion queue (same as stage 5d)
    Stage,
    /// List staged promotion proposals awaiting confirmation
    Queue {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
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
    /// Promote a staged workflow draft into a wisdom template (human gate)
    ConfirmWorkflow {
        /// Draft name from the induction queue (`zen discover report`)
        draft_name: String,
    },
    /// Reject a staged workflow draft and quarantine its shape signature
    RejectWorkflow {
        /// Draft name from the induction queue (`zen discover report`)
        draft_name: String,
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
    /// Evaluate a candidate System-1 model against zen's decision workloads (T174 gate)
    VendorEval {
        /// Candidate model name (a local Ollama model today; the vendor model when early access lands)
        #[arg(long)]
        model: String,
        /// Provider name in config (default: ollama — local-first)
        #[arg(long, default_value = "ollama")]
        provider: String,
        /// Bounded concurrency for the latency-under-load measurement (default 4, clamp 1..=8)
        #[arg(long, default_value_t = 4)]
        max_concurrent: usize,
    },
    /// Derive calibration thresholds from labeled decisions (T173 → T168-T171)
    Calibrate {
        /// Write the selected operating points to logs/decision-audit/thresholds.json.
        /// Without this flag the calibration is reported only — writing OPENS the
        /// decision gates, so it is never implicit.
        #[arg(long)]
        write: bool,
    },
    /// Adjudicate decision-audit records (T173 label supply)
    Label {
        #[command(subcommand)]
        command: LabelCommands,
    },
}

/// Labeling workflow subcommands — the human half of the T173 calibration
/// harness. Labels are appended to `logs/decision-audit/labels.jsonl`; a
/// later entry for the same id supersedes an earlier one (last wins).
#[derive(Subcommand)]
pub enum LabelCommands {
    /// Show labeling progress (total / labeled / per-kind)
    Status,
    /// Show the next unlabeled records for adjudication
    Next {
        /// Only records of this kind (e.g. `intent`)
        #[arg(long)]
        kind: Option<String>,
        /// Maximum number of records to show (default 10)
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Append a ground-truth label for a record id
    Set {
        /// Record id (from `zen discover label next`)
        id: String,
        /// Ground-truth label (e.g. the correct intent category)
        label: String,
    },
}

/// FR-034 reward-coverage section (T241) for `zen discover report`. Reads the
/// same `logs/memory-strength-report.json` as the memory_strength section and
/// mirrors its renderer exactly: a missing report, a pre-T241 report without
/// the block, or a corrupt file all render `{"recorded": false}` — advisory,
/// never an error (the replay-section fail-open precedent).
fn render_reward_coverage(logs_dir: &std::path::Path) -> Result<serde_json::Value, ZenError> {
    zen_agents::scheduler::read_memory_strength_report(logs_dir)
        .and_then(|report| report.reward_coverage)
        .map(|coverage| {
            serde_json::to_value(&coverage).map_err(|e| ZenError::Message(e.to_string()))
        })
        .transpose()
        .map(|coverage| coverage.unwrap_or_else(|| serde_json::json!({ "recorded": false })))
}

/// W13.8 advisory `workflow_induction` section for `zen discover report`.
///
/// Reads the last-run sidecar (per-run counters + insufficiency) and the
/// drafts queue — both fail-open. A missing/corrupt sidecar renders
/// `{"recorded": false}` (mirrors the `memory_strength` discipline); a
/// recorded run renders the design §7 shape. Side-effect-free: reads only.
fn render_workflow_induction(logs_dir: &std::path::Path) -> Result<serde_json::Value, ZenError> {
    let Some(summary) = read_last_run(logs_dir) else {
        return Ok(serde_json::json!({ "recorded": false }));
    };
    let drafts: Vec<serde_json::Value> = read_drafts(logs_dir)
        .iter()
        .map(|draft| {
            serde_json::json!({
                "name": draft.name,
                "shape_signature": draft.shape_signature,
                "cluster_size": draft.success_stats.cluster_size,
                "success_rate": draft.success_stats.success_rate,
            })
        })
        .collect();
    Ok(serde_json::json!({
        "advisory_only": true,
        "insufficient": summary.insufficient,
        "insufficient_reason": summary.missing,
        "drafts_count": drafts.len(),
        "skipped_corrupt": summary.skipped_corrupt,
        "skipped_titles_only": summary.skipped_titles_only,
        "drafts": drafts,
    }))
}

fn print_workflow_induction(logs_dir: &std::path::Path) {
    let Some(summary) = read_last_run(logs_dir) else {
        return;
    };
    let drafts = read_drafts(logs_dir);
    println!(
        "{}",
        format!("workflow induction (advisory, {} draft(s)):", drafts.len()).dimmed()
    );
    for draft in &drafts {
        println!(
            "  {}",
            format!(
                "- {} (cluster {}, success {:.0}%)",
                draft.name,
                draft.success_stats.cluster_size,
                draft.success_stats.success_rate * 100.0,
            )
            .dimmed()
        );
    }
    if let Some(reason) = &summary.missing {
        println!("  {}", format!("insufficient: {reason}").dimmed());
    }
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
        DiscoverCommands::Queue { json } => {
            let worker = PromotionWorker::with_paths();
            let pending = worker.pending(&paths.logs()).map_err(map_err)?;
            if *json {
                let payload: Vec<_> = pending
                    .iter()
                    .map(|item| {
                        serde_json::json!({
                            "id": item.id,
                            "target": format!("{:?}", item.target),
                            "summary": item.summary,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&payload)
                        .map_err(|e| ZenError::Message(format!("json: {e}")))?
                );
                return Ok(());
            }
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
        DiscoverCommands::RejectWorkflow { draft_name } => {
            let now = chrono::Utc::now().timestamp();
            match reject_workflow(&paths.logs(), draft_name, now).await {
                Ok(RejectOutcome::Rejected { shape_signature }) => {
                    println!(
                        "{} {draft_name} (signature {}, permanently quarantined)",
                        "workflow draft rejected".yellow().bold(),
                        shape_sha8(&shape_signature),
                    );
                }
                Ok(RejectOutcome::AlreadyRejected { shape_signature }) => {
                    println!(
                        "{} {draft_name} (signature {} already quarantined; queue entry removed)",
                        "already rejected".yellow().bold(),
                        shape_sha8(&shape_signature),
                    );
                }
                Err(e) => {
                    return Err(ZenError::Message(format!(
                        "workflow reject failed: {e} — run `zen discover report` to see staged drafts"
                    )));
                }
            }
        }
        DiscoverCommands::ConfirmWorkflow { draft_name } => {
            let now = chrono::Utc::now().timestamp();
            match confirm_workflow(&paths.logs(), &paths.vault(), draft_name, now).await {
                Ok(ConfirmOutcome::Promoted {
                    template_path,
                    suffixed,
                }) => {
                    println!(
                        "{} {draft_name} → {}",
                        "workflow template promoted".green().bold(),
                        template_path.display(),
                    );
                    if suffixed {
                        println!(
                            "  {}",
                            "name collision: deterministic shape-suffixed variant written".dimmed()
                        );
                    }
                }
                Ok(ConfirmOutcome::AlreadyPromoted { template_path }) => {
                    println!(
                        "{} {draft_name} — {} (not rewritten)",
                        "already promoted".yellow().bold(),
                        template_path.display(),
                    );
                }
                Err(e) => {
                    return Err(ZenError::Message(format!(
                        "workflow confirm failed: {e} — run `zen discover report` to see staged drafts"
                    )));
                }
            }
        }
        DiscoverCommands::Calibrate { write } => {
            let records = load_dataset(&paths.logs())
                .map_err(|e| ZenError::Message(format!("decision audit error: {e}")))?;
            let target = CalibrationTarget::default();
            let calibrations = calibrate(&records, target);

            if *write {
                let written = write_thresholds(&paths.logs(), &calibrations)
                    .map_err(|e| ZenError::Message(format!("threshold write error: {e}")))?;
                println!("{} {}", "thresholds:".green().bold(), written.display());
            }

            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "labeled_records": records.iter().filter(|r| r.label.is_some()).count(),
                    "target": {
                        "min_precision": target.min_precision,
                        "min_coverage": target.min_coverage,
                        "min_labels": target.min_labels,
                        "min_precision_lift": target.min_precision_lift,
                        "delta": target.delta,
                    },
                    "fields": calibrations,
                    "written": *write,
                }))
                .map_err(|e| ZenError::Message(e.to_string()))?
            );

            if !*write {
                println!(
                    "{}",
                    "report-only: pass --write to persist the selected operating points (this OPENS the decision gates)"
                        .dimmed()
                );
            }
        }
        DiscoverCommands::Label { command } => match command {
            LabelCommands::Status => {
                let records = load_dataset(&paths.logs())
                    .map_err(|e| ZenError::Message(format!("decision audit error: {e}")))?;
                let status = label_status(&records);
                println!(
                    "{}",
                    serde_json::to_string_pretty(&status)
                        .map_err(|e| ZenError::Message(e.to_string()))?
                );
            }
            LabelCommands::Next { kind, limit } => {
                let records = load_dataset(&paths.logs())
                    .map_err(|e| ZenError::Message(format!("decision audit error: {e}")))?;
                let next = unlabeled(&records, kind.as_deref(), *limit);
                if next.is_empty() {
                    println!("{}", "no unlabeled records".dimmed());
                    return Ok(());
                }
                for record in next {
                    println!(
                        "{} kind={} rung={} confidence={} decision={} ts={}",
                        record.id.cyan().bold(),
                        record.kind,
                        record.rung,
                        record
                            .confidence
                            .map(|c| format!("{c:.4}"))
                            .unwrap_or_else(|| "—".to_string()),
                        record.decision.as_deref().unwrap_or("—"),
                        record.timestamp.as_deref().unwrap_or("—"),
                    );
                    match &record.input_excerpt {
                        Some(excerpt) => println!("  input: {excerpt}"),
                        None => println!(
                            "  {}",
                            "no input excerpt recorded — set [agentic.audit] decision_excerpt_chars > 0 (default 0 = off) to capture one"
                                .yellow()
                        ),
                    }
                }
            }
            LabelCommands::Set { id, label } => {
                let path = append_label(&paths.logs(), id, label)
                    .map_err(|e| ZenError::Message(format!("label append error: {e}")))?;
                let records = load_dataset(&paths.logs())
                    .map_err(|e| ZenError::Message(format!("decision audit error: {e}")))?;
                let status = label_status(&records);
                let stored = load_labels(&paths.logs())
                    .ok()
                    .and_then(|labels| labels.get(id).cloned())
                    .unwrap_or_else(|| label.clone());
                println!(
                    "{} {id} → {stored} ({}/{})",
                    "labeled".green().bold(),
                    status.labeled,
                    status.total,
                );
                println!("  {}", path.display().to_string().dimmed());
            }
        },
        DiscoverCommands::Report => {
            let history = load_reports(&paths.logs())
                .map_err(|e| ZenError::Message(format!("discover metrics I/O error: {e}")))?;
            let metrics = aggregate(&history);
            let orchestration = aggregate_orchestration(&paths.logs())
                .map_err(|e| ZenError::Message(format!("orchestration stats I/O error: {e}")))?;
            let calibration = analyze(&paths.logs())
                .map_err(|e| ZenError::Message(format!("decision audit error: {e}")))?;
            let replay = zen_vault::distill::score_from_log(&paths.logs());
            let baselines = compute_baselines(&paths.logs());
            let first_attempt_success = compute_first_attempt_success(&paths.logs());
            let threshold_fields = {
                let records = load_dataset(&paths.logs()).unwrap_or_default();
                calibrate(&records, CalibrationTarget::default())
            };
            let vendor_eval = latest_report(&paths.logs())
                .map_err(|e| ZenError::Message(format!("vendor eval I/O error: {e}")))?
                .unwrap_or_else(|| {
                    not_evaluated(
                        "unknown".to_string(),
                        "unknown".to_string(),
                        "no vendor evaluation has been run — run `zen discover vendor-eval --model <name>`"
                            .to_string(),
                    )
                });
            let skill_eval = zen_agents::skill_trigger_eval::compute(&paths.skills());
            // T210: additive memory-strength section (the replay-section
            // precedent — a missing/corrupt report is "not recorded", never
            // an error; the section is advisory).
            let memory_strength = zen_agents::scheduler::read_memory_strength_report(&paths.logs())
                .map(|report| {
                    serde_json::to_value(&report).map_err(|e| ZenError::Message(e.to_string()))
                })
                .transpose()?
                .unwrap_or_else(|| serde_json::json!({ "recorded": false }));
            // Phase 34 T241: additive FR-034 reward-coverage section read from
            // the same report file — same fail-open shape as memory_strength.
            let reward_coverage = render_reward_coverage(&paths.logs())?;
            let workflow_induction = render_workflow_induction(&paths.logs())?;
            let success_criteria = serde_json::json!({
                "first_attempt_success": serde_json::to_value(&first_attempt_success)
                    .map_err(|e| ZenError::Message(e.to_string()))?,
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "rsi_health": metrics,
                    "orchestration": orchestration,
                    "calibration": calibration,
                    "thresholds": threshold_fields,
                    "baselines": baselines,
                    "success_criteria": success_criteria,
                    "replay": replay,
                    "vendor_eval": vendor_eval,
                    "skill_eval": skill_eval,
                    "memory_strength": memory_strength,
                    "reward_coverage": reward_coverage,
                    "workflow_induction": workflow_induction,
                }))
                .map_err(|e| ZenError::Message(e.to_string()))?
            );
            match first_attempt_success.first_attempt_success_pct {
                Some(pct) => println!(
                    "{} {:.1}% (n={})",
                    "first-attempt success:".green().bold(),
                    pct,
                    first_attempt_success.n
                ),
                None => println!(
                    "{} n={} (need {}) — percentage withheld, sample below the reporting minimum",
                    "first-attempt success:".yellow().bold(),
                    first_attempt_success.n,
                    first_attempt_success
                        .insufficient
                        .map(|ins| ins.required)
                        .unwrap_or(0),
                ),
            }
            print_workflow_induction(&paths.logs());
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
            let hypotheses_dir = paths.vault().join("wiki/wisdom/hypotheses");
            let report = run_arena(&contestants, &paths.logs(), &cycle_id, &hypotheses_dir)
                .map_err(map_err)?;
            for case in &report.cases {
                let mark = if case.winner == zen_vault::distill::INCUMBENT {
                    "✓".green()
                } else {
                    "✗".red()
                };
                println!("{mark} {:22} winner: {}", case.case_id, case.winner.bold());
            }
            println!(
                "zen-distill {}/{} cases (regression gate; loss hypotheses staged: {})",
                report.zen_wins,
                report.total_cases,
                report.staged_losses.len(),
            );
        }
        DiscoverCommands::VendorEval {
            model,
            provider,
            max_concurrent,
        } => {
            let config = zen_core::config::load_config()
                .map_err(|e| ZenError::Message(format!("config load error: {e}")))?;
            let report = evaluate(config, &paths.logs(), provider, model, *max_concurrent)
                .await
                .map_err(|e| ZenError::Message(format!("vendor eval error: {e}")))?;
            let saved = save_report(&paths.logs(), &report)
                .map_err(|e| ZenError::Message(format!("vendor eval save error: {e}")))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&report)
                    .map_err(|e| ZenError::Message(e.to_string()))?
            );
            println!("saved to {}", saved.display());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reward_coverage_section_missing_report_is_not_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let section = render_reward_coverage(dir.path()).unwrap();
        assert_eq!(section, json!({ "recorded": false }));
    }

    #[test]
    fn reward_coverage_section_corrupt_report_is_not_recorded() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("memory-strength-report.json"), "{ broken").unwrap();
        let section = render_reward_coverage(dir.path()).unwrap();
        assert_eq!(section, json!({ "recorded": false }));
    }

    use zen_agents::workflow_induction::{DRAFTS_FILE, LAST_RUN_FILE};

    #[test]
    fn workflow_induction_section_missing_sidecar_is_not_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let section = render_workflow_induction(dir.path()).unwrap();
        assert_eq!(section, json!({ "recorded": false }));
    }

    #[test]
    fn workflow_induction_section_corrupt_sidecar_is_not_recorded() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(LAST_RUN_FILE), "{ broken").unwrap();
        let section = render_workflow_induction(dir.path()).unwrap();
        assert_eq!(section, json!({ "recorded": false }));
    }

    #[test]
    fn workflow_induction_section_renders_insufficiency_reason() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(LAST_RUN_FILE),
            r#"{"drafts_written":0,"skipped_corrupt":1,"skipped_titles_only":2,
                "insufficient":true,"missing":"no similar shape cluster (threshold 0.72)",
                "induced_at":"2026-11-18T10:30:00Z"}"#,
        )
        .unwrap();
        let section = render_workflow_induction(dir.path()).unwrap();
        assert_eq!(section["advisory_only"], json!(true));
        assert_eq!(section["insufficient"], json!(true));
        assert_eq!(
            section["insufficient_reason"],
            json!("no similar shape cluster (threshold 0.72)")
        );
        assert_eq!(section["drafts_count"], json!(0));
        assert_eq!(section["skipped_corrupt"], json!(1));
        assert_eq!(section["skipped_titles_only"], json!(2));
        assert_eq!(section["drafts"], json!([]));
    }

    #[test]
    fn workflow_induction_section_renders_recorded_drafts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(LAST_RUN_FILE),
            r#"{"drafts_written":1,"skipped_corrupt":0,"skipped_titles_only":0,
                "insufficient":false,"missing":null,
                "induced_at":"2026-11-18T10:30:00Z"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join(DRAFTS_FILE),
            r#"[{"name":"research-then-synthesize",
                "shape_signature":"analyst:1,1;researcher:0,1;synthesizer:1,0",
                "source_plan_ids":["a","b"],"source_plan_dates":["2026-11-10","2026-11-15"],
                "tasks":[],
                "success_stats":{"cluster_size":2,"avg_duration_ms":45000,
                    "avg_cost_usd":0.12,"success_rate":1.0},
                "induced_at":"2026-11-18T10:30:00Z"}]"#,
        )
        .unwrap();
        let section = render_workflow_induction(dir.path()).unwrap();
        assert_eq!(section["advisory_only"], json!(true));
        assert_eq!(section["insufficient"], json!(false));
        assert_eq!(section["insufficient_reason"], json!(null));
        assert_eq!(section["drafts_count"], json!(1));
        assert_eq!(
            section["drafts"][0]["name"],
            json!("research-then-synthesize")
        );
        assert_eq!(section["drafts"][0]["cluster_size"], json!(2));
        assert_eq!(section["drafts"][0]["success_rate"], json!(1.0));
        assert!(
            section["drafts"][0].get("source_plan_ids").is_none(),
            "report drafts expose only the §7 summary fields"
        );
    }

    #[test]
    fn workflow_induction_section_corrupt_drafts_file_is_empty_queue() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(LAST_RUN_FILE),
            r#"{"drafts_written":0,"skipped_corrupt":0,"skipped_titles_only":0,
                "insufficient":false,"missing":null,
                "induced_at":"2026-11-18T10:30:00Z"}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join(DRAFTS_FILE), "{ broken").unwrap();
        let section = render_workflow_induction(dir.path()).unwrap();
        assert_eq!(section["drafts_count"], json!(0));
        assert_eq!(section["drafts"], json!([]));
    }

    #[test]
    fn reward_coverage_section_renders_recorded_block() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("memory-strength-report.json"),
            r#"{"generated_at":"2026-01-01T00:00:00Z","half_life_days":30.0,"count":0,"items":[],
                "reward_coverage":{"with_reward":2,"scored_cards":4,"pct":50.0,"recorded_at":"2026-01-01T00:00:00Z"}}"#,
        )
        .unwrap();
        let section = render_reward_coverage(dir.path()).unwrap();
        assert_eq!(section["with_reward"], json!(2));
        assert_eq!(section["scored_cards"], json!(4));
        assert_eq!(section["pct"], json!(50.0));
        assert_eq!(section["recorded"], serde_json::Value::Null);
    }
}
