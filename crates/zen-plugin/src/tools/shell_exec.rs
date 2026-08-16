use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

#[derive(Clone)]
pub struct ShellExecTool {
    workspace_root: PathBuf,
}

// CHK024 code-audit note (2026-08-16): v0.0.6 does NOT wrap execution with
// sandbox-exec (macOS) / bubblewrap (Linux) — the FR-028 sandbox composition
// is deferred. Fail-closed is enforced instead at the seatbelt hook layer:
// `binary` is registered as a command arg (wiring.rs), so `check_command_arg`
// terminates blocked network binaries (curl/wget) pre-dispatch, before any
// spawn. The tool NEVER falls back to unsandboxed execution on missing
// sandbox tooling — it simply never attempts one; spawn only happens for
// binaries the seatbelt allowed. See spec.md FR-028 Clarification 2026-08-15.

const NAME: &str = "shell.exec";
const DESCRIPTION: &str =
    "Execute a binary with an argv array, optional stdin, env overrides, and a timeout";

static ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "binary": { "type": "string", "description": "Absolute path or PATH-resolved binary name" },
            "args": { "type": "array", "items": { "type": "string" }, "description": "argv array, NOT a shell string" },
            "cwd": { "type": "string", "description": "Working directory (default: workspace root)" },
            "timeout_ms": { "type": "integer", "description": "Timeout in ms (default 30000, clamped 1000..=600000)" },
            "stdin": { "type": "string", "description": "Content written to child stdin, then closed" },
            "env": { "type": "object", "additionalProperties": { "type": "string" }, "description": "Env vars injected into the child (the only env injection path; parent secrets are scrubbed)" }
        },
        "required": ["binary"]
    })
});

static RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "exit_code": { "type": "integer" },
            "stdout": { "type": "string" },
            "stderr": { "type": "string" },
            "timed_out": { "type": "boolean" },
            "duration_ms": { "type": "integer" }
        }
    })
});

const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const MIN_TIMEOUT_MS: u64 = 1_000;
const MAX_TIMEOUT_MS: u64 = 600_000;
const MAX_OUTPUT_CHARS: usize = 10_000;
// Byte cap for drain reads, sized well above MAX_OUTPUT_CHARS so the char
// truncation is never lossy before it happens; bounds memory against
// runaway children.
const MAX_OUTPUT_BYTES: u64 = 64 * 1024;

impl ShellExecTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }
}

/// Resolve a binary: absolute paths pass through, otherwise search PATH.
fn resolve_binary(binary: &str) -> Result<PathBuf, KernelError> {
    let path = Path::new(binary);
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    which::which(binary)
        .map_err(|_| KernelError::InvalidArgument(format!("Binary not found in PATH: {binary}")))
}

/// Lossy-decode output and truncate to a bounded number of chars.
fn truncate_output(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .take(MAX_OUTPUT_CHARS)
        .collect()
}

/// Await a spawned output-reader task, defaulting to empty on failure.
async fn join_output(task: Option<tokio::task::JoinHandle<Vec<u8>>>) -> Vec<u8> {
    match task {
        Some(handle) => handle.await.unwrap_or_default(),
        None => Vec::new(),
    }
}

#[async_trait]
impl Tool for ShellExecTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: NAME.to_string(),
            description: DESCRIPTION.to_string(),
            args_schema: ARGS_SCHEMA.clone(),
            result_schema: RESULT_SCHEMA.clone(),
        }
    }

    async fn invoke(&self, args: Value) -> Result<Value, KernelError> {
        #[cfg(windows)]
        {
            return Ok(json!({ "error": "shell.exec unsupported on Windows" }));
        }

        let binary = args["binary"].as_str().ok_or_else(|| {
            KernelError::InvalidArgument("Missing or invalid 'binary' field".into())
        })?;
        if binary.trim().is_empty() {
            return Err(KernelError::InvalidArgument(
                "'binary' must not be empty".into(),
            ));
        }

        let argv: Vec<String> = args
            .get("args")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let cwd = args
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.workspace_root.clone());

        let timeout_ms = args
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS);

        let stdin_data = args.get("stdin").and_then(|v| v.as_str()).map(String::from);

        let resolved = resolve_binary(binary)?;

        let mut cmd = Command::new(&resolved);
        cmd.args(&argv)
            .current_dir(&cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(if stdin_data.is_some() {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            });

        // FR-037: the child env is the parent env minus secret-bearing vars,
        // plus the explicit `env` arg — the ONLY injection path.
        let inject_map: HashMap<String, String> = args
            .get("env")
            .and_then(|v| v.as_object())
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        let scrubbed = zen_core::env_scrub::scrubbed_env(
            &inject_map,
            &zen_core::env_scrub::EnvScrubConfig::default(),
        );
        cmd.env_clear().envs(&scrubbed);

        // New process group so the whole tree can be killed on timeout.
        #[cfg(unix)]
        cmd.process_group(0);

        tracing::debug!(tool = NAME, binary, ?argv, ?cwd, timeout_ms, "spawning");

        let mut child = cmd
            .spawn()
            .map_err(|e| KernelError::ToolFailed(format!("Failed to spawn {binary}: {e}")))?;
        // Capture the pid before any kill: tokio's `kill()` reaps the child,
        // after which `id()` returns None.
        let child_pid = child.id();

        // Drain stdout/stderr BEFORE writing stdin: a child that writes more
        // than the pipe capacity (64KB) before consuming its stdin would
        // otherwise block the parent's stdin write forever (pipe deadlock).
        // Reads are capped at MAX_OUTPUT_BYTES so a runaway child cannot
        // exhaust host memory; MAX_OUTPUT_CHARS truncation happens later.
        let stdout_task = child.stdout.take().map(|out| {
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let _ = out.take(MAX_OUTPUT_BYTES).read_to_end(&mut buf).await;
                buf
            })
        });
        let stderr_task = child.stderr.take().map(|err| {
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let _ = err.take(MAX_OUTPUT_BYTES).read_to_end(&mut buf).await;
                buf
            })
        });

        // Write stdin (if any) then close the handle so the child sees EOF.
        if let (Some(data), Some(mut stdin)) = (stdin_data, child.stdin.take()) {
            if let Err(e) = stdin.write_all(data.as_bytes()).await {
                tracing::warn!(tool = NAME, error = %e, "failed to write stdin");
            }
            drop(stdin);
        }

        let start = Instant::now();
        let waited = tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait()).await;

        match waited {
            Ok(Ok(status)) => {
                let stdout = join_output(stdout_task).await;
                let stderr = join_output(stderr_task).await;
                let duration_ms = start.elapsed().as_millis() as u64;
                tracing::debug!(
                    tool = NAME,
                    exit_code = status.code(),
                    duration_ms,
                    "exited"
                );
                Ok(json!({
                    "exit_code": status.code().unwrap_or(-1),
                    "stdout": truncate_output(&stdout),
                    "stderr": truncate_output(&stderr),
                    "timed_out": false,
                    "duration_ms": duration_ms
                }))
            }
            Ok(Err(e)) => Err(KernelError::ToolFailed(format!(
                "Failed to wait for {binary}: {e}"
            ))),
            Err(_elapsed) => {
                tracing::warn!(tool = NAME, binary, timeout_ms, "timed out, killing");
                // Kill the whole process group first so descendants die too.
                #[cfg(unix)]
                if let Some(pid) = child_pid {
                    unsafe {
                        libc::kill(-(pid as i32), libc::SIGKILL);
                    }
                }
                // SIGKILL the child and reap it (tokio's kill waits for exit).
                let _ = child.kill().await;
                let stdout = join_output(stdout_task).await;
                let stderr = join_output(stderr_task).await;
                Ok(json!({
                    "exit_code": -1,
                    "stdout": truncate_output(&stdout),
                    "stderr": truncate_output(&stderr),
                    "timed_out": true,
                    "duration_ms": timeout_ms
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> ShellExecTool {
        ShellExecTool::new(PathBuf::from("/"))
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn happy_path_git_status() {
        let res = tool()
            .invoke(json!({
                "binary": "/bin/echo",
                "args": ["hello"]
            }))
            .await
            .unwrap();
        assert_eq!(res["exit_code"], 0);
        assert!(res["stdout"].as_str().unwrap().contains("hello"));
        assert_eq!(res["timed_out"], false);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn timeout_kills_process() {
        let start = Instant::now();
        let res = tool()
            .invoke(json!({
                "binary": "/bin/sleep",
                "args": ["30"],
                "timeout_ms": 500
            }))
            .await
            .unwrap();
        assert_eq!(res["timed_out"], true);
        assert_eq!(res["exit_code"], -1);
        assert!(
            start.elapsed().as_secs() < 10,
            "timeout should return quickly"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn stdin_piped() {
        let res = tool()
            .invoke(json!({
                "binary": "/bin/cat",
                "args": [],
                "stdin": "data"
            }))
            .await
            .unwrap();
        assert_eq!(res["exit_code"], 0);
        assert!(res["stdout"].as_str().unwrap().contains("data"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn missing_binary_errors() {
        let res = tool()
            .invoke(json!({
                "binary": "definitely-not-a-real-binary-xyz"
            }))
            .await;
        assert!(res.is_err());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn env_contains_no_secrets_and_injects_only_via_env_arg() {
        // SAFETY: single-threaded test; std::env::set_var is process-global.
        unsafe {
            std::env::set_var("ZEN_TEST_SCRUB_API_KEY", "leak-me");
        }
        let res = tool()
            .invoke(json!({
                "binary": "/usr/bin/env",
                "args": [],
                "env": { "ZEN_TEST_INJECTED": "present" }
            }))
            .await
            .unwrap();
        let stdout = res["stdout"].as_str().unwrap();
        assert!(
            stdout.contains("ZEN_TEST_INJECTED=present"),
            "env arg must be the injection path: {stdout}"
        );
        assert!(
            !stdout.contains("ZEN_TEST_SCRUB_API_KEY"),
            "secret leaked into child env: {stdout}"
        );
        unsafe {
            std::env::remove_var("ZEN_TEST_SCRUB_API_KEY");
        }
    }
}
