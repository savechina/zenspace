//! Wiring layer — wires all existing Skills and Tools into rig-compose registries.
//!
//! Provides a single [`ZenWiring`] struct that creates and populates
//! `SkillRegistry`, `ToolRegistry`, and `DelegateRegistry` with all
//! existing implementations from `zen_vault`.
//!
//! When `ZenPaths::detect()` succeeds, `ZenWiring::new()` also auto-opens
//! a [`MemvidStore`] at `<memory>/mem1.mv2` for downstream consumers
//! (orchestrator, executor). Failure is non-fatal: wiring still constructs
//! with `memvid_store: None`.

use std::sync::Arc;

use async_trait::async_trait;
use rig_compose::context::InvestigationContext;
use rig_compose::delegate::DelegateRegistry;
use rig_compose::registry::{KernelError, SkillRegistry, ToolRegistry};
use rig_compose::skill::{Skill, SkillOutcome};
use rig_compose::tool::Tool;
use serde_json::Value;
use tracing::debug;
use zen_core::constants::MEMVID_STORE_FILE;
use zen_core::paths::ZenPaths;
use zen_memory::ZenMemvidStore;
use zen_vault::tools::{ZenTool, ZenToolError};

// Re-exports for consumers
pub use rig_compose::delegate::DelegateRegistry as _DelegateRegistry;
pub use rig_compose::registry::KernelError as _KernelError;
pub use rig_compose::registry::SkillRegistry as _SkillRegistry;
pub use rig_compose::registry::ToolRegistry as _ToolRegistry;

// ---------------------------------------------------------------------------
// Adapter: ZenTool → rig_compose::tool::Tool
// ---------------------------------------------------------------------------

/// Bridges `zen_vault::tools::ZenTool` (used by Tier2Search, Tier4Search,
/// ComputeEmbeddings) into `rig_compose::tool::Tool` so they can be registered
/// in the rig-compose `ToolRegistry`.
pub struct ZenToolToolAdapter<T: ZenTool> {
    inner: T,
}

impl<T: ZenTool> ZenToolToolAdapter<T> {
    pub fn new(inner: T) -> Self {
        Self { inner }
    }
}

fn zen_error_to_kernel(error: ZenToolError) -> KernelError {
    match error {
        ZenToolError::InvalidArgs(msg) => KernelError::InvalidArgument(msg),
        ZenToolError::ExecutionFailed(msg) => KernelError::ToolFailed(msg),
        ZenToolError::NotFound(msg) => KernelError::ToolNotFound(msg),
    }
}

#[async_trait]
impl<T: ZenTool + Send + Sync> Tool for ZenToolToolAdapter<T> {
    fn schema(&self) -> rig_compose::tool::ToolSchema {
        let s = self.inner.schema();
        rig_compose::tool::ToolSchema {
            name: s.name,
            description: s.description,
            args_schema: s.args_schema,
            result_schema: s.result_schema,
        }
    }

    async fn invoke(&self, args: Value) -> Result<Value, KernelError> {
        self.inner.invoke(args).await.map_err(zen_error_to_kernel)
    }
}

// ---------------------------------------------------------------------------
// Adapter: ConsolidationPipeline → rig_compose::skill::Skill
// ---------------------------------------------------------------------------

/// Wraps `zen_vault::ConsolidationPipeline` (which implements `Workflow`
/// but not `Skill`) into a `rig_compose::skill::Skill` so it can be registered
/// in the rig-compose `SkillRegistry`.
pub struct ConsolidationPipelineSkillAdapter;

#[async_trait]
impl Skill for ConsolidationPipelineSkillAdapter {
    fn id(&self) -> &str {
        "zen-consolidation-pipeline"
    }

    fn description(&self) -> &str {
        "Run the full consolidation pipeline: extract notions, compile wiki pages, detect contradictions"
    }

    fn applies(&self, ctx: &InvestigationContext) -> bool {
        ctx.evidence.iter().any(|ev| {
            ev.detail
                .get("inbox_dir")
                .and_then(|v| v.as_str())
                .is_some()
                && ev.detail.get("wiki_dir").and_then(|v| v.as_str()).is_some()
        })
    }

    async fn execute(
        &self,
        ctx: &mut InvestigationContext,
        _tools: &ToolRegistry,
    ) -> Result<SkillOutcome, KernelError> {
        let inbox_dir = ctx
            .evidence
            .iter()
            .filter_map(|ev| ev.detail.get("inbox_dir").and_then(|v| v.as_str()))
            .next()
            .map(std::path::PathBuf::from)
            .ok_or_else(|| {
                KernelError::InvalidArgument("missing inbox_dir in context".to_string())
            })?;

        let wiki_dir = ctx
            .evidence
            .iter()
            .filter_map(|ev| ev.detail.get("wiki_dir").and_then(|v| v.as_str()))
            .next()
            .map(std::path::PathBuf::from)
            .ok_or_else(|| {
                KernelError::InvalidArgument("missing wiki_dir in context".to_string())
            })?;

        let pipeline = zen_vault::ConsolidationPipeline::new();
        let report = pipeline
            .run(&inbox_dir, &wiki_dir)
            .map_err(|e| KernelError::SkillFailed(e.to_string()))?;

        ctx.evidence.push(
            rig_compose::context::Evidence::new(self.id(), "consolidation_pipeline_report")
                .with_detail(serde_json::json!({
                    "notes_processed": report.notes_processed,
                    "entities_extracted": report.entities_extracted,
                    "wiki_pages_created": report.wiki_pages_created,
                    "contradictions_found": report.contradictions_found,
                })),
        );

        let delta = if report.contradictions_found > 0 {
            (report.contradictions_found.min(5) as f32) * -0.02
        } else {
            0.0
        };

        Ok(SkillOutcome::noop().with_delta(delta))
    }
}

// ---------------------------------------------------------------------------
// ZenWiring
// ---------------------------------------------------------------------------

/// Central wiring struct that creates and populates rig-compose registries
/// with all existing skill and tool implementations.
pub struct ZenWiring {
    pub skills: SkillRegistry,
    pub tools: ToolRegistry,
    pub delegates: DelegateRegistry,
    pub memvid_store: Option<rig_memvid::MemvidStore>,
}

impl ZenWiring {
    /// Create a new `ZenWiring` with all skills and tools registered.
    ///
    /// When `ZenPaths::detect()` succeeds and `<memory>/memvid.db` can be
    /// opened, the resulting [`rig_memvid::MemvidStore`] is exposed via
    /// [`Self::memvid_store`] for downstream consumers (orchestrator,
    /// executor). Otherwise `memvid_store` is `None` and the registries
    /// are still usable.
    ///
    /// # Skills registered
    /// - `zen-wiki-compilation` → `WikiCompiler`
    /// - `zen-learning-loop` → `LearningLoop`
    /// - `zen-notion-extraction` → `NotionExtractor`
    /// - `zen-contradiction-detection` → `ContradictionDetector`
    /// - `zen-consolidation-pipeline` → `ConsolidationPipeline` (via adapter)
    ///
    /// # Tools registered
    /// - `tier2_search` → `Tier2Search` (via adapter)
    /// - `tier4_search` → `Tier4Search` (via adapter)
    /// - `compute_embeddings` → `ComputeEmbeddings` (via adapter)
    #[must_use]
    pub fn new() -> Self {
        let skills = SkillRegistry::new();
        let tools = ToolRegistry::new();
        let delegates = DelegateRegistry::new();

        skills.register(Arc::new(zen_vault::WikiCompiler::new()));
        skills.register(Arc::new(zen_vault::LearningLoop::new()));
        skills.register(Arc::new(zen_vault::NotionExtractor::new()));
        skills.register(Arc::new(zen_vault::ContradictionDetector::new()));
        skills.register(Arc::new(ConsolidationPipelineSkillAdapter));

        tools.register(Arc::new(ZenToolToolAdapter::new(zen_vault::Tier2Search)));
        tools.register(Arc::new(ZenToolToolAdapter::new(zen_vault::Tier4Search)));
        tools.register(Arc::new(ZenToolToolAdapter::new(
            zen_vault::ComputeEmbeddings,
        )));

        let memvid_store = Self::try_open_memvid_store();

        Self {
            skills,
            tools,
            delegates,
            memvid_store,
        }
    }

    fn try_open_memvid_store() -> Option<rig_memvid::MemvidStore> {
        let paths = ZenPaths::detect().ok()?;
        let store_path = paths.memory().join(MEMVID_STORE_FILE);

        if let Some(parent) = store_path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                debug!(path = %parent.display(), error = %e, "ZenWiring: failed to create memvid parent dir");
                return None;
            }
        }

        match ZenMemvidStore::new(store_path.clone()) {
            Ok(store) => {
                debug!(path = %store_path.display(), "ZenWiring: memvid store opened");
                Some(store.into_inner())
            }
            Err(e) => {
                debug!(path = %store_path.display(), error = %e, "ZenWiring: memvid store unavailable, running without persistent memory");
                None
            }
        }
    }
}

impl Default for ZenWiring {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zen_wiring_registers_all_skills() {
        let wiring = ZenWiring::new();
        assert_eq!(wiring.skills.len(), 5);

        assert!(wiring.skills.get("zen-wiki-compilation").is_ok());
        assert!(wiring.skills.get("zen-vault-learning-loop").is_ok());
        assert!(wiring.skills.get("zen-notion-extraction").is_ok());
        assert!(wiring.skills.get("zen-contradiction-detection").is_ok());
        assert!(wiring.skills.get("zen-consolidation-pipeline").is_ok());
    }

    #[test]
    fn zen_wiring_registers_all_tools() {
        let wiring = ZenWiring::new();
        assert_eq!(wiring.tools.len(), 3);

        assert!(wiring.tools.get("tier2_search").is_ok());
        assert!(wiring.tools.get("tier4_search").is_ok());
        assert!(wiring.tools.get("compute_embeddings").is_ok());
    }

    #[test]
    fn zen_wiring_delegates_is_empty() {
        let wiring = ZenWiring::new();
        assert!(wiring.delegates.is_empty());
    }

    #[test]
    fn zen_wiring_default_matches_new() {
        let wiring1 = ZenWiring::new();
        let wiring2 = ZenWiring::default();

        assert_eq!(wiring1.skills.len(), wiring2.skills.len());
        assert_eq!(wiring1.tools.len(), wiring2.tools.len());
        assert_eq!(wiring1.delegates.len(), wiring2.delegates.len());
    }

    #[test]
    fn skill_ideas_return_correct_ids() {
        let wiring = ZenWiring::new();

        let wiki = wiring.skills.get("zen-wiki-compilation").unwrap();
        assert_eq!(wiki.id(), "zen-wiki-compilation");

        let learning = wiring.skills.get("zen-vault-learning-loop").unwrap();
        assert_eq!(learning.id(), "zen-vault-learning-loop");

        let notion = wiring.skills.get("zen-notion-extraction").unwrap();
        assert_eq!(notion.id(), "zen-notion-extraction");

        let contradiction = wiring.skills.get("zen-contradiction-detection").unwrap();
        assert_eq!(contradiction.id(), "zen-contradiction-detection");

        let pipeline = wiring.skills.get("zen-consolidation-pipeline").unwrap();
        assert_eq!(pipeline.id(), "zen-consolidation-pipeline");
    }

    #[test]
    fn tool_schemas_have_correct_names() {
        let wiring = ZenWiring::new();

        let tier2 = wiring.tools.get("tier2_search").unwrap();
        assert_eq!(tier2.schema().name, "tier2_search");

        let tier4 = wiring.tools.get("tier4_search").unwrap();
        assert_eq!(tier4.schema().name, "tier4_search");

        let embeddings = wiring.tools.get("compute_embeddings").unwrap();
        assert_eq!(embeddings.schema().name, "compute_embeddings");
    }
}
