use std::fs;

use anyhow::{Context, Result};
use tracing::{debug, info};

use zen_core::paths::ZenPaths;
use zen_memory::{MemvidIndexer, ZenMemvidStore};

use super::super::{WorkerContext, WorkerReport, ZenWorker};

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

    async fn execute(&self, _ctx: &WorkerContext) -> Result<WorkerReport> {
        let start = std::time::Instant::now();
        let paths = ZenPaths::detect()?;

        let workspace_root = match paths.workspace_root() {
            Some(root) => root.clone(),
            None => {
                debug!("no workspace root configured, skipping memvid indexing");
                return Ok(WorkerReport {
                    worker_id: self.id().to_string(),
                    success: true,
                    fact_count: 0,
                    duration_ms: start.elapsed().as_millis() as u64,
                });
            }
        };

        let store_path = paths.memory().join("memvid.db");
        fs::create_dir_all(store_path.parent().unwrap_or(&store_path)).with_context(|| {
            format!(
                "failed to create memvid store dir: {}",
                store_path.display()
            )
        })?;

        let mut store =
            ZenMemvidStore::new(store_path).with_context(|| "failed to open memvid store")?;

        let indexer = MemvidIndexer::new(workspace_root);
        let report = indexer
            .index_all(&mut store)
            .with_context(|| "memvid indexing failed")?;

        info!(
            files = report.files_scanned,
            chunks = report.chunks_indexed,
            errors = report.errors.len(),
            "memvid indexing complete"
        );

        Ok(WorkerReport {
            worker_id: self.id().to_string(),
            success: true,
            fact_count: report.chunks_indexed,
            duration_ms: start.elapsed().as_millis() as u64,
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
}
