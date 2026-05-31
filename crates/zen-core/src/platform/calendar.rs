use async_trait::async_trait;
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::{Value, json};
use std::process::Command;
use std::sync::LazyLock;

#[derive(Clone)]
pub struct CalendarTool;

const NAME: &str = "system.calendar";
const DESCRIPTION: &str = "Calendar CRUD operations (list, create, query events)";

static ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": ["list", "create", "query"],
                "description": "The calendar action to perform"
            },
            "start": {
                "type": "string",
                "description": "Start date/time (ISO 8601), default now"
            },
            "end": {
                "type": "string",
                "description": "End date/time (ISO 8601), default 7 days from now"
            },
            "title": {
                "type": "string",
                "description": "Event title (for create action)"
            },
            "notes": {
                "type": "string",
                "description": "Event notes/description (for create action)"
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
            "events": { "type": "array", "items": { "type": "object" } }
        }
    })
});

impl Default for CalendarTool {
    fn default() -> Self {
        Self::new()
    }
}

impl CalendarTool {
    pub fn new() -> Self {
        CalendarTool
    }

    fn run_cmd(&self, args: &[&str]) -> Result<String, String> {
        let output = Command::new(args[0])
            .args(&args[1..])
            .output()
            .map_err(|e| e.to_string())?;
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if output.status.success() {
            Ok(stdout)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            Err(if stderr.is_empty() { stdout } else { stderr })
        }
    }
}

#[cfg(target_os = "macos")]
impl CalendarTool {
    fn osascript_list_events(
        &self,
        _start: Option<&str>,
        _end: Option<&str>,
    ) -> Result<Value, KernelError> {
        let script = r#"
tell application "Calendar"
    set eventList to {}
    repeat with cal in calendars
        repeat with evt in (every event of cal whose start date is greater than (current date) - 1 * days)
            set end of eventList to (name of evt) & "||" & (start date of evt as text) & "||" & (location of evt)
        end repeat
    end repeat
    return eventList as text
end tell
"#;
        let raw = self
            .run_cmd(&["osascript", "-e", script])
            .map_err(KernelError::ToolFailed)?;

        let mut events = Vec::new();
        for line in raw.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.split("||").collect();
            if parts.len() >= 2 {
                events.push(json!({
                    "title": parts.first().unwrap_or(&"").trim(),
                    "start_date": parts.get(1).unwrap_or(&"").trim(),
                    "location": parts.get(2).unwrap_or(&"").trim(),
                }));
            }
        }

        Ok(json!({ "action": "list", "events": events }))
    }
}

#[cfg(target_os = "linux")]
impl CalendarTool {
    fn linux_list_calendars(&self) -> Result<Value, KernelError> {
        let ics = std::fs::read_to_string("/usr/share/zoneinfo/").is_ok();

        if ics {
            Ok(json!({
                "action": "list",
                "events": Vec::<Value>::new(),
                "note": "No CalDAV server configured. Configure via ical-rs or point to a CalDAV URL."
            }))
        } else {
            Err(KernelError::ToolFailed("No calendar backend found".into()))
        }
    }
}

#[cfg(target_os = "windows")]
impl CalendarTool {
    fn windows_list_calendars(&self) -> Result<Value, KernelError> {
        Ok(json!({
            "action": "list",
            "events": Vec::<Value>::new(),
            "note": "Windows calendar access requires CalDAV URL or Outlook integration via COM."
        }))
    }
}

#[async_trait]
impl Tool for CalendarTool {
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

        if action != "list" {
            return Ok(json!({
                "action": action,
                "note": format!("Action '{action}' not yet implemented for this platform. Use 'list' for now.")
            }));
        }

        let _start = args.get("start").and_then(|v| v.as_str());
        let _end = args.get("end").and_then(|v| v.as_str());

        #[cfg(target_os = "macos")]
        {
            return self.osascript_list_events(_start, _end);
        }

        #[cfg(target_os = "linux")]
        {
            return self.linux_list_calendars();
        }

        #[cfg(target_os = "windows")]
        {
            return self.windows_list_calendars();
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            Err(KernelError::ToolFailed("Unsupported platform".into()))
        }
    }
}
