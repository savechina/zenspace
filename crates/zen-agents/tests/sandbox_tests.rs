// 4D Test: WasmSandbox, ResourceLimits, ExecutionOutput
//
// Dimensions:
//   Normal: Default resource limits, sandbox creation
//   Reverse: Zero limits, empty syscall whitelist
//   Adversarial: Extreme memory limits, invalid WASM bytes
//   Logic Tree: Resource limit boundaries

use zen_agents::sandbox::{ExecutionOutput, ResourceLimits, WasmSandbox};

// ============================================================================
// Normal Dimension
// ============================================================================

#[test]
fn resource_limits_default_values() {
    let limits = ResourceLimits::default();
    assert_eq!(limits.max_memory_bytes, 64 * 1024 * 1024); // 64MB
    assert_eq!(limits.max_execution_time_ms, 5000); // 5 seconds
    assert!(limits.allowed_syscalls.contains("fd_write"));
    assert!(limits.allowed_syscalls.contains("fd_read"));
    assert!(limits.allowed_syscalls.contains("proc_exit"));
    assert_eq!(limits.allowed_syscalls.len(), 3);
}

#[test]
fn wasm_sandbox_creation_succeeds() {
    let limits = ResourceLimits::default();
    let sandbox = WasmSandbox::new(limits);
    assert!(sandbox.is_ok(), "WasmSandbox should create successfully");
}

#[test]
fn execution_output_default_construction() {
    let output = ExecutionOutput {
        stdout: String::new(),
        stderr: String::new(),
        exit_code: 0,
        execution_time_ms: 0,
        memory_used_bytes: 0,
    };
    assert_eq!(output.exit_code, 0);
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn resource_limits_clone_and_debug() {
    let limits = ResourceLimits::default();
    let cloned = limits.clone();
    assert_eq!(cloned.max_memory_bytes, limits.max_memory_bytes);
    assert_eq!(cloned.max_execution_time_ms, limits.max_execution_time_ms);
    assert_eq!(cloned.allowed_syscalls, limits.allowed_syscalls);

    let _debug = format!("{:?}", limits);
    let _debug_output = format!("{:?}", ExecutionOutput {
        stdout: "out".into(),
        stderr: "err".into(),
        exit_code: 0,
        execution_time_ms: 10,
        memory_used_bytes: 1024,
    });
}

// ============================================================================
// Reverse Dimension
// ============================================================================

#[test]
fn resource_limits_zero_values() {
    let limits = ResourceLimits {
        max_memory_bytes: 0,
        max_execution_time_ms: 0,
        allowed_syscalls: Default::default(),
    };
    assert_eq!(limits.max_memory_bytes, 0);
    assert_eq!(limits.max_execution_time_ms, 0);
    assert!(limits.allowed_syscalls.is_empty());
}

#[test]
fn wasm_sandbox_creation_with_zero_limits() {
    let limits = ResourceLimits {
        max_memory_bytes: 0,
        max_execution_time_ms: 0,
        allowed_syscalls: Default::default(),
    };
    let sandbox = WasmSandbox::new(limits);
    assert!(sandbox.is_ok(), "Sandbox should create even with zero limits");
}

#[test]
fn validate_module_invalid_bytes() {
    let limits = ResourceLimits::default();
    let sandbox = WasmSandbox::new(limits).expect("sandbox creation");
    let result = sandbox.validate_module(b"not wasm binary");
    assert!(result.is_err(), "Invalid WASM bytes should fail validation");
}

#[test]
fn validate_module_empty_bytes() {
    let limits = ResourceLimits::default();
    let sandbox = WasmSandbox::new(limits).expect("sandbox creation");
    let result = sandbox.validate_module(b"");
    assert!(result.is_err(), "Empty bytes should fail validation");
}

// ============================================================================
// Adversarial Dimension
// ============================================================================

#[test]
fn resource_limits_extreme_values() {
    let limits = ResourceLimits {
        max_memory_bytes: u64::MAX,
        max_execution_time_ms: u64::MAX,
        allowed_syscalls: (0..1000).map(|i| format!("syscall_{}", i)).collect(),
    };
    assert_eq!(limits.max_memory_bytes, u64::MAX);
    assert_eq!(limits.max_execution_time_ms, u64::MAX);
    assert_eq!(limits.allowed_syscalls.len(), 1000);
}

#[test]
fn sandbox_engine_accessible() {
    let limits = ResourceLimits::default();
    let sandbox = WasmSandbox::new(limits).expect("sandbox creation");
    let _engine = sandbox.engine(); // just verify accessor returns
}

// ============================================================================
// Logic Tree Dimension
// ============================================================================

#[test]
fn resource_limits_allowed_syscalls_default_includes_io() {
    let limits = ResourceLimits::default();
    assert!(limits.allowed_syscalls.contains("fd_write"), "Default should allow fd_write");
    assert!(limits.allowed_syscalls.contains("fd_read"), "Default should allow fd_read");
    assert!(limits.allowed_syscalls.contains("proc_exit"), "Default should allow proc_exit");
}

#[test]
fn resource_limits_copy_behaviors() {
    let limits = ResourceLimits::default();
    let _clone = limits.clone();
}
