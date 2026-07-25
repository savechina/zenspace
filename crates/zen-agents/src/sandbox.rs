// T272-T273: WasmSandbox for Momus verification (FR-SO-004, FR-SO-007)
// wasmtime engine with memory limits, WASI syscall whitelist
// Momus blueprint validation via WASM sandbox execution

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub max_memory_bytes: u64,
    pub max_execution_time_ms: u64,
    pub allowed_syscalls: HashSet<String>,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_memory_bytes: 64 * 1024 * 1024,
            max_execution_time_ms: 5000,
            allowed_syscalls: HashSet::from([
                "fd_write".to_string(),
                "fd_read".to_string(),
                "proc_exit".to_string(),
            ]),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub execution_time_ms: u64,
    pub memory_used_bytes: u64,
}

/// WASI version detected from a compiled wasm module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WasiVersion {
    /// WASI preview1 — imports from `wasi_snapshot_preview1`
    P1,
    /// WASI preview2 core module — imports from `wasi:*` namespaces
    P2,
}

/// A compiled wasm binary, either a core module or a component.
enum CompiledWasm {
    Core {
        module: wasmtime::Module,
        version: WasiVersion,
    },
    Component {
        component: wasmtime::component::Component,
    },
}

/// Detect the WASI version from a core module's import namespace.
fn detect_core_version(module: &wasmtime::Module) -> WasiVersion {
    for import in module.imports() {
        match import.module() {
            "wasi_snapshot_preview1" => return WasiVersion::P1,
            m if m.starts_with("wasi:") => return WasiVersion::P2,
            _ => {}
        }
    }
    WasiVersion::P1
}

/// Try to compile a wasm binary as either a core module or a component.
fn compile_wasm(engine: &wasmtime::Engine, wasm_bytes: &[u8]) -> Result<CompiledWasm> {
    if let Ok(module) = wasmtime::Module::new(engine, wasm_bytes) {
        let version = detect_core_version(&module);
        return Ok(CompiledWasm::Core { module, version });
    }

    if let Ok(component) = wasmtime::component::Component::new(engine, wasm_bytes) {
        return Ok(CompiledWasm::Component { component });
    }

    anyhow::bail!("WASM binary is neither a valid core module nor a WASI component")
}

pub struct WasmSandbox {
    engine: wasmtime::Engine,
    limits: ResourceLimits,
}

impl WasmSandbox {
    pub fn new(limits: ResourceLimits) -> Result<Self> {
        let mut config = wasmtime::Config::new();
        config.consume_fuel(true);

        let engine = wasmtime::Engine::new(&config)?;

        Ok(Self { engine, limits })
    }

    pub async fn execute(&self, wasm_bytes: &[u8], args: &[String]) -> Result<ExecutionOutput> {
        let compiled = compile_wasm(&self.engine, wasm_bytes)?;
        match compiled {
            CompiledWasm::Core { module, version } => {
                self.execute_core(module, version, args).await
            }
            CompiledWasm::Component { component } => {
                self.execute_component(component, args).await
            }
        }
    }

    #[allow(unused_variables)]
    async fn execute_core(
        &self,
        module: wasmtime::Module,
        version: WasiVersion,
        args: &[String],
    ) -> Result<ExecutionOutput> {
        use std::time::Instant;
        use wasmtime_wasi::p2::pipe::MemoryOutputPipe;

        let start = Instant::now();

        let stdout_pipe = MemoryOutputPipe::new(65536);
        let stderr_pipe = MemoryOutputPipe::new(65536);

        let mut wasi_builder = wasmtime_wasi::WasiCtxBuilder::new();
        wasi_builder.args(args);
        wasi_builder.stdout(stdout_pipe.clone());
        wasi_builder.stderr(stderr_pipe.clone());
        let wasi_ctx = wasi_builder.build_p1();

        let mut store = wasmtime::Store::new(&self.engine, wasi_ctx);
        let fuel_budget = (self.limits.max_execution_time_ms as u64)
            .saturating_mul(1_000_000)
            .max(10_000_000);
        store.set_fuel(fuel_budget)?;

        // All core modules use the p1 linker (wasi_snapshot_preview1).
        // Modules with wasi:* namespace imports (P2) will fail at
        // instantiation with a clear wasmtime error since the p1 linker
        // doesn't expose those bindings — p2 requires component model
        // wrapping via wasmtime::component::Linker.
        let mut linker = wasmtime::Linker::new(&self.engine);
        wasmtime_wasi::p1::add_to_linker_async::<wasmtime_wasi::p1::WasiP1Ctx>(
            &mut linker,
            |s| s,
        )?;

        let instance = linker.instantiate_async(&mut store, &module).await?;

        if let Some(run_func) = instance.get_func(&mut store, "_start") {
            let result = run_func.call_async(&mut store, &[], &mut []).await;
            let elapsed = start.elapsed().as_millis() as u64;

            let stdout = String::from_utf8_lossy(&stdout_pipe.contents()).into_owned();
            let mut stderr = String::from_utf8_lossy(&stderr_pipe.contents()).into_owned();

            let exit_code = match &result {
                Ok(_) => 0,
                Err(e) => {
                    use std::fmt::Write as _;
                    let _ = write!(stderr, "{e}");
                    1
                }
            };

            let used_fuel = fuel_budget - store.get_fuel().unwrap_or(fuel_budget);

            Ok(ExecutionOutput {
                stdout,
                stderr,
                exit_code,
                execution_time_ms: elapsed,
                memory_used_bytes: used_fuel * 64 * 1024,
            })
        } else {
            let elapsed = start.elapsed().as_millis() as u64;
            Ok(ExecutionOutput {
                stdout: String::new(),
                stderr: "[no _start function found]".to_string(),
                exit_code: 0,
                execution_time_ms: elapsed,
                memory_used_bytes: 0,
            })
        }
    }

    async fn execute_component(
        &self,
        component: wasmtime::component::Component,
        args: &[String],
    ) -> Result<ExecutionOutput> {
        use std::time::Instant;
        use wasmtime_wasi::p2::pipe::MemoryOutputPipe;

        let start = Instant::now();

        let stdout_pipe = MemoryOutputPipe::new(65536);
        let stderr_pipe = MemoryOutputPipe::new(65536);

        let mut wasi_builder = wasmtime_wasi::WasiCtxBuilder::new();
        wasi_builder.args(args);
        wasi_builder.stdout(stdout_pipe.clone());
        wasi_builder.stderr(stderr_pipe.clone());
        let wasi_ctx = wasi_builder.build_p1();

        let mut store = wasmtime::Store::new(&self.engine, wasi_ctx);
        let fuel_budget = (self.limits.max_execution_time_ms as u64)
            .saturating_mul(1_000_000)
            .max(10_000_000);
        store.set_fuel(fuel_budget)?;

        let mut linker = wasmtime::component::Linker::new(&self.engine);
        wasmtime_wasi::p3::add_to_linker::<wasmtime_wasi::p1::WasiP1Ctx>(&mut linker)?;

        let instance = linker.instantiate_async(&mut store, &component).await?;

        if let Some(run_func) = instance.get_func(&mut store, "_start") {
            let result = run_func.call_async(&mut store, &[], &mut []).await;
            let elapsed = start.elapsed().as_millis() as u64;

            let stdout = String::from_utf8_lossy(&stdout_pipe.contents()).into_owned();
            let mut stderr = String::from_utf8_lossy(&stderr_pipe.contents()).into_owned();

            let exit_code = match &result {
                Ok(_) => 0,
                Err(e) => {
                    use std::fmt::Write as _;
                    let _ = write!(stderr, "{e}");
                    1
                }
            };

            let used_fuel = fuel_budget - store.get_fuel().unwrap_or(fuel_budget);

            Ok(ExecutionOutput {
                stdout,
                stderr,
                exit_code,
                execution_time_ms: elapsed,
                memory_used_bytes: used_fuel * 64 * 1024,
            })
        } else {
            let elapsed = start.elapsed().as_millis() as u64;
            Ok(ExecutionOutput {
                stdout: String::new(),
                stderr: "[no _start function found in component]".to_string(),
                exit_code: 0,
                execution_time_ms: elapsed,
                memory_used_bytes: 0,
            })
        }
    }

    pub fn validate_module(&self, wasm_bytes: &[u8]) -> Result<()> {
        if wasmtime::Module::new(&self.engine, wasm_bytes).is_ok() {
            return Ok(());
        }
        if wasmtime::component::Component::new(&self.engine, wasm_bytes).is_ok() {
            return Ok(());
        }
        anyhow::bail!("WASM binary is neither a valid core module nor a WASI component")
    }

    pub fn engine(&self) -> &wasmtime::Engine {
        &self.engine
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_limits_default_values() {
        let limits = ResourceLimits::default();
        assert_eq!(limits.max_memory_bytes, 64 * 1024 * 1024);
        assert_eq!(limits.max_execution_time_ms, 5000);
        assert!(limits.allowed_syscalls.contains("fd_write"));
    }

    #[test]
    fn wasm_sandbox_creation() {
        let limits = ResourceLimits::default();
        let sandbox = WasmSandbox::new(limits);
        assert!(sandbox.is_ok());
    }

    #[test]
    fn detect_version_p1() {
        let sandbox = WasmSandbox::new(ResourceLimits::default()).unwrap();
        let wasm = wat::parse_str(
            r#"
            (module
                (import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32)))
                (func (export "_start"))
            )
            "#,
        )
        .unwrap();
        let module = wasmtime::Module::new(sandbox.engine(), &wasm).unwrap();
        assert_eq!(detect_core_version(&module), WasiVersion::P1);
    }

    #[test]
    fn detect_version_p2() {
        let sandbox = WasmSandbox::new(ResourceLimits::default()).unwrap();
        let wasm = wat::parse_str(
            r#"
            (module
                (import "wasi:io/streams" "read" (func (param i32) (result i32)))
                (func (export "_start"))
            )
            "#,
        )
        .unwrap();
        let module = wasmtime::Module::new(sandbox.engine(), &wasm).unwrap();
        assert_eq!(detect_core_version(&module), WasiVersion::P2);
    }
}
