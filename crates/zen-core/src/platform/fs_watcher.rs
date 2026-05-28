use async_trait::async_trait;
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::LazyLock;
use std::sync::{Arc, Mutex, mpsc};

#[derive(Clone)]
pub struct FsWatcherTool {
    watchers: Arc<Mutex<Vec<(RecommendedWatcher, String)>>>,
}

const NAME: &str = "system.fs_watcher";
const DESCRIPTION: &str = "File system monitoring - watch paths for changes";

static ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": ["watch", "unwatch", "list"],
                "description": "Action to perform"
            },
            "path": {
                "type": "string",
                "description": "Path to watch (for watch/unwatch actions)"
            },
            "recursive": {
                "type": "boolean",
                "description": "Watch directories recursively (default true)"
            }
        },
        "required": ["action"]
    })
});

static RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "action": { "type": "string" },
            "path": { "type": "string" },
            "watchers": { "type": "array", "items": { "type": "string" } }
        }
    })
});

impl Default for FsWatcherTool {
    fn default() -> Self {
        Self::new()
    }
}

impl FsWatcherTool {
    pub fn new() -> Self {
        FsWatcherTool {
            watchers: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn handle_event(&self, event: notify::Result<notify::Event>) {
        match event {
            Ok(e) => {
                let kind_str = match e.kind {
                    EventKind::Any => "modified",
                    EventKind::Access(_) => "accessed",
                    EventKind::Create(_) => "created",
                    EventKind::Remove(_) => "removed",
                    EventKind::Modify(_) => "modified",
                    EventKind::Other => "other",
                };
                let paths: Vec<String> = e
                    .paths
                    .iter()
                    .filter_map(|p| p.to_str())
                    .map(|s| s.to_string())
                    .collect();
                tracing::info!(fs_event = kind_str, paths = ?paths);
            },
            Err(e) => {
                tracing::error!(fs_error = %e);
            },
        }
    }
}

#[async_trait]
impl Tool for FsWatcherTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: NAME.to_string(),
            description: DESCRIPTION.to_string(),
            args_schema: ARGS_SCHEMA.clone(),
            result_schema: RESULT_SCHEMA.clone(),
        }
    }

    async fn invoke(&self, args: Value) -> Result<Value, KernelError> {
        let action = args["action"].as_str().ok_or_else(|| {
            KernelError::InvalidArgument("Missing or invalid 'action' field".into())
        })?;

        match action {
            "watch" => {
                let path_str = args["path"].as_str().ok_or_else(|| {
                    KernelError::InvalidArgument("Missing 'path' field for watch action".into())
                })?;
                let recursive = args
                    .get("recursive")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let mode = if recursive {
                    RecursiveMode::Recursive
                } else {
                    RecursiveMode::NonRecursive
                };

                let (tx, rx) = mpsc::channel();
                let mut watcher = RecommendedWatcher::new(tx, Config::default())
                    .map_err(|e| KernelError::ToolFailed(e.to_string()))?;

                let path = Path::new(path_str);
                if !path.exists() {
                    return Err(KernelError::InvalidArgument(format!(
                        "Path does not exist: {path_str}"
                    )));
                }

                watcher
                    .watch(path, mode)
                    .map_err(|e| KernelError::ToolFailed(e.to_string()))?;

                let tool_clone = self.clone();
                std::thread::spawn(move || {
                    while let Ok(event) = rx.recv() {
                        tool_clone.handle_event(event);
                    }
                });

                let mut watchers = self.watchers.lock().unwrap();
                watchers.push((watcher, path_str.to_string()));

                Ok(json!({
                    "action": "watch",
                    "path": path_str,
                    "message": format!("Watching {path_str} (recursive: {recursive})")
                }))
            },
            "unwatch" => {
                let path_str = args["path"].as_str().ok_or_else(|| {
                    KernelError::InvalidArgument("Missing 'path' field for unwatch action".into())
                })?;

                let mut watchers = self.watchers.lock().unwrap();
                let before = watchers.len();
                watchers.retain(|(_, p)| p != path_str);
                let removed = before - watchers.len();

                Ok(json!({
                    "action": "unwatch",
                    "path": path_str,
                    "removed": removed
                }))
            },
            "list" => {
                let watchers = self.watchers.lock().unwrap();
                let paths: Vec<&str> = watchers.iter().map(|(_, p)| p.as_str()).collect();

                Ok(json!({
                    "action": "list",
                    "watchers": paths,
                    "count": paths.len()
                }))
            },
            _ => Err(KernelError::InvalidArgument(format!(
                "Invalid action: {action}"
            ))),
        }
    }
}
