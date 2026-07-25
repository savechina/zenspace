use std::fs;
use std::io::BufRead;
use std::path::Path;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::Value;
use tracing::{info, warn};

/// Imports chat exports in JSONL format into inbox notes.
pub struct ChatImporter;

impl ChatImporter {
    pub fn new() -> Self {
        Self
    }

    /// Import messages from a JSONL chat export file.
    ///
    /// Each line is parsed as a JSON object with optional `role` and
    /// `content` fields.  Valid messages are written to individual
    /// markdown files under `inbox/`.
    pub fn import(&self, chat_export_path: &Path) -> Result<usize> {
        if !chat_export_path.is_file() {
            warn!(path = %chat_export_path.display(), "chat export file not found");
            return Ok(0);
        }

        let inbox = chat_export_path
            .parent()
            .map(|vault| vault.join("inbox"))
            .unwrap_or_else(|| PathBuf::from("inbox"));
        fs::create_dir_all(&inbox)
            .with_context(|| format!("create inbox dir: {}", inbox.display()))?;

        let file = fs::File::open(chat_export_path)
            .with_context(|| format!("open chat export: {}", chat_export_path.display()))?;
        let reader = std::io::BufReader::new(file);

        let date_str = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
        let mut imported = 0usize;

        for (i, line_result) in reader.lines().enumerate() {
            let line = line_result?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let parsed: Value = serde_json::from_str(trimmed).unwrap_or_default();
            let role = parsed["role"].as_str().unwrap_or("unknown");
            let content = parsed["content"].as_str().unwrap_or("").trim();
            if content.is_empty() {
                continue;
            }

            let filename = format!("chat-{date_str}-{i:04}-{role}.md");
            let dest = inbox.join(&filename);

            let header = parsed["sender"]
                .as_str()
                .map(|s| format!("**{}** ({})  \n", s, role))
                .unwrap_or_else(|| format!("**{}**  \n", role));
            let body =
                format!("---\nsource: chat-import\nrole: {role}\n---\n\n{header}{content}\n");

            fs::write(&dest, &body)
                .with_context(|| format!("write chat note: {}", dest.display()))?;
            imported += 1;
        }

        info!(imported, path = %chat_export_path.display(), "chat import complete");
        Ok(imported)
    }
}

impl Default for ChatImporter {
    fn default() -> Self {
        Self::new()
    }
}
