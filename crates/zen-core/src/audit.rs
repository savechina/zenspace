use chrono::Utc;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize)]
pub struct AuditEntry {
    pub timestamp: String,
    pub content_id: String,
    pub sensitivity_level: String,
    pub provider: Option<String>,
    pub stripped_patterns: Vec<String>,
    pub status: String,
}

pub struct PromptAuditLogger {
    log_path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error("I/O error writing audit log: {0}")]
    Io(#[from] std::io::Error),
}

impl Default for PromptAuditLogger {
    fn default() -> Self {
        Self::new()
    }
}

impl PromptAuditLogger {
    pub fn new() -> Self {
        let log_dir = home::home_dir()
            .unwrap_or_default()
            .join(".zen")
            .join("logs");
        let log_path = log_dir.join("safety-audit.jsonl");
        Self { log_path }
    }

    pub fn with_log_path(log_path: PathBuf) -> Self {
        Self { log_path }
    }

    pub fn log_prompt(
        &self,
        content: &str,
        sensitivity_level: &str,
        provider: Option<&str>,
        stripped_patterns: &[String],
        status: &str,
    ) -> Result<AuditEntry, AuditError> {
        let content_hash = Self::hash_content(content);

        let entry = AuditEntry {
            timestamp: Utc::now().to_rfc3339(),
            content_id: content_hash,
            sensitivity_level: sensitivity_level.to_string(),
            provider: provider.map(String::from),
            stripped_patterns: stripped_patterns.to_vec(),
            status: status.to_string(),
        };

        let log_dir = self.log_path.parent().unwrap_or(Path::new(""));
        if !log_dir.exists() {
            fs::create_dir_all(log_dir)?;
        }

        let line = serde_json::to_string(&entry)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)?;
        writeln!(file, "{line}")?;

        Ok(entry)
    }

    pub fn log_sanitized(
        &self,
        original_content: &str,
        provider: Option<&str>,
        stripped_patterns: &[String],
    ) -> Result<AuditEntry, AuditError> {
        let sensitivity = if stripped_patterns.is_empty() {
            "safe"
        } else if stripped_patterns.contains(&"code_execution".to_string())
            || stripped_patterns.contains(&"system_tag".to_string())
        {
            "high"
        } else if stripped_patterns.contains(&"shell_injection".to_string())
            || stripped_patterns.contains(&"privilege_escalation".to_string())
        {
            "medium"
        } else {
            "low"
        };

        self.log_prompt(
            original_content,
            sensitivity,
            provider,
            stripped_patterns,
            "sanitized",
        )
    }

    pub fn log_blocked(
        &self,
        original_content: &str,
        stripped_patterns: &[String],
    ) -> Result<AuditEntry, AuditError> {
        self.log_prompt(
            original_content,
            "critical",
            None,
            stripped_patterns,
            "blocked",
        )
    }

    fn hash_content(content: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        let result = hasher.finalize();
        let hex: String = result.iter().map(|b| format!("{:02x}", b)).collect();
        hex[..8.min(hex.len())].to_string()
    }

    pub fn log_path(&self) -> &Path {
        &self.log_path
    }
}
