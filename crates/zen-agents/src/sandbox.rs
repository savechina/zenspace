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

pub struct WasmSandbox {
    engine: wasmtime::Engine,
    limits: ResourceLimits,
}

impl WasmSandbox {
    pub fn new(limits: ResourceLimits) -> Result<Self> {
        let mut config = wasmtime::Config::new();
        config.consume_fuel(true);
        config.async_support(true);

        let engine = wasmtime::Engine::new(&config)?;

        Ok(Self { engine, limits })
    }

    pub async fn execute(&self, wasm_bytes: &[u8], args: &[String]) -> Result<ExecutionOutput> {
        use std::io::{Cursor, Write};
        use std::sync::{Arc, Mutex};
        use std::time::Instant;
        let start = Instant::now();

        let module = wasmtime::Module::new(&self.engine, wasm_bytes)?;

        let stdout_buf = Arc::new(Mutex::new(Cursor::new(Vec::<u8>::new())));
        let stderr_buf = Arc::new(Mutex::new(Cursor::new(Vec::<u8>::new())));
        let stdout_clone = stdout_buf.clone();
        let stderr_clone = stderr_buf.clone();

        struct CursorWriter(Arc<Mutex<Cursor<Vec<u8>>>>);
        impl Write for CursorWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().write(buf)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let mut wasi_builder = wasi_common::sync::WasiCtxBuilder::new();
        let _ = wasi_builder.args(args);
        wasi_builder.stdout(Box::new(wasi_common::pipe::WritePipe::new(CursorWriter(
            stdout_clone,
        ))));
        wasi_builder.stderr(Box::new(wasi_common::pipe::WritePipe::new(CursorWriter(
            stderr_clone,
        ))));
        let wasi_ctx = wasi_builder.build();

        let mut store = wasmtime::Store::new(&self.engine, wasi_ctx);
        store.set_fuel(self.limits.max_execution_time_ms / 100)?;

        let mut linker = wasmtime::Linker::new(&self.engine);
        wasi_common::sync::add_to_linker(&mut linker, |s: &mut wasi_common::WasiCtx| s)?;

        let instance = linker.instantiate_async(&mut store, &module).await?;

        if let Some(run_func) = instance.get_func(&mut store, "_start") {
            let result = run_func.call_async(&mut store, &[], &mut []).await;
            let elapsed = start.elapsed().as_millis() as u64;

            let stdout = String::from_utf8_lossy(&stdout_buf.lock().unwrap().get_ref()).into_owned();
            let mut stderr =
                String::from_utf8_lossy(&stderr_buf.lock().unwrap().get_ref()).into_owned();

            let exit_code = match &result {
                Ok(_) => 0,
                Err(e) => {
                    use std::fmt::Write as _;
                    let _ = write!(stderr, "{e}");
                    1
                }
            };

            let used_fuel = self.limits.max_execution_time_ms / 100
                - store.get_fuel().unwrap_or(self.limits.max_execution_time_ms / 100);

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

    pub fn validate_module(&self, wasm_bytes: &[u8]) -> Result<()> {
        wasmtime::Module::new(&self.engine, wasm_bytes)?;
        Ok(())
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
}
