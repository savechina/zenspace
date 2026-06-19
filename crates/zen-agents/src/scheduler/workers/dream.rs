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

#[async_trait::async_trait]
impl ZenWorker for DreamWorker {
    fn id(&self) -> &'static str {
         "dream"
     }

    fn description(&self) -> &'static str {
         "Nightly consolidation: extract facts, update memory, compress logs, recompute entities"
     }

    fn schedule(&self) -> &'static str {
        self.scheduled.unwrap_or("0 0 2-4 * * *")
     }

    async fn execute(&self, _ctx: &WorkerContext) -> Result<WorkerReport> {
        let start = std::time::Instant::now();
        let paths = ZenPaths::detect()?;
        let today = Utc::now().date_naive();

        let report = ZenDream::new().run_cycle(&paths, today)?;

        info!(
             "dream cycle: facts={}, memory={}, logs={}, entities={}",
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
