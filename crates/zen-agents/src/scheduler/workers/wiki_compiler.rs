use anyhow::Result;
use tracing::{debug, info};

use zen_core::paths::ZenPaths;
use zen_vault::{EntityData, EntityService, WikiCompiler};

use super::super::{WorkerContext, WorkerReport, ZenWorker};

pub struct WikiCompilerWorker {
    scheduled: Option<&'static str>,
}

impl WikiCompilerWorker {
    pub fn new() -> Self {
        Self { scheduled: None }
    }

    pub fn with_schedule(mut self, expr: &str) -> Self {
        self.scheduled = Some(Box::leak(expr.to_string().into_boxed_str()));
        self
    }
}

#[async_trait::async_trait]
impl ZenWorker for WikiCompilerWorker {
    fn id(&self) -> &'static str {
        "wiki-compiler"
    }

    fn description(&self) -> &'static str {
        "Compile wiki pages from graph.db entities"
    }

    fn schedule(&self) -> &'static str {
        self.scheduled.unwrap_or("0 */30 * * * *")
    }

    async fn execute(&self, _ctx: &WorkerContext) -> Result<WorkerReport> {
        let start = std::time::Instant::now();
        let paths = ZenPaths::detect()?;

        let graph_db = paths.db().join("graph.db");
        let wiki_dir = paths.vault().join("wiki");

        let svc = EntityService::new();

        let entities = svc.load_all_entities(&graph_db)?;
        if entities.is_empty() {
            debug!("no entities in graph.db, skipping wiki compilation");
            return Ok(WorkerReport {
                worker_id: self.id().to_string(),
                success: true,
                fact_count: 0,
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }

        let mut entity_data_list: Vec<EntityData> = Vec::new();
        for entity in &entities {
            let relationships = svc
                .load_relationships_for_entity(&graph_db, &entity.id)
                .unwrap_or_default();

            entity_data_list.push(EntityData {
                entity: entity.clone(),
                facts: Vec::new(),
                relationships,
            });
        }

        let pages_written =
            match WikiCompiler::new().compile_from_entities(&entity_data_list, &wiki_dir) {
                Ok(n) => {
                    info!(pages = n, "wiki pages compiled from graph.db entities");
                    n
                }
                Err(e) => {
                    tracing::error!(error = %e, "WikiCompiler failed");
                    0
                }
            };

        Ok(WorkerReport {
            worker_id: self.id().to_string(),
            success: true,
            fact_count: pages_written,
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }
}
