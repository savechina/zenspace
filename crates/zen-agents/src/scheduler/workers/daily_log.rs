use anyhow::Result;
use chrono::Utc;
use tracing::info;

use zen_core::paths::ZenPaths;
use zen_memory::journal::Journal;
use zen_memory::dream::{extract_durable_facts, update_memory};

use super::super::{WorkerContext, WorkerReport, ZenWorker};

pub struct DailyLogWorker {
    scheduled: Option<&'static str>,
    last_entry_count: std::sync::atomic::AtomicUsize,
}

impl DailyLogWorker {
    pub fn new() -> Self {
        Self {
            scheduled: None,
            last_entry_count: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub fn with_schedule(mut self, expr: &str) -> Self {
        self.scheduled = Some(Box::leak(expr.to_string().into_boxed_str()));
        self
    }
}

#[async_trait::async_trait]
impl ZenWorker for DailyLogWorker {
    fn id(&self) -> &'static str {
          "daily-log"
      }

    fn description(&self) -> &'static str {
          "Check daily log for new entries, extract facts, update MEMORY.md"
      }

    fn schedule(&self) -> &'static str {
        self.scheduled.unwrap_or("0 */5 * * * *")
      }

    async fn execute(&self, _ctx: &WorkerContext) -> Result<WorkerReport> {
        let start = std::time::Instant::now();
        let paths = ZenPaths::detect()?;
        let today = Utc::now().date_naive();
        let entries = Journal::read_entries(&paths, today)?;

        if entries.len() <= self.last_entry_count.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(WorkerReport {
                worker_id: self.id().to_string(),
                success: true,
                fact_count: 0,
                duration_ms: start.elapsed().as_millis() as u64,
             });
         }

        let new_entries = &entries[self.last_entry_count.load(std::sync::atomic::Ordering::Relaxed)..];
        let mut total_facts = 0;
        for entry in new_entries {
            let facts = extract_durable_facts(&entry.content);
            total_facts += facts.len();
         }

        if total_facts > 0 {
            update_memory(&paths)?;
            info!(facts = total_facts, "MEMORY.md updated via light tick");
         }

        self.last_entry_count
              .store(entries.len(), std::sync::atomic::Ordering::Relaxed);

        Ok(WorkerReport {
            worker_id: self.id().to_string(),
            success: true,
            fact_count: total_facts,
            duration_ms: start.elapsed().as_millis() as u64,
         })
      }
}
