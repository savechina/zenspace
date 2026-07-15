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
        self.scheduled.unwrap_or("0 0 2-4 * * *")
    }

    async fn execute(&self, _ctx: &WorkerContext) -> Result<WorkerReport> {
        let start = std::time::Instant::now();
        let paths = ZenPaths::detect()?;
        let today = Utc::now().date_naive();

        let state_db = paths.db().join("state.db");
        let notion_graph = if state_db.exists() {
            match zen_repo::SqliteClient::open(&state_db).await {
                Ok(client) => {
                    let adapter = zen_vault::NotionGraphAdapter::from_client(client);
                    Some(std::sync::Arc::new(adapter)
                        as std::sync::Arc<dyn zen_core::notion_graph::NotionGraphProvider>)
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

        Ok(WorkerReport {
            worker_id: self.id().to_string(),
            success: true,
            fact_count: report.facts_extracted,
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }
}
