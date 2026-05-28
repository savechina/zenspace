use async_trait::async_trait;
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::{Value, json};
use std::process::Command;

#[derive(Clone)]
pub struct DaemonTool;

const NAME: &str = "system.daemon";
const DESCRIPTION: &str = "Manage daemon lifecycle (start, stop, restart, status)";

fn args_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": ["start", "stop", "restart", "status"],
                "description": "The daemon action to perform"
            },
            "name": {
                "type": "string",
                "description": "Daemon/service name (e.g. nginx, sshd)"
            }
        },
        "required": ["action", "name"]
    })
}

fn result_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": { "type": "string" },
            "name": { "type": "string" },
            "status": { "type": "string" },
            "stdout": { "type": "string" },
            "running": { "type": "boolean" }
        }
    })
}

impl Default for DaemonTool {
    fn default() -> Self {
        Self::new()
    }
}

impl DaemonTool {
    pub fn new() -> Self {
        DaemonTool
    }

    fn run_cmd(&self, args: &[&str]) -> Result<(bool, String), String> {
        let output = Command::new(args[0])
            .args(&args[1..])
            .output()
            .map_err(|e| e.to_string())?;
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if output.status.success() {
            Ok((true, stdout))
        } else {
            Err(if stderr.is_empty() {
                stdout.clone()
            } else {
                stderr
            })
        }
    }
}

#[cfg(target_os = "macos")]
impl DaemonTool {
    fn platform_action(&self, name: &str, action: &str) -> Result<Value, KernelError> {
        let domain = format!("system/{name}");
        match action {
            "start" => {
                let (_, out) = self
                    .run_cmd(&["launchctl", "start", &domain])
                    .map_err(KernelError::ToolFailed)?;
                Ok(json!({ "action": "start", "name": name, "status": out, "running": true }))
            },
            "stop" => {
                let (_, out) = self
                    .run_cmd(&["launchctl", "stop", &domain])
                    .map_err(KernelError::ToolFailed)?;
                Ok(json!({ "action": "stop", "name": name, "status": out, "running": false }))
            },
            "restart" => {
                let _ = self.run_cmd(&["launchctl", "stop", &domain]);
                let (_, out) = self
                    .run_cmd(&["launchctl", "start", &domain])
                    .map_err(KernelError::ToolFailed)?;
                Ok(json!({ "action": "restart", "name": name, "status": out, "running": true }))
            },
            "status" => {
                let (running, out) = match self.run_cmd(&["launchctl", "list", &domain]) {
                    Ok(r) => r,
                    Err(_) => (false, format!("Service {name} not loaded")),
                };
                Ok(json!({ "action": "status", "name": name, "status": out, "running": running }))
            },
            _ => Err(KernelError::InvalidArgument(format!(
                "Invalid action: {action}"
            ))),
        }
    }
}

#[cfg(target_os = "linux")]
impl DaemonTool {
    fn platform_action(&self, name: &str, action: &str) -> Result<Value, KernelError> {
        let svc = if name.ends_with(".service") {
            name.to_string()
        } else {
            format!("{name}.service")
        };
        match action {
            "start" => {
                let (_, out) = self
                    .run_cmd(&["systemctl", "start", &svc])
                    .map_err(|e| KernelError::ToolFailed(e))?;
                Ok(json!({ "action": "start", "name": name, "status": out, "running": true }))
            },
            "stop" => {
                let (_, out) = self
                    .run_cmd(&["systemctl", "stop", &svc])
                    .map_err(|e| KernelError::ToolFailed(e))?;
                Ok(json!({ "action": "stop", "name": name, "status": out, "running": false }))
            },
            "restart" => {
                let (_, out) = self
                    .run_cmd(&["systemctl", "restart", &svc])
                    .map_err(|e| KernelError::ToolFailed(e))?;
                Ok(json!({ "action": "restart", "name": name, "status": out, "running": true }))
            },
            "status" => {
                let (running, out) = match self.run_cmd(&["systemctl", "is-active", &svc]) {
                    Ok(r) => r,
                    Err(out) => (false, out),
                };
                Ok(json!({ "action": "status", "name": name, "status": out, "running": running }))
            },
            _ => Err(KernelError::InvalidArgument(format!(
                "Invalid action: {action}"
            ))),
        }
    }
}

#[cfg(target_os = "windows")]
impl DaemonTool {
    fn platform_action(&self, name: &str, action: &str) -> Result<Value, KernelError> {
        match action {
            "start" => {
                let (_, out) = self
                    .run_cmd(&["net", "start", name])
                    .map_err(|e| KernelError::ToolFailed(e))?;
                Ok(json!({ "action": "start", "name": name, "status": out, "running": true }))
            },
            "stop" => {
                let (_, out) = self
                    .run_cmd(&["net", "stop", name])
                    .map_err(|e| KernelError::ToolFailed(e))?;
                Ok(json!({ "action": "stop", "name": name, "status": out, "running": false }))
            },
            "restart" => {
                let _ = self.run_cmd(&["net", "stop", name]);
                let (_, out) = self
                    .run_cmd(&["net", "start", name])
                    .map_err(|e| KernelError::ToolFailed(e))?;
                Ok(json!({ "action": "restart", "name": name, "status": out, "running": true }))
            },
            "status" => {
                let (running, out) = match self.run_cmd(&["sc", "query", name]) {
                    Ok(r) => r,
                    Err(out) => (false, out),
                };
                Ok(json!({ "action": "status", "name": name, "status": out, "running": running }))
            },
            _ => Err(KernelError::InvalidArgument(format!(
                "Invalid action: {action}"
            ))),
        }
    }
}

#[async_trait]
impl Tool for DaemonTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: NAME.to_string(),
            description: DESCRIPTION.to_string(),
            args_schema: args_schema(),
            result_schema: result_schema(),
        }
    }

    async fn invoke(&self, args: Value) -> Result<Value, KernelError> {
        let action = args["action"].as_str().ok_or_else(|| {
            KernelError::InvalidArgument("Missing or invalid 'action' field".into())
        })?;
        let name = args["name"].as_str().ok_or_else(|| {
            KernelError::InvalidArgument("Missing or invalid 'name' field".into())
        })?;

        #[cfg(target_os = "macos")]
        {
            return self.platform_action(name, action);
        }

        #[cfg(target_os = "linux")]
        {
            return self.platform_action(name, action);
        }

        #[cfg(target_os = "windows")]
        {
            return self.platform_action(name, action);
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            Err(KernelError::ToolFailed("Unsupported platform".into()))
        }
    }
}
