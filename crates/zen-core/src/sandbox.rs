use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use async_trait::async_trait;
use rig_compose::normalizer::{
    ToolDispatchAction, ToolDispatchHook, ToolInvocation, ToolInvocationResult,
};
use rig_compose::registry::KernelError;
use serde::{Deserialize, Serialize};
use tracing::debug;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SandboxMode {
    ReadOnly,
    #[default]
    WorkspaceWrite,
    Ask,
    DangerFullAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    Allow,
    Deny,
}

pub type ApprovalCallback = Arc<dyn Fn(&ToolInvocation) -> ApprovalDecision + Send + Sync>;

impl SandboxMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            SandboxMode::ReadOnly => "read-only",
            SandboxMode::WorkspaceWrite => "workspace-write",
            SandboxMode::Ask => "ask",
            SandboxMode::DangerFullAccess => "danger-full-access",
        }
    }

    pub fn parse_str(s: &str) -> Option<Self> {
        match s {
            "read-only" | "readonly" | "ro" => Some(SandboxMode::ReadOnly),
            "workspace-write" | "workspacemode" | "ww" => Some(SandboxMode::WorkspaceWrite),
            "ask" | "askme" => Some(SandboxMode::Ask),
            "danger-full-access" | "full" | "unsafe" => Some(SandboxMode::DangerFullAccess),
            _ => None,
        }
    }
}

impl std::fmt::Display for SandboxMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

pub const PROTECTED_PATHS: &[&str] = &[".git", ".zen", ".ssh", ".aws", ".gnupg", ".env"];

pub const BLOCKED_COMMAND_PATTERNS: &[&str] = &["rm -rf /", "rm -rf ~", "sudo ", "chmod 777"];

pub const BLOCKED_NETWORK_COMMANDS: &[&str] = &["curl", "wget"];

pub fn is_env_file(path: &Path) -> bool {
    path.components().any(|c| {
        let name = c.as_os_str().to_string_lossy();
        name == ".env" || name.starts_with(".env.") || name.ends_with(".env")
    })
}

pub fn is_metadata_path(path: &Path) -> bool {
    is_env_file(path)
        || path.components().any(|c| {
            let name = c.as_os_str().to_string_lossy();
            PROTECTED_PATHS.contains(&name.as_ref())
        })
}

/// Lexically normalize a path: collapse `.` components and resolve `..`
/// against the accumulating prefix (clamped at the path's own root anchor so
/// `..` can never pop above an absolute root). Does NOT touch the filesystem,
/// so it is safe for paths that do not yet exist (e.g. a file about to be
/// written). Symlink resolution requires `std::fs::canonicalize` and is
/// intentionally not performed here.
fn lexical_normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out: Vec<Component> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if let Some(Component::Normal(_)) = out.last() {
                    out.pop();
                }
            }
            other => out.push(other),
        }
    }
    let mut normalized = PathBuf::new();
    for c in &out {
        normalized.push(c.as_os_str());
    }
    if normalized.as_os_str().is_empty() {
        normalized.push(".");
    }
    normalized
}

/// Whether `path` stays within `root` after lexical normalization. Rejects
/// `..` traversal that would escape the configured root (D8: root-bound path
/// validation). Both operands are normalized, so a workspace root that itself
/// contains a `..` is handled correctly.
fn is_within_root(path: &Path, root: &Path) -> bool {
    lexical_normalize(path).starts_with(lexical_normalize(root))
}

/// FR-024 — Canonicalize a path that may have a non-existent tail.
///
/// `std::fs::canonicalize` fails on paths whose final component does not
/// exist (e.g. a file about to be written through a symlinked directory).
/// For sandbox verification we still need to resolve the symlinked prefix
/// so we can check the eventual target. This helper walks up the path to
/// the deepest existing ancestor, canonicalizes that prefix, then re-appends
/// the missing tail. If the entire path exists it short-circuits to a plain
/// `canonicalize` call.
fn canonicalize_for_check(path: &Path) -> std::io::Result<PathBuf> {
    if let Ok(c) = std::fs::canonicalize(path) {
        return Ok(c);
    }
    // If the final component is itself a symlink that fails to resolve, it's
    // a broken symlink — surface the error rather than walking up, since
    // following a broken link at runtime will fail anyway.
    let is_broken_symlink = std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    if is_broken_symlink {
        return std::fs::canonicalize(path);
    }
    // Otherwise the tail doesn't exist (likely a write target). Walk up to
    // the deepest existing ancestor, canonicalize that prefix, then re-append
    // the missing tail.
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut current = path.to_path_buf();
    loop {
        let parent = match current.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => return std::fs::canonicalize(path),
        };
        if let Some(name) = current.file_name() {
            missing.push(name.to_os_string());
        }
        if std::fs::symlink_metadata(&parent).is_ok() {
            let canonical_parent = std::fs::canonicalize(&parent)?;
            let mut result = canonical_parent;
            for name in missing.iter().rev() {
                result.push(name);
            }
            return Ok(result);
        }
        current = parent;
    }
}

pub fn is_blocked_command(cmd: &str) -> Option<&'static str> {
    let cmd_lower = cmd.to_lowercase();

    for pattern in BLOCKED_COMMAND_PATTERNS {
        if cmd_lower.contains(&pattern.to_lowercase()) {
            return Some(pattern);
        }
    }

    for blocked in BLOCKED_NETWORK_COMMANDS {
        let cmd_first_token = cmd_lower.split_whitespace().next().unwrap_or("");
        if cmd_first_token == *blocked {
            return Some(blocked);
        }
    }

    None
}

pub fn is_dangerous_network_target(cmd: &str) -> bool {
    let known_hosts = [
        "localhost",
        "127.0.0.1",
        "0.0.0.0",
        "api.openai.com",
        "api.anthropic.com",
        "github.com",
        "raw.githubusercontent.com",
    ];

    let cmd_trimmed = cmd.trim();
    // First token covers both command strings ("curl http://…") and the bare
    // or path-qualified binary the structured shell.exec registry passes
    // ("curl", "/usr/bin/curl"). Without this, the trailing-space pattern
    // below let bare binary names through (FR-028 pre-dispatch gap).
    let first_token = cmd_trimmed.split_whitespace().next().unwrap_or("");
    let binary_name = first_token.rsplit('/').next().unwrap_or(first_token);

    for host in BLOCKED_NETWORK_COMMANDS {
        let pattern = format!("{} ", host);
        // Bare binary via shell.exec arg registry: the seatbelt passes `binary`
        // alone as cmd_trimmed; args[] is never joined here, so there's no URL
        // to inspect — block the binary (FR-028). The agent uses
        // web.fetch/web.search (which go through NetworkPolicy) for HTTP.
        if binary_name == *host && cmd_trimmed == first_token {
            return true;
        }
        if cmd_trimmed.contains(&pattern) {
            let next = cmd_trimmed.find(&pattern).unwrap_or(0) + pattern.len();
            if next > cmd_trimmed.len() {
                return true;
            }
            let after_cmd = &cmd_trimmed[next..];
            let url_or_arg = after_cmd.split_whitespace().next().unwrap_or("");
            let host_part = url_or_arg
                .trim_start_matches("http://")
                .trim_start_matches("https://")
                .split('/')
                .next()
                .unwrap_or(url_or_arg);

            if !known_hosts.iter().any(|known| host_part.contains(known)) {
                return true;
            }
        }
    }

    false
}

/// FR-035 — Per-tool arg-key registry mapping tool name → arg keys that
/// carry paths or commands. Tools whose sensitive args are not named
/// `command` or `path` (e.g. `system.daemon` uses `daemon_action`,
/// `plugin.wasm_sandbox` uses `wasm_path`) bypassed `SeatbeltPolicy`
/// validation entirely before this registry existed.
#[derive(Debug, Clone, Default)]
pub struct ToolArgRegistry {
    entries: HashMap<String, ToolArgEntry>,
}

#[derive(Debug, Clone, Default)]
struct ToolArgEntry {
    path_args: Vec<String>,
    command_args: Vec<String>,
}

impl ToolArgRegistry {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Register the path-bearing and command-bearing arg keys for a tool.
    /// Path args are validated via [`is_metadata_path`] on mutating ops;
    /// command args are validated via [`is_blocked_command`] +
    /// [`is_dangerous_network_target`].
    pub fn register_tool_args(
        &mut self,
        tool_name: &str,
        path_args: &[&str],
        command_args: &[&str],
    ) {
        self.entries.insert(
            tool_name.to_string(),
            ToolArgEntry {
                path_args: path_args.iter().map(|s| s.to_string()).collect(),
                command_args: command_args.iter().map(|s| s.to_string()).collect(),
            },
        );
    }

    fn get(&self, tool_name: &str) -> Option<&ToolArgEntry> {
        self.entries.get(tool_name)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct SeatbeltPolicy {
    pub mode: SandboxMode,
    pub workspace_roots: Vec<PathBuf>,
    pub timeout_secs: u64,
    pub arg_registry: ToolArgRegistry,
}

impl SeatbeltPolicy {
    pub fn new(mode: SandboxMode, workspace_roots: Vec<PathBuf>) -> Self {
        Self {
            mode,
            workspace_roots,
            timeout_secs: 300,
            arg_registry: ToolArgRegistry::new(),
        }
    }

    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    /// Attach a per-tool arg-key registry (FR-035) so the seatbelt inspects
    /// arg names other than the default `command` / `path`.
    pub fn with_arg_registry(mut self, registry: ToolArgRegistry) -> Self {
        self.arg_registry = registry;
        self
    }

    #[cfg(target_os = "macos")]
    pub fn generate_sandbox_exec_profile(&self) -> String {
        let mut profile = String::new();

        profile.push_str("(version 1)\n");
        profile.push_str("(deny default)\n");

        profile.push_str("(allow process-execute (literal \"/bin/sh\"))\n");
        profile.push_str("(allow process-execute (literal \"/bin/bash\"))\n");
        profile.push_str("(allow process-execute (literal \"/usr/bin/env\"))\n");

        profile.push_str("(allow file-read-data (literal \"/dev/null\"))\n");
        profile.push_str("(allow file-read-data (literal \"/dev/zero\"))\n");
        profile.push_str("(allow file-read-data (literal \"/dev/random\"))\n");
        profile.push_str("(allow file-read-data (literal \"/dev/urandom\"))\n");

        profile.push_str("(allow file-read-metadata)\n");

        profile.push_str("(allow sysctl-read)\n");

        profile.push_str("(allow network-outbound (local tcp))\n");
        profile.push_str("(allow network-outbound (local udp))\n");

        if self.mode == SandboxMode::ReadOnly {
            profile.push_str("(deny file-write*)\n");
        } else {
            for root in &self.workspace_roots {
                let root_str = root.to_string_lossy();
                if !is_metadata_path(root) {
                    profile.push_str(&format!("(allow file-write* (subpath \"{}\"))\n", root_str));
                }
            }
        }

        for protected in PROTECTED_PATHS {
            if let Some(home) = home::home_dir() {
                let protected_path = home.join(protected);
                let path_str = protected_path.to_string_lossy();
                if self.mode == SandboxMode::ReadOnly {
                    profile.push_str(&format!("(allow file-read* (subpath \"{}\"))\n", path_str));
                } else {
                    profile.push_str(&format!("(deny file-write* (subpath \"{}\"))\n", path_str));
                    profile.push_str(&format!("(allow file-read* (subpath \"{}\"))\n", path_str));
                }
            }
        }

        if self.mode == SandboxMode::DangerFullAccess {
            profile.clear();
            profile.push_str("(version 1)\n");
            profile.push_str("(allow default)\n");
        }

        profile
    }

    #[cfg(not(target_os = "macos"))]
    pub fn generate_sandbox_exec_profile(&self) -> String {
        "(non-macos platform, no-op)\n".to_string()
    }

    pub fn is_write_allowed(&self, path: &Path) -> bool {
        if self.mode == SandboxMode::ReadOnly {
            return false;
        }

        if self.mode == SandboxMode::DangerFullAccess {
            return true;
        }

        if is_metadata_path(path) {
            return false;
        }

        if self.workspace_roots.is_empty() {
            return false;
        }

        self.workspace_roots
            .iter()
            .any(|root| is_within_root(path, root))
    }

    pub fn validate_invocation(
        &self,
        invocation: &ToolInvocation,
    ) -> Result<ToolDispatchAction, KernelError> {
        // FR-035: prefer per-tool registered arg keys; fall back to the
        // legacy `command` / `path` keys for unregistered tools.
        let (command_keys, path_keys): (Vec<&str>, Vec<&str>) =
            match self.arg_registry.get(&invocation.name) {
                Some(entry) => (
                    entry.command_args.iter().map(|s| s.as_str()).collect(),
                    entry.path_args.iter().map(|s| s.as_str()).collect(),
                ),
                None => (vec!["command"], vec!["path"]),
            };

        for key in &command_keys {
            if let Some(args_str) = invocation.args.get(*key).and_then(|v| v.as_str())
                && let Some(termination) = self.check_command_arg(args_str)
            {
                return Ok(termination);
            }
        }

        for key in &path_keys {
            if let Some(path_str) = invocation.args.get(*key).and_then(|v| v.as_str())
                && let Some(termination) = self.check_path_arg(invocation, path_str)
            {
                return Ok(termination);
            }
        }

        Ok(ToolDispatchAction::Continue)
    }

    fn check_command_arg(&self, args_str: &str) -> Option<ToolDispatchAction> {
        if let Some(pattern) = is_blocked_command(args_str) {
            return Some(ToolDispatchAction::Terminate {
                reason: format!("sandbox blocked command matching pattern: \"{}\"", pattern),
            });
        }

        if is_dangerous_network_target(args_str) && self.mode != SandboxMode::DangerFullAccess {
            return Some(ToolDispatchAction::Terminate {
                reason: "sandbox blocked network command to unknown host".to_string(),
            });
        }

        None
    }

    fn check_path_arg(
        &self,
        invocation: &ToolInvocation,
        path_str: &str,
    ) -> Option<ToolDispatchAction> {
        let path = PathBuf::from(path_str);
        if is_metadata_path(&path)
            && let Some(op) = invocation.args.get("operation").and_then(|v| v.as_str())
            && matches!(op, "write" | "delete" | "remove" | "modify")
        {
            return Some(ToolDispatchAction::Terminate {
                reason: format!(
                    "sandbox blocked write operation on metadata path: \"{}\"",
                    path_str
                ),
            });
        }
        None
    }
}

/// Seatbelt-based hook that validates tool invocations against a
/// [`SeatbeltPolicy`]. Denies operations on metadata paths, enforces
/// workspace-root containment for path args, and validates command args
/// via the per-tool [`ToolArgRegistry`].
pub struct SeatbeltHook {
    policy: Arc<SeatbeltPolicy>,
}

impl SeatbeltHook {
    pub fn new(policy: SeatbeltPolicy) -> Self {
        Self {
            policy: Arc::new(policy),
        }
    }

    pub fn policy(&self) -> &SeatbeltPolicy {
        &self.policy
    }

    pub fn into_policy(self) -> Arc<SeatbeltPolicy> {
        self.policy
    }
}

impl Clone for SeatbeltHook {
    fn clone(&self) -> Self {
        Self {
            policy: self.policy.clone(),
        }
    }
}

#[async_trait]
impl ToolDispatchHook for SeatbeltHook {
    async fn before_invocation(
        &self,
        invocation: &ToolInvocation,
    ) -> Result<ToolDispatchAction, KernelError> {
        self.policy.validate_invocation(invocation)
    }

    async fn after_invocation(&self, _result: &ToolInvocationResult) -> Result<(), KernelError> {
        Ok(())
    }

    async fn on_invocation_error(
        &self,
        invocation: &ToolInvocation,
        _error: &KernelError,
    ) -> Result<(), KernelError> {
        debug!(
            tool = %invocation.name,
            "invocation errored under sandbox"
        );
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct SandboxValidator {
    mode: SandboxMode,
    workspace_roots: Vec<PathBuf>,
    protected_set: HashSet<String>,
}

impl SandboxValidator {
    pub fn new(mode: SandboxMode, workspace_roots: Vec<PathBuf>) -> Self {
        let mut protected_set = HashSet::new();
        for p in PROTECTED_PATHS {
            protected_set.insert(p.to_string());
            if let Some(stripped) = p.strip_prefix('.') {
                protected_set.insert(stripped.to_string());
            }
        }

        Self {
            mode,
            workspace_roots,
            protected_set,
        }
    }

    pub fn mode(&self) -> SandboxMode {
        self.mode
    }

    pub fn validate_path_for_write(&self, path: &Path) -> Result<(), String> {
        if self.mode == SandboxMode::ReadOnly {
            return Err("sandbox is in read-only mode, no write allowed".to_string());
        }

        if self.mode == SandboxMode::DangerFullAccess {
            return Ok(());
        }

        let normalized = lexical_normalize(path);

        if is_env_file(&normalized) {
            return Err(format!(
                "write blocked: {} is an environment file",
                path.display()
            ));
        }

        let path_str = normalized.to_string_lossy();
        for component in normalized.components() {
            let name = component.as_os_str().to_string_lossy();
            if self.protected_set.contains(name.as_ref()) {
                return Err(format!(
                    "write blocked: {} is a protected metadata path",
                    path_str
                ));
            }
        }

        if self.workspace_roots.is_empty() {
            return Err("no workspace roots configured, writes denied".to_string());
        }

        let allowed = self
            .workspace_roots
            .iter()
            .any(|root| is_within_root(path, root));
        if !allowed {
            return Err(format!(
                "write denied: {} is not under any workspace root",
                path_str
            ));
        }

        // FR-024: After lexical checks pass, verify no symlink component
        // silently redirects the path outside the workspace root or onto a
        // protected path. Only fires when the path actually exists and
        // contains symlinks (detected via `symlink_metadata`); non-existent
        // paths (about-to-be-written files) skip this step.
        self.check_symlink_escape(path)?;

        Ok(())
    }

    pub fn validate_path_for_read(&self, path: &Path) -> Result<(), String> {
        if self.mode == SandboxMode::DangerFullAccess {
            return Ok(());
        }

        let normalized = lexical_normalize(path);

        if is_env_file(&normalized) {
            return Err(format!(
                "read blocked: {} is an environment file",
                path.display()
            ));
        }

        for component in normalized.components() {
            let name = component.as_os_str().to_string_lossy();
            if self.protected_set.contains(name.as_ref()) {
                return Err(format!(
                    "read blocked: {} is a protected metadata path",
                    path.display()
                ));
            }
        }

        // FR-024: After lexical checks pass, verify no symlink component
        // silently redirects the path onto a protected target. See
        // `validate_path_for_write` for the full rationale.
        self.check_symlink_escape(path)?;

        Ok(())
    }

    /// FR-024 — Symlink escape check.
    ///
    /// Walks `path` component-by-component via `symlink_metadata` to detect
    /// any symlink component (intermediate or terminal). If a symlink is
    /// present AND the path exists, the path is canonicalized transitively
    /// (`std::fs::canonicalize` resolves symlink chains) and the canonical
    /// target is re-checked against:
    ///   1. `PROTECTED_PATHS` / env files (always — a symlink landing on
    ///      `~/.ssh/id_rsa` or `.env` is blocked regardless of mode),
    ///   2. Workspace root containment (when roots are configured — a
    ///      symlink escaping the workspace root is blocked).
    ///
    /// Policy (per Clarifications 2026-08-11): FOLLOW symlinks rather than
    /// rejecting all symlinks, so legitimate workflows (Obsidian vaults with
    /// symlinked folders, monorepo workspaces) keep working — but reject if
    /// the canonical target escapes the workspace root or lands on a
    /// protected path.
    ///
    /// Non-existent paths (about-to-be-written files) skip canonicalization
    /// entirely — `lexical_normalize` already handles `..` traversal for
    /// those, and `canonicalize` would fail anyway.
    fn check_symlink_escape(&self, path: &Path) -> Result<(), String> {
        if !self.path_contains_symlink(path) {
            return Ok(());
        }

        let canonical = match canonicalize_for_check(path) {
            Ok(c) => c,
            Err(e) => {
                return Err(format!(
                    "sandbox blocked: cannot canonicalize symlink path {}: {}",
                    path.display(),
                    e
                ));
            }
        };

        if is_env_file(&canonical) {
            return Err(format!(
                "sandbox blocked: symlink {} resolves to environment file {}",
                path.display(),
                canonical.display()
            ));
        }

        for component in canonical.components() {
            let name = component.as_os_str().to_string_lossy();
            if self.protected_set.contains(name.as_ref()) {
                return Err(format!(
                    "sandbox blocked: symlink {} resolves to protected path {}",
                    path.display(),
                    canonical.display()
                ));
            }
        }

        // Root containment is only enforced when roots are configured; an
        // empty root set keeps the existing permissive read semantics.
        // Both sides are canonicalized for comparison so platforms that
        // rewrite paths via system symlinks (macOS `/tmp` → `/private/tmp`)
        // do not trigger false-positive escapes.
        if !self.workspace_roots.is_empty() {
            let inside = self.workspace_roots.iter().any(|root| {
                let root_cmp = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
                is_within_root(&canonical, &root_cmp)
            });
            if !inside {
                return Err(format!(
                    "sandbox blocked: symlink {} escapes workspace root (resolves to {})",
                    path.display(),
                    canonical.display()
                ));
            }
        }

        Ok(())
    }

    /// Walk `path` from its root anchor through every component, returning
    /// `true` if any prefix is a symlink. Stops at the first prefix that
    /// does not exist on disk — beyond that point no symlink is possible.
    /// Uses `symlink_metadata` (not `metadata`) so symlinks themselves are
    /// NOT followed during detection.
    fn path_contains_symlink(&self, path: &Path) -> bool {
        let mut current = PathBuf::new();
        for component in path.components() {
            current.push(component);
            match std::fs::symlink_metadata(&current) {
                Ok(meta) if meta.file_type().is_symlink() => return true,
                Ok(_) => continue,
                Err(_) => return false,
            }
        }
        false
    }

    pub fn validate_command(&self, cmd: &str) -> Result<(), String> {
        if let Some(pattern) = is_blocked_command(cmd) {
            return Err(format!("blocked command pattern: {}", pattern));
        }

        if is_dangerous_network_target(cmd) && self.mode != SandboxMode::DangerFullAccess {
            return Err("blocked network command to unknown host".to_string());
        }

        Ok(())
    }
}

pub fn apply_resource_limits() -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use libc::{RLIMIT_CORE, RLIMIT_NOFILE, RLIMIT_NPROC, getrlimit, rlimit, setrlimit};

        // libc types the rlimit resource argument per-target: glibc Linux uses
        // `__rlimit_resource_t` (u32) for both the fns and the RLIMIT_* consts,
        // while macOS/musl/BSD use `c_int`. Aliasing to libc's own type keeps
        // `soft_cap` and the constants type-identical on every unix target.
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        type RlimitResource = libc::__rlimit_resource_t;
        #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
        type RlimitResource = libc::c_int;

        // Lower only the SOFT limit and keep the existing (or infinite) HARD
        // limit: setting rlim_max == rlim_cur made the cap irreversible, so a
        // busy process (tokio workers + reqwest pool + sqlx) that legitimately
        // exceeded NOFILE=256 or NPROC=50 failed with EMFILE/EAGAIN forever.
        // Soft-only caps still constrain the process and every descendant that
        // does not explicitly raise them.
        fn soft_cap(resource: RlimitResource, cur: libc::rlim_t) -> Result<(), std::io::Error> {
            // SAFETY: getrlimit/setrlimit read/write the fully-initialized
            // stack struct below; no pointers escape. Return values checked.
            unsafe {
                let mut current = rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                if getrlimit(resource, &mut current) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let capped = rlimit {
                    rlim_cur: cur.min(current.rlim_max),
                    rlim_max: current.rlim_max,
                };
                if setrlimit(resource, &capped) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        }

        // SAFETY: RLIMIT writes use the fully-initialized stack struct; the
        // core-dump hard-zero has no legitimate reason to be raisable.
        unsafe {
            soft_cap(RLIMIT_NPROC, 50)?;
            soft_cap(RLIMIT_NOFILE, 256)?;
            let rlimit_core = rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if setrlimit(RLIMIT_CORE, &rlimit_core) != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metadata_path_detection() {
        assert!(is_metadata_path(&PathBuf::from("/home/user/.git/config")));
        assert!(is_metadata_path(&PathBuf::from("/home/user/.zen/db")));
        assert!(is_metadata_path(&PathBuf::from("/home/user/.ssh/id_rsa")));
        assert!(is_metadata_path(&PathBuf::from(
            "/home/user/.aws/credentials"
        )));
        assert!(is_metadata_path(&PathBuf::from(
            "/home/user/.gnupg/secring.gpg"
        )));

        assert!(!is_metadata_path(&PathBuf::from(
            "/home/user/projects/readme.md"
        )));
        assert!(!is_metadata_path(&PathBuf::from("/home/user/.gitignore")));

        assert!(is_env_file(&PathBuf::from("/project/.env")));
        assert!(is_env_file(&PathBuf::from("/project/.env.local")));
        assert!(is_env_file(&PathBuf::from("/project/.env.production")));
        assert!(is_env_file(&PathBuf::from("/project/production.env")));
        assert!(!is_env_file(&PathBuf::from("/project/env.txt")));
    }

    #[test]
    fn test_sandbox_validator_blocks_env_files() {
        let validator = SandboxValidator::new(
            SandboxMode::WorkspaceWrite,
            vec![PathBuf::from("/workspace")],
        );
        assert!(
            validator
                .validate_path_for_write(&PathBuf::from("/workspace/.env"))
                .is_err()
        );
        assert!(
            validator
                .validate_path_for_write(&PathBuf::from("/workspace/.env.local"))
                .is_err()
        );
        assert!(
            validator
                .validate_path_for_read(&PathBuf::from("/workspace/.env"))
                .is_err()
        );
        assert!(
            validator
                .validate_path_for_read(&PathBuf::from("/workspace/.git/config"))
                .is_err()
        );
        assert!(
            validator
                .validate_path_for_read(&PathBuf::from("/workspace/notes.md"))
                .is_ok()
        );
    }

    #[test]
    fn test_ask_mode_allows_workspace_writes() {
        let validator = SandboxValidator::new(SandboxMode::Ask, vec![PathBuf::from("/workspace")]);
        assert!(
            validator
                .validate_path_for_write(&PathBuf::from("/workspace/notes.md"))
                .is_ok()
        );
        assert!(
            validator
                .validate_path_for_write(&PathBuf::from("/workspace/.git/config"))
                .is_err()
        );
    }

    #[test]
    fn test_blocked_command_detection() {
        assert!(is_blocked_command("rm -rf /").is_some());
        assert!(is_blocked_command("rm -rf ~").is_some());
        assert!(is_blocked_command("sudo systemctl restart").is_some());
        assert!(is_blocked_command("chmod 777 /etc/passwd").is_some());

        assert!(is_blocked_command("ls -la").is_none());
        assert!(is_blocked_command("cat /etc/hosts").is_none());
    }

    #[test]
    fn dangerous_network_target_matches_bare_binary_name() {
        // FR-028: the structured shell.exec registry passes just the binary
        // string — bare and path-qualified curl/wget must be rejected.
        assert!(is_dangerous_network_target("curl"));
        assert!(is_dangerous_network_target("wget"));
        assert!(is_dangerous_network_target("/usr/bin/curl"));
        assert!(is_dangerous_network_target("curl http://169.254.169.254/"));
        // Known hosts still pass in full command-string form.
        assert!(!is_dangerous_network_target(
            "curl https://api.openai.com/v1"
        ));
        assert!(!is_dangerous_network_target("git status"));
    }

    #[test]
    fn test_sandbox_mode_display() {
        assert_eq!(SandboxMode::ReadOnly.as_str(), "read-only");
        assert_eq!(SandboxMode::WorkspaceWrite.as_str(), "workspace-write");
        assert_eq!(SandboxMode::Ask.as_str(), "ask");
        assert_eq!(SandboxMode::DangerFullAccess.as_str(), "danger-full-access");
    }

    #[test]
    fn test_sandbox_mode_parse_str() {
        assert_eq!(
            SandboxMode::parse_str("read-only"),
            Some(SandboxMode::ReadOnly)
        );
        assert_eq!(SandboxMode::parse_str("ro"), Some(SandboxMode::ReadOnly));
        assert_eq!(
            SandboxMode::parse_str("workspace-write"),
            Some(SandboxMode::WorkspaceWrite)
        );
        assert_eq!(SandboxMode::parse_str("ask"), Some(SandboxMode::Ask));
        assert_eq!(SandboxMode::parse_str("askme"), Some(SandboxMode::Ask));
        assert_eq!(
            SandboxMode::parse_str("danger-full-access"),
            Some(SandboxMode::DangerFullAccess)
        );
        assert_eq!(SandboxMode::parse_str("invalid"), None);
    }

    #[test]
    fn test_write_allowed_by_mode() {
        let policy = SeatbeltPolicy::new(SandboxMode::ReadOnly, vec![PathBuf::from("/workspace")]);
        assert!(!policy.is_write_allowed(&PathBuf::from("/workspace/file.txt")));

        let policy = SeatbeltPolicy::new(
            SandboxMode::WorkspaceWrite,
            vec![PathBuf::from("/workspace")],
        );
        assert!(policy.is_write_allowed(&PathBuf::from("/workspace/file.txt")));
        assert!(!policy.is_write_allowed(&PathBuf::from("/workspace/.git/config")));
        assert!(!policy.is_write_allowed(&PathBuf::from("/other/file.txt")));

        let policy = SeatbeltPolicy::new(
            SandboxMode::DangerFullAccess,
            vec![PathBuf::from("/workspace")],
        );
        assert!(policy.is_write_allowed(&PathBuf::from("/workspace/file.txt")));
        assert!(policy.is_write_allowed(&PathBuf::from("/workspace/.git/config")));
        assert!(policy.is_write_allowed(&PathBuf::from("/other/file.txt")));
    }

    #[test]
    fn test_sandbox_validator_rejects_metadata_paths() {
        let validator = SandboxValidator::new(
            SandboxMode::WorkspaceWrite,
            vec![PathBuf::from("/workspace")],
        );
        assert!(
            validator
                .validate_path_for_write(&PathBuf::from("/workspace/.git/config"))
                .is_err()
        );
        assert!(
            validator
                .validate_path_for_write(&PathBuf::from("/workspace/notes.md"))
                .is_ok()
        );
    }

    #[test]
    fn test_sandbox_validator_read_only_blocks_all_writes() {
        let validator =
            SandboxValidator::new(SandboxMode::ReadOnly, vec![PathBuf::from("/workspace")]);
        assert!(
            validator
                .validate_path_for_write(&PathBuf::from("/workspace/notes.md"))
                .is_err()
        );
    }

    #[test]
    fn test_validate_invocation_blocks_dangerous() {
        let policy = SeatbeltPolicy::new(
            SandboxMode::WorkspaceWrite,
            vec![PathBuf::from("/workspace")],
        );
        let hook = SeatbeltHook::new(policy);

        let rt = tokio::runtime::Runtime::new().unwrap();

        let invocation = ToolInvocation {
            name: "shell".to_string(),
            args: serde_json::json!({"command": "rm -rf /"}),
        };
        let result = rt.block_on(hook.before_invocation(&invocation)).unwrap();
        assert!(matches!(result, ToolDispatchAction::Terminate { .. }));
    }

    // ── FR-035: ToolArgRegistry seatbelt extension ───────────────────────────

    #[test]
    fn arg_registry_blocks_dangerous_daemon_action() {
        // Without registration, system.daemon's `action` arg bypasses the
        // seatbelt entirely (it only inspects args named `command`/`path`).
        let mut registry = ToolArgRegistry::new();
        registry.register_tool_args("system.daemon", &[], &["daemon_action", "action"]);

        let policy = SeatbeltPolicy::new(
            SandboxMode::WorkspaceWrite,
            vec![PathBuf::from("/workspace")],
        )
        .with_arg_registry(registry);

        let invocation = ToolInvocation {
            name: "system.daemon".to_string(),
            args: serde_json::json!({
                "action": "sudo systemctl stop sshd",
                "name": "sshd",
            }),
        };
        let result = policy.validate_invocation(&invocation).unwrap();
        assert!(
            matches!(result, ToolDispatchAction::Terminate { .. }),
            "dangerous daemon_action must be blocked via registry, got {result:?}"
        );
    }

    #[test]
    fn arg_registry_blocks_wasm_sandbox_protected_path() {
        let mut registry = ToolArgRegistry::new();
        registry.register_tool_args("plugin.wasm_sandbox", &["wasm_path"], &[]);

        let policy = SeatbeltPolicy::new(
            SandboxMode::WorkspaceWrite,
            vec![PathBuf::from("/workspace")],
        )
        .with_arg_registry(registry);

        let invocation = ToolInvocation {
            name: "plugin.wasm_sandbox".to_string(),
            args: serde_json::json!({
                "wasm_path": "/workspace/.git/evil.wasm",
                "operation": "write",
            }),
        };
        let result = policy.validate_invocation(&invocation).unwrap();
        assert!(
            matches!(result, ToolDispatchAction::Terminate { .. }),
            "write to metadata path via wasm_path must be blocked, got {result:?}"
        );
    }

    #[test]
    fn arg_registry_allows_benign_invocation() {
        let mut registry = ToolArgRegistry::new();
        registry.register_tool_args("system.daemon", &[], &["daemon_action", "action"]);

        let policy = SeatbeltPolicy::new(
            SandboxMode::WorkspaceWrite,
            vec![PathBuf::from("/workspace")],
        )
        .with_arg_registry(registry);

        let invocation = ToolInvocation {
            name: "system.daemon".to_string(),
            args: serde_json::json!({
                "action": "status",
                "name": "nginx",
            }),
        };
        let result = policy.validate_invocation(&invocation).unwrap();
        assert!(
            matches!(result, ToolDispatchAction::Continue),
            "benign daemon status must continue, got {result:?}"
        );
    }

    #[test]
    fn unregistered_tool_falls_back_to_legacy_arg_keys() {
        // Unregistered tools still get the legacy `command`/`path` check.
        let policy = SeatbeltPolicy::new(
            SandboxMode::WorkspaceWrite,
            vec![PathBuf::from("/workspace")],
        );

        let invocation = ToolInvocation {
            name: "shell".to_string(),
            args: serde_json::json!({"command": "rm -rf /"}),
        };
        let result = policy.validate_invocation(&invocation).unwrap();
        assert!(matches!(result, ToolDispatchAction::Terminate { .. }));
    }

    #[test]
    fn test_root_bound_rejects_traversal_escape() {
        let validator = SandboxValidator::new(
            SandboxMode::WorkspaceWrite,
            vec![PathBuf::from("/workspace")],
        );
        assert!(
            validator
                .validate_path_for_write(&PathBuf::from("/workspace/../../etc/passwd"))
                .is_err(),
            "traversal escaping the workspace root must be rejected"
        );
        assert!(
            validator
                .validate_path_for_read(&PathBuf::from("/workspace/../.ssh/id_rsa"))
                .is_err(),
            "traversal into a protected path must be rejected"
        );
        assert!(
            validator
                .validate_path_for_write(&PathBuf::from("/workspace/sub/../notes.md"))
                .is_ok(),
            "traversal that stays inside the root must be allowed"
        );

        let policy = SeatbeltPolicy::new(
            SandboxMode::WorkspaceWrite,
            vec![PathBuf::from("/workspace")],
        );
        assert!(
            !policy.is_write_allowed(&PathBuf::from("/workspace/../etc/passwd")),
            "seatbelt must not allow traversal escape"
        );
        assert!(
            policy.is_write_allowed(&PathBuf::from("/workspace/sub/../notes.md")),
            "seatbelt must allow in-root traversal"
        );
    }

    // ── FR-024: Symlink canonicalization ──────────────────────────────────────

    #[cfg(unix)]
    fn make_symlink_validator() -> (SandboxValidator, tempfile::TempDir, PathBuf) {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let ws_root = workspace.path().to_path_buf();
        let validator = SandboxValidator::new(SandboxMode::WorkspaceWrite, vec![ws_root.clone()]);
        (validator, workspace, ws_root)
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_protected_path_rejected_on_read() {
        use std::os::unix::fs::symlink;

        let (_guard, _workspace, ws_root) = make_symlink_validator();

        // Place the fake secret INSIDE the workspace so the lexical check
        // (which only sees `workspace/shortcut`) passes; only canonicalize
        // can catch it.
        let fake_secret_dir = ws_root.join(".ssh");
        std::fs::create_dir_all(&fake_secret_dir).unwrap();
        let fake_secret = fake_secret_dir.join("id_rsa");
        std::fs::write(&fake_secret, "PRIVATE").unwrap();

        let shortcut = ws_root.join("shortcut");
        symlink(&fake_secret, &shortcut).unwrap();

        let validator = SandboxValidator::new(SandboxMode::WorkspaceWrite, vec![ws_root.clone()]);
        let err = validator
            .validate_path_for_read(&shortcut)
            .expect_err("symlink to protected path must be rejected");
        assert!(
            err.contains("protected path"),
            "error must mention protected path, got: {err}"
        );

        let err = validator
            .validate_path_for_write(&shortcut)
            .expect_err("symlink to protected path must be rejected on write");
        assert!(
            err.contains("protected path"),
            "write error must mention protected path, got: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn legitimate_workspace_symlink_allowed() {
        use std::os::unix::fs::symlink;

        let (_guard, _workspace, ws_root) = make_symlink_validator();

        let real_dir = ws_root.join("real_data");
        std::fs::create_dir_all(&real_dir).unwrap();
        std::fs::write(real_dir.join("notes.md"), "ok").unwrap();

        // Obsidian-style vault symlink: target stays inside workspace.
        let vault = ws_root.join("vault");
        symlink(&real_dir, &vault).unwrap();

        let validator = SandboxValidator::new(SandboxMode::WorkspaceWrite, vec![ws_root.clone()]);
        validator
            .validate_path_for_read(&vault.join("notes.md"))
            .expect("legitimate workspace-local symlink must be allowed");
        validator
            .validate_path_for_write(&vault.join("new.md"))
            .expect("legitimate workspace-local symlink must be writable");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_chain_to_protected_rejected() {
        use std::os::unix::fs::symlink;

        let (_guard, _workspace, ws_root) = make_symlink_validator();

        // Chain: workspace/a → workspace/b → workspace/.aws/credentials
        let aws_dir = ws_root.join(".aws");
        std::fs::create_dir_all(&aws_dir).unwrap();
        let secret = aws_dir.join("credentials");
        std::fs::write(&secret, "KEY").unwrap();

        let link_b = ws_root.join("b");
        symlink(&secret, &link_b).unwrap();

        let link_a = ws_root.join("a");
        symlink(&link_b, &link_a).unwrap();

        let validator = SandboxValidator::new(SandboxMode::WorkspaceWrite, vec![ws_root.clone()]);
        let err = validator
            .validate_path_for_read(&link_a)
            .expect_err("symlink chain to protected path must be rejected");
        assert!(
            err.contains("protected path"),
            "chain error must mention protected path, got: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escaping_workspace_root_rejected() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let ws_root = workspace.path().to_path_buf();
        let out_root = outside.path().to_path_buf();

        std::fs::write(out_root.join("outside.txt"), "x").unwrap();

        let escape = ws_root.join("escape");
        symlink(out_root.join("outside.txt"), &escape).unwrap();

        let validator = SandboxValidator::new(SandboxMode::WorkspaceWrite, vec![ws_root.clone()]);
        let err = validator
            .validate_path_for_read(&escape)
            .expect_err("symlink escaping workspace root must be rejected");
        assert!(
            err.contains("escapes workspace root"),
            "escape error must mention workspace root, got: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn broken_symlink_rejected() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().unwrap();
        let ws_root = workspace.path().to_path_buf();

        let dangling = ws_root.join("dangling");
        symlink(ws_root.join("does_not_exist"), &dangling).unwrap();

        let validator = SandboxValidator::new(SandboxMode::WorkspaceWrite, vec![ws_root.clone()]);
        let err = validator
            .validate_path_for_read(&dangling)
            .expect_err("broken symlink must be rejected");
        assert!(
            err.contains("cannot canonicalize"),
            "broken symlink error must mention canonicalize, got: {err}"
        );
    }

    #[test]
    fn nonexistent_path_skips_canonicalize() {
        // Non-existent paths (about-to-be-written files) skip canonicalize;
        // `lexical_normalize` already handles `..` traversal for those.
        let validator = SandboxValidator::new(
            SandboxMode::WorkspaceWrite,
            vec![PathBuf::from("/workspace")],
        );
        assert!(
            validator
                .validate_path_for_write(&PathBuf::from("/workspace/brand_new.md"))
                .is_ok(),
            "non-existent path must skip canonicalize and pass"
        );
        assert!(
            validator
                .validate_path_for_read(&PathBuf::from("/workspace/never_existed.md"))
                .is_ok(),
            "non-existent read path must skip canonicalize and pass"
        );
    }
}

/// OS sandbox profile derived from the active sandbox mode (D3, D6).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OsSandboxProfile {
    /// Workspace roots the process may READ (all modes except full access).
    pub readable_roots: Vec<PathBuf>,
    /// Subset the process may WRITE (empty in `ReadOnly` mode).
    pub writable_roots: Vec<PathBuf>,
    /// OS-layer network access (`[sandbox] network_access`).
    pub network: bool,
    /// False only for `DangerFullAccess` — sandboxing is bypassed entirely.
    pub sandboxed: bool,
}

impl OsSandboxProfile {
    pub fn from_mode(
        mode: SandboxMode,
        workspace_roots: Vec<PathBuf>,
        network_access: bool,
    ) -> Self {
        let (readable_roots, writable_roots, sandboxed) = match mode {
            SandboxMode::ReadOnly => (workspace_roots, Vec::new(), true),
            SandboxMode::WorkspaceWrite | SandboxMode::Ask => {
                (workspace_roots.clone(), workspace_roots, true)
            }
            SandboxMode::DangerFullAccess => (Vec::new(), Vec::new(), false),
        };
        Self {
            readable_roots,
            writable_roots,
            network: network_access,
            sandboxed,
        }
    }
}

/// Error returned when the OS sandbox cannot be applied.
#[derive(Debug, thiserror::Error)]
pub enum OsSandboxError {
    #[error("OS sandbox unavailable: {0}")]
    SandboxUnavailable(String),
}

/// Substrings in child stderr (and signal/exit codes) that indicate a
/// sandbox denial rather than a genuine program failure (D3/A1).
const DENIAL_KEYWORDS: &[&str] = &[
    "operation not permitted",
    "permission denied",
    "read-only file system",
    "sandbox",
    "landlock",
    "seccomp",
    "failed to write file",
];

/// Quick classifier: did the child exit because the sandbox denied it?
///
/// Matches the sandbox-exec/bwrap/seccomp failure signatures: exit codes
/// 2/126/127 (cannot exec / command not permitted), 128+SIGSYS (seccomp
/// kill), and stderr keywords.
pub fn is_likely_sandbox_denied(exit_code: Option<i32>, stderr: &str) -> bool {
    if let Some(code) = exit_code {
        if matches!(code, 2 | 126 | 127) {
            return true;
        }
        // SIGSYS (31) — seccomp/landlock violations terminate the process
        // with a signal, reported as 128 + signum in shell exit status.
        if code == 128 + 31 {
            return true;
        }
    }
    let lower = stderr.to_lowercase();
    DENIAL_KEYWORDS.iter().any(|k| lower.contains(k))
}

/// Wrap `cmd` so that spawning it runs inside the OS sandbox described by
/// `profile`, returning the wrapped command ready to `.spawn()`.
///
/// Fail-closed: macOS requires `/usr/bin/sandbox-exec`; if it is absent the
/// function returns [`OsSandboxError::SandboxUnavailable`]. Linux requires
/// `bubblewrap`; if absent the function returns [`OsSandboxError::SandboxUnavailable`].
/// On unsupported platforms, returns `Ok(cmd)` unchanged.
pub fn sandbox_spawn(
    cmd: Command,
    profile: &OsSandboxProfile,
    allow_network: bool,
) -> Result<Command, OsSandboxError> {
    #[cfg(target_os = "macos")]
    {
        let _ = allow_network;
        macos::wrap_sandbox_exec(cmd, profile)
    }
    #[cfg(target_os = "linux")]
    {
        linux::wrap_bubblewrap(cmd, profile, allow_network)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (profile, allow_network);
        Ok(cmd)
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;

    const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

    fn escape_sbpl_path(path: &std::path::Path) -> String {
        path.to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    }

    /// Generate a codex-derived Seatbelt profile (D19).
    ///
    /// Allowance set: process-exec/fork, sysctl-read, POSIX sem/shm, PTY,
    /// /dev devices, dyld/libSystem (read-only system paths), /tmp, Mach
    /// bootstrap. Workspace roots are readable; writable roots additionally
    /// writable. Network denied unless `profile.network`.
    pub(super) fn sandbox_exec_profile(profile: &OsSandboxProfile) -> String {
        let mut sb = String::new();
        sb.push_str("(version 1)\n");
        sb.push_str("(deny default)\n");

        sb.push_str("(allow process-exec)\n");
        sb.push_str("(allow process-fork)\n");
        sb.push_str("(allow sysctl-read)\n");
        sb.push_str("(allow ipc-posix-sem)\n");
        sb.push_str("(allow ipc-posix-shm)\n");

        sb.push_str("(allow mach-lookup)\n");
        sb.push_str("(allow file-read-metadata)\n");

        // Broad file-read: dynamic linker, dyld, system libs need to read
        // from /usr/lib, /System, /bin, etc. Allow reads from root.
        sb.push_str("(allow file-read* (subpath \"/\"))\n");

        // Device nodes (read-only).
        for dev in ["/dev/null", "/dev/zero", "/dev/random", "/dev/urandom"] {
            sb.push_str(&format!("(allow file-read* (literal \"{dev}\"))\n"));
        }

        // Temp scratch (read + write).
        sb.push_str("(allow file-write* (subpath \"/private/tmp\"))\n");

        // Workspace roots: writable roots also get write permission.
        for root in &profile.writable_roots {
            let root = escape_sbpl_path(root);
            sb.push_str(&format!("(allow file-write* (subpath \"{root}\"))\n"));
        }

        if profile.network {
            sb.push_str("(allow network*)\n");
        } else {
            sb.push_str("(deny network*)\n");
        }

        sb
    }

    pub(super) fn wrap_sandbox_exec(
        cmd: Command,
        profile: &OsSandboxProfile,
    ) -> Result<Command, OsSandboxError> {
        if !std::path::Path::new(SANDBOX_EXEC).exists() {
            return Err(OsSandboxError::SandboxUnavailable(
                "sandbox-exec not found; refusing unsandboxed spawn".to_string(),
            ));
        }

        let program = cmd.get_program().to_os_string();
        let args: Vec<std::ffi::OsString> = cmd.get_args().map(|a| a.to_os_string()).collect();
        let envs: Vec<(std::ffi::OsString, Option<std::ffi::OsString>)> = cmd
            .get_envs()
            .map(|(k, v)| (k.to_os_string(), v.map(|s| s.to_os_string())))
            .collect();
        let cwd = cmd.get_current_dir().map(|p| p.to_path_buf());

        let sbpl = sandbox_exec_profile(profile);

        let mut wrapped = Command::new(SANDBOX_EXEC);
        wrapped
            .arg("-p")
            .arg(sbpl)
            .arg("--")
            .arg(program)
            .args(args);
        for (k, v) in envs {
            match v {
                Some(v) => wrapped.env(k, v),
                None => wrapped.env_remove(k),
            };
        }
        if let Some(dir) = cwd {
            wrapped.current_dir(dir);
        }

        Ok(wrapped)
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;

    const BWRAP: &str = "bwrap";

    pub(super) fn wrap_bubblewrap(
        cmd: Command,
        profile: &OsSandboxProfile,
        allow_network: bool,
    ) -> Result<Command, OsSandboxError> {
        if which_bwrap().is_none() {
            return Err(OsSandboxError::SandboxUnavailable(
                "bubblewrap (bwrap) not found; install via: sudo apt install bubblewrap"
                    .to_string(),
            ));
        }

        let program = cmd.get_program().to_os_string();
        let args: Vec<std::ffi::OsString> = cmd.get_args().map(|a| a.to_os_string()).collect();
        let envs: Vec<(std::ffi::OsString, Option<std::ffi::OsString>)> = cmd
            .get_envs()
            .map(|(k, v)| (k.to_os_string(), v.map(|s| s.to_os_string())))
            .collect();
        let cwd = cmd.get_current_dir().map(|p| p.to_path_buf());

        let mut wrapped = Command::new(BWRAP);
        wrapped
            .arg("--ro-bind")
            .arg("/")
            .arg("/")
            .arg("--dev")
            .arg("/dev")
            .arg("--proc")
            .arg("/proc")
            .arg("--unshare-pid")
            .args(["--die-with-parent", "--new-session"]);

        if !allow_network {
            wrapped.arg("--unshare-net");
        }

        for root in &profile.readable_roots {
            let root_str = root.to_str().ok_or_else(|| {
                OsSandboxError::SandboxUnavailable(format!(
                    "workspace root {} is not valid UTF-8",
                    root.display()
                ))
            })?;
            if profile.writable_roots.contains(root) {
                wrapped.args(["--bind", root_str, root_str]);
            } else {
                wrapped.args(["--ro-bind", root_str, root_str]);
            }
        }

        // Pass --chdir to bubblewrap before the -- separator so the sandboxed
        // process starts in the correct working directory.
        if let Some(ref dir) = cwd {
            let dir_str = dir.to_str().ok_or_else(|| {
                OsSandboxError::SandboxUnavailable(format!(
                    "cwd {} is not valid UTF-8",
                    dir.display()
                ))
            })?;
            wrapped.args(["--chdir", dir_str]);
        }

        wrapped.arg("--").arg(program).args(args);
        for (k, v) in envs {
            match v {
                Some(v) => wrapped.env(k, v),
                None => wrapped.env_remove(k),
            };
        }

        Ok(wrapped)
    }

    fn which_bwrap() -> Option<PathBuf> {
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths).find_map(|dir| {
                let full = dir.join(BWRAP);
                if full.is_file() { Some(full) } else { None }
            })
        })
    }
}

#[cfg(test)]
mod os_sandbox_tests {
    use super::*;

    fn roots() -> Vec<PathBuf> {
        vec![PathBuf::from("/workspace")]
    }

    #[test]
    fn read_only_mode_yields_no_writable_roots() {
        let p = OsSandboxProfile::from_mode(SandboxMode::ReadOnly, roots(), false);
        assert_eq!(p.readable_roots, roots());
        assert!(p.writable_roots.is_empty());
        assert!(!p.network);
        assert!(p.sandboxed);
    }

    #[test]
    fn workspace_write_mode_yields_writable_roots() {
        let p = OsSandboxProfile::from_mode(SandboxMode::WorkspaceWrite, roots(), false);
        assert_eq!(p.writable_roots, roots());
        assert!(p.sandboxed);
    }

    #[test]
    fn ask_mode_matches_workspace_write() {
        let p = OsSandboxProfile::from_mode(SandboxMode::Ask, roots(), false);
        assert_eq!(p.writable_roots, roots());
        assert!(p.sandboxed);
    }

    #[test]
    fn full_access_mode_is_not_sandboxed() {
        let p = OsSandboxProfile::from_mode(SandboxMode::DangerFullAccess, roots(), false);
        assert!(!p.sandboxed);
        assert!(p.readable_roots.is_empty());
        assert!(p.writable_roots.is_empty());
    }

    #[test]
    fn network_flag_preserved() {
        let p = OsSandboxProfile::from_mode(SandboxMode::WorkspaceWrite, roots(), true);
        assert!(p.network);
    }

    #[test]
    fn sandbox_denial_exit_codes_detected() {
        for code in [2, 126, 127, 128 + 31] {
            assert!(
                is_likely_sandbox_denied(Some(code), ""),
                "exit {code} must be flagged as denial"
            );
        }
        assert!(!is_likely_sandbox_denied(Some(0), ""));
        assert!(!is_likely_sandbox_denied(Some(1), ""));
    }

    #[test]
    fn sandbox_denial_stderr_keywords_detected() {
        assert!(is_likely_sandbox_denied(None, "Operation not permitted"));
        assert!(is_likely_sandbox_denied(None, "read-only file system"));
        assert!(is_likely_sandbox_denied(None, "seccomp violation"));
        assert!(!is_likely_sandbox_denied(None, "file not found"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sbpl_profile_contains_expected_rules() {
        let p = OsSandboxProfile::from_mode(SandboxMode::WorkspaceWrite, roots(), false);
        let sbpl = macos::sandbox_exec_profile(&p);

        assert!(sbpl.contains("(version 1)"));
        assert!(sbpl.contains("(deny default)"));
        assert!(sbpl.contains("(allow process-exec)"));
        assert!(sbpl.contains("(allow sysctl-read)"));
        assert!(sbpl.contains("(allow mach-lookup)"));
        assert!(sbpl.contains("(deny network*)"));
        assert!(sbpl.contains("(allow file-read* (subpath \"/\"))"));
        assert!(sbpl.contains("(allow file-write* (subpath \"/workspace\"))"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sbpl_network_flag_flips_deny_to_allow() {
        let p = OsSandboxProfile::from_mode(SandboxMode::WorkspaceWrite, roots(), true);
        let sbpl = macos::sandbox_exec_profile(&p);
        assert!(sbpl.contains("(allow network*)"));
        assert!(!sbpl.contains("(deny network*)"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sbpl_read_only_omits_write_rules() {
        let p = OsSandboxProfile::from_mode(SandboxMode::ReadOnly, roots(), false);
        let sbpl = macos::sandbox_exec_profile(&p);
        assert!(sbpl.contains("(allow file-read* (subpath \"/\"))"));
        assert!(!sbpl.contains("(allow file-write* (subpath \"/workspace\"))"));
    }
}
