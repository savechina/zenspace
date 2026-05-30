#![allow(dead_code)]

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use rig_compose::budget::{AtomicBudget, BudgetError, BudgetGuard, DispatchBudgetHook};
use rig_compose::normalizer::ToolDispatchHook;
use tokio::sync::Mutex;
use tracing::debug;

use zen_core::sandbox::{
    SandboxMode, SandboxValidator, SeatbeltHook, SeatbeltPolicy, apply_resource_limits,
};

pub struct SandboxConfig {
    pub mode: SandboxMode,
    pub workspace_roots: Vec<PathBuf>,
    pub rate_limit_per_minute: u64,
    pub timeout_secs: u64,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            mode: SandboxMode::WorkspaceWrite,
            workspace_roots: Vec::new(),
            rate_limit_per_minute: 20,
            timeout_secs: 300,
        }
    }
}

impl SandboxConfig {
    pub fn new(mode: SandboxMode) -> Self {
        Self {
            mode,
            ..Default::default()
        }
    }

    pub fn with_workspaces(mut self, roots: Vec<PathBuf>) -> Self {
        self.workspace_roots = roots;
        self
    }

    pub fn with_rate_limit(mut self, per_minute: u64) -> Self {
        self.rate_limit_per_minute = per_minute;
        self
    }

    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }
}

pub const DEFAULT_RATE_LIMIT: u64 = 20;

pub struct RateLimitGuard {
    budget: Arc<AtomicBudget>,
    window: Arc<Mutex<VecDeque<Instant>>>,
    window_size: Duration,
}

impl RateLimitGuard {
    pub fn new(per_minute: u64) -> Self {
        let budget = Arc::new(AtomicBudget::new(per_minute));
        Self {
            budget: budget.clone(),
            window: Arc::new(Mutex::new(VecDeque::new())),
            window_size: Duration::from_secs(60),
        }
    }

    pub fn as_budget_arc(&self) -> Arc<AtomicBudget> {
        self.budget.clone()
    }

    pub fn window_arc(&self) -> Arc<Mutex<VecDeque<Instant>>> {
        self.window.clone()
    }

    fn window_push(&self) {
        if let Ok(mut guard) = self.window.try_lock() {
            guard.push_back(Instant::now());
        }
    }
}

#[async_trait]
impl BudgetGuard for RateLimitGuard {
    async fn try_reserve(&self, cost: u64) -> Result<bool, BudgetError> {
        let reserved = self.budget.try_reserve(cost).await?;
        if reserved {
            self.window_push();
        }
        Ok(reserved)
    }

    async fn release(&self, cost: u64) {
        self.budget.release(cost).await;
    }
}

struct WindowPruneGuard {
    window: Arc<Mutex<VecDeque<Instant>>>,
    window_size: Duration,
}

impl WindowPruneGuard {
    fn new(window: Arc<Mutex<VecDeque<Instant>>>, window_size: Duration) -> Self {
        Self {
            window,
            window_size,
        }
    }

    fn maybe_prune(&self) {
        if let Ok(mut guard) = self.window.try_lock() {
            let cutoff = Instant::now().checked_sub(self.window_size);

            if let Some(cutoff) = cutoff {
                while guard.front().is_some_and(|t| *t < cutoff) {
                    guard.pop_front();
                }
            }
        }
    }
}

pub struct SandboxManager {
    config: SandboxConfig,
    validator: SandboxValidator,
    policy: SeatbeltPolicy,
    rate_limit_hook: Box<dyn ToolDispatchHook>,
    seatbelt_hook: SeatbeltHook,
    window_prune: WindowPruneGuard,
}

impl SandboxManager {
    pub fn new(config: SandboxConfig) -> Self {
        let validator = SandboxValidator::new(config.mode, config.workspace_roots.clone());
        let policy = SeatbeltPolicy::new(config.mode, config.workspace_roots.clone())
            .with_timeout(config.timeout_secs);

        let rate_limit_guard = RateLimitGuard::new(config.rate_limit_per_minute);
        let budget_arc = rate_limit_guard.as_budget_arc();
        let window_arc = rate_limit_guard.window_arc();
        let dispatch_budget_hook = DispatchBudgetHook::new(budget_arc, 1);

        let seatbelt_hook = SeatbeltHook::new(policy.clone());

        Self {
            config,
            validator,
            policy,
            rate_limit_hook: Box::new(dispatch_budget_hook),
            seatbelt_hook,
            window_prune: WindowPruneGuard::new(window_arc, Duration::from_secs(60)),
        }
    }

    pub async fn check_command_before_exec(&self, cmd: &str) -> Result<(), String> {
        self.window_prune.maybe_prune();
        self.validator.validate_command(cmd)
    }

    pub async fn check_path_write(&self, path: &Path) -> Result<(), String> {
        self.validator.validate_path_for_write(path)
    }

    pub fn is_danger_mode(&self) -> bool {
        self.config.mode == SandboxMode::DangerFullAccess
    }

    pub fn mode(&self) -> SandboxMode {
        self.config.mode
    }

    pub fn policy(&self) -> &SeatbeltPolicy {
        &self.policy
    }

    pub fn hooks(&self) -> Vec<&dyn ToolDispatchHook> {
        let rate_limit: &dyn ToolDispatchHook = &*self.rate_limit_hook;
        let seatbelt: &dyn ToolDispatchHook = &self.seatbelt_hook;
        vec![rate_limit, seatbelt]
    }

    pub fn generate_sandbox_profile(&self) -> String {
        self.policy.generate_sandbox_exec_profile()
    }

    pub fn init_with_resource_limits(&self) -> Result<(), String> {
        if self.config.mode != SandboxMode::DangerFullAccess {
            apply_resource_limits()
                .map_err(|e| format!("failed to apply resource limits: {}", e))?;
            debug!(
                mode = %self.config.mode,
                rate_limit = self.config.rate_limit_per_minute,
                "sandbox initialized"
            );
        }
        Ok(())
    }

    pub fn confirm_danger_access() -> bool {
        cfg!(target_os = "macos")
    }
}

pub fn create_default_sandbox_manager() -> SandboxManager {
    let workspace_roots = zen_core::paths::ZenPaths::detect()
        .ok()
        .and_then(|p| p.workspace_root().cloned())
        .map(|w| vec![w])
        .unwrap_or_default();

    let config = SandboxConfig::default()
        .with_workspaces(workspace_roots)
        .with_rate_limit(DEFAULT_RATE_LIMIT)
        .with_timeout(300);

    SandboxManager::new(config)
}

pub fn create_readonly_sandbox_manager() -> SandboxManager {
    let config = SandboxConfig::new(SandboxMode::ReadOnly)
        .with_rate_limit(DEFAULT_RATE_LIMIT)
        .with_timeout(300);

    SandboxManager::new(config)
}

pub fn create_full_access_sandbox_manager() -> SandboxManager {
    let config = SandboxConfig::new(SandboxMode::DangerFullAccess)
        .with_rate_limit(DEFAULT_RATE_LIMIT)
        .with_timeout(300);

    SandboxManager::new(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_sandbox_config_defaults() {
        let config = SandboxConfig::default();
        assert_eq!(config.mode, SandboxMode::WorkspaceWrite);
        assert_eq!(config.rate_limit_per_minute, 20);
        assert_eq!(config.timeout_secs, 300);
    }

    #[test]
    fn test_sandbox_config_builder() {
        let config = SandboxConfig::new(SandboxMode::ReadOnly)
            .with_workspaces(vec![PathBuf::from("/workspace")])
            .with_rate_limit(10)
            .with_timeout(60);

        assert_eq!(config.mode, SandboxMode::ReadOnly);
        assert_eq!(config.rate_limit_per_minute, 10);
        assert_eq!(config.timeout_secs, 60);
        assert_eq!(config.workspace_roots.len(), 1);
    }

    #[test]
    fn test_create_default_manager() {
        let manager = create_default_sandbox_manager();
        assert_eq!(manager.mode(), SandboxMode::WorkspaceWrite);
    }

    #[test]
    fn test_create_readonly_manager() {
        let manager = create_readonly_sandbox_manager();
        assert_eq!(manager.mode(), SandboxMode::ReadOnly);
    }

    #[test]
    fn test_create_full_access_manager() {
        let manager = create_full_access_sandbox_manager();
        assert!(manager.is_danger_mode());
    }

    #[tokio::test]
    async fn test_rate_limit_guard_reserves_and_releases() {
        let guard = RateLimitGuard::new(5);
        for _ in 0..5 {
            assert!(guard.try_reserve(1).await.unwrap());
        }
        assert!(!guard.try_reserve(1).await.unwrap());
        guard.release(1).await;
        assert!(guard.try_reserve(1).await.unwrap());
    }

    #[tokio::test]
    async fn test_manager_check_command_blocks_dangerous() {
        let manager = create_default_sandbox_manager();
        assert!(manager.check_command_before_exec("rm -rf /").await.is_err());
        assert!(
            manager
                .check_command_before_exec("sudo rm -rf /")
                .await
                .is_err()
        );
        assert!(manager.check_command_before_exec("ls -la").await.is_ok());
    }

    #[tokio::test]
    async fn test_manager_check_path_write_blocks_metadata() {
        let config = SandboxConfig::default().with_workspaces(vec![PathBuf::from("/workspace")]);
        let manager = SandboxManager::new(config);
        assert!(
            manager
                .check_path_write(Path::new("/workspace/.git/config"))
                .await
                .is_err()
        );
        assert!(
            manager
                .check_path_write(Path::new("/workspace/.zen/db"))
                .await
                .is_err()
        );
        assert!(
            manager
                .check_path_write(Path::new("/workspace/notes.md"))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn test_readonly_manager_blocks_all_writes() {
        let manager = create_readonly_sandbox_manager();
        assert!(
            manager
                .check_path_write(Path::new("/workspace/notes.md"))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn test_full_access_manager_allows_all() {
        let manager = create_full_access_sandbox_manager();
        assert!(
            manager
                .check_path_write(Path::new("/workspace/.git/config"))
                .await
                .is_ok()
        );
        assert!(
            manager
                .check_path_write(Path::new("/workspace/notes.md"))
                .await
                .is_ok()
        );
    }
}
