use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use crate::task_registry;
use async_trait::async_trait;
use rig_compose::normalizer::{ToolDispatchAction, ToolInvocation};
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zen_core::sandbox::SandboxValidator;
use zen_core::sandbox::{
    OsSandboxProfile, PROTECTED_PATHS, SandboxMode, SeatbeltPolicy, ToolArgRegistry, is_env_file,
    is_likely_sandbox_denied, sandbox_spawn,
};

#[derive(Clone)]
pub struct ShellExecTool {
    workspace_root: PathBuf,
    sandbox_validator: SandboxValidator,
    sandbox_profile: OsSandboxProfile,
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
const DESCRIPTION: &str = "Execute a binary with an argv array, optional stdin, env overrides, and a timeout; background:true returns a task_id immediately and the child keeps running (retrieve with shell.collect, stop with shell.kill)";

static ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "binary": { "type": "string", "description": "Absolute path or PATH-resolved binary name" },
            "args": { "type": "array", "items": { "type": "string" }, "description": "argv array, NOT a shell string" },
            "cwd": { "type": "string", "description": "Working directory (default: workspace root)" },
            "timeout_ms": { "type": "integer", "description": "Timeout in ms (default 30000, clamped 1000..=600000)" },
            "stdin": { "type": "string", "description": "Content written to child stdin, then closed" },
            "env": { "type": "object", "additionalProperties": { "type": "string" }, "description": "Env vars injected into the child (the only env injection path; parent secrets are scrubbed)" },
            "background": { "type": "boolean", "description": "Run detached: return a task_id immediately, drain output to logs/tasks/<task_id>/, enforce [agentic.background] wall-clock deadline (default false)" }
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
            "duration_ms": { "type": "integer" },
            "sandbox_denied": { "type": "boolean" },
            "task_id": { "type": "string" },
            "status": { "type": "string" },
            "output_dir": { "type": "string" },
            "stdout_path": { "type": "string" },
            "stderr_path": { "type": "string" },
            "note": { "type": "string" }
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

const SHELL_BINARIES: &[&str] = &["sh", "bash", "zsh", "dash"];
const MAX_SUBSTITUTION_DEPTH: usize = 16;

/// One command unit extracted from a compound shell `-c` string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompoundSegment {
    /// Trimmed source text, used verbatim in denial messages.
    pub(crate) text: String,
    /// POSIX tokens (quotes resolved) used for path inspection.
    pub(crate) tokens: Vec<String>,
}

/// Decompose a shell `-c` command string into independently validatable units.
///
/// GIVEN an embedded `sh -c` command string,
/// WHEN it is lexically decomposable with POSIX quote rules,
/// THEN each command joined by `&&`, `||`, `;`, `|`, `&`, or a newline becomes
/// its own [`CompoundSegment`], and each `$(...)` / backtick substitution body
/// is flattened into additional units while the enclosing command keeps the
/// original text (so the outer head command is still inspected).
///
/// Returns `None` when a high-confidence split is impossible — unmatched
/// quotes, a trailing escape, heredocs, or process/arithmetic substitution.
/// Callers must then evaluate the whole string as a single unit.
pub(crate) fn decompose_compound(cmd: &str) -> Option<Vec<CompoundSegment>> {
    let chars: Vec<char> = cmd.chars().collect();
    decompose_chars(&chars, 0)
}

fn decompose_chars(chars: &[char], depth: usize) -> Option<Vec<CompoundSegment>> {
    if depth > MAX_SUBSTITUTION_DEPTH {
        return None;
    }
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];

        if in_single {
            current.push(c);
            if c == '\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }

        if in_double {
            match c {
                '\\' => {
                    current.push(c);
                    current.push(*chars.get(i + 1)?);
                    i += 2;
                }
                '"' => {
                    current.push(c);
                    in_double = false;
                    i += 1;
                }
                '`' => {
                    let (end, inner) = scan_backtick(chars, i)?;
                    segments.extend(decompose_chars(&inner, depth + 1)?);
                    current.push_str(&chars[i..end].iter().collect::<String>());
                    i = end;
                }
                '$' if chars.get(i + 1) == Some(&'(') => {
                    if chars.get(i + 2) == Some(&'(') {
                        return None;
                    }
                    let (end, inner) = scan_dollar_paren(chars, i)?;
                    segments.extend(decompose_chars(&inner, depth + 1)?);
                    current.push_str(&chars[i..end].iter().collect::<String>());
                    i = end;
                }
                _ => {
                    current.push(c);
                    i += 1;
                }
            }
            continue;
        }

        match c {
            '\'' => {
                in_single = true;
                current.push(c);
                i += 1;
            }
            '"' => {
                in_double = true;
                current.push(c);
                i += 1;
            }
            '\\' => {
                current.push(c);
                current.push(*chars.get(i + 1)?);
                i += 2;
            }
            '`' => {
                let (end, inner) = scan_backtick(chars, i)?;
                segments.extend(decompose_chars(&inner, depth + 1)?);
                current.push_str(&chars[i..end].iter().collect::<String>());
                i = end;
            }
            '$' if chars.get(i + 1) == Some(&'(') => {
                if chars.get(i + 2) == Some(&'(') {
                    return None;
                }
                let (end, inner) = scan_dollar_paren(chars, i)?;
                segments.extend(decompose_chars(&inner, depth + 1)?);
                current.push_str(&chars[i..end].iter().collect::<String>());
                i = end;
            }
            '<' if chars.get(i + 1) == Some(&'<') => return None,
            '<' | '>' if chars.get(i + 1) == Some(&'(') => return None,
            '\n' | '\r' | ';' => {
                flush_segment(&mut segments, &mut current)?;
                i += 1;
            }
            '&' => {
                if chars.get(i + 1) == Some(&'&') {
                    flush_segment(&mut segments, &mut current)?;
                    i += 2;
                } else if current.trim_end().ends_with('>') || chars.get(i + 1) == Some(&'>') {
                    current.push(c);
                    i += 1;
                } else {
                    flush_segment(&mut segments, &mut current)?;
                    i += 1;
                }
            }
            '|' => {
                flush_segment(&mut segments, &mut current)?;
                if chars.get(i + 1) == Some(&'|') {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            _ => {
                current.push(c);
                i += 1;
            }
        }
    }

    if in_single || in_double {
        return None;
    }
    flush_segment(&mut segments, &mut current)?;
    Some(segments)
}

fn flush_segment(segments: &mut Vec<CompoundSegment>, current: &mut String) -> Option<()> {
    let raw = current.trim();
    if raw.is_empty() {
        current.clear();
        return Some(());
    }
    let tokens = shlex::split(raw)?;
    segments.push(CompoundSegment {
        text: raw.to_string(),
        tokens,
    });
    current.clear();
    Some(())
}

fn scan_dollar_paren(chars: &[char], start: usize) -> Option<(usize, Vec<char>)> {
    let mut depth = 1usize;
    let mut quote: Option<char> = None;
    let mut j = start + 2;
    while j < chars.len() {
        let c = chars[j];
        match quote {
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                }
            }
            Some('"') => {
                if c == '\\' {
                    j += 1;
                } else if c == '"' {
                    quote = None;
                }
            }
            Some(_) => {}
            None => match c {
                '\'' | '"' => quote = Some(c),
                '\\' => j += 1,
                '`' => {
                    let (end, _) = scan_backtick(chars, j)?;
                    j = end;
                    continue;
                }
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((j + 1, chars[start + 2..j].to_vec()));
                    }
                }
                _ => {}
            },
        }
        j += 1;
    }
    None
}

fn scan_backtick(chars: &[char], start: usize) -> Option<(usize, Vec<char>)> {
    let mut j = start + 1;
    while j < chars.len() {
        match chars[j] {
            '\\' => j += 2,
            '`' => return Some((j + 1, chars[start + 1..j].to_vec())),
            _ => j += 1,
        }
    }
    None
}

/// Return the embedded command string when `binary` is a shell invoked with a
/// `-c`-style flag (`-c`, `-lc`, `-ec`, …) whose value follows it.
fn shell_command_string<'a>(binary: &str, argv: &'a [String]) -> Option<&'a str> {
    let shell = Path::new(binary).file_name()?.to_string_lossy();
    if !SHELL_BINARIES.contains(&shell.as_ref()) {
        return None;
    }
    let flag = argv.iter().position(|arg| {
        arg.strip_prefix('-')
            .filter(|cluster| !cluster.is_empty() && !cluster.starts_with('-'))
            .is_some_and(|cluster| cluster.contains('c'))
    })?;
    argv.get(flag + 1).map(String::as_str)
}

/// Reject runtime-expansion shapes the per-segment scanner cannot statically
/// validate, wherever the bytes are NOT single-quoted-literal (D4 seal). A
/// `$var` lookup, `$(...)` substitution or backtick executes once the shell
/// expands it — the executed text (fed via the `env` arg, for example) can be
/// any string the literal token checks would have rejected, so `sh -c '$P'`
/// with `{"env":{"P":"sudo cat ~/.ssh/id_ed25519"}}` would sail through the
/// scanner. Single-quoted bytes are literal in POSIX sh, so `echo '$HOME'`
/// still passes.
#[derive(PartialEq, Copy, Clone)]
enum QuoteState {
    Plain,
    Single,
    Double,
}

fn unquoted_expansion_reason(text: &str) -> Option<&'static str> {
    let mut state = QuoteState::Plain;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match state {
            QuoteState::Plain => match c {
                '\'' => state = QuoteState::Single,
                '"' => state = QuoteState::Double,
                '$' | '`' => {
                    return Some(
                        "runtime expansion outside single quotes ($var / $(...) / \\`) executes text the per-segment scanner never validated",
                    );
                }
                '\\' => {
                    chars.next();
                }
                _ => {}
            },
            QuoteState::Single => {
                if c == '\'' {
                    state = QuoteState::Plain;
                }
            }
            QuoteState::Double => match c {
                '"' => state = QuoteState::Plain,
                '\\' => {
                    chars.next();
                }
                '$' | '`' => {
                    return Some(
                        "runtime expansion inside double quotes ($var / $(...) / \\`) executes text the per-segment scanner never validated",
                    );
                }
                _ => {}
            },
        }
    }
    None
}

/// The segment text minus all quoted spans (D11): the bytes a POSIX shell
/// treats as unquoted command words. Quoted contents never influence which
/// blocked pattern a segment matches.
fn unquoted_remainder(text: &str) -> String {
    let mut state = QuoteState::Plain;
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match state {
            QuoteState::Plain => match c {
                '\'' => state = QuoteState::Single,
                '"' => state = QuoteState::Double,
                '\\' => out.push('\\'),
                _ => out.push(c),
            },
            QuoteState::Single => {
                if c == '\'' {
                    state = QuoteState::Plain;
                }
            }
            QuoteState::Double => match c {
                '"' => state = QuoteState::Plain,
                '\\' => {
                    // Backslash escapes the NEXT byte inside double quotes;
                    // consume it so `\"` cannot smuggle the quote out.
                    chars.next();
                }
                _ => {}
            },
        }
    }
    // Unmatched opening quote: the scanner's span model is invalid here.
    // Conservatively treat the WHOLE text as unquoted (match the fallback
    // contract — nothing gets hidden by the never-closed quote).
    if state != QuoteState::Plain {
        return text.to_string();
    }
    out
}

/// Tokens naming a protected metadata path or env file, which the sandbox
/// validator must reject (`cat ~/.ssh/id_ed25519`, `cat .env`).
fn protected_path_candidate(token: &str) -> Option<&str> {
    let value = match token.split_once('=') {
        Some((flag, value)) if flag.starts_with('-') => value,
        _ => token,
    };
    if value.is_empty() {
        return None;
    }
    let path = Path::new(value);
    let protected = is_env_file(path)
        || path.components().any(|component| {
            let name = component.as_os_str().to_string_lossy();
            PROTECTED_PATHS.contains(&name.as_ref())
        });
    protected.then_some(value)
}

impl ShellExecTool {
    /// A token that reads as a path AND carries glob metachars could shell-
    /// expand onto a protected path whose literal component the token hides
    /// (`~/.[s]sh/id_rsa` has no literal `.ssh` component). Conservative:
    /// reject when any component, read as a glob pattern, matches a protected
    /// name; an uncompilable pattern is treated as a match (fail-closed).
    fn glob_protected_rejection(raw_token: &str) -> Result<(), KernelError> {
        // Path-anchored only: a bare `ls *` stays allowed; relative/absolute
        // tokens are the ones a shell can expand onto protected names.
        if !(raw_token.contains('/') || raw_token.starts_with('~')) {
            return Ok(());
        }
        let hit = Path::new(raw_token).components().any(|component| {
            let name = component.as_os_str().to_string_lossy();
            match glob::Pattern::new(&name) {
                Ok(pattern) => PROTECTED_PATHS
                    .iter()
                    .any(|protected| pattern.matches(protected)),
                Err(_) => true,
            }
        });
        if hit {
            return Err(KernelError::InvalidArgument(
                "shell.exec path token uses glob characters that can select a protected path (e.g. ~/.ssh); use explicit literal paths instead".to_string(),
            ));
        }
        Ok(())
    }

    pub fn new(
        workspace_root: PathBuf,
        sandbox_validator: SandboxValidator,
        sandbox_profile: OsSandboxProfile,
    ) -> Self {
        Self {
            workspace_root,
            sandbox_validator,
            sandbox_profile,
        }
    }

    /// Gate every sub-command of an embedded `sh -c` string through the same
    /// checks the seatbelt applies to the `binary` arg (FR-042).
    fn validate_embedded_command(&self, command: &str) -> Result<(), KernelError> {
        let segments = decompose_compound(command).unwrap_or_else(|| {
            // shlex failure must never narrow the gate: fall back to rough
            // whitespace tokens so eval/protected scans still see candidates
            // (conservative — rough tokens may over-match a flag value).
            let tokens = shlex::split(command)
                .unwrap_or_else(|| command.split_whitespace().map(str::to_string).collect());
            vec![CompoundSegment {
                text: command.trim().to_string(),
                tokens,
            }]
        });
        let policy = self.segment_gate();

        for segment in &segments {
            if segment.text.is_empty() {
                continue;
            }
            if let Some(reason) = unquoted_expansion_reason(&segment.text) {
                return Err(segment_denied(segment, reason));
            }
            if segment.tokens.iter().any(|token| token == "eval") {
                return Err(segment_denied(
                    segment,
                    "'eval' re-parses its argument as a shell command, defeating per-segment validation",
                ));
            }
            // The blocked-command check runs on the segment's UNQUOTED
            // remainder: quote-aware scanning strips quoted spans first, so
            // `grep "sudo " log` scans as `grep log` (the quoted word is a
            // search pattern, not a command), while unquoted dangerous
            // patterns (`sudo id`) still hit the pattern list.
            let unquoted = unquoted_remainder(&segment.text);
            if !unquoted.is_empty() {
                let invocation = ToolInvocation {
                    name: NAME.to_string(),
                    args: json!({ "binary": unquoted.as_str() }),
                };
                if let ToolDispatchAction::Terminate { reason } = policy
                    .validate_invocation(&invocation)
                    .map_err(|e| KernelError::ToolFailed(format!("segment gate failed: {e}")))?
                {
                    return Err(segment_denied(segment, reason));
                }
            }
            // Flag VALUE tokens are patterns/limits from the caller's own
            // argv (e.g. `find . -name .git`), not paths the child opens, so
            // the protected-path scan skips them.
            let mut prev_token_was_flag = false;
            for token in &segment.tokens {
                let is_flag = token.starts_with('-') && token.len() > 1;
                let is_flag_value = prev_token_was_flag && !is_flag && !token.contains('=');
                prev_token_was_flag = is_flag;
                if is_flag_value {
                    continue;
                }
                let Some(path) = protected_path_candidate(token) else {
                    Self::glob_protected_rejection(token)?;
                    continue;
                };
                let check = self
                    .sandbox_validator
                    .validate_path_for_read(Path::new(path));
                if let Err(reason) = check {
                    return Err(segment_denied(segment, reason));
                }
            }
        }
        Ok(())
    }

    /// The seatbelt gate mirrors wiring: `shell.exec` maps its command-bearing
    /// arg `binary` to the command check. `sandboxed` is false only for
    /// `DangerFullAccess`, which is also the only mode where the seatbelt skips
    /// the network-target check.
    fn segment_gate(&self) -> SeatbeltPolicy {
        let mode = if self.sandbox_profile.sandboxed {
            SandboxMode::WorkspaceWrite
        } else {
            SandboxMode::DangerFullAccess
        };
        let mut registry = ToolArgRegistry::new();
        registry.register_tool_args(NAME, &[], &["binary"]);
        SeatbeltPolicy::new(mode, self.sandbox_profile.readable_roots.clone())
            .with_arg_registry(registry)
    }

    /// Register a validated command as a background task (design Section 5).
    ///
    /// All validation gates (binary resolution, cwd, embedded-command scan,
    /// protected-path scan, env scrub, OS sandbox wrap, process group 0) have
    /// already run in `invoke` — this method only differs from the foreground
    /// path in ownership: the registry takes the Command and holds the Child.
    async fn spawn_background_task(
        &self,
        argv: Vec<String>,
        cmd: tokio::process::Command,
        binary: &str,
    ) -> Result<Value, KernelError> {
        let limits = match zen_core::config::load_config() {
            Ok(config) => task_registry::BackgroundLimits {
                max_wall_secs: config.agentic.background.max_wall_secs_or_default(),
                max_concurrent: config.agentic.background.max_concurrent_or_default(),
                output_cap_bytes: task_registry::OUTPUT_CAP_BYTES,
            },
            Err(_) => {
                let defaults = zen_core::config::BackgroundConfig::default();
                task_registry::BackgroundLimits {
                    max_wall_secs: defaults.max_wall_secs_or_default(),
                    max_concurrent: defaults.max_concurrent_or_default(),
                    output_cap_bytes: task_registry::OUTPUT_CAP_BYTES,
                }
            }
        };
        let session_id = task_registry::current_session_scope();
        let command_digest = task_registry::make_command_digest(binary, &argv);
        match task_registry::registry().spawn_background(cmd, &session_id, &command_digest, &limits)
        {
            Ok(snapshot) => {
                let output_dir = snapshot.output_dir.clone();
                let stdout_path = task_registry::meta_stdout_path(Path::new(&output_dir))
                    .to_string_lossy()
                    .into_owned();
                let stderr_path = task_registry::meta_stderr_path(Path::new(&output_dir))
                    .to_string_lossy()
                    .into_owned();
                Ok(json!({
                    "task_id": snapshot.task_id,
                    "status": snapshot.status.as_str(),
                    "output_dir": output_dir,
                    "stdout_path": stdout_path,
                    "stderr_path": stderr_path,
                    "note": "running in background; poll with shell.collect(task_id), stop with shell.kill(task_id)"
                }))
            }
            Err(e) => {
                tracing::warn!(tool = NAME, binary, error = %e, "background task rejected");
                Ok(json!({ "error": e.to_string() }))
            }
        }
    }
}

fn segment_denied(segment: &CompoundSegment, reason: impl AsRef<str>) -> KernelError {
    KernelError::InvalidArgument(format!(
        "shell.exec embedded command rejected at sub-command \"{}\": {}",
        segment.text,
        reason.as_ref()
    ))
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

        let background = args
            .get("background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if background && stdin_data.is_some() {
            return Err(KernelError::InvalidArgument(
                "shell.exec background:true cannot be combined with 'stdin' — the spawning scope returns immediately, so no writer would stay attached to the child's stdin pipe".into(),
            ));
        }

        let resolved = resolve_binary(binary)?;

        // FR-028: cwd must be a safe path (not env/protected/symlink-escape).
        self.sandbox_validator
            .validate_path_for_read(&cwd)
            .map_err(|e| KernelError::InvalidArgument(format!("invalid cwd: {e}")))?;

        if let Some(embedded) = shell_command_string(binary, &argv) {
            // D4 seal: `env` is the only injection path (FR-037), and a shell
            // consumes it via `$var` expansion — combining the two lets
            // `{"args":["-c","$P"],"env":{"P":"<rejected text>"}}` execute
            // anything the segment scanner would have refused.
            let injects_env = args
                .get("env")
                .and_then(|v| v.as_object())
                .is_some_and(|obj| !obj.is_empty());
            if injects_env {
                return Err(KernelError::InvalidArgument(
                    "shell.exec: 'env' injection combined with an embedded shell -c command is rejected — runtime variable expansion bypasses per-segment validation".into(),
                ));
            }
            self.validate_embedded_command(embedded)?;
        } else {
            // Oracle A4 (2026-10-22): structured args bypassed every
            // protected-path check — the seatbelt hook reads string keys only,
            // the OS profile allows all reads, so `{"binary":"cat",
            // "args":["~/.ssh/id_ed25519"]}` returned credentials to the model
            // unreviewed. The direct-args run here mirrors the embedded
            // path's protected-class scan.
            for arg in &argv {
                let Some(path) = protected_path_candidate(arg) else {
                    continue;
                };
                self.sandbox_validator
                    .validate_path_for_read(Path::new(path))
                    .map_err(|e| {
                        KernelError::InvalidArgument(format!(
                            "shell.exec arg \"{arg}\" rejected: {e}"
                        ))
                    })?;
            }
        }

        let mut cmd = std::process::Command::new(&resolved);
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

        // FR-028: wrap the spawn in the OS sandbox (sandbox-exec / bwrap),
        // unless the profile is full access (DangerFullAccess).
        let cmd = if self.sandbox_profile.sandboxed {
            sandbox_spawn(cmd, &self.sandbox_profile, self.sandbox_profile.network)
                .map_err(|e| KernelError::ToolFailed(e.to_string()))?
        } else {
            cmd
        };

        // Convert to tokio's async Command for spawn + timeout handling.
        let mut cmd: tokio::process::Command = cmd.into();

        // New process group so the whole tree can be killed on timeout.
        #[cfg(unix)]
        cmd.process_group(0);

        // T165: kill the child if the turn is cancelled and this future is
        // dropped — with its own process group an orphan is unreachable by the
        // parent's signals. Background tasks invert this (D8): the registry
        // holds the Child past this scope, so dropping the handle must NOT
        // kill the child.
        cmd.kill_on_drop(!background);

        if background {
            return self.spawn_background_task(argv, cmd, binary).await;
        }

        // Foreground path below is byte-identical to pre-background behavior.

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
                let exit_code = status.code().unwrap_or(-1);
                let stdout_str = truncate_output(&stdout);
                let stderr_str = truncate_output(&stderr);
                let sandbox_denied = is_likely_sandbox_denied(status.code(), &stderr_str);
                let hint = if sandbox_denied {
                    tracing::warn!(
                        tool = NAME,
                        binary,
                        exit_code,
                        "shell.exec denied by OS sandbox"
                    );
                    Some("sandbox denied this command. escalate: retry with --sandbox danger-full-access, or use ask-for-approval".to_string())
                } else {
                    None
                };
                tracing::debug!(tool = NAME, exit_code, duration_ms, "exited");
                Ok(json!({
                    "exit_code": exit_code,
                    "stdout": stdout_str,
                    "stderr": stderr_str,
                    "timed_out": false,
                    "duration_ms": duration_ms,
                    "sandbox_denied": sandbox_denied,
                    "hint": hint
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
                    // SAFETY: negative pid targets the group led by our own
                    // spawned child (spawned with process_group(0) above, so
                    // pgid == child pid); permission is guaranteed because the
                    // child is our descendant. This is the timeout-enforcement
                    // path (FR-038) — a pid-reuse race is bounded because we
                    // reap the child immediately after.
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
                    "duration_ms": timeout_ms,
                    "sandbox_denied": false
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> ShellExecTool {
        use zen_core::sandbox::SandboxMode;
        // DangerFullAccess bypasses the OS sandbox so the spawn/timeout/stdin
        // tests exercise the bare process logic deterministically.
        let validator =
            zen_core::sandbox::SandboxValidator::new(SandboxMode::DangerFullAccess, vec![]);
        let profile = OsSandboxProfile::from_mode(SandboxMode::DangerFullAccess, vec![], false);
        ShellExecTool::new(PathBuf::from("/"), validator, profile)
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

    #[tokio::test]
    #[cfg(unix)]
    async fn cwd_on_protected_path_rejected() {
        use zen_core::sandbox::SandboxMode;
        let dir = tempfile::tempdir().unwrap();
        let protected = dir.path().join(".git");
        std::fs::create_dir_all(&protected).unwrap();
        let validator = zen_core::sandbox::SandboxValidator::new(
            SandboxMode::WorkspaceWrite,
            vec![dir.path().to_path_buf()],
        );
        let profile = OsSandboxProfile::from_mode(SandboxMode::WorkspaceWrite, vec![], false);
        let t = ShellExecTool::new(dir.path().to_path_buf(), validator, profile);

        let res = t
            .invoke(json!({
                "binary": "/bin/echo",
                "args": ["hi"],
                "cwd": protected.to_str().unwrap()
            }))
            .await;
        assert!(res.is_err(), "cwd on a protected path must be rejected");
    }

    fn segment_texts(cmd: &str) -> Vec<String> {
        decompose_compound(cmd)
            .unwrap()
            .into_iter()
            .map(|segment| segment.text)
            .collect()
    }

    #[test]
    fn compound_operators_split_and_validate_each_segment() {
        let segments = decompose_compound("ps aux && grep secret").unwrap();
        assert_eq!(
            segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
            vec!["ps aux", "grep secret"]
        );
        assert_eq!(segments[0].tokens, vec!["ps", "aux"]);
        assert_eq!(segments[1].tokens, vec!["grep", "secret"]);
        assert!(
            tool()
                .validate_embedded_command("ps aux && grep secret")
                .is_ok()
        );
    }

    #[test]
    fn one_denied_segment_denies_the_whole_compound() {
        use zen_core::sandbox::SandboxMode;
        let dir = tempfile::tempdir().unwrap();
        let validator =
            SandboxValidator::new(SandboxMode::WorkspaceWrite, vec![dir.path().to_path_buf()]);
        let profile = OsSandboxProfile::from_mode(SandboxMode::WorkspaceWrite, vec![], false);
        let t = ShellExecTool::new(dir.path().to_path_buf(), validator, profile);

        let err = t
            .validate_embedded_command("echo safe; cat ~/.ssh/id_ed25519")
            .unwrap_err()
            .to_string();
        assert!(err.contains("cat ~/.ssh/id_ed25519"), "{err}");
        assert!(err.contains("protected metadata path"), "{err}");
    }

    #[test]
    fn quoted_operator_does_not_split() {
        let segments = decompose_compound(r#"echo "a && b""#).unwrap();
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].tokens, vec!["echo", "a && b"]);
    }

    #[test]
    fn unmatched_quote_falls_back_to_whole_string_evaluation() {
        let cmd = r#"echo "unterminated && sudo id"#;
        assert!(decompose_compound(cmd).is_none());
        let err = tool()
            .validate_embedded_command(cmd)
            .unwrap_err()
            .to_string();
        assert!(err.contains("sudo id"), "{err}");
    }

    #[test]
    fn subscripted_commands_are_decomposed_as_separate_units() {
        let cmd = r#"echo "$(cd /tmp && rm -rf /build)""#;
        let texts = segment_texts(cmd);
        assert!(texts.iter().any(|text| text == "cd /tmp"), "{texts:?}");
        assert!(
            texts.iter().any(|text| text == "rm -rf /build"),
            "{texts:?}"
        );
        let err = tool()
            .validate_embedded_command(cmd)
            .unwrap_err()
            .to_string();
        assert!(err.contains("rm -rf /build"), "{err}");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn fr041_shlex_failure_still_scans_protected_paths() {
        let tool = scope_tool();
        // Force the fallback: an unclosed quote makes shlex::split fail.
        let err = tool
            .validate_embedded_command("cat ~/.ssh/id && echo \"unterminated")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("protected metadata path") || err.contains(".ssh"),
            "{err}"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn fr041_glob_hidden_protected_component_rejected() {
        let err = scope_tool()
            .validate_embedded_command("cat ~/.[s]sh/id_rsa")
            .unwrap_err()
            .to_string();
        assert!(err.contains("protected path"), "{err}");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn fr041_glob_on_bare_words_still_passes() {
        let res = tool()
            .invoke(json!({
                "binary": "/bin/sh",
                "args": ["-c", "echo /tmp/*glob-pin-nonexistent*"]
            }))
            .await
            .unwrap();
        assert_eq!(res["exit_code"], 0);
        assert!(res["error"].as_str().is_none());
    }

    #[test]
    fn fr041_backslash_newline_continuation_joins_words_and_still_gates() {
        let cmd = "echo a \\\n sudo id";
        let segments = decompose_compound(cmd).unwrap();
        assert_eq!(segments.len(), 1);
        let err = tool()
            .validate_embedded_command(cmd)
            .unwrap_err()
            .to_string();
        assert!(err.contains("sudo id"), "{err}");
    }

    #[test]
    fn backtick_substitution_is_decomposed_as_a_separate_unit() {
        let texts = segment_texts("echo `true && sudo id`");
        assert!(texts.iter().any(|text| text == "sudo id"), "{texts:?}");
    }

    #[test]
    fn heredoc_is_not_decomposed() {
        assert!(decompose_compound("cat <<EOF\npayload\nEOF").is_none());
    }

    #[test]
    fn shell_flag_cluster_detection() {
        let argv = |args: &[&str]| args.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            shell_command_string("/bin/sh", &argv(&["-c", "echo hi"])),
            Some("echo hi")
        );
        assert_eq!(
            shell_command_string("bash", &argv(&["-lc", "echo hi"])),
            Some("echo hi")
        );
        assert_eq!(
            shell_command_string("zsh", &argv(&["--noprofile", "-c", "echo hi"])),
            Some("echo hi")
        );
        assert_eq!(shell_command_string("dash", &argv(&["-s"])), None);
        assert_eq!(
            shell_command_string("/bin/echo", &argv(&["-c", "hi"])),
            None
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn sh_dash_c_compound_is_denied_before_spawn() {
        let res = tool()
            .invoke(json!({
                "binary": "/bin/sh",
                "args": ["-c", "echo safe && sudo id"]
            }))
            .await;
        let err = res.unwrap_err().to_string();
        assert!(err.contains("sudo id"), "{err}");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn sh_dash_c_benign_compound_runs() {
        let res = tool()
            .invoke(json!({
                "binary": "/bin/sh",
                "args": ["-c", "echo one && echo two"]
            }))
            .await
            .unwrap();
        assert_eq!(res["exit_code"], 0);
        assert!(res["stdout"].as_str().unwrap().contains("one"));
    }

    // Oracle A4 fix pin: structured args of a NON-shell binary get the same
    // protected-class path scan as the embedded-command path.
    // WorkspaceWrite validator: the shared `tool()` helper is DangerFullAccess
    // by design (validator is a pass-through there), which would make this pin
    // vacuous.
    fn scope_tool() -> ShellExecTool {
        let validator =
            zen_core::sandbox::SandboxValidator::new(SandboxMode::WorkspaceWrite, vec![]);
        let profile = OsSandboxProfile::from_mode(SandboxMode::WorkspaceWrite, vec![], false);
        ShellExecTool::new(PathBuf::from("/"), validator, profile)
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn direct_args_protected_path_is_denied() {
        let res = scope_tool()
            .invoke(json!({
                "binary": "cat",
                "args": ["/Users/me/.ssh/id_ed25519"]
            }))
            .await;
        let err = res.unwrap_err().to_string();
        assert!(err.contains("id_ed25519"), "{err}");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn direct_args_benign_path_runs() {
        let home = std::env::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
        let res = scope_tool()
            .invoke(json!({
                "binary": "cat",
                "args": [home.join(".gitconfig").to_string_lossy().to_string()]
            }))
            .await
            .unwrap();
        assert_ne!(res["error"].as_str(), Some("invalid argument"));
    }

    // D4 seal pins: runtime expansion / eval shapes the token scanner cannot
    // statically validate must be refused before spawn.
    #[tokio::test]
    #[cfg(unix)]
    async fn t087_unquoted_var_expansion_with_env_injection_rejected() {
        let res = tool()
            .invoke(json!({
                "binary": "/bin/sh",
                "args": ["-c", "echo $P"]
            }))
            .await;
        let err = res.unwrap_err().to_string();
        assert!(err.contains("runtime expansion"), "{err}");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn t087_quoted_literal_var_passes() {
        let res = tool()
            .invoke(json!({
                "binary": "/bin/sh",
                "args": ["-c", "echo '$HOME'"]
            }))
            .await
            .unwrap();
        assert!(res["stdout"].as_str().unwrap().contains("$HOME"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn t087_backtick_body_rejected() {
        let res = tool()
            .invoke(json!({
                "binary": "/bin/sh",
                "args": ["-c", "echo `safe word`"]
            }))
            .await;
        let err = res.unwrap_err().to_string();
        assert!(err.contains("runtime expansion"), "{err}");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn t087_eval_body_rejected() {
        let res = tool()
            .invoke(json!({
                "binary": "/bin/sh",
                "args": ["-c", "eval \"$P\""]
            }))
            .await;
        let err = res.unwrap_err().to_string();
        assert!(err.contains("eval"), "{err}");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn t087_env_plus_shell_combo_rejected() {
        let res = tool()
            .invoke(json!({
                "binary": "/bin/sh",
                "args": ["-c", "echo hi"],
                "env": { "P": "sudo id" }
            }))
            .await;
        let err = res.unwrap_err().to_string();
        assert!(err.contains("env"), "{err}");
    }

    // D11 false-positive pins: quote-aware tokenization makes patterns that
    // merely CONTAIN a blocked word or protected name readable invocations.
    #[tokio::test]
    #[cfg(unix)]
    async fn t087_quoted_search_pattern_sudo_passes() {
        // The searched line contains `sudo ` only via the quoted data — the
        // grep PATTERN itself is what must not trip the pattern list.
        let res = tool()
            .invoke(json!({
                "binary": "/bin/sh",
                "args": ["-c", "echo 'log line sudo id here' | grep \"sudo \" -"]
            }))
            .await
            .unwrap();
        assert_eq!(res["exit_code"], 0);
        assert!(res["stdout"].as_str().unwrap().contains("sudo"));
        assert!(res["error"].as_str().is_none());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn t087_find_name_dotgit_passes() {
        let res = tool()
            .invoke(json!({
                "binary": "/bin/sh",
                "args": ["-c", "find . -name .git || true"]
            }))
            .await
            .unwrap();
        assert_ne!(res["error"].as_str(), Some("invalid argument"));
        assert_ne!(res["error"].as_str(), Some("denied"));
    }
}
