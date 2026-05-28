use async_trait::async_trait;
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::{Value, json};
use std::sync::LazyLock;
use sysinfo::{Disk, Disks, System};

#[derive(Clone)]
pub struct HealthTool;

const NAME: &str = "system.health";
const DESCRIPTION: &str = "System health metrics (CPU, memory, disk, uptime, processes)";

static ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "include_processes": {
                "type": "boolean",
                "description": "Include top processes by CPU usage (default true)"
            },
            "max_processes": {
                "type": "integer",
                "description": "Maximum number of processes to return (default 10)"
            }
        }
    })
});

static RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "cpu_percent": { "type": "number" },
            "memory_total_bytes": { "type": "integer" },
            "memory_used_bytes": { "type": "integer" },
            "memory_free_bytes": { "type": "integer" },
            "swap_total_bytes": { "type": "integer" },
            "swap_used_bytes": { "type": "integer" },
            "uptime_seconds": { "type": "integer" },
            "disks": { "type": "array" },
            "processes": { "type": "array" },
            "hostname": { "type": "string" },
            "os_name": { "type": "string" },
            "os_version": { "type": "string" }
        }
    })
});

fn disk_info(disk: &Disk) -> Value {
    json!({
        "name": disk.name().to_string_lossy(),
        "mount_point": disk.mount_point().to_string_lossy(),
        "file_system": disk.file_system().to_string_lossy(),
        "total_bytes": disk.total_space(),
        "used_bytes": disk.total_space() - disk.available_space(),
        "available_bytes": disk.available_space(),
        "usage_percent": if disk.total_space() > 0 {
            ((disk.total_space() - disk.available_space()) as f64 / disk.total_space() as f64 * 100.0).round()
        } else {
            0.0
        }
    })
}

#[async_trait]
impl Tool for HealthTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: NAME.to_string(),
            description: DESCRIPTION.to_string(),
            args_schema: ARGS_SCHEMA.clone(),
            result_schema: RESULT_SCHEMA.clone(),
        }
    }

    async fn invoke(&self, args: Value) -> Result<Value, KernelError> {
        let include_processes = args
            .get("include_processes")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let max_processes = args
            .get("max_processes")
            .and_then(|v| v.as_i64())
            .unwrap_or(10) as usize;

        let sys = System::new_all();
        let disks = Disks::new_with_refreshed_list();
        let disk_list: Vec<Value> = disks.list().iter().map(disk_info).collect();

        let mut result = json!({
            "cpu_percent": {
                "current": sys.global_cpu_usage(),
                "logical_cores": sys.cpus().len()
            },
            "memory_total_bytes": sys.total_memory(),
            "memory_used_bytes": sys.used_memory(),
            "memory_free_bytes": sys.total_memory() - sys.used_memory(),
            "swap_total_bytes": sys.total_swap(),
            "swap_used_bytes": sys.used_swap(),
            "disks": disk_list,
            "disks_count": disk_list.len(),
            "uptime_seconds": System::uptime(),
            "hostname": sysinfo::System::host_name(),
            "os_name": System::name(),
            "os_version": System::os_version(),
            "kernel_version": System::kernel_version()
        });

        if include_processes {
            let mut procs: Vec<_> = sys.processes().values().collect();
            procs.sort_by(|a, b| b.cpu_usage().total_cmp(&a.cpu_usage()));
            procs.truncate(max_processes);

            let process_list: Vec<Value> = procs
                .iter()
                .map(|p| {
                    json!({
                        "pid": p.pid().as_u32(),
                        "name": p.name().to_string_lossy(),
                        "cpu_percent": p.cpu_usage(),
                        "memory_bytes": p.memory(),
                        "status": format!("{:?}", p.status()),
                        "run_time_seconds": p.run_time(),
                    })
                })
                .collect();

            result["processes"] = json!(process_list);
        }

        Ok(result)
    }
}
