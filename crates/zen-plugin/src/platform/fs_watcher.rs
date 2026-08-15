use async_trait::async_trait;
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};

#[derive(Clone)]
pub struct FsWatcherTool {
    watchers: Arc<Mutex<Vec<(RecommendedWatcher, String)>>>,
    active_watchers: Arc<AtomicUsize>,
}

const NAME: &str = "system.fs_watcher";
const MAX_WATCHERS: usize = 8;
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
            active_watchers: Arc::new(AtomicUsize::new(0)),
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
            }
            Err(e) => {
                tracing::error!(fs_error = %e);
            }
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

                // Reject protected paths (e.g. ~/.ssh, .git, .env) before any
                // registration work.
                if zen_core::sandbox::is_metadata_path(Path::new(path_str)) {
                    return Err(KernelError::InvalidArgument(format!(
                        "Path is protected and cannot be watched: {path_str}"
                    )));
                }

                let recursive = args
                    .get("recursive")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let mode = if recursive {
                    RecursiveMode::Recursive
                } else {
                    RecursiveMode::NonRecursive
                };

                // Enforce the per-instance watcher cap. Reserve a slot first so
                // concurrent invocations cannot overshoot the limit.
                let current = self.active_watchers.fetch_add(1, Ordering::SeqCst);
                if current >= MAX_WATCHERS {
                    self.active_watchers.fetch_sub(1, Ordering::SeqCst);
                    return Err(KernelError::InvalidArgument(
                        "watcher limit reached (max 8)".into(),
                    ));
                }

                let (tx, rx) = mpsc::channel();
                let mut watcher = match RecommendedWatcher::new(tx, Config::default()) {
                    Ok(w) => w,
                    Err(e) => {
                        self.active_watchers.fetch_sub(1, Ordering::SeqCst);
                        return Err(KernelError::ToolFailed(e.to_string()));
                    }
                };

                let path = Path::new(path_str);
                if !path.exists() {
                    self.active_watchers.fetch_sub(1, Ordering::SeqCst);
                    return Err(KernelError::InvalidArgument(format!(
                        "Path does not exist: {path_str}"
                    )));
                }

                if let Err(e) = watcher.watch(path, mode) {
                    self.active_watchers.fetch_sub(1, Ordering::SeqCst);
                    return Err(KernelError::ToolFailed(e.to_string()));
                }

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
            }
            "unwatch" => {
                let path_str = args["path"].as_str().ok_or_else(|| {
                    KernelError::InvalidArgument("Missing 'path' field for unwatch action".into())
                })?;

                let mut watchers = self.watchers.lock().unwrap();
                let before = watchers.len();
                watchers.retain(|(_, p)| p != path_str);
                let removed = before - watchers.len();
                if removed > 0 {
                    self.active_watchers.fetch_sub(removed, Ordering::SeqCst);
                }

                Ok(json!({
                    "action": "unwatch",
                    "path": path_str,
                    "removed": removed
                }))
            }
            "list" => {
                let watchers = self.watchers.lock().unwrap();
                let paths: Vec<&str> = watchers.iter().map(|(_, p)| p.as_str()).collect();

                Ok(json!({
                    "action": "list",
                    "watchers": paths,
                    "count": paths.len()
                }))
            }
            _ => Err(KernelError::InvalidArgument(format!(
                "Invalid action: {action}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watch_args(path: &str) -> Value {
        json!({
            "action": "watch",
            "path": path,
            "recursive": false
        })
    }

    #[tokio::test]
    async fn ninth_watcher_rejected() {
        let tool = FsWatcherTool::new();
        let dirs: Vec<_> = (0..MAX_WATCHERS)
            .map(|_| tempfile::tempdir().unwrap())
            .collect();
        for d in &dirs {
            let res = tool.invoke(watch_args(d.path().to_str().unwrap())).await;
            assert!(
                res.is_ok(),
                "watch {} should succeed: {:?}",
                d.path().display(),
                res
            );
        }
        assert_eq!(tool.active_watchers.load(Ordering::SeqCst), MAX_WATCHERS);

        let extra = tempfile::tempdir().unwrap();
        let err = tool
            .invoke(watch_args(extra.path().to_str().unwrap()))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("watcher limit reached"),
            "expected limit error, got: {err}"
        );
        assert_eq!(tool.active_watchers.load(Ordering::SeqCst), MAX_WATCHERS);
    }

    #[tokio::test]
    async fn protected_ssh_path_rejected() {
        let tool = FsWatcherTool::new();
        let err = tool.invoke(watch_args("~/.ssh")).await.unwrap_err();
        assert!(
            err.to_string().contains("protected"),
            "expected protected-path error, got: {err}"
        );
        assert_eq!(tool.active_watchers.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn unwatch_releases_slot() {
        let tool = FsWatcherTool::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        tool.invoke(watch_args(path)).await.unwrap();
        assert_eq!(tool.active_watchers.load(Ordering::SeqCst), 1);

        let res = tool
            .invoke(json!({ "action": "unwatch", "path": path }))
            .await
            .unwrap();
        assert_eq!(res["removed"], 1);
        assert_eq!(tool.active_watchers.load(Ordering::SeqCst), 0);
    }
}
