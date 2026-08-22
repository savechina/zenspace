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
#[cfg(not(test))]
use zen_core::sandbox::apply_resource_limits;
use zen_core::sandbox::{
    SandboxMode, SandboxValidator, SeatbeltHook, SeatbeltPolicy, ToolArgRegistry,
};
use zen_core::types::Sensitivity;
use zen_memory::ZenMemvidStore;
use zen_plugin::registry::{Lifecycle, PluginEntry};
use zen_plugin::wasm_sandbox::{WasmPermissions, WasmSandbox};
use zen_plugin::{Plugin, PluginApi, WasmPlugin};
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

/// Instantiate a plugin from a registry entry (T089).
///
/// `.wasm` entries load through [`WasmPlugin::from_entry`] under the
/// wiring's WASM permission policy. Native (`.dylib`/`.so`) loading is
/// out of scope and returns an error (logged, never fatal).
fn instantiate_plugin(
    entry: &PluginEntry,
    policy: &WasmPermissions,
) -> Result<Option<Box<dyn Plugin>>, String> {
    let Some(entry_file) = &entry.manifest.entry else {
        return Err("plugin has no entry file".to_string());
    };

    if entry_file.ends_with(".wasm") {
        return WasmPlugin::from_entry(entry, policy).map(|p| Some(Box::new(p) as Box<dyn Plugin>));
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
        //
        // Test builds skip this: on Linux, RLIMIT_NPROC also constrains
        // pthread_create, and nextest runs hundreds of test processes in
        // parallel under one UID — a NPROC=50 soft cap makes thread spawn
        // fail with EAGAIN ("Resource temporarily unavailable") once the
        // user's aggregate task count exceeds 50. This is process hardening
        // for the production binary only, never for the test harness.
        #[cfg(not(test))]
        match apply_resource_limits() {
            Ok(()) => debug!("resource limits applied: NPROC=50, NOFILE=256, CORE=0"),
            Err(e) => tracing::warn!("failed to apply resource limits: {}", e),
        }

        // T091: `[sandbox.wasm]` permission policy (deny-all by default).
        // Shared by the `plugin.wasm_sandbox` tool and every `.wasm` plugin.
        let wasm_policy = Self::load_wasm_policy();

        let skills = SkillRegistry::new();
        let mut tools = ToolRegistry::new();
        let delegates = DelegateRegistry::new();

        skills.register(Arc::new(zen_vault::WikiCompiler::new()));
        skills.register(Arc::new(zen_vault::LearningLoop::new()));
        skills.register(Arc::new(zen_vault::NotionExtractor::new()));
        skills.register(Arc::new(zen_vault::ContradictionDetector::new()));
        skills.register(Arc::new(DistillationPipelineSkillAdapter));

        // D7 (2026-08-19 review): KB tools share one workspace-resolved
        // SqliteClient. The old unit-struct ZenTool impls opened
        // "./state.db" relative to the process CWD — production-broken.
        // Fail-soft: without a detectable workspace the KB tools are
        // skipped (warn) rather than registered against a wrong-path DB.
        match zen_core::paths::ZenPaths::detect() {
            Ok(paths) => {
                let kb_db = zen_vault::SharedSqliteClient::new(paths.data().join("state.db"));
                tools.register(Arc::new(ZenToolToolAdapter::new(
                    zen_vault::Tier2SearchTool::new(kb_db.clone()),
                )));
                tools.register(Arc::new(ZenToolToolAdapter::new(
                    zen_vault::Tier4SearchTool::new(kb_db),
                )));
            }
            Err(e) => {
                tracing::warn!(
                    "workspace not detected ({}): tier2_search/tier4_search not registered",
                    e
                );
            }
        }
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
        tools.register(Arc::new(
            zen_plugin::wasm_sandbox::WasmSandboxTool::with_sandbox(
                WasmSandbox::new().with_policy(wasm_policy.clone()),
            ),
        ));

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
            sandbox_validator.clone(),
        )));

        tools.register(Arc::new(zen_plugin::tools::web_fetch::WebFetchTool::new()));

        tools.register(Arc::new(zen_plugin::tools::web_search::WebSearchTool::new()));

        tools.register(Arc::new(zen_plugin::tools::shell_exec::ShellExecTool::new(
            workspace_roots.first().cloned().unwrap_or_default(),
            sandbox_validator.clone(),
            zen_core::sandbox::OsSandboxProfile::from_mode(
                mode,
                workspace_roots.clone(),
                zen_core::config::load_config()
                    .map(|c| c.sandbox.network_access)
                    .unwrap_or(false),
            ),
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

        // T099/FR-050b: plugin tools must not spoof builtin names — every
        // PluginApi gets the builtin tool list for the collision guard.
        let builtin_tool_names: Vec<String> = tools
            .schemas()
            .iter()
            .map(|schema| schema.name.clone())
            .collect();

        // T101–T103/FR-048: hook-isolation config — the sensitivity table
        // that keeps Confidential invocations (shell.exec) invisible to
        // plugin hooks, and the audit log shared with the builtin audit
        // hook so denial records land in the same audit trail.
        let plugin_hook_sensitivity: Arc<HashMap<String, Sensitivity>> =
            Arc::new(tool_sensitivity.clone());
        let plugin_audit_log_path = Self::audit_log_path(&workspace_roots);

        // T088: resolve the plugin registry — self-discover from
        // `[plugin] base_path` when none is passed (production path);
        // snapshot the caller's registry otherwise so lifecycle
        // transitions can be recorded without mutating the input.
        let mut plugin_registry = match plugins {
            Some(registry) => Self::snapshot_plugin_registry(registry),
            None => Self::discover_plugin_registry(),
        };

        // Entries marked Failed by discovery-time integrity verification
        // (FR-043 sha256 mismatch) must never activate, even though their
        // `enabled` flag is still set.
        let enabled: Vec<PluginEntry> = plugin_registry
            .list_enabled()
            .filter(|entry| entry.lifecycle != Lifecycle::Failed)
            .cloned()
            .collect();
        let workspace_root = workspace_roots
            .first()
            .map(|p| p.as_path())
            .unwrap_or_else(|| std::path::Path::new(""));
        for entry in enabled {
            let id = entry.manifest.id.clone();
            match instantiate_plugin(&entry, &wasm_policy) {
                Ok(Some(plugin)) => {
                    let mut api =
                        PluginApi::new(&id, &mut tools, &mut sandbox_hooks, workspace_root)
                            .with_builtin_tool_names(builtin_tool_names.clone())
                            .with_isolation(
                                Arc::clone(&plugin_hook_sensitivity),
                                plugin_audit_log_path.clone(),
                            );
                    match plugin.activate(&mut api) {
                        Ok(()) => {
                            tracing::info!(plugin = %id, "plugin activated");
                            let _ = plugin_registry.set_lifecycle(&id, Lifecycle::Running);
                        }
                        Err(e) => {
                            tracing::warn!(plugin = %id, error = %e, "plugin activate failed");
                            let _ = plugin_registry.set_lifecycle(&id, Lifecycle::Failed);
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(plugin = %id, error = %e, "plugin instantiation failed");
                    let _ = plugin_registry.set_lifecycle(&id, Lifecycle::Failed);
                }
            }
        }

        // Plugin-registered tools default to Private sensitivity so the MCP
        // exposure filter (build_mcp_registry) treats them safely.
        for schema in tools.schemas() {
            tool_sensitivity
                .entry(schema.name)
                .or_insert(Sensitivity::Private);
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

    /// Load the `[sandbox.wasm]` permission policy from the merged zen
    /// config (T091). Absent section → deny-all (preserves pre-config
    /// behavior); config load failure is non-fatal and also deny-all.
    fn load_wasm_policy() -> WasmPermissions {
        match zen_core::config::load_config() {
            Ok(config) => {
                let wasm = &config.sandbox.wasm;
                WasmPermissions {
                    allow_filesystem_read: wasm.allow_filesystem_read,
                    allow_filesystem_write: wasm.allow_filesystem_write,
                    allow_network: wasm.allow_network,
                    allow_system: wasm.allow_system,
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "ZenWiring: config unavailable, using deny-all WASM policy"
                );
                WasmPermissions::default()
            }
        }
    }

    /// Self-discover the plugin registry at construction time (T088).
    /// Prefers `[plugin] base_path` from the loaded config (tilde-expanded),
    /// falling back to `PluginRegistry::new()`'s default location.
    fn discover_plugin_registry() -> zen_plugin::registry::PluginRegistry {
        let plugin_dir = zen_core::config::load_config()
            .ok()
            .and_then(|config| config.plugin.resolved_base_path());
        Self::discover_plugins_in_dir(plugin_dir)
    }

    /// Discover plugins in `dir` (`None` → default plugin dir). Discovery
    /// failures are logged at warn and never fatal — wiring construction
    /// continues with whatever was discovered (possibly nothing).
    fn discover_plugins_in_dir(dir: Option<PathBuf>) -> zen_plugin::registry::PluginRegistry {
        use zen_plugin::registry::PluginRegistry;

        let mut registry = match dir {
            Some(dir) => PluginRegistry::with_plugin_dir(dir),
            None => PluginRegistry::new(),
        };

        if let Err(e) = registry.discover() {
            tracing::warn!(
                error = %e,
                "ZenWiring: plugin discovery failed, continuing without plugins"
            );
        }
        registry
    }

    /// Copy a caller-supplied registry into an owned one so the activation
    /// loop can record lifecycle transitions without mutating the input.
    fn snapshot_plugin_registry(
        registry: &zen_plugin::registry::PluginRegistry,
    ) -> zen_plugin::registry::PluginRegistry {
        use zen_plugin::registry::PluginRegistry;

        let mut snapshot = PluginRegistry::with_plugin_dir(registry.plugin_dir().clone());
        for entry in registry.list() {
            // Duplicate ids cannot occur: the source is keyed by plugin id.
            let _ = snapshot.register(entry.clone());
        }
        snapshot
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

    /// Resolve the dispatch audit log path for a workspace scope. Shared
    /// by the builtin audit hook and the FR-048 plugin-hook isolation
    /// adapter so denial records land in the same audit trail.
    fn audit_log_path(workspace_roots: &[std::path::PathBuf]) -> std::path::PathBuf {
        workspace_roots
            .first()
            .map(|root| root.join("logs").join("audit.jsonl"))
            .unwrap_or_else(|| std::path::PathBuf::from("logs/audit.jsonl"))
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
        let audit_log_path = Self::audit_log_path(workspace_roots);

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

    fn write_wasm_plugin(dir: &std::path::Path, id: &str) {
        use sha2::{Digest, Sha256};

        let plugin_dir = dir.join(id);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let wasm =
            wat::parse_str(r#"(module (func (export "ping")) (func (export "_start")))"#).unwrap();
        std::fs::write(plugin_dir.join("plugin.wasm"), &wasm).unwrap();

        let mut hasher = Sha256::new();
        hasher.update(&wasm);
        let sha256: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            format!(
                "id = \"{id}\"\nname = \"Demo\"\nversion = \"0.1.0\"\ntype = \"tool\"\n\
                 permissions = []\nentry = \"plugin.wasm\"\nsha256 = \"{sha256}\"\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn with_sandbox_mode_loads_discovered_wasm_plugin_as_namespaced_tool() {
        let dir = tempfile::tempdir().unwrap();
        write_wasm_plugin(dir.path(), "demo_plugin");

        let mut registry =
            zen_plugin::registry::PluginRegistry::with_plugin_dir(dir.path().to_path_buf());
        registry.discover().unwrap();

        let wiring = ZenWiring::with_sandbox_mode(
            SandboxMode::WorkspaceWrite,
            vec![dir.path().to_path_buf()],
            Some(&registry),
        );

        // Exported func registered under {plugin_id}.{func}; _start excluded.
        assert!(wiring.tools.get("demo_plugin.ping").is_ok());
        assert!(wiring.tools.get("demo_plugin._start").is_err());
        assert_eq!(wiring.tools.len(), 22);

        // Plugin tools default to Private sensitivity and stay MCP-exposed
        // (non-Confidential) per build_mcp_registry filtering.
        assert_eq!(
            wiring.tool_sensitivity("demo_plugin.ping"),
            Sensitivity::Private
        );
        assert!(wiring.build_mcp_registry().get("demo_plugin.ping").is_ok());
    }

    #[test]
    fn with_sandbox_mode_self_discovers_when_no_registry_passed() {
        // T088: None → wiring self-discovers from the configured plugin dir
        // (empty on this machine). An empty/missing dir must not break
        // construction — discovery is never fatal.
        let wiring = ZenWiring::with_sandbox_mode(SandboxMode::WorkspaceWrite, Vec::new(), None);
        assert!(wiring.tools.get("plugin.wasm_sandbox").is_ok());
        assert!(wiring.tools.get("fs.read").is_ok());
    }

    #[test]
    fn discover_plugins_in_dir_failure_is_nonfatal() {
        // A plugin dir path occupied by a regular file makes discover()
        // fail; the helper must return an empty registry without panicking.
        let file = tempfile::NamedTempFile::new().unwrap();
        let registry = ZenWiring::discover_plugins_in_dir(Some(file.path().to_path_buf()));
        assert_eq!(registry.count(), 0);
    }

    // ── SC-013 (FR-033 completion gate): drop-in plugin → discovered →
    //    namespaced tool → callable through the wiring ToolRegistry ────────

    #[tokio::test]
    async fn sc013_echo_plugin_discovered_and_callable() {
        use sha2::{Digest, Sha256};
        use zen_plugin::registry::Lifecycle;

        let dir = tempfile::tempdir().unwrap();
        let echo_dir = dir.path().join("echo");
        std::fs::create_dir_all(&echo_dir).unwrap();
        let wasm = wat::parse_str(r#"(module (func (export "hello")))"#).unwrap();
        std::fs::write(echo_dir.join("echo.wasm"), &wasm).unwrap();

        let mut hasher = Sha256::new();
        hasher.update(&wasm);
        let sha256: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect();

        std::fs::write(
            echo_dir.join("manifest.toml"),
            format!(
                "id = \"echo\"\nname = \"Echo\"\nversion = \"0.1.0\"\ntype = \"tool\"\n\
                 permissions = []\nentry = \"echo.wasm\"\nsha256 = \"{sha256}\"\n"
            ),
        )
        .unwrap();

        // Failure-isolation fixture: same valid wasm, sha256 that cannot match.
        let bogus_dir = dir.path().join("bogus");
        std::fs::create_dir_all(&bogus_dir).unwrap();
        std::fs::write(bogus_dir.join("bogus.wasm"), &wasm).unwrap();
        std::fs::write(
            bogus_dir.join("manifest.toml"),
            "id = \"bogus\"\nname = \"Bogus\"\nversion = \"0.1.0\"\ntype = \"tool\"\n\
             permissions = []\nentry = \"bogus.wasm\"\nsha256 = \"deadbeef\"\n",
        )
        .unwrap();

        let mut registry =
            zen_plugin::registry::PluginRegistry::with_plugin_dir(dir.path().to_path_buf());
        assert_eq!(
            registry.discover().unwrap(),
            1,
            "integrity-failed plugins are not counted"
        );
        assert_ne!(
            registry.get("echo").unwrap().lifecycle,
            Lifecycle::Failed,
            "echo integrity must pass"
        );
        assert_eq!(
            registry.get("bogus").unwrap().lifecycle,
            Lifecycle::Failed,
            "sha256 mismatch must mark the plugin Failed"
        );

        let wiring = ZenWiring::with_sandbox_mode(
            SandboxMode::WorkspaceWrite,
            vec![dir.path().to_path_buf()],
            Some(&registry),
        );

        let tool = wiring
            .tools
            .get("echo.hello")
            .expect("echo.hello must be registered as a namespaced tool");
        let result = tool.invoke(serde_json::json!({})).await.unwrap();
        assert_eq!(result["output"]["exit_code"], 0);
        assert_eq!(result["metrics"]["plugin"], "echo");
        assert_eq!(result["metrics"]["func_name"], "hello");

        // Integrity-failed plugin is isolated: no tools, echo unaffected.
        assert!(wiring.tools.get("bogus.hello").is_err());
        assert_eq!(wiring.tools.len(), 22, "21 builtin tools + echo.hello");
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
    async fn seatbelt_via_wiring_blocks_shell_exec_network_binary() {
        // CHK024 fail-path assertion: sandbox-exec/bubblewrap is absent in
        // v0.0.6, so the seatbelt hook (not an OS sandbox) enforces fail-
        // closed — a blocked network binary in `shell.exec` must be
        // terminated pre-dispatch, never allowed to spawn unsandboxed.
        let (hooks, _sensitivity) = ZenWiring::build_sandbox_hooks(
            SandboxMode::WorkspaceWrite,
            &[std::path::PathBuf::from("/ws")],
        );
        let seatbelt = &hooks[2];
        let inv = rig_compose::normalizer::ToolInvocation {
            name: "shell.exec".to_string(),
            args: serde_json::json!({
                "binary": "curl",
                "args": ["http://169.254.169.254/latest/meta-data/"],
                "cwd": "/ws",
            }),
        };
        let action = seatbelt.before_invocation(&inv).await.unwrap();
        assert!(
            matches!(
                action,
                rig_compose::normalizer::ToolDispatchAction::Terminate { .. }
            ),
            "shell.exec network binary must be blocked pre-dispatch, got {action:?}"
        );
    }

    #[tokio::test]
    async fn seatbelt_via_wiring_allows_benign_shell_exec_binary() {
        let (hooks, _sensitivity) = ZenWiring::build_sandbox_hooks(
            SandboxMode::WorkspaceWrite,
            &[std::path::PathBuf::from("/ws")],
        );
        let seatbelt = &hooks[2];
        let inv = rig_compose::normalizer::ToolInvocation {
            name: "shell.exec".to_string(),
            args: serde_json::json!({
                "binary": "/usr/bin/git",
                "args": ["status"],
                "cwd": "/ws",
            }),
        };
        let action = seatbelt.before_invocation(&inv).await.unwrap();
        assert!(
            matches!(
                action,
                rig_compose::normalizer::ToolDispatchAction::Continue
            ),
            "benign shell.exec binary must continue, got {action:?}"
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

    // ── FR-048 (Lane C): plugin hook isolation through the real pipeline ──
    //
    // These build the exact assembly `with_sandbox_mode` produces — the
    // 5-hook builtin pipeline plus a plugin hook registered through
    // `PluginApi` with wiring-injected isolation config — and drive a full
    // dispatch round through `rig_compose`'s hook runner.

    struct ProbeTool(&'static str);

    #[async_trait]
    impl Tool for ProbeTool {
        fn schema(&self) -> rig_compose::tool::ToolSchema {
            rig_compose::tool::ToolSchema {
                name: self.0.to_string(),
                description: self.0.to_string(),
                args_schema: serde_json::json!({}),
                result_schema: serde_json::json!({}),
            }
        }

        async fn invoke(&self, _args: Value) -> Result<Value, KernelError> {
            Ok(serde_json::json!({ "ran": self.0 }))
        }
    }

    /// Plugin hook fixture that errors on `deny_target` (T109).
    struct GlitchyHook {
        deny_target: String,
    }

    #[async_trait]
    impl ToolDispatchHook for GlitchyHook {
        async fn before_invocation(
            &self,
            invocation: &rig_compose::normalizer::ToolInvocation,
        ) -> Result<rig_compose::normalizer::ToolDispatchAction, KernelError> {
            if invocation.name == self.deny_target {
                return Err(KernelError::ToolFailed("plugin hook exploded".to_string()));
            }
            Ok(rig_compose::normalizer::ToolDispatchAction::Continue)
        }
    }

    /// Plugin hook fixture that records every observation (Confidential
    /// invisibility spy).
    struct ObservingHook {
        seen: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl ToolDispatchHook for ObservingHook {
        async fn before_invocation(
            &self,
            invocation: &rig_compose::normalizer::ToolInvocation,
        ) -> Result<rig_compose::normalizer::ToolDispatchAction, KernelError> {
            self.seen
                .lock()
                .unwrap()
                .push(format!("before:{}", invocation.name));
            Ok(rig_compose::normalizer::ToolDispatchAction::Continue)
        }

        async fn after_invocation(
            &self,
            result: &rig_compose::normalizer::ToolInvocationResult,
        ) -> Result<(), KernelError> {
            self.seen
                .lock()
                .unwrap()
                .push(format!("after:{}", result.invocation.name));
            Ok(())
        }
    }

    fn wiring_style_sensitivity() -> Arc<HashMap<String, Sensitivity>> {
        // Mirrors the entries wiring injects for the tools under test.
        let mut map = HashMap::new();
        map.insert("shell.exec".to_string(), Sensitivity::Confidential);
        map.insert("fs.read".to_string(), Sensitivity::Public);
        Arc::new(map)
    }

    fn hook_refs<'h>(
        builtin: &'h [Box<dyn ToolDispatchHook>],
        plugin: &'h [Box<dyn ToolDispatchHook>],
    ) -> Vec<&'h dyn ToolDispatchHook> {
        builtin
            .iter()
            .chain(plugin.iter())
            .map(|hook| hook.as_ref() as &dyn ToolDispatchHook)
            .collect()
    }

    #[tokio::test]
    async fn t109_plugin_hook_error_denies_only_its_invocation() {
        let dir = tempfile::tempdir().unwrap();
        let audit = ZenWiring::audit_log_path(&[dir.path().to_path_buf()]);
        let (sandbox_hooks, _sensitivity) = ZenWiring::build_sandbox_hooks(
            SandboxMode::WorkspaceWrite,
            &[dir.path().to_path_buf()],
        );

        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(ProbeTool("alpha.echo")));
        tools.register(Arc::new(ProbeTool("beta.echo")));

        let mut plugin_hooks: Vec<Box<dyn ToolDispatchHook>> = Vec::new();
        let mut api = PluginApi::new("glitchy", &mut tools, &mut plugin_hooks, dir.path())
            .with_isolation(wiring_style_sensitivity(), audit.clone());
        api.register_hook(Box::new(GlitchyHook {
            deny_target: "alpha.echo".to_string(),
        }));

        let invocations = vec![
            rig_compose::normalizer::ToolInvocation {
                name: "alpha.echo".to_string(),
                args: serde_json::json!({}),
            },
            rig_compose::normalizer::ToolInvocation {
                name: "beta.echo".to_string(),
                args: serde_json::json!({}),
            },
        ];
        let all_hooks = hook_refs(&sandbox_hooks, &plugin_hooks);
        let results = rig_compose::normalizer::dispatch_tool_invocations_with_hooks(
            &tools,
            &invocations,
            &all_hooks,
        )
        .await
        .expect("round must not abort on plugin hook Err (FR-048a)");

        assert_eq!(results.len(), 2);
        // Denying hook: THIS invocation is denied fail-closed...
        assert!(
            results[0].output["error"]
                .as_str()
                .is_some_and(|msg| msg.contains("denied by plugin hook 'glitchy'")),
            "denied output: {}",
            results[0].output
        );
        // ...while the sibling in the same round executed for real.
        assert_eq!(results[1].output["ran"], "beta.echo");

        // FR-048c: the denial is audit-correlated.
        let audit_text = std::fs::read_to_string(&audit).unwrap();
        let denial: serde_json::Value = audit_text
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .find(|record| record["outcome"] == "denied_by_plugin_hook")
            .expect("audit must contain a denied_by_plugin_hook record");
        assert_eq!(denial["tool"], "alpha.echo");
        assert_eq!(denial["plugin"], "glitchy");
        assert_eq!(denial["success"], false);
    }

    #[tokio::test]
    async fn confidential_invocations_are_invisible_to_plugin_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let audit = ZenWiring::audit_log_path(&[dir.path().to_path_buf()]);
        let (sandbox_hooks, _sensitivity) = ZenWiring::build_sandbox_hooks(
            SandboxMode::WorkspaceWrite,
            &[dir.path().to_path_buf()],
        );

        // Dummy tools named after real sensitivity entries; dispatching
        // through the pipeline exercises the wiring-shaped sensitivity map.
        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(ProbeTool("shell.exec")));
        tools.register(Arc::new(ProbeTool("fs.read")));

        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut plugin_hooks: Vec<Box<dyn ToolDispatchHook>> = Vec::new();
        let mut api = PluginApi::new("spy", &mut tools, &mut plugin_hooks, dir.path())
            .with_isolation(wiring_style_sensitivity(), audit);
        api.register_hook(Box::new(ObservingHook {
            seen: Arc::clone(&seen),
        }));

        let invocations = vec![
            rig_compose::normalizer::ToolInvocation {
                name: "shell.exec".to_string(),
                args: serde_json::json!({}),
            },
            rig_compose::normalizer::ToolInvocation {
                name: "fs.read".to_string(),
                args: serde_json::json!({}),
            },
        ];
        let all_hooks = hook_refs(&sandbox_hooks, &plugin_hooks);
        let results = rig_compose::normalizer::dispatch_tool_invocations_with_hooks(
            &tools,
            &invocations,
            &all_hooks,
        )
        .await
        .unwrap();
        assert_eq!(results.len(), 2, "both invocations must dispatch");

        assert_eq!(
            *seen.lock().unwrap(),
            vec!["before:fs.read", "after:fs.read"],
            "plugin hook must observe nothing for the Confidential invocation"
        );
    }
    #[test]
    fn t112_reserved_prefix_plugin_rejected_via_wiring() {
        use sha2::{Digest, Sha256};
        use zen_plugin::registry::PluginRegistry;

        fn write_hashed_wasm_plugin(dir: &std::path::Path, id: &str, func: &str) {
            let plugin_dir = dir.join(id);
            std::fs::create_dir_all(&plugin_dir).unwrap();
            let wasm = wat::parse_str(format!(
                r#"(module (func (export "{func}")) (func (export "_start")))"#
            ))
            .unwrap();
            std::fs::write(plugin_dir.join("plugin.wasm"), &wasm).unwrap();

            let mut hasher = Sha256::new();
            hasher.update(&wasm);
            let sha256: String = hasher
                .finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            std::fs::write(
                plugin_dir.join("manifest.toml"),
                format!(
                    "id = \"{id}\"\nname = \"Demo\"\nversion = \"0.1.0\"\ntype = \"tool\"\n\
                     permissions = []\nentry = \"plugin.wasm\"\nsha256 = \"{sha256}\"\n"
                ),
            )
            .unwrap();
        }

        let dir = tempfile::tempdir().unwrap();
        // "fs" is a valid-charset id but a reserved top-level namespace:
        // its tools would spoof the fs.* builtin namespace.
        write_hashed_wasm_plugin(dir.path(), "fs", "spoof");
        write_hashed_wasm_plugin(dir.path(), "good", "ping");

        let mut registry = PluginRegistry::with_plugin_dir(dir.path().to_path_buf());
        registry.discover().unwrap();

        let wiring = ZenWiring::with_sandbox_mode(
            SandboxMode::WorkspaceWrite,
            vec![dir.path().to_path_buf()],
            Some(&registry),
        );

        assert!(
            wiring.tools.get("fs.spoof").is_err(),
            "reserved-prefix plugin tool must be rejected (FR-050b)"
        );
        assert!(
            wiring.tools.get("good.ping").is_ok(),
            "sibling plugin's tools must still register"
        );
        assert_eq!(wiring.tools.len(), 22, "21 builtin tools + good.ping only");
    }

    // ── FR-046c(4): grant overlay does not change MCP exposure ───────────

    #[test]
    fn fr046_grant_overlay_leaves_mcp_exposure_filter_unchanged() {
        use crate::delegate_tools::resolve_agent_tool_grants;

        let dir = tempfile::tempdir().unwrap();
        write_wasm_plugin(dir.path(), "demo_plugin");
        let mut registry =
            zen_plugin::registry::PluginRegistry::with_plugin_dir(dir.path().to_path_buf());
        registry.discover().unwrap();

        let wiring = ZenWiring::with_sandbox_mode(
            SandboxMode::WorkspaceWrite,
            vec![dir.path().to_path_buf()],
            Some(&registry),
        );

        let overlay = vec!["plugin:*".to_string()];
        let granted = resolve_agent_tool_grants("Sisyphus", &overlay, &wiring.tools);
        assert!(
            granted.contains(&"demo_plugin.ping".to_string()),
            "plugin:* overlay must grant the plugin tool to the agent"
        );

        // FR-017 regression: grants only widen agent-side reach; the
        // external MCP registry keeps filtering Confidential regardless.
        let mcp = wiring.build_mcp_registry();
        assert!(
            mcp.get("demo_plugin.ping").is_ok(),
            "Private plugin tool stays MCP-exposed"
        );
        assert!(
            mcp.get("shell.exec").is_err(),
            "Confidential builtin must stay excluded from MCP exposure"
        );
        assert!(mcp.get("fs.read").is_ok());
    }
}
