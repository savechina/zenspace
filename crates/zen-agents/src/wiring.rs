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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use rig_compose::budget::{AtomicBudget, DispatchBudgetHook};
use rig_compose::context::InvestigationContext;
use rig_compose::delegate::DelegateRegistry;
use rig_compose::normalizer::ToolDispatchHook;
use rig_compose::registry::{KernelError, SkillRegistry, ToolRegistry};
use rig_compose::skill::{Skill, SkillOutcome};
use rig_compose::tool::Tool;
use serde_json::Value;
use tracing::debug;
use zen_core::constants::MEMVID_STORE_FILE;
use zen_core::paths::ZenPaths;
use zen_core::sandbox::{
    SandboxMode, SandboxValidator, SeatbeltHook, SeatbeltPolicy, ToolArgRegistry,
    apply_resource_limits,
};
use zen_core::types::Sensitivity;
use zen_memory::ZenMemvidStore;
use zen_plugin::registry::PluginEntry;
use zen_plugin::{Plugin, PluginApi};
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
// Adapter: DistillationPipeline → rig_compose::skill::Skill
// ---------------------------------------------------------------------------

/// Wraps `zen_vault::DistillationPipeline` (which implements `Workflow`
/// but not `Skill`) into a `rig_compose::skill::Skill` so it can be registered
/// in the rig-compose `SkillRegistry`.
pub struct DistillationPipelineSkillAdapter;

#[async_trait]
impl Skill for DistillationPipelineSkillAdapter {
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

        let pipeline = zen_vault::DistillationPipeline::new();
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
// Plugin instantiation (FR-033)
// ---------------------------------------------------------------------------

/// Instantiate a plugin from a registry entry.
///
/// MVP scope: native (`.dylib`/`.so`) loading is not yet supported and
/// returns an error (logged, never fatal). `.wasm` entries are deferred to
/// the WASM executor integration — logged and skipped so a plugin directory
/// with wasm plugins does not block wiring construction.
fn instantiate_plugin(entry: &PluginEntry) -> Result<Option<Box<dyn Plugin>>, String> {
    let Some(entry_file) = &entry.manifest.entry else {
        return Err("plugin has no entry file".to_string());
    };

    if entry_file.ends_with(".wasm") {
        tracing::info!(
            plugin = %entry.manifest.id,
            "wasm plugin loading deferred to WASM executor integration"
        );
        return Ok(None);
    }

    Err("native plugin loading not yet supported".to_string())
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
    pub tool_sensitivity: HashMap<String, Sensitivity>,
    sandbox_mode: SandboxMode,
    sandbox_hooks: Vec<Box<dyn ToolDispatchHook>>,
    mcp_connected: Arc<AtomicBool>,
    confidentiality_sensitivity: Arc<Mutex<Sensitivity>>,
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
    /// - `zen-consolidation-pipeline` → `DistillationPipeline` (via adapter)
    ///
    /// # Tools registered
    /// - `tier2_search` → `Tier2Search` (via adapter)
    /// - `tier4_search` → `Tier4Search` (via adapter)
    /// - `compute_embeddings` → `ComputeEmbeddings` (via adapter)
    #[must_use]
    pub fn new() -> Self {
        let workspace_roots = ZenPaths::detect()
            .ok()
            .and_then(|p| p.workspace_root().cloned())
            .map(|w| vec![w])
            .unwrap_or_default();
        Self::with_sandbox_mode(SandboxMode::WorkspaceWrite, workspace_roots, None)
    }

    /// Create a `ZenWiring` with the given sandbox mode.
    ///
    /// The mode drives both the per-tool `SandboxValidator` (path checks
    /// inside each fs tool) and the dispatch-time sandbox hook pipeline
    /// (rate limit → seatbelt → audit → approval) that runs before every
    /// tool invocation. Default is [`SandboxMode::WorkspaceWrite`].
    #[must_use]
    pub fn with_sandbox_mode(
        mode: SandboxMode,
        workspace_roots: Vec<PathBuf>,
        plugins: Option<&zen_plugin::registry::PluginRegistry>,
    ) -> Self {
        // FR-038: Apply rlimits early so the agent process and all its
        // descendants (shell.exec subprocesses, MCP stdio children, WASM-
        // forked processes) are resource-capped. Defense-in-depth — a
        // failure to set limits is logged but does not abort construction.
        match apply_resource_limits() {
            Ok(()) => debug!("resource limits applied: NPROC=50, NOFILE=256, CORE=0"),
            Err(e) => tracing::warn!("failed to apply resource limits: {}", e),
        }

        let skills = SkillRegistry::new();
        let mut tools = ToolRegistry::new();
        let delegates = DelegateRegistry::new();

        skills.register(Arc::new(zen_vault::WikiCompiler::new()));
        skills.register(Arc::new(zen_vault::LearningLoop::new()));
        skills.register(Arc::new(zen_vault::NotionExtractor::new()));
        skills.register(Arc::new(zen_vault::ContradictionDetector::new()));
        skills.register(Arc::new(DistillationPipelineSkillAdapter));

        tools.register(Arc::new(ZenToolToolAdapter::new(zen_vault::Tier2Search)));
        tools.register(Arc::new(ZenToolToolAdapter::new(zen_vault::Tier4Search)));
        tools.register(Arc::new(ZenToolToolAdapter::new(
            zen_vault::ComputeEmbeddings,
        )));

        tools.register(Arc::new(zen_plugin::platform::health::HealthTool));
        tools.register(Arc::new(
            zen_plugin::platform::notifications::NotificationTool,
        ));
        tools.register(Arc::new(zen_plugin::platform::calendar::CalendarTool::new()));
        tools.register(Arc::new(zen_plugin::platform::daemon::DaemonTool::new()));
        tools.register(Arc::new(
            zen_plugin::platform::fs_watcher::FsWatcherTool::new(),
        ));
        tools.register(Arc::new(zen_plugin::wasm_sandbox::WasmSandboxTool::new()));

        let sandbox_validator = SandboxValidator::new(mode, workspace_roots.clone());

        tools.register(Arc::new(zen_plugin::tools::fs_read::FsReadTool::new(
            sandbox_validator.clone(),
        )));
        tools.register(Arc::new(zen_plugin::tools::fs_write::FsWriteTool::new(
            sandbox_validator.clone(),
        )));
        tools.register(Arc::new(zen_plugin::tools::fs_edit::FsEditTool::new(
            sandbox_validator.clone(),
        )));
        tools.register(Arc::new(zen_plugin::tools::fs_delete::FsDeleteTool::new(
            sandbox_validator.clone(),
            workspace_roots.first().cloned(),
        )));
        tools.register(Arc::new(zen_plugin::tools::fs_move::FsMoveTool::new(
            sandbox_validator.clone(),
        )));
        tools.register(Arc::new(zen_plugin::tools::fs_copy::FsCopyTool::new(
            sandbox_validator.clone(),
        )));
        tools.register(Arc::new(zen_plugin::tools::fs_list::FsListTool::new(
            sandbox_validator.clone(),
        )));
        tools.register(Arc::new(zen_plugin::tools::fs_grep::FsGrepTool::new(
            sandbox_validator.clone(),
        )));
        tools.register(Arc::new(zen_plugin::tools::fs_glob::FsGlobTool::new(
            sandbox_validator,
        )));

        tools.register(Arc::new(zen_plugin::tools::web_fetch::WebFetchTool::new()));

        tools.register(Arc::new(zen_plugin::tools::web_search::WebSearchTool::new()));

        tools.register(Arc::new(zen_plugin::tools::shell_exec::ShellExecTool::new(
            workspace_roots.first().cloned().unwrap_or_default(),
        )));

        let memvid_store = Self::try_open_memvid_store();

        let mut tool_sensitivity = HashMap::new();
        tool_sensitivity.insert("tier2_search".to_string(), Sensitivity::Private);
        tool_sensitivity.insert("tier4_search".to_string(), Sensitivity::Private);
        tool_sensitivity.insert("compute_embeddings".to_string(), Sensitivity::Private);
        tool_sensitivity.insert("system.health".to_string(), Sensitivity::Public);
        tool_sensitivity.insert("system.notifications".to_string(), Sensitivity::Public);
        tool_sensitivity.insert("system.calendar".to_string(), Sensitivity::Private);
        tool_sensitivity.insert("system.daemon".to_string(), Sensitivity::Private);
        tool_sensitivity.insert("system.fs_watcher".to_string(), Sensitivity::Private);
        tool_sensitivity.insert("plugin.wasm_sandbox".to_string(), Sensitivity::Private);
        tool_sensitivity.insert("fs.read".to_string(), Sensitivity::Public);
        tool_sensitivity.insert("fs.write".to_string(), Sensitivity::Private);
        tool_sensitivity.insert("fs.edit".to_string(), Sensitivity::Private);
        tool_sensitivity.insert("fs.delete".to_string(), Sensitivity::Private);
        tool_sensitivity.insert("fs.move".to_string(), Sensitivity::Private);
        tool_sensitivity.insert("fs.copy".to_string(), Sensitivity::Private);
        tool_sensitivity.insert("fs.list".to_string(), Sensitivity::Public);
        tool_sensitivity.insert("fs.grep".to_string(), Sensitivity::Public);
        tool_sensitivity.insert("shell.exec".to_string(), Sensitivity::Confidential);
        tool_sensitivity.insert("fs.glob".to_string(), Sensitivity::Public);
        tool_sensitivity.insert("web.fetch".to_string(), Sensitivity::Private);
        tool_sensitivity.insert("web.search".to_string(), Sensitivity::Private);

        let (mut sandbox_hooks, confidentiality_sensitivity) =
            Self::build_sandbox_hooks(mode, &workspace_roots);

        if let Some(registry) = plugins {
            let enabled: Vec<_> = registry.list_enabled().collect();
            let workspace_root = workspace_roots
                .first()
                .map(|p| p.as_path())
                .unwrap_or_else(|| std::path::Path::new(""));
            for entry in enabled {
                match instantiate_plugin(entry) {
                    Ok(Some(plugin)) => {
                        let mut api =
                            PluginApi::new(&mut tools, &mut sandbox_hooks, workspace_root);
                        match plugin.activate(&mut api) {
                            Ok(()) => {
                                tracing::info!(plugin = %entry.manifest.id, "plugin activated")
                            }
                            Err(e) => tracing::warn!(
                                plugin = %entry.manifest.id,
                                error = %e,
                                "plugin activate failed"
                            ),
                        }
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!(
                        plugin = %entry.manifest.id,
                        error = %e,
                        "plugin instantiation failed"
                    ),
                }
            }
        }

        Self {
            skills,
            tools,
            delegates,
            memvid_store,
            tool_sensitivity,
            sandbox_mode: mode,
            sandbox_hooks,
            mcp_connected: Arc::new(AtomicBool::new(false)),
            confidentiality_sensitivity,
        }
    }

    /// Build the per-tool arg-key registry for the seatbelt (FR-035).
    ///
    /// Tools whose sensitive args are not named `command` or `path` bypassed
    /// the seatbelt entirely before this registry existed. Each entry maps a
    /// tool name to the arg keys that carry paths or commands requiring
    /// validation.
    fn build_tool_arg_registry() -> ToolArgRegistry {
        let mut registry = ToolArgRegistry::new();

        registry.register_tool_args("system.daemon", &[], &["daemon_action", "action"]);
        registry.register_tool_args("shell.exec", &["cwd"], &["binary"]);
        registry.register_tool_args("plugin.wasm_sandbox", &["wasm_path"], &[]);

        // fs.* tools already match the default `path` fallback; register
        // explicitly so the seatbelt still inspects them if the fallback is
        // ever removed.
        let fs_path_arg = &["path"];
        for fs_tool in [
            "fs.read",
            "fs.write",
            "fs.edit",
            "fs.delete",
            "fs.move",
            "fs.copy",
            "fs.list",
            "fs.grep",
            "fs.glob",
        ] {
            registry.register_tool_args(fs_tool, fs_path_arg, &[]);
        }

        registry
    }

    /// Build the dispatch-time sandbox hook pipeline.
    ///
    /// Order matters: confidentiality gate blocks cloud tools in
    /// `Confidential` sessions first (FR-009), then rate limit reserves
    /// budget, the seatbelt validates the invocation path against the mode,
    /// audit records the invocation, and approval consults the interactive
    /// callback in `Ask` mode. A `Terminate` from any hook stops dispatch
    /// before the tool body runs.
    fn build_sandbox_hooks(
        mode: SandboxMode,
        workspace_roots: &[std::path::PathBuf],
    ) -> (Vec<Box<dyn ToolDispatchHook>>, Arc<Mutex<Sensitivity>>) {
        let budget = Arc::new(AtomicBudget::new(20));
        let policy = SeatbeltPolicy::new(mode, workspace_roots.to_vec())
            .with_timeout(300)
            .with_arg_registry(Self::build_tool_arg_registry());
        let audit_log_path = workspace_roots
            .first()
            .map(|root| root.join("logs").join("audit.jsonl"))
            .unwrap_or_else(|| std::path::PathBuf::from("logs/audit.jsonl"));

        let confidentiality =
            zen_plugin::tools::confidentiality_hook::ConfidentialityHook::new(Sensitivity::Private);
        let shared = confidentiality.shared_sensitivity();

        (
            vec![
                Box::new(confidentiality),
                Box::new(DispatchBudgetHook::new(budget, 1)),
                Box::new(SeatbeltHook::new(policy)),
                Box::new(zen_plugin::tools::audit_hook::ToolAuditHook::new(
                    audit_log_path,
                )),
                Box::new(zen_plugin::tools::approval_hook::AskApprovalHook::new(mode)),
            ],
            shared,
        )
    }

    fn try_open_memvid_store() -> Option<rig_memvid::MemvidStore> {
        let paths = ZenPaths::detect().ok()?;
        let store_path = paths.memory().join(MEMVID_STORE_FILE);

        if let Some(parent) = store_path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            debug!(path = %parent.display(), error = %e, "ZenWiring: failed to create memvid parent dir");
            return None;
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

    pub fn tool_sensitivity(&self, tool_name: &str) -> Sensitivity {
        self.tool_sensitivity
            .get(tool_name)
            .copied()
            .unwrap_or(Sensitivity::Private)
    }

    pub fn sandbox_mode(&self) -> SandboxMode {
        self.sandbox_mode
    }

    /// Set the session sensitivity consulted by the confidentiality gate.
    ///
    /// The orchestrator calls this once per session execution so the
    /// dispatch pipeline blocks cloud tools when the session is
    /// `Confidential` (FR-009).
    pub fn set_sensitivity(&self, sensitivity: Sensitivity) {
        if let Ok(mut guard) = self.confidentiality_sensitivity.lock() {
            *guard = sensitivity;
        }
    }

    /// Render a compact tool manifest for system-prompt injection.
    ///
    /// Lists every registered tool with its description and argument schema
    /// so the model knows which tools exist and how to invoke them via
    /// fenced-JSON tool calls.
    pub fn tool_manifest(&self) -> String {
        let mut lines = Vec::new();
        for schema in self.tools.schemas() {
            lines.push(format!(
                "- {}: {} (args: {})",
                schema.name, schema.description, schema.args_schema
            ));
        }
        lines.join("\n")
    }

    /// Dispatch-time sandbox hooks, in execution order.
    pub fn dispatch_hooks(&self) -> Vec<&dyn ToolDispatchHook> {
        self.sandbox_hooks
            .iter()
            .map(|hook| hook.as_ref() as &dyn ToolDispatchHook)
            .collect()
    }

    /// Register an interactive approval callback for `SandboxMode::Ask`.
    ///
    /// The hook pipeline holds the callback behind a trait object, so the
    /// setter is applied by rebuilding the approval hook. No-op outside
    /// `Ask` mode.
    pub fn set_approval_callback(&mut self, callback: zen_core::sandbox::ApprovalCallback) {
        if self.sandbox_mode == SandboxMode::Ask {
            let mut hook = zen_plugin::tools::approval_hook::AskApprovalHook::new(SandboxMode::Ask);
            hook.set_callback(callback);
            self.sandbox_hooks[4] = Box::new(hook);
        }
    }

    /// Connect stdio MCP servers once per wiring instance.
    ///
    /// Idempotent: repeated calls are no-ops after the first successful
    /// (or attempted) bootstrap. Failures are logged and never fatal —
    /// a broken MCP server must not prevent the session from starting.
    pub async fn connect_mcp_servers(&self) {
        if self.mcp_connected.load(Ordering::SeqCst) {
            return;
        }
        self.mcp_connected.store(true, Ordering::SeqCst);

        let config = match zen_core::config::load_config() {
            Ok(c) => c,
            Err(e) => {
                debug!(error = %e, "ZenWiring: config unavailable, skipping MCP client bootstrap");
                return;
            }
        };

        if config.mcp_servers.is_empty() {
            return;
        }

        let paths = match ZenPaths::detect() {
            Ok(p) => p,
            Err(_) => return,
        };

        let mut trust_store = match zen_core::config::McpTrustStore::load(&paths) {
            Ok(t) => t,
            Err(e) => {
                debug!(error = %e, "ZenWiring: MCP trust store unavailable");
                return;
            }
        };

        // The orchestrator is headless — pass `None` for the trust prompt so
        // untrusted servers are skipped (non-fatally) rather than blocking.
        zen_plugin::tools::mcp_client::bootstrap_mcp_clients(
            &self.tools,
            &config.mcp_servers,
            &mut trust_store,
            &paths,
            None,
        )
        .await;
    }

    /// Build the MCP-exposed tool registry (FR-016/FR-017).
    ///
    /// Exposes every registered tool whose sensitivity is `Public` or
    /// `Private`, and **excludes** tools tagged `Confidential` so external
    /// MCP clients cannot discover sensitive capabilities. Iterates the
    /// tool registry itself (not the sensitivity map) so that any newly
    /// registered tool defaults to the safe `Private` exposure level via
    /// [`Self::tool_sensitivity`].
    pub fn build_mcp_registry(&self) -> ToolRegistry {
        let filtered = ToolRegistry::new();
        for schema in self.tools.schemas() {
            if self.tool_sensitivity(&schema.name) != Sensitivity::Confidential
                && let Ok(tool) = self.tools.get(&schema.name)
            {
                filtered.register(tool);
            }
        }
        filtered
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
        assert_eq!(wiring.tools.len(), 21);

        assert!(wiring.tools.get("tier2_search").is_ok());
        assert!(wiring.tools.get("tier4_search").is_ok());
        assert!(wiring.tools.get("compute_embeddings").is_ok());
        assert!(wiring.tools.get("system.health").is_ok());
        assert!(wiring.tools.get("system.notifications").is_ok());
        assert!(wiring.tools.get("system.calendar").is_ok());
        assert!(wiring.tools.get("system.daemon").is_ok());
        assert!(wiring.tools.get("system.fs_watcher").is_ok());
        assert!(wiring.tools.get("plugin.wasm_sandbox").is_ok());
        assert!(wiring.tools.get("shell.exec").is_ok());
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

    #[test]
    fn build_mcp_registry_exposes_public_and_private_tools() {
        let wiring = ZenWiring::new();
        let registry = wiring.build_mcp_registry();

        assert!(registry.get("fs.read").is_ok());
        assert!(registry.get("system.health").is_ok());
        assert!(registry.get("web.search").is_ok());
        assert!(registry.get("fs.write").is_ok());
    }

    #[test]
    fn build_mcp_registry_excludes_confidential_tools() {
        let mut wiring = ZenWiring::new();
        wiring
            .tool_sensitivity
            .insert("web.search".to_string(), Sensitivity::Confidential);

        let registry = wiring.build_mcp_registry();
        assert!(
            registry.get("web.search").is_err(),
            "Confidential tool must be excluded from tools/list"
        );
        assert!(registry.get("fs.read").is_ok());
    }

    // ── D20: restored dispatch-hook wiring tests ─────────────────────────────
    //
    // These exercise the actual sandbox-hook pipeline assembled by
    // `build_sandbox_hooks` and the confidentiality/approval hooks it installs.
    // Each test drives `before_invocation` and asserts a concrete dispatch
    // decision, so they FAIL if a hook stops firing or is mis-ordered.

    fn make_invocation(name: &str) -> rig_compose::normalizer::ToolInvocation {
        rig_compose::normalizer::ToolInvocation {
            name: name.to_string(),
            args: serde_json::json!({}),
        }
    }

    #[test]
    fn build_sandbox_hooks_assembles_full_pipeline() {
        let (hooks, sensitivity) =
            ZenWiring::build_sandbox_hooks(SandboxMode::Ask, &[std::path::PathBuf::from("/ws")]);
        assert_eq!(
            hooks.len(),
            5,
            "pipeline must be: confidentiality, budget, seatbelt, audit, approval"
        );
        // Shared sensitivity arc is live and mutable (orchestrator updates it).
        {
            let mut guard = sensitivity.lock().unwrap();
            *guard = Sensitivity::Confidential;
        }
        assert_eq!(*sensitivity.lock().unwrap(), Sensitivity::Confidential);
    }

    #[tokio::test]
    async fn confidentiality_hook_blocks_cloud_in_confidential_session() {
        let hook = zen_plugin::tools::confidentiality_hook::ConfidentialityHook::new(
            Sensitivity::Confidential,
        );
        let inv = make_invocation("web.search");
        match hook.before_invocation(&inv).await.unwrap() {
            rig_compose::normalizer::ToolDispatchAction::Skip { output, .. } => {
                assert!(
                    output["error"].is_string(),
                    "Skip must carry an error payload"
                );
            }
            other => panic!("web.search under Confidential must Skip, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn confidentiality_hook_allows_local_in_confidential_session() {
        let hook = zen_plugin::tools::confidentiality_hook::ConfidentialityHook::new(
            Sensitivity::Confidential,
        );
        let inv = make_invocation("fs.read");
        assert!(matches!(
            hook.before_invocation(&inv).await.unwrap(),
            rig_compose::normalizer::ToolDispatchAction::Continue
        ));
    }

    #[tokio::test]
    async fn approval_callback_gates_mutating_tool_in_ask_mode() {
        use zen_core::sandbox::{ApprovalCallback, ApprovalDecision};

        let cb: ApprovalCallback = Arc::new(|_inv| ApprovalDecision::Deny);
        let hook = zen_plugin::tools::approval_hook::AskApprovalHook::new(SandboxMode::Ask)
            .with_callback(cb);

        let mutating = make_invocation("fs.write");
        assert!(matches!(
            hook.before_invocation(&mutating).await.unwrap(),
            rig_compose::normalizer::ToolDispatchAction::Terminate { .. }
        ));

        let readonly = make_invocation("fs.read");
        assert!(matches!(
            hook.before_invocation(&readonly).await.unwrap(),
            rig_compose::normalizer::ToolDispatchAction::Continue
        ));
    }

    #[tokio::test]
    async fn ask_mode_without_callback_is_direct_for_mutating_tools() {
        let hook = zen_plugin::tools::approval_hook::AskApprovalHook::new(SandboxMode::Ask);
        let inv = make_invocation("fs.write");
        assert!(matches!(
            hook.before_invocation(&inv).await.unwrap(),
            rig_compose::normalizer::ToolDispatchAction::Continue
        ));
    }

    #[test]
    fn set_approval_callback_swaps_hook_in_ask_mode() {
        use zen_core::sandbox::{ApprovalCallback, ApprovalDecision};
        let mut wiring = ZenWiring::with_sandbox_mode(SandboxMode::Ask, Vec::new(), None);
        let cb: ApprovalCallback = Arc::new(|_inv| ApprovalDecision::Allow);
        wiring.set_approval_callback(cb);
        // Index 4 is the approval hook slot (see build_sandbox_hooks order).
        assert_eq!(wiring.sandbox_hooks.len(), 5);
    }

    #[test]
    fn with_sandbox_mode_isolates_plugin_failures() {
        use zen_plugin::registry::{Manifest, PluginKind, PluginRegistry};

        let mut registry =
            PluginRegistry::with_plugin_dir(std::path::PathBuf::from("/nonexistent"));
        // A `.wasm` entry that does not exist on disk → deferred, no panic.
        registry
            .register(PluginEntry::new(
                Manifest {
                    id: "bogus_wasm".to_string(),
                    name: "Bogus Wasm".to_string(),
                    version: "0.1.0".to_string(),
                    kind: PluginKind::Tool,
                    permissions: vec![],
                    config_schema: None,
                    entry: Some("missing.wasm".to_string()),
                    sha256: None,
                },
                std::path::PathBuf::from("/nonexistent"),
            ))
            .unwrap();
        // A native entry → instantiation error, logged, no panic.
        registry
            .register(PluginEntry::new(
                Manifest {
                    id: "bogus_native".to_string(),
                    name: "Bogus Native".to_string(),
                    version: "0.1.0".to_string(),
                    kind: PluginKind::Tool,
                    permissions: vec![],
                    config_schema: None,
                    entry: Some("missing.dylib".to_string()),
                    sha256: None,
                },
                std::path::PathBuf::from("/nonexistent"),
            ))
            .unwrap();

        let wiring = ZenWiring::with_sandbox_mode(
            SandboxMode::WorkspaceWrite,
            vec![std::path::PathBuf::from("/ws")],
            Some(&registry),
        );
        // Failure is isolated: wiring still constructs with all builtin tools.
        assert_eq!(wiring.tools.len(), 21);
    }

    // ── FR-035: arg-registry end-to-end via ZenWiring hook pipeline ─────────

    #[tokio::test]
    async fn seatbelt_via_wiring_blocks_dangerous_daemon_action() {
        // Confirms build_tool_arg_registry is wired into the seatbelt hook
        // assembled by build_sandbox_hooks (catches regressions where the
        // registry is built but not attached).
        let (hooks, _sensitivity) = ZenWiring::build_sandbox_hooks(
            SandboxMode::WorkspaceWrite,
            &[std::path::PathBuf::from("/ws")],
        );

        // hooks[2] is the seatbelt (see build_sandbox_hooks order).
        let seatbelt = &hooks[2];
        let inv = rig_compose::normalizer::ToolInvocation {
            name: "system.daemon".to_string(),
            args: serde_json::json!({
                "action": "sudo systemctl stop sshd",
                "name": "sshd",
            }),
        };
        let action = seatbelt.before_invocation(&inv).await.unwrap();
        assert!(
            matches!(
                action,
                rig_compose::normalizer::ToolDispatchAction::Terminate { .. }
            ),
            "dangerous daemon action must be blocked via wired registry, got {action:?}"
        );
    }

    #[tokio::test]
    async fn seatbelt_via_wiring_allows_benign_daemon_status() {
        let (hooks, _sensitivity) = ZenWiring::build_sandbox_hooks(
            SandboxMode::WorkspaceWrite,
            &[std::path::PathBuf::from("/ws")],
        );
        let seatbelt = &hooks[2];
        let inv = rig_compose::normalizer::ToolInvocation {
            name: "system.daemon".to_string(),
            args: serde_json::json!({
                "action": "status",
                "name": "nginx",
            }),
        };
        let action = seatbelt.before_invocation(&inv).await.unwrap();
        assert!(
            matches!(
                action,
                rig_compose::normalizer::ToolDispatchAction::Continue
            ),
            "benign daemon status must continue, got {action:?}"
        );
    }

    #[tokio::test]
    async fn seatbelt_via_wiring_blocks_wasm_sandbox_metadata_path() {
        let (hooks, _sensitivity) = ZenWiring::build_sandbox_hooks(
            SandboxMode::WorkspaceWrite,
            &[std::path::PathBuf::from("/ws")],
        );
        let seatbelt = &hooks[2];
        let inv = rig_compose::normalizer::ToolInvocation {
            name: "plugin.wasm_sandbox".to_string(),
            args: serde_json::json!({
                "wasm_path": "/ws/.ssh/evil.wasm",
                "operation": "write",
            }),
        };
        let action = seatbelt.before_invocation(&inv).await.unwrap();
        assert!(
            matches!(
                action,
                rig_compose::normalizer::ToolDispatchAction::Terminate { .. }
            ),
            "write to metadata path via wasm_path must be blocked, got {action:?}"
        );
    }
}
