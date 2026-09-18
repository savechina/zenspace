use anyhow::Result;
use chrono::Utc;
use tracing::info;

use zen_core::paths::ZenPaths;
use zen_memory::dream::ZenDream;

use super::super::{WorkerContext, WorkerReport, ZenWorker};

pub struct DreamWorker {
    scheduled: Option<&'static str>,
}

impl DreamWorker {
    pub fn new() -> Self {
        Self { scheduled: None }
    }

    pub fn with_schedule(mut self, expr: &str) -> Self {
        self.scheduled = Some(Box::leak(expr.to_string().into_boxed_str()));
        self
    }
}

impl Default for DreamWorker {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ZenWorker for DreamWorker {
    fn id(&self) -> &'static str {
        "dream"
    }

    fn description(&self) -> &'static str {
        "Nightly consolidation: extract facts, update memory, compress logs, recompute notions"
    }

    fn schedule(&self) -> &'static str {
        self.scheduled.unwrap_or("0 0 2 * * *")
    }

    async fn execute(&self, _ctx: &WorkerContext) -> Result<WorkerReport> {
        let start = std::time::Instant::now();
        let paths = ZenPaths::detect()?;
        let today = Utc::now().date_naive();

        let state_db = paths.data().join("state.db");
        let notion_graph = if state_db.exists() {
            match zen_repo::SqliteClient::open(&state_db).await {
                Ok(client) => {
                    let adapter = zen_vault::NotionGraphAdapter::from_client(client);
                    Some(std::sync::Arc::new(adapter)
                        as std::sync::Arc<
                            dyn zen_core::notion_graph::NotionGraphProvider,
                        >)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to open state.db for dream cycle");
                    None
                }
            }
        } else {
            None
        };

        let report = ZenDream::new(notion_graph).run_cycle(&paths, today).await?;

        info!(
            "dream cycle: facts={}, memory={}, logs={}, notions={}",
            report.facts_extracted,
            report.memory_updated,
            report.logs_compressed,
            report.entities_recomputed
        );

        // FR-037 skill precipitation (Hybrid C): distill repeated successes
        // / corrections into staged drafts; never fatal to the dream cycle.
        let drafts_staged = match precipitate_skills(&paths).await {
            Ok(count) => count,
            Err(e) => {
                tracing::warn!(error = %e, "skill precipitation failed; dream cycle unaffected");
                0
            }
        };
        info!("dream cycle: skill drafts staged = {drafts_staged}");

        // FR-035: scan verified corrections for recurrence within 30-day window
        let corrections_dir = paths.wiki().join("wisdom").join("corrections");
        let (corrections_scanned, corrections_high_recurrence) =
            zen_memory::scan_correction_recurrence(&corrections_dir, 30);
        if corrections_scanned > 0 {
            info!(
                scanned = corrections_scanned,
                high_recurrence = corrections_high_recurrence,
                "FR-035 correction recurrence scan complete"
            );
        }
        // FR-035: high recurrence (>0.5 recurrence rate on any verified
        // correction, i.e. high_recurrence > 0) boosts the loss-aversion
        // guard for the decision quality gate until the next clean scan.
        if corrections_high_recurrence > 0 {
            write_loss_aversion_boost(&paths, corrections_high_recurrence, corrections_scanned);
        } else if loss_aversion_boost_path(&paths).exists() {
            let _ = std::fs::remove_file(loss_aversion_boost_path(&paths));
        }

        // FR-036: aggregate tool call outcomes, flag tools with <70% success
        let sessions_dir = paths.sessions();
        zen_vault::distill::prune_expired_sessions(&sessions_dir, 30);
        let tool_aggs = zen_vault::distill::aggregate_all_sessions(&sessions_dir);
        let tool_calls_flagged = tool_aggs
            .iter()
            .filter(|a| a.success_rate < 70.0 && a.total_calls >= 5)
            .count();
        let tool_call_entries_scanned: usize =
            tool_aggs.iter().map(|a| a.total_calls as usize).sum();
        if tool_call_entries_scanned > 0 {
            for agg in &tool_aggs {
                if agg.success_rate < 70.0 && agg.total_calls >= 5 {
                    tracing::warn!(
                        tool = %agg.tool,
                        success_rate = agg.success_rate,
                        total_calls = agg.total_calls,
                        "FR-036 tool flagged: success rate below 70%"
                    );
                }
            }
            info!(
                entries_scanned = tool_call_entries_scanned,
                tools_flagged = tool_calls_flagged,
                "FR-036 tool call aggregation complete"
            );
        }

        Ok(WorkerReport {
            worker_id: self.id().to_string(),
            success: true,
            fact_count: report.facts_extracted,
            duration_ms: start.elapsed().as_millis() as u64,
            llm_cost_usd: 0.0,
        })
    }
}

/// Detect and stage skill drafts from execution histories (FR-037).
///
/// Preference triggers come from the M4 preference pages under
/// `vault/wiki/wisdom/preferences/` when present; read failures are
/// non-fatal (drafts just seed with no triggers).
async fn precipitate_skills(paths: &ZenPaths) -> anyhow::Result<usize> {
    let precipitator =
        crate::skill_precipitation::SkillPrecipitator::new(paths.skills(), paths.logs());
    let history = crate::skill_history::SkillHistory::new(paths);
    let names = precipitator.list_history_names()?;
    if names.is_empty() {
        return Ok(0);
    }
    let preferences = load_preferences(paths).unwrap_or_default();
    let drafts = precipitator.detect(&history, &names, &preferences)?;
    if drafts.is_empty() {
        return Ok(0);
    }
    precipitator.stage(&drafts)
}

/// Load M4 preference triples from `vault/wiki/wisdom/preferences/*.md`.
fn load_preferences(paths: &ZenPaths) -> anyhow::Result<Vec<zen_memory::Preference>> {
    let dir = paths.wiki().join("wisdom").join("preferences");
    let mut preferences = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let content = std::fs::read_to_string(&path)?;
        if let Some(p) = zen_memory::Preference::parse_page(&content) {
            preferences.push(p);
        }
    }
    Ok(preferences)
}

/// FR-035 loss-aversion boost marker lives at `logs/loss-aversion-boost.json`.
pub(super) fn loss_aversion_boost_path(paths: &ZenPaths) -> std::path::PathBuf {
    paths.logs().join("loss-aversion-boost.json")
}

/// Record the active boost; `updated_at` drives the 30-day freshness window.
pub(super) fn write_loss_aversion_boost(paths: &ZenPaths, high: usize, scanned: usize) {
    let payload = serde_json::json!({
        "high_recurrence": high,
        "scanned": scanned,
        "updated_at": Utc::now().to_rfc3339(),
    });
    let target = loss_aversion_boost_path(paths);
    let _ = std::fs::create_dir_all(paths.logs());
    let _ = std::fs::write(&target, payload.to_string());
}

/// FR-035 boost is active while the marker exists and was written within
/// the 30-day recurrence window; dream rewrites or removes it nightly.
pub(super) fn loss_aversion_boost_active(paths: &ZenPaths) -> bool {
    let Ok(raw) = std::fs::read_to_string(loss_aversion_boost_path(paths)) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    value["updated_at"]
        .as_str()
        .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
        .is_some_and(|ts| (Utc::now() - ts.with_timezone(&Utc)).num_days() <= 30)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boost_marker_roundtrip_activates_and_expires() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let paths = ZenPaths::for_testing(dir.path().to_path_buf());

        assert!(!loss_aversion_boost_active(&paths));

        write_loss_aversion_boost(&paths, 2, 5);
        assert!(loss_aversion_boost_active(&paths));

        let stale = serde_json::json!({
            "high_recurrence": 1,
            "scanned": 2,
            "updated_at": (Utc::now() - chrono::Duration::days(31)).to_rfc3339(),
        });
        std::fs::write(loss_aversion_boost_path(&paths), stale.to_string())
            .expect("write stale marker");
        assert!(
            !loss_aversion_boost_active(&paths),
            "31-day-old marker is outside the FR-035 window"
        );
    }
}
