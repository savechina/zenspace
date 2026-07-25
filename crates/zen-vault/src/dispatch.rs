use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tracing::warn;
use uuid::Uuid;

/// Supported dispatch targets for sub-agent spawning.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DispatchTarget {
    Codex,
    Opencode,
}

impl DispatchTarget {
    pub fn binary_name(&self) -> &'static str {
        match self {
            DispatchTarget::Codex => "codex",
            DispatchTarget::Opencode => "opencode",
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            DispatchTarget::Codex => "codex",
            DispatchTarget::Opencode => "opencode",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "codex" => Ok(DispatchTarget::Codex),
            "opencode" | "open-code" => Ok(DispatchTarget::Opencode),
            _ => anyhow::bail!("unknown dispatch target: {s}"),
        }
    }
}

impl std::fmt::Display for DispatchTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DispatchStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

impl std::fmt::Display for DispatchStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DispatchStatus::Pending => f.write_str("pending"),
            DispatchStatus::Running => f.write_str("running"),
            DispatchStatus::Completed => f.write_str("completed"),
            DispatchStatus::Failed => f.write_str("failed"),
        }
    }
}

/// A dispatch task record tracking a sub-agent execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchTask {
    pub id: String,
    pub task_description: String,
    pub target: String,
    pub status: DispatchStatus,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub files_changed: Vec<String>,
    pub key_decisions: Vec<String>,
    pub lessons_learned: Vec<String>,
    pub raw_output: String,
    pub error: Option<String>,
}

impl DispatchTask {
    /// Create a new pending task.
    fn new(task: &str, target: &DispatchTarget) -> Self {
        Self {
            id: Uuid::now_v7().to_string(),
            task_description: task.to_string(),
            target: target.as_str().to_string(),
            status: DispatchStatus::Pending,
            created_at: Utc::now().to_rfc3339(),
            completed_at: None,
            files_changed: Vec::new(),
            key_decisions: Vec::new(),
            lessons_learned: Vec::new(),
            raw_output: String::new(),
            error: None,
        }
    }

    /// Short ID for display (first 8 chars).
    pub fn short_id(&self) -> &str {
        if self.id.len() >= 8 {
            &self.id[..8]
        } else {
            &self.id
        }
    }
}

/// Service for dispatching tasks to external sub-agent CLIs.
pub struct DispatchService {
    coding_dir: PathBuf,
}

impl DispatchService {
    pub fn new(coding_dir: PathBuf) -> Self {
        Self { coding_dir }
    }

    pub fn from_paths(paths: &zen_core::paths::ZenPaths) -> Self {
        Self::new(paths.vault().join("coding"))
    }

    /// Dispatch a task to a sub-agent.
    ///
    /// 1. Generates task ID
    /// 2. Saves initial pending record
    /// 3. Builds context from provided files
    /// 4. Spawns subprocess (codex/opencode)
    /// 5. Captures output
    /// 6. Updates and saves final record
    pub async fn dispatch(
        &self,
        task: &str,
        target: DispatchTarget,
        context_files: &[PathBuf],
        timeout_secs: u64,
    ) -> Result<DispatchTask> {
        let mut task_record = DispatchTask::new(task, &target);

        // Ensure coding directory exists
        std::fs::create_dir_all(&self.coding_dir).with_context(|| {
            format!("failed to create coding dir: {}", self.coding_dir.display())
        })?;

        // Save initial pending state
        self.save_task(&task_record)?;

        // Build context from files
        let context = self.build_context(task, context_files);

        task_record.status = DispatchStatus::Running;
        self.save_task(&task_record)?;

        // Check if binary exists
        let binary = target.binary_name();
        let which = Command::new("which")
            .arg(binary)
            .output()
            .await
            .context("failed to run 'which'")?;

        if !which.status.success() {
            task_record.status = DispatchStatus::Failed;
            task_record.error = Some(format!(
                "Binary '{binary}' not found in PATH. Install it to use dispatch."
            ));
            task_record.completed_at = Some(Utc::now().to_rfc3339());
            self.save_task(&task_record)?;
            anyhow::bail!("Binary '{binary}' not found in PATH. Install it to use dispatch.");
        }

        // Spawn subprocess
        let mut cmd = Command::new(binary);
        cmd.arg(task);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn().context(format!("failed to spawn {binary}"))?;

        // Write context to stdin if child has stdin
        if let Some(mut stdin) = child.stdin.take()
            && let Err(e) =
                tokio::io::AsyncWriteExt::write_all(&mut stdin, context.as_bytes()).await
        {
            warn!(error = %e, "failed to write context to subprocess stdin");
        }

        let timeout = Duration::from_secs(timeout_secs);
        let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                task_record.status = DispatchStatus::Failed;
                task_record.error = Some(format!("subprocess error: {e}"));
                task_record.completed_at = Some(Utc::now().to_rfc3339());
                self.save_task(&task_record)?;
                anyhow::bail!("subprocess error: {e}");
            }
            Err(_) => {
                task_record.status = DispatchStatus::Failed;
                task_record.error = Some(format!("timed out after {timeout_secs}s"));
                task_record.completed_at = Some(Utc::now().to_rfc3339());
                self.save_task(&task_record)?;
                anyhow::bail!("dispatch timed out after {timeout_secs}s");
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

        task_record.raw_output = stdout.clone();
        if !output.status.success() {
            task_record.status = DispatchStatus::Failed;
            task_record.error = Some(format!(
                "exit code {:?}: {}",
                output.status.code(),
                stderr.trim()
            ));
        } else {
            task_record.status = DispatchStatus::Completed;
            // Try to extract structured data from output
            self.extract_structured_data(&stdout, &mut task_record);
        }
        task_record.completed_at = Some(Utc::now().to_rfc3339());

        self.save_task(&task_record)?;
        Ok(task_record)
    }

    /// Build context markdown from task description and files.
    fn build_context(&self, task: &str, context_files: &[PathBuf]) -> String {
        let mut md = format!("# Task\n\n{task}\n\n## Project Context\n\n");

        for file in context_files {
            if let Ok(content) = std::fs::read_to_string(file) {
                md.push_str(&format!(
                    "### File: {}\n\n{}\n\n",
                    file.file_name()
                        .map(|n| n.to_string_lossy())
                        .unwrap_or_default(),
                    content
                ));
            }
        }

        md
    }

    /// Best-effort extraction of structured data from sub-agent output.
    fn extract_structured_data(&self, output: &str, task: &mut DispatchTask) {
        for line in output.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("FILE_CHANGED:") {
                task.files_changed.push(rest.trim().to_string());
            } else if let Some(rest) = trimmed.strip_prefix("DECISION:") {
                task.key_decisions.push(rest.trim().to_string());
            } else if let Some(rest) = trimmed.strip_prefix("LESSON:") {
                task.lessons_learned.push(rest.trim().to_string());
            }
        }
    }

    /// Load a task from its markdown file.
    pub fn load_task(&self, task_id: &str) -> Result<DispatchTask> {
        let path = self.coding_dir.join(format!("{task_id}.md"));
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("task file: {}", path.display()))?;

        Self::parse_task_markdown(&content)
    }

    /// List all task IDs in the coding directory.
    pub fn list_task_ids(&self) -> Result<Vec<String>> {
        if !self.coding_dir.is_dir() {
            return Ok(Vec::new());
        }

        let mut ids = Vec::new();
        for entry in std::fs::read_dir(&self.coding_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "md")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                && Uuid::parse_str(stem).is_ok()
            {
                ids.push(stem.to_string());
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Load all tasks (for list display).
    pub fn list_tasks(&self) -> Vec<DispatchTask> {
        self.list_task_ids()
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.load_task(id).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Save task to `coding/<task-id>.md` with markdown + frontmatter.
    fn save_task(&self, task: &DispatchTask) -> Result<()> {
        std::fs::create_dir_all(&self.coding_dir)?;

        let path = self.coding_dir.join(format!("{}.md", task.id));
        let md = Self::render_task_markdown(task);

        std::fs::write(&path, md)
            .with_context(|| format!("failed to write task file: {}", path.display()))?;
        Ok(())
    }

    /// Render task as markdown with YAML frontmatter.
    fn render_task_markdown(task: &DispatchTask) -> String {
        let completed = task.completed_at.as_deref().unwrap_or("");

        let mut md = format!(
            "---\n\
             type: dispatch\n\
             task_id: {id}\n\
             target: {target}\n\
             status: {status}\n\
             created_at: {created}\n\
             completed_at: {completed}\n\
             ---\n\n\
             # Dispatch: {desc}\n\n",
            id = task.id,
            target = task.target,
            status = task.status,
            created = task.created_at,
            completed = completed,
            desc = task.task_description,
        );

        md.push_str("## Files Changed\n\n");
        if task.files_changed.is_empty() {
            md.push_str("_(none)_\n\n");
        } else {
            for f in &task.files_changed {
                md.push_str(&format!("- {f}\n"));
            }
            md.push('\n');
        }

        md.push_str("## Key Decisions\n\n");
        if task.key_decisions.is_empty() {
            md.push_str("_(none)_\n\n");
        } else {
            for d in &task.key_decisions {
                md.push_str(&format!("- {d}\n"));
            }
            md.push('\n');
        }

        md.push_str("## Lessons Learned\n\n");
        if task.lessons_learned.is_empty() {
            md.push_str("_(none)_\n\n");
        } else {
            for l in &task.lessons_learned {
                md.push_str(&format!("- {l}\n"));
            }
            md.push('\n');
        }

        if let Some(err) = &task.error {
            md.push_str(&format!("## Error\n\n```\n{err}\n```\n\n"));
        }

        md.push_str("## Raw Output\n\n");
        md.push_str("```\n");
        md.push_str(&task.raw_output);
        md.push_str("\n```\n");

        md
    }

    /// Parse task from markdown with frontmatter.
    fn parse_task_markdown(content: &str) -> Result<DispatchTask> {
        let content = content.trim();
        let fm_end = content
            .find("\n---\n")
            .or_else(|| content.find("\n---\r\n"))
            .context("no frontmatter end marker")?;

        let fm_block = &content[3..fm_end]; // skip leading "---\n"
        let body = &content[fm_end + 4..]; // skip "\n---\n"

        let mut id = String::new();
        let mut target = String::new();
        let mut status_str = String::new();
        let mut created_at = String::new();
        let mut completed_at: Option<String> = None;

        for line in fm_block.lines() {
            if let Some(v) = Self::frontmatter_value(line, "task_id:") {
                id = v;
            } else if let Some(v) = Self::frontmatter_value(line, "target:") {
                target = v;
            } else if let Some(v) = Self::frontmatter_value(line, "status:") {
                status_str = v;
            } else if let Some(v) = Self::frontmatter_value(line, "created_at:") {
                created_at = v;
            } else if let Some(v) = Self::frontmatter_value(line, "completed_at:")
                && !v.is_empty()
            {
                completed_at = Some(v);
            }
        }

        let status = match status_str.as_str() {
            "pending" => DispatchStatus::Pending,
            "running" => DispatchStatus::Running,
            "failed" => DispatchStatus::Failed,
            _ => DispatchStatus::Completed,
        };

        // Parse body sections
        let mut files_changed = Vec::new();
        let mut key_decisions = Vec::new();
        let mut lessons_learned = Vec::new();
        let mut raw_output = String::new();
        let mut error: Option<String> = None;

        let mut current_section = "";
        let mut in_code_block = false;

        for line in body.lines() {
            let trimmed = line.trim();

            if trimmed.starts_with("```") {
                in_code_block = !in_code_block;
                continue;
            }

            if !in_code_block && trimmed.starts_with("## ") {
                current_section = trimmed;
                continue;
            }

            if in_code_block && current_section.contains("Raw Output") {
                raw_output.push_str(line);
                raw_output.push('\n');
            } else if in_code_block && current_section.contains("Error") {
                error = Some(trimmed.to_string());
            } else if let Some(item) = trimmed.strip_prefix("- ") {
                match current_section {
                    s if s.contains("Files Changed") => files_changed.push(item.to_string()),
                    s if s.contains("Key Decisions") => key_decisions.push(item.to_string()),
                    s if s.contains("Lessons Learned") => lessons_learned.push(item.to_string()),
                    _ => {}
                }
            }
        }

        // Extract task description from H1
        let task_description = body
            .lines()
            .find_map(|l| l.strip_prefix("# Dispatch: ").map(|s| s.to_string()))
            .unwrap_or_default();

        Ok(DispatchTask {
            id,
            task_description,
            target,
            status,
            created_at,
            completed_at,
            files_changed,
            key_decisions,
            lessons_learned,
            raw_output: raw_output.trim().to_string(),
            error,
        })
    }

    fn frontmatter_value(line: &str, key: &str) -> Option<String> {
        let line = line.trim();
        line.strip_prefix(key)?
            .trim()
            .strip_suffix('\r')
            .map(|s| s.trim().to_string())
            .or_else(|| Some(line.strip_prefix(key)?.trim().to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_service() -> (TempDir, DispatchService) {
        let tmp = TempDir::new().unwrap();
        let service = DispatchService::new(tmp.path().join("coding"));
        (tmp, service)
    }

    #[test]
    fn test_task_creation() {
        let task = DispatchTask::new("fix bug", &DispatchTarget::Codex);
        assert!(!task.id.is_empty());
        assert_eq!(task.target, "codex");
        assert_eq!(task.status, DispatchStatus::Pending);
        assert!(task.files_changed.is_empty());
    }

    #[test]
    fn test_save_and_load_round_trip() {
        let (_tmp, service) = setup_service();

        let task = DispatchTask {
            id: Uuid::now_v7().to_string(),
            task_description: "implement feature X".to_string(),
            target: "codex".to_string(),
            status: DispatchStatus::Completed,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            completed_at: Some("2024-01-01T00:05:00Z".to_string()),
            files_changed: vec!["src/main.rs".to_string()],
            key_decisions: vec!["used tokio".to_string()],
            lessons_learned: vec!["always check types".to_string()],
            raw_output: "All done".to_string(),
            error: None,
        };

        service.save_task(&task).unwrap();
        let loaded = service.load_task(&task.id).unwrap();

        assert_eq!(loaded.id, task.id);
        assert_eq!(loaded.task_description, task.task_description);
        assert_eq!(loaded.target, task.target);
        assert_eq!(loaded.status, task.status);
        assert_eq!(loaded.files_changed, task.files_changed);
        assert_eq!(loaded.key_decisions, task.key_decisions);
        assert_eq!(loaded.lessons_learned, task.lessons_learned);
        assert_eq!(loaded.raw_output, task.raw_output);
    }

    #[test]
    fn test_list_task_ids() {
        let (_tmp, service) = setup_service();

        let task1 = DispatchTask::new("task one", &DispatchTarget::Codex);
        let task2 = DispatchTask::new("task two", &DispatchTarget::Opencode);

        service.save_task(&task1).unwrap();
        service.save_task(&task2).unwrap();

        let ids = service.list_task_ids().unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&task1.id));
        assert!(ids.contains(&task2.id));
    }

    #[test]
    fn test_list_empty_dir() {
        let (_tmp, service) = setup_service();
        let ids = service.list_task_ids().unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn test_target_from_str() {
        assert_eq!(
            DispatchTarget::from_str("codex").unwrap(),
            DispatchTarget::Codex
        );
        assert_eq!(
            DispatchTarget::from_str("opencode").unwrap(),
            DispatchTarget::Opencode
        );
        assert!(DispatchTarget::from_str("unknown").is_err());
    }

    #[test]
    fn test_extract_structured_data() {
        let (_tmp, service) = setup_service();
        let mut task = DispatchTask::new("test", &DispatchTarget::Codex);

        let output = "Some preamble\n\
                      FILE_CHANGED: src/main.rs\n\
                      DECISION: use tokio\n\
                      LESSON: check types\n\
                      FILE_CHANGED: README.md\n";

        service.extract_structured_data(output, &mut task);

        assert_eq!(task.files_changed, vec!["src/main.rs", "README.md"]);
        assert_eq!(task.key_decisions, vec!["use tokio"]);
        assert_eq!(task.lessons_learned, vec!["check types"]);
    }

    #[test]
    fn test_short_id() {
        let task = DispatchTask {
            id: "abcdefgh-1234-7xxx-xxxx-xxxxxxxxxxxx".to_string(),
            task_description: "test".to_string(),
            target: "codex".to_string(),
            status: DispatchStatus::Completed,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            completed_at: None,
            files_changed: vec![],
            key_decisions: vec![],
            lessons_learned: vec![],
            raw_output: String::new(),
            error: None,
        };
        assert_eq!(task.short_id(), "abcdefgh");
    }
}
