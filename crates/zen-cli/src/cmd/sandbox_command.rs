use std::path::PathBuf;
use std::process;

use clap::{Args, Subcommand};
use serde::Serialize;

use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;
use zen_core::sandbox::{OsSandboxProfile, SandboxMode, sandbox_spawn};

/// Command-line arguments for the `zen sandbox` subcommand.
///
/// Provides sandboxed execution of commands with OS-level isolation.
///
/// # Platform Support
///
/// - **macOS**: Seatbelt (sandbox-exec) with SBPL profiles
/// - **Linux**: 3-layer stack:
///   - Bubblewrap (bwrap): Namespace/mount isolation
///   - Landlock: Filesystem access control (Linux 5.13+)
///   - Seccomp: Syscall filtering (ptrace, io_uring, network)
///
/// # Examples
///
/// ```bash
/// # Run a command in workspace-write sandbox mode
/// zen sandbox run --binary echo "hello"
///
/// # Show current sandbox status
/// zen sandbox status
///
/// # Test if sandbox is working
/// zen sandbox test
/// ```
#[derive(Args)]
pub struct SandboxArgs {
    #[command(subcommand)]
    pub operation: SandboxCommands,
}

/// Available sandbox operations.
///
/// Each operation provides a specific sandbox management function:
/// - `run`: Execute a command inside the sandbox
/// - `status`: Show current sandbox state and configuration
/// - `policy`: Show active sandbox policy
/// - `test`: Verify sandbox is working correctly
#[derive(Subcommand)]
pub enum SandboxCommands {
    /// Run a command inside the sandbox.
    ///
    /// Executes the specified binary with OS-level sandboxing applied.
    /// The sandbox mode determines what filesystem and network access
    /// the command has.
    ///
    /// # Scope Logic
    ///
    /// ```text
    /// --sandbox <MODE>
    ///   Functionality: Override sandbox mode for this invocation
    ///   User impact: Controls what filesystem/network access the command has
    ///   Default: workspace-write (from ZEN_SANDBOX_MODE env var)
    ///   Values: read-only, workspace-write, ask, danger-full-access
    ///   Interaction: --sandbox overrides ZEN_SANDBOX_MODE env var
    /// ```
    Run {
        /// Binary to execute.
        ///
        /// Can be an absolute path or a command name found in PATH.
        /// The binary must exist and be executable.
        #[arg(long)]
        binary: String,

        /// Arguments to pass to the binary.
        ///
        /// All arguments after the binary name are passed directly.
        /// Supports flags and values (e.g., `-f`, `--flag`, `value`).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,

        /// Sandbox mode override.
        ///
        /// Overrides the ZEN_SANDBOX_MODE environment variable for this
        /// invocation only. Valid modes:
        /// - `read-only`: Read-only filesystem access
        /// - `workspace-write`: Read/write to workspace only
        /// - `ask`: Prompt before each operation
        /// - `danger-full-access`: No sandboxing (insecure)
        #[arg(long, short = 's')]
        sandbox: Option<String>,

        /// Dry run: show what would happen without executing.
        ///
        /// Displays the command that would be executed and the sandbox
        /// mode that would be applied, but does not actually run the
        /// command. Useful for testing configuration.
        #[arg(long)]
        dry_run: bool,

        /// JSONL output for machine consumption.
        ///
        /// Outputs events as JSON lines instead of human-readable text.
        /// Useful for scripting and automation. Each line is a JSON
        /// object with `event`, `mode`, `sandboxed`, `command`, and
        /// optional `error` fields.
        #[arg(long)]
        json: bool,

        /// Working directory.
        ///
        /// Sets the working directory for the sandboxed command.
        /// The directory must exist. Defaults to the current directory.
        #[arg(long, short = 'C')]
        cd: Option<PathBuf>,

        /// Additional directories that should be writable.
        ///
        /// Adds extra directories to the writable roots list.
        /// Multiple directories can be specified by repeating the flag.
        /// The workspace root is always included.
        #[arg(long)]
        add_dir: Vec<PathBuf>,

        /// Skip pre-flight checks for sandbox binary.
        ///
        /// When set, skips the check for sandbox-exec (macOS) or
        /// bubblewrap (Linux). Commands will run without sandboxing
        /// if the binary is not found. Use with caution.
        #[arg(long)]
        skip_check: bool,
    },

    /// Show current sandbox state and configuration.
    ///
    /// Displays the active sandbox mode, whether sandboxing is
    /// available, and configuration details.
    Status {
        /// JSONL output for machine consumption.
        ///
        /// Outputs status as a JSON object instead of human-readable
        /// text. The JSON includes `event`, `mode`, and `sandboxed`
        /// fields.
        #[arg(long)]
        json: bool,
    },

    /// Show active sandbox policy.
    ///
    /// Displays the sandbox policy for each supported platform:
    /// - **macOS**: sandbox-exec (Seatbelt) with SBPL profiles
    /// - **Linux**: 3-layer stack:
    ///   - Bubblewrap (bwrap): Namespace/mount isolation
    ///   - Landlock: Filesystem access control (Linux 5.13+)
    ///   - Seccomp: Syscall filtering (ptrace, io_uring, network)
    /// - **Other**: No sandboxing (fail-closed)
    Policy,

    /// Verify sandbox is working correctly.
    ///
    /// Runs a test command inside the sandbox to verify that
    /// sandboxing is properly configured. Uses `echo` by default.
    ///
    /// # Examples
    ///
    /// ```bash
    /// # Test with default binary (echo)
    /// zen sandbox test
    ///
    /// # Test with specific binary
    /// zen sandbox test --binary ls
    /// ```
    Test {
        /// Binary to test.
        ///
        /// The binary to execute for testing. Defaults to `echo`.
        /// The binary must exist and be executable.
        #[arg(long, default_value = "echo")]
        binary: String,

        /// JSONL output for machine consumption.
        ///
        /// Outputs test results as a JSON object instead of
        /// human-readable text. The JSON includes `event`,
        /// `binary`, and optional `error` fields.
        #[arg(long)]
        json: bool,
    },
}

/// Event type for JSONL output.
///
/// Represents a sandbox event that can be serialized to JSON.
/// Used for machine-readable output when `--json` flag is set.
///
/// # Fields
///
/// - `event`: Event type (status, policy, test_pass, test_fail, etc.)
/// - `mode`: Sandbox mode (read-only, workspace-write, etc.)
/// - `sandboxed`: Whether sandboxing is active
/// - `message`: Human-readable message
/// - `error`: Error message if event failed
/// - `command`: Command being executed
/// - `binary`: Binary being tested/executed
#[derive(Debug, Serialize)]
struct SandboxEvent {
    event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sandboxed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    binary: Option<String>,
}

impl SandboxEvent {
    /// Create a new sandbox event with the given event type.
    ///
    /// # Arguments
    ///
    /// * `event` - Event type identifier (e.g., "status", "test_pass")
    ///
    /// # Returns
    ///
    /// A new SandboxEvent with all optional fields set to None.
    fn new(event: &str) -> Self {
        Self {
            event: event.to_string(),
            mode: None,
            sandboxed: None,
            message: None,
            error: None,
            command: None,
            binary: None,
        }
    }

    /// Set the sandbox mode for this event.
    ///
    /// # Arguments
    ///
    /// * `mode` - The sandbox mode (read-only, workspace-write, etc.)
    ///
    /// # Returns
    ///
    /// The modified SandboxEvent (builder pattern).
    fn with_mode(mut self, mode: &SandboxMode) -> Self {
        self.mode = Some(mode.to_string());
        self
    }

    /// Set whether sandboxing is active for this event.
    ///
    /// # Arguments
    ///
    /// * `sandboxed` - true if sandboxing is active, false otherwise
    ///
    /// # Returns
    ///
    /// The modified SandboxEvent (builder pattern).
    fn with_sandboxed(mut self, sandboxed: bool) -> Self {
        self.sandboxed = Some(sandboxed);
        self
    }

    /// Set an error message for this event.
    ///
    /// # Arguments
    ///
    /// * `err` - Error description
    ///
    /// # Returns
    ///
    /// The modified SandboxEvent (builder pattern).
    fn with_error(mut self, err: &str) -> Self {
        self.error = Some(err.to_string());
        self
    }

    /// Set the command being executed for this event.
    ///
    /// # Arguments
    ///
    /// * `cmd` - Command string (binary + args)
    ///
    /// # Returns
    ///
    /// The modified SandboxEvent (builder pattern).
    fn with_command(mut self, cmd: &str) -> Self {
        self.command = Some(cmd.to_string());
        self
    }

    /// Set the binary being tested/executed for this event.
    ///
    /// # Arguments
    ///
    /// * `bin` - Binary name or path
    ///
    /// # Returns
    ///
    /// The modified SandboxEvent (builder pattern).
    fn with_binary(mut self, bin: &str) -> Self {
        self.binary = Some(bin.to_string());
        self
    }
}

fn print_event(event: &SandboxEvent, json: bool) {
    if json {
        if let Ok(json_str) = serde_json::to_string(event) {
            println!("{json_str}");
        }
    } else {
        match event.event.as_str() {
            "status" => {
                let mode = event.mode.as_deref().unwrap_or("unknown");
                let sandboxed = event.sandboxed.unwrap_or(false);
                let indicator = if sandboxed { "🔒" } else { "🔓" };
                println!("{indicator} Sandbox mode: {mode}");
                if !sandboxed {
                    println!("  ⚠️  No sandboxing — process has full access");
                }
            }
            "policy" => {
                println!("Sandbox policy:");
                println!("  macOS:   sandbox-exec (Seatbelt)");
                println!("  Linux:   bubblewrap (Landlock + seccomp)");
                println!("  Other:   No sandboxing");
            }
            "test_pass" => {
                let binary = event.binary.as_deref().unwrap_or("echo");
                println!("✅ Sandbox test passed — {binary} executed successfully");
            }
            "test_fail" => {
                let binary = event.binary.as_deref().unwrap_or("echo");
                let err = event.error.as_deref().unwrap_or("unknown error");
                println!("❌ Sandbox test failed — {binary}: {err}");
            }
            "test_skip" => {
                println!("⏭️  Sandbox test skipped — no sandbox binary found");
            }
            "run_start" => {
                let cmd = event.command.as_deref().unwrap_or("");
                let mode = event.mode.as_deref().unwrap_or("unknown");
                println!("🚀 Running in sandbox ({mode}): {cmd}");
            }
            "run_dry" => {
                let cmd = event.command.as_deref().unwrap_or("");
                let mode = event.mode.as_deref().unwrap_or("unknown");
                println!("🔍 Dry run ({mode}): {cmd}");
            }
            "error" => {
                let err = event.error.as_deref().unwrap_or("unknown");
                eprintln!("❌ Error: {err}");
            }
            _ => {}
        }
    }
}

fn check_sandbox_binary() -> Result<PathBuf, String> {
    #[cfg(target_os = "macos")]
    {
        let path = PathBuf::from("/usr/bin/sandbox-exec");
        if path.exists() {
            return Ok(path);
        }
        Err("sandbox-exec not found at /usr/bin/sandbox-exec\n\
             On macOS, sandbox-exec is part of the Xcode Command Line Tools.\n\
             Install with: xcode-select --install"
            .to_string())
    }

    #[cfg(target_os = "linux")]
    {
        let bwrap = which::which("bwrap").ok();
        if let Some(path) = bwrap {
            return Ok(path);
        }

        if std::path::Path::new("/sys/kernel/security/apparmor").exists() {
            return Ok(PathBuf::from("landlock"));
        }

        Err("bubblewrap (bwrap) not found\n\
             On Linux, install bubblewrap:\n\
              Debian/Ubuntu: sudo apt install bubblewrap\n\
              Fedora/RHEL:   sudo dnf install bubblewrap\n\
              Arch:          sudo pacman -S bubblewrap"
            .to_string())
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err("Sandboxing is only supported on macOS and Linux\n\
             On this platform, commands run without sandboxing"
            .to_string())
    }
}

pub fn execute_command(args: &SandboxArgs) -> Result<(), ZenError> {
    match &args.operation {
        SandboxCommands::Run {
            binary,
            args: cmd_args,
            sandbox,
            dry_run,
            json,
            cd,
            add_dir,
            skip_check,
        } => {
            let mode = if let Some(s) = sandbox {
                SandboxMode::parse_str(s).ok_or_else(|| {
                    let err = format!(
                        "Invalid sandbox mode: '{s}'\n\
                         Valid modes: read-only, workspace-write, ask, danger-full-access\n\
                         Aliases: ro, ww, full, unsafe"
                    );
                    if *json {
                        let event = SandboxEvent::new("error").with_error(&err);
                        print_event(&event, true);
                    }
                    ZenError::Message(err)
                })?
            } else {
                let mode_env = std::env::var("ZEN_SANDBOX_MODE").unwrap_or_default();
                SandboxMode::parse_str(&mode_env).unwrap_or_default()
            };

            let mut workspace_roots: Vec<PathBuf> = ZenPaths::detect()
                .ok()
                .and_then(|p| p.workspace_root().cloned())
                .map(|r| vec![r])
                .unwrap_or_default();

            workspace_roots.extend(add_dir.iter().cloned());

            if let Some(dir) = cd {
                if !dir.exists() {
                    let err = format!(
                        "Working directory does not exist: {}\n\
                         Create it first or use a different path",
                        dir.display()
                    );
                    if *json {
                        let event = SandboxEvent::new("error").with_error(&err);
                        print_event(&event, true);
                    }
                    return Err(ZenError::Message(err));
                }
                std::env::set_current_dir(dir).map_err(|e| {
                    let err = format!(
                        "Failed to set working directory to {}: {e}\n\
                         Check that the path exists and is accessible",
                        dir.display()
                    );
                    if *json {
                        let event = SandboxEvent::new("error").with_error(&err);
                        print_event(&event, true);
                    }
                    ZenError::Message(err)
                })?;
            }

            let profile = OsSandboxProfile::from_mode(mode, workspace_roots, false);

            if !skip_check
                && profile.sandboxed
                && let Err(e) = check_sandbox_binary()
            {
                let err = format!(
                    "Sandbox binary check failed: {e}\n\
                         Use --skip-check to bypass (commands will run unsandboxed)"
                );
                if *json {
                    let event = SandboxEvent::new("error").with_error(&err);
                    print_event(&event, true);
                }
                return Err(ZenError::Message(err));
            }

            let cmd_str = format!("{binary} {}", cmd_args.join(" "));

            if *dry_run {
                let event = SandboxEvent::new("run_dry")
                    .with_mode(&mode)
                    .with_command(&cmd_str)
                    .with_sandboxed(profile.sandboxed);
                print_event(&event, *json);
                return Ok(());
            }

            let event = SandboxEvent::new("run_start")
                .with_mode(&mode)
                .with_command(&cmd_str)
                .with_sandboxed(profile.sandboxed);
            print_event(&event, *json);

            let mut cmd = process::Command::new(binary);
            cmd.args(cmd_args);

            let mut wrapped = if profile.sandboxed {
                sandbox_spawn(cmd, &profile, profile.network).map_err(|e| {
                    let err = format!(
                        "Failed to apply sandbox: {e}\n\n\
                         What happened: The sandbox layer (sandbox-exec/bwrap) could not wrap the command.\n\
                         Why: The sandbox binary may be missing or misconfigured.\n\
                         How to fix:\n\
                          1. Install sandbox binary (see above)\n\
                          2. Use --skip-check to bypass (insecure)\n\
                          3. Use --sandbox danger-full-access to disable sandboxing"
                    );
                    if *json {
                        let event = SandboxEvent::new("error").with_error(&err);
                        print_event(&event, true);
                    }
                    ZenError::Message(err)
                })?
            } else {
                cmd
            };

            let status = wrapped.status().map_err(|e| {
                let err = format!(
                    "Failed to execute command: {e}\n\n\
                     What happened: The process could not be spawned.\n\
                     Why: The binary may not exist, may not be executable, or may be in a restricted path.\n\
                     How to fix:\n\
                      1. Check that '{binary}' exists and is executable\n\
                      2. Use full path: --binary /usr/bin/{binary}\n\
                      3. Check file permissions: ls -la {binary}"
                );
                if *json {
                    let event = SandboxEvent::new("error").with_error(&err);
                    print_event(&event, true);
                }
                ZenError::Message(err)
            })?;

            process::exit(status.code().unwrap_or(1));
        }

        SandboxCommands::Status { json } => {
            let env = std::env::var("ZEN_SANDBOX_MODE").unwrap_or_default();
            let mode = SandboxMode::parse_str(&env).unwrap_or_default();

            let sandboxed = check_sandbox_binary().is_ok();

            let event = SandboxEvent::new("status")
                .with_mode(&mode)
                .with_sandboxed(sandboxed);
            print_event(&event, *json);

            if !json {
                println!();
                println!("Configuration:");
                println!("  ZEN_SANDBOX_MODE={env}");
                if let Ok(paths) = ZenPaths::detect()
                    && let Some(root) = paths.workspace_root()
                {
                    println!("  Workspace: {}", root.display());
                }
                println!();
                println!("Sandbox binaries:");
                #[cfg(target_os = "macos")]
                {
                    let exists = std::path::Path::new("/usr/bin/sandbox-exec").exists();
                    let status = if exists { "✅" } else { "❌" };
                    println!("  {status} sandbox-exec (/usr/bin/sandbox-exec)");
                }
                #[cfg(target_os = "linux")]
                {
                    match which::which("bwrap") {
                        Ok(p) => println!("  ✅ bubblewrap ({})", p.display()),
                        Err(_) => println!("  ❌ bubblewrap (not found)"),
                    }
                }
            }

            Ok(())
        }

        SandboxCommands::Policy => {
            let event = SandboxEvent::new("policy");
            print_event(&event, false);
            Ok(())
        }

        SandboxCommands::Test { binary, json } => {
            let env = std::env::var("ZEN_SANDBOX_MODE").unwrap_or_default();
            let mode = SandboxMode::parse_str(&env).unwrap_or_default();

            // Pre-flight check
            if let Err(e) = check_sandbox_binary() {
                let event = SandboxEvent::new("test_skip").with_error(&e);
                print_event(&event, *json);
                return Ok(());
            }

            let workspace_roots = ZenPaths::detect()
                .ok()
                .and_then(|p| p.workspace_root().cloned())
                .map(|r| vec![r])
                .unwrap_or_default();

            let profile = OsSandboxProfile::from_mode(mode, workspace_roots, false);

            let mut cmd = process::Command::new(binary);
            cmd.arg("zen-sandbox-test-12345");

            if profile.sandboxed {
                match sandbox_spawn(cmd, &profile, profile.network) {
                    Ok(mut wrapped) => match wrapped.output() {
                        Ok(output) => {
                            if output.status.success() {
                                let stdout = String::from_utf8_lossy(&output.stdout);
                                let event = SandboxEvent::new("test_pass").with_binary(binary);
                                print_event(&event, *json);
                                if !json {
                                    println!("  Output: {}", stdout.trim());
                                }
                            } else {
                                let stderr = String::from_utf8_lossy(&output.stderr);
                                let event = SandboxEvent::new("test_fail")
                                    .with_binary(binary)
                                    .with_error(&stderr);
                                print_event(&event, *json);
                            }
                        }
                        Err(e) => {
                            let event = SandboxEvent::new("test_fail")
                                .with_binary(binary)
                                .with_error(&e.to_string());
                            print_event(&event, *json);
                        }
                    },
                    Err(e) => {
                        let event = SandboxEvent::new("test_fail")
                            .with_binary(binary)
                            .with_error(&e.to_string());
                        print_event(&event, *json);
                    }
                }
            } else {
                // No sandbox, just run directly
                match cmd.output() {
                    Ok(output) => {
                        if output.status.success() {
                            let event = SandboxEvent::new("test_pass").with_binary(binary);
                            print_event(&event, *json);
                        } else {
                            let stderr = String::from_utf8_lossy(&output.stderr);
                            let event = SandboxEvent::new("test_fail")
                                .with_binary(binary)
                                .with_error(&stderr);
                            print_event(&event, *json);
                        }
                    }
                    Err(e) => {
                        let event = SandboxEvent::new("test_fail")
                            .with_binary(binary)
                            .with_error(&e.to_string());
                        print_event(&event, *json);
                    }
                }
            }

            Ok(())
        }
    }
}
