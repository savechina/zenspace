use async_trait::async_trait;
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::{Value, json};
use std::sync::LazyLock;

#[derive(Clone)]
pub struct NotificationTool;

const NAME: &str = "system.notifications";
const DESCRIPTION: &str = "Desktop notifications with title, body, urgency";

static ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "title": {
                "type": "string",
                "description": "Notification title"
            },
            "body": {
                "type": "string",
                "description": "Notification body text"
            },
            "urgency": {
                "type": "string",
                "enum": ["low", "normal", "critical"],
                "description": "Urgency level (default: normal)"
            },
            "timeout": {
                "type": "integer",
                "description": "Timeout in milliseconds, 0 = auto, -1 = persistent"
            }
        },
        "required": ["title", "body"]
    })
});

static RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "sent": { "type": "boolean" },
            "title": { "type": "string" },
            "body": { "type": "string" }
        }
    })
});

#[async_trait]
impl Tool for NotificationTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: NAME.to_string(),
            description: DESCRIPTION.to_string(),
            args_schema: ARGS_SCHEMA.clone(),
            result_schema: RESULT_SCHEMA.clone(),
        }
    }

    async fn invoke(&self, args: Value) -> Result<Value, KernelError> {
        let title = args["title"].as_str().ok_or_else(|| {
            KernelError::InvalidArgument("Missing or invalid 'title' field".into())
        })?;

        let body = args["body"].as_str().ok_or_else(|| {
            KernelError::InvalidArgument("Missing or invalid 'body' field".into())
        })?;

        let urgency_str = args
            .get("urgency")
            .and_then(|v| v.as_str())
            .unwrap_or("normal");
        let timeout_ms = args.get("timeout").and_then(|v| v.as_i64()).unwrap_or(-1);

        let _ = urgency_str;

        let mut notif = notify_rust::Notification::new();
        notif.summary(title);
        notif.body(body);
        notif.appname("zen");

        if timeout_ms >= 0 {
            notif.timeout(std::time::Duration::from_millis(timeout_ms as u64));
        }

        let _ = notif
            .show()
            .map_err(|e| KernelError::ToolFailed(e.to_string()))?;

        Ok(json!({
            "sent": true,
            "title": title,
            "body": body
        }))
    }
}
