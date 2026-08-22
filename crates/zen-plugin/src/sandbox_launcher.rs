use std::io::Read;
use std::process;

use serde::Deserialize;
use zen_core::sandbox::{OsSandboxProfile, sandbox_spawn};

#[derive(Deserialize)]
struct LauncherRequest {
    profile: OsSandboxProfile,
    allow_network: bool,
    binary: String,
    args: Vec<String>,
}

pub async fn run_sandbox_launcher() -> Result<(), zen_core::errors::ZenError> {
    let buf = tokio::task::spawn_blocking(|| {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| zen_core::errors::ZenError::Message(format!("stdin read: {e}")))?;
        Ok::<_, zen_core::errors::ZenError>(buf)
    })
    .await
    .map_err(|e| zen_core::errors::ZenError::Message(format!("stdin task failed: {e}")))??;

    let req: LauncherRequest = serde_json::from_str(&buf).map_err(|e| {
        zen_core::errors::ZenError::Message(format!("invalid launcher request: {e}"))
    })?;

    // Layer 1: Landlock (filesystem restrictions)
    #[cfg(target_os = "linux")]
    apply_landlock(&req.profile)?;

    // Layer 2: Seccomp (syscall filtering)
    #[cfg(target_os = "linux")]
    apply_seccomp(req.allow_network)?;

    // Layer 3: PR_SET_NO_NEW_PRIVS (prevent privilege escalation)
    #[cfg(target_os = "linux")]
    apply_no_new_privs()?;

    let mut cmd = process::Command::new(&req.binary);
    cmd.args(&req.args);

    let mut wrapped = sandbox_spawn(cmd, &req.profile, req.allow_network)
        .map_err(|e| zen_core::errors::ZenError::Message(format!("sandbox wrap failed: {e}")))?;

    let status = wrapped
        .status()
        .map_err(|e| zen_core::errors::ZenError::Message(format!("spawn failed: {e}")))?;

    process::exit(status.code().unwrap_or(1));
}

/// Apply landlock ruleset to restrict filesystem access.
///
/// - Read-all: Allow reading all files
/// - Writable-roots: Allow writing only to specified directories
#[cfg(target_os = "linux")]
fn apply_landlock(profile: &OsSandboxProfile) -> Result<(), zen_core::errors::ZenError> {
    use landlock::{
        ABI, Access, AccessFs, CompatLevel, Compatible, Ruleset, RulesetAttr, RulesetCreatedAttr,
        RulesetStatus,
    };

    let abi = ABI::V5;

    // Create ruleset with read-all + write access
    let access_rw = AccessFs::from_all(abi);
    let access_ro = AccessFs::from_read(abi);

    // Create the ruleset first, then add rules to the created ruleset
    let mut created = Ruleset::default()
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(access_rw)
        .map_err(|e| zen_core::errors::ZenError::Message(format!("landlock handle_access: {e}")))?
        .create()
        .map_err(|e| zen_core::errors::ZenError::Message(format!("landlock create: {e}")))?;

    // Read-only for the entire filesystem
    created = created
        .add_rules(landlock::path_beneath_rules(&["/"], access_ro))
        .map_err(|e| zen_core::errors::ZenError::Message(format!("landlock add root rule: {e}")))?;

    // Read-write for writable roots
    if !profile.writable_roots.is_empty() {
        let writable_strs: Vec<&str> = profile
            .writable_roots
            .iter()
            .filter_map(|p| p.to_str())
            .collect();

        created = created
            .add_rules(landlock::path_beneath_rules(&writable_strs, access_rw))
            .map_err(|e| {
                zen_core::errors::ZenError::Message(format!("landlock add write rules: {e}"))
            })?;
    }

    let status = created
        .restrict_self()
        .map_err(|e| zen_core::errors::ZenError::Message(format!("landlock restrict_self: {e}")))?;

    // Best-effort: warn if not fully enforced
    if status.ruleset != RulesetStatus::FullyEnforced {
        tracing::warn!(
            "landlock not fully enforced (status: {:?}), running in best-effort mode",
            status.ruleset
        );
    }

    Ok(())
}

/// Apply seccomp-bpf filter to block dangerous syscalls.
///
/// Always blocked:
/// - ptrace (prevent process inspection)
/// - process_vm_readv/writev (prevent memory access)
/// - io_uring_* (prevent io_uring abuse)
///
/// When network is off:
/// - connect, accept, bind, listen (block network syscalls)
#[cfg(target_os = "linux")]
fn apply_seccomp(allow_network: bool) -> Result<(), zen_core::errors::ZenError> {
    use seccompiler::{BpfProgram, SeccompAction, SeccompFilter, SeccompRule, TargetArch};
    use std::collections::BTreeMap;

    let arch: TargetArch = std::env::consts::ARCH.try_into().map_err(|e| {
        zen_core::errors::ZenError::Message(format!("seccomp unsupported arch: {e}"))
    })?;

    // Build syscall filter map: syscall_number -> rules
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();

    // Always blocked syscalls (empty vec = unconditional match)
    rules.insert(libc::SYS_ptrace, vec![]); // ptrace
    rules.insert(libc::SYS_process_vm_readv, vec![]); // process_vm_readv
    rules.insert(libc::SYS_process_vm_writev, vec![]); // process_vm_writev
    rules.insert(425, vec![]); // io_uring_setup (not in libc)
    rules.insert(426, vec![]); // io_uring_enter (not in libc)
    rules.insert(427, vec![]); // io_uring_register (not in libc)

    // Network-blocked syscalls (when allow_network=false)
    if !allow_network {
        rules.insert(libc::SYS_socket, vec![]); // socket
        rules.insert(libc::SYS_connect, vec![]); // connect
        rules.insert(libc::SYS_accept, vec![]); // accept
        rules.insert(libc::SYS_sendto, vec![]); // sendto
        rules.insert(libc::SYS_recvfrom, vec![]); // recvfrom
        rules.insert(libc::SYS_sendmsg, vec![]); // sendmsg
        rules.insert(libc::SYS_recvmsg, vec![]); // recvmsg
        rules.insert(libc::SYS_bind, vec![]); // bind
        rules.insert(libc::SYS_listen, vec![]); // listen
        rules.insert(libc::SYS_getsockname, vec![]); // getsockname
        rules.insert(libc::SYS_getpeername, vec![]); // getpeername
        rules.insert(libc::SYS_socketpair, vec![]); // socketpair
        rules.insert(libc::SYS_accept4, vec![]); // accept4
    }

    // Blocklist: Allow by default, Errno when rule matches
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,                     // default: allow everything
        SeccompAction::Errno(libc::EPERM as u32), // blocked: return EPERM
        arch,
    )
    .map_err(|e| zen_core::errors::ZenError::Message(format!("seccomp filter create: {e}")))?;

    let bpf: BpfProgram = filter
        .try_into()
        .map_err(|e| zen_core::errors::ZenError::Message(format!("seccomp BPF compile: {e}")))?;

    seccompiler::apply_filter(&bpf)
        .map_err(|e| zen_core::errors::ZenError::Message(format!("seccomp apply: {e}")))?;

    Ok(())
}

/// Set PR_SET_NO_NEW_PRIVS to prevent privilege escalation.
#[cfg(target_os = "linux")]
fn apply_no_new_privs() -> Result<(), zen_core::errors::ZenError> {
    use libc::{PR_SET_NO_NEW_PRIVS, c_int, prctl};

    let ret = unsafe { prctl(PR_SET_NO_NEW_PRIVS, 1 as c_int, 0, 0, 0) };

    if ret != 0 {
        return Err(zen_core::errors::ZenError::Message(format!(
            "PR_SET_NO_NEW_PRIVS failed: {}",
            std::io::Error::last_os_error()
        )));
    }

    Ok(())
}
