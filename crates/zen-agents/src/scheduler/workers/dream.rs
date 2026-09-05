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
