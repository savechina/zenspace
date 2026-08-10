use std::collections::HashSet;
use std::path::{Path, PathBuf};
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

    for host in BLOCKED_NETWORK_COMMANDS {
        let pattern = format!("{} ", host);
        if cmd.contains(&pattern) {
            let after_cmd_start = cmd.find(&pattern).unwrap_or(0) + pattern.len();
            let after_cmd = &cmd[after_cmd_start..];
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

#[derive(Debug, Clone)]
pub struct SeatbeltPolicy {
    pub mode: SandboxMode,
    pub workspace_roots: Vec<PathBuf>,
    pub timeout_secs: u64,
}

impl SeatbeltPolicy {
    pub fn new(mode: SandboxMode, workspace_roots: Vec<PathBuf>) -> Self {
        Self {
            mode,
            workspace_roots,
            timeout_secs: 300,
        }
    }

    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
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
        profile.push_str("(allow file-read-mode)\n");

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
        if let Some(args_str) = invocation.args.get("command").and_then(|v| v.as_str()) {
            if let Some(pattern) = is_blocked_command(args_str) {
                return Ok(ToolDispatchAction::Terminate {
                    reason: format!("sandbox blocked command matching pattern: \"{}\"", pattern),
                });
            }

            if is_dangerous_network_target(args_str) && self.mode != SandboxMode::DangerFullAccess {
                return Ok(ToolDispatchAction::Terminate {
                    reason: "sandbox blocked network command to unknown host".to_string(),
                });
            }
        }

        if let Some(path_str) = invocation.args.get("path").and_then(|v| v.as_str()) {
            let path = PathBuf::from(path_str);
            if is_metadata_path(&path)
                && let Some(op) = invocation.args.get("operation").and_then(|v| v.as_str())
                && matches!(op, "write" | "delete" | "remove" | "modify")
            {
                return Ok(ToolDispatchAction::Terminate {
                    reason: format!(
                        "sandbox blocked write operation on metadata path: \"{}\"",
                        path_str
                    ),
                });
            }
        }

        Ok(ToolDispatchAction::Continue)
    }
}

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

        Ok(())
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
        use libc::{RLIMIT_CORE, RLIMIT_NOFILE, RLIMIT_NPROC, rlimit, setrlimit};

        unsafe {
            let rlimit_nproc = rlimit {
                rlim_cur: 50,
                rlim_max: 50,
            };
            if setrlimit(RLIMIT_NPROC, &rlimit_nproc) != 0 {
                return Err(std::io::Error::last_os_error());
            }

            let rlimit_nofile = rlimit {
                rlim_cur: 256,
                rlim_max: 256,
            };
            if setrlimit(RLIMIT_NOFILE, &rlimit_nofile) != 0 {
                return Err(std::io::Error::last_os_error());
            }

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
}
