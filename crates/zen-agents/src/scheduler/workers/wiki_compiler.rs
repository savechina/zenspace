use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use zen_core::paths::ZenPaths;
use zen_vault::{EntityData, EntityService, WikiCompiler};

use super::super::{WorkerContext, WorkerReport, ZenWorker};

const STATE_FILE: &str = ".wiki_compiler_state.json";

/// Persistent state for incremental wiki compilation.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CompilerState {
    last_compile_time: DateTime<Utc>,
}

impl CompilerState {
    fn new() -> Self {
        Self {
            last_compile_time: DateTime::UNIX_EPOCH,
        }
    }

    fn load(path: &std::path::Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
                debug!(error = %e, path = %path.display(), "failed to parse compiler state, resetting");
                Self::new()
            }),
            Err(_) => Self::new(),
        }
    }

    fn save(&self, path: &std::path::Path) -> Result<()> {
        let content = serde_json::to_string_pretty(self)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create state dir: {}", parent.display()))?;
        }
        std::fs::write(path, &content)
            .with_context(|| format!("write compiler state: {}", path.display()))?;
        Ok(())
    }
}

pub struct WikiCompilerWorker {
    scheduled: Option<&'static str>,
}

impl Default for WikiCompilerWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl WikiCompilerWorker {
    pub fn new() -> Self {
        Self { scheduled: None }
    }

    pub fn with_schedule(mut self, expr: &str) -> Self {
        self.scheduled = Some(Box::leak(expr.to_string().into_boxed_str()));
        self
    }

    fn state_path(global_root: &Path) -> PathBuf {
        global_root.join("db").join(STATE_FILE)
    }

    async fn build_entity_data(
        svc: &EntityService,
        client: &zen_vault::SqliteClient,
        entity: &zen_vault::Entity,
    ) -> EntityData {
        let relationships = svc
            .load_relationships_for_entity(client, &entity.id)
            .await
            .unwrap_or_default();

        let fact = format!(
            "{} is a {} entity in the {} domain, first seen on {}, last updated on {}",
            entity.name,
            entity.entity_type,
            entity.domain.as_deref().unwrap_or("unknown"),
            entity.created_at.format("%Y-%m-%d"),
            entity.last_updated.format("%Y-%m-%d"),
        );

        EntityData {
            entity: entity.clone(),
            facts: vec![fact],
            relationships,
        }
    }
}

#[async_trait::async_trait]
impl ZenWorker for WikiCompilerWorker {
    fn id(&self) -> &'static str {
        "wiki-compiler"
    }

    fn description(&self) -> &'static str {
        "Compile wiki pages from state.db entities (incremental)"
    }

    fn schedule(&self) -> &'static str {
        self.scheduled.unwrap_or("0 */30 * * * *")
    }

    async fn execute(&self, _ctx: &WorkerContext) -> Result<WorkerReport> {
        let start = std::time::Instant::now();
        let paths = ZenPaths::detect()?;
        let global_root = paths.global_root();

        let state_path = Self::state_path(global_root);
        let state = CompilerState::load(&state_path);
        debug!(last_compile_time = %state.last_compile_time, "loaded compiler state");

        let state_db = paths.db().join("state.db");
        let wiki_dir = paths.vault().join("wiki");

        let client = match zen_vault::SqliteClient::open(&state_db).await {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "failed to open state.db, skipping wiki compilation");
                return Ok(WorkerReport {
                    worker_id: self.id().to_string(),
                    success: true,
                    fact_count: 0,
                    duration_ms: start.elapsed().as_millis() as u64,
                });
            }
        };
        let svc = EntityService::new();

        let entities = svc.load_entities_updated_since(&client, state.last_compile_time).await?;
        if entities.is_empty() {
            debug!(
                since = %state.last_compile_time,
                "wiki-compiler: no entities updated since last compile, nothing to do"
            );
            return Ok(WorkerReport {
                worker_id: self.id().to_string(),
                success: true,
                fact_count: 0,
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }

        info!(
            count = entities.len(),
            since = %state.last_compile_time,
            "incremental wiki compile"
        );

        let mut entity_data_list: Vec<EntityData> = Vec::with_capacity(entities.len());
        for entity in &entities {
            entity_data_list.push(Self::build_entity_data(&svc, &client, entity).await);
        }

        let pages_written =
            match WikiCompiler::new().compile_from_entities(&entity_data_list, &wiki_dir) {
                Ok(n) => {
                    info!(pages = n, "wiki pages compiled from state.db entities");
                    n
                }
                Err(e) => {
                    tracing::error!(error = %e, "WikiCompiler failed");
                    0
                }
            };

        let new_state = CompilerState {
            last_compile_time: Utc::now(),
        };
        if let Err(e) = new_state.save(&state_path) {
            tracing::error!(error = %e, "failed to save compiler state");
        }

        Ok(WorkerReport {
            worker_id: self.id().to_string(),
            success: true,
            fact_count: pages_written,
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_state_dir() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn test_state_default_is_epoch() {
        let state = CompilerState::new();
        assert_eq!(
            state.last_compile_time,
            DateTime::UNIX_EPOCH,
            "default state should be UNIX_EPOCH"
        );
    }

    #[test]
    fn test_state_roundtrip() {
        let dir = setup_state_dir();
        let path = dir.path().join(STATE_FILE);

        let now = Utc::now();
        let state = CompilerState {
            last_compile_time: now,
        };
        state.save(&path).unwrap();

        let loaded = CompilerState::load(&path);
        assert!(
            (loaded.last_compile_time - now).num_seconds().abs() <= 1,
            "loaded time should match saved time within 1 second"
        );
    }

    #[test]
    fn test_state_load_missing_file_returns_epoch() {
        let dir = setup_state_dir();
        let path = dir.path().join("nonexistent.json");
        let state = CompilerState::load(&path);
        assert_eq!(
            state.last_compile_time,
            DateTime::UNIX_EPOCH,
            "missing state file should return epoch"
        );
    }

    #[test]
    fn test_state_load_corrupted_file_returns_epoch() {
        let dir = setup_state_dir();
        let path = dir.path().join(STATE_FILE);
        std::fs::write(&path, "not json").unwrap();
        let state = CompilerState::load(&path);
        assert_eq!(
            state.last_compile_time,
            DateTime::UNIX_EPOCH,
            "corrupted state should return epoch"
        );
    }

    #[tokio::test]
    async fn test_build_entity_data_creates_synthetic_fact() {
        let entity = zen_vault::Entity {
            id: "test-1".to_string(),
            name: "Rust".to_string(),
            entity_type: zen_vault::EntityType::Technology,
            description: String::new(),
            source_note_id: "note-1".to_string(),
            created_at: DateTime::UNIX_EPOCH,
            last_updated: DateTime::UNIX_EPOCH,
            domain: Some("programming".to_string()),
            aliases: vec!["rust-lang".to_string(), "rs".to_string()],
            metadata: std::collections::HashMap::new(),
        };

        let svc = EntityService::new();
        let dir = setup_state_dir();
        let db_path = dir.path().join("state.db");
        let client = zen_vault::SqliteClient::open(&db_path).await.unwrap();

        let data = WikiCompilerWorker::build_entity_data(&svc, &client, &entity).await;

        assert!(!data.facts.is_empty(), "should have at least one fact");
        let fact = &data.facts[0];
        assert!(fact.contains("Rust"), "fact should contain entity name");
        assert!(
            fact.contains("technology"),
            "fact should contain entity type"
        );
        assert!(fact.contains("programming"), "fact should contain domain");
        assert!(
            fact.contains("1970-01-01"),
            "fact should contain first_seen date"
        );
    }

    #[tokio::test]
    async fn test_build_entity_data_without_domain() {
        let entity = zen_vault::Entity {
            id: "test-2".to_string(),
            name: "Python".to_string(),
            entity_type: zen_vault::EntityType::Technology,
            description: String::new(),
            source_note_id: "note-2".to_string(),
            created_at: DateTime::UNIX_EPOCH,
            last_updated: DateTime::UNIX_EPOCH,
            domain: None,
            aliases: Vec::new(),
            metadata: std::collections::HashMap::new(),
        };

        let svc = EntityService::new();
        let dir = setup_state_dir();
        let db_path = dir.path().join("state.db");
        let client = zen_vault::SqliteClient::open(&db_path).await.unwrap();

        let data = WikiCompilerWorker::build_entity_data(&svc, &client, &entity).await;

        assert!(!data.facts.is_empty(), "should have at least one fact");
        let fact = &data.facts[0];
        assert!(fact.contains("Python"), "fact should contain entity name");
        assert!(
            fact.contains("unknown"),
            "fact should say unknown domain when none exists"
        );
    }

    #[test]
    fn test_state_path_ends_with_state_file() {
        let path = PathBuf::from("/tmp/.zen");
        let state_path = WikiCompilerWorker::state_path(&path);
        assert!(
            state_path.ends_with(STATE_FILE),
            "state path should end with state file name"
        );
        assert!(
            state_path.to_string_lossy().contains("/db/"),
            "state path should be under db directory"
        );
    }
}
