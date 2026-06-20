use std::fs;
use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use zen_core::paths::ZenPaths;
use zen_memory::dream::update_memory_from_facts;

use super::super::{WorkerContext, WorkerReport, ZenWorker};

pub struct JournalWorker {
    scheduled: Option<&'static str>,
}

impl JournalWorker {
    pub fn new() -> Self {
        Self { scheduled: None }
    }

    pub fn with_schedule(mut self, expr: &str) -> Self {
        self.scheduled = Some(Box::leak(expr.to_string().into_boxed_str()));
        self
    }
}

#[async_trait::async_trait]
impl ZenWorker for JournalWorker {
    fn id(&self) -> &'static str {
        "journal-worker"
    }

    fn description(&self) -> &'static str {
        "Read journal entries from SessionJournaler, extract facts, update MEMORY.md"
    }

    fn schedule(&self) -> &'static str {
        self.scheduled.unwrap_or("0 */5 * * * *")
    }

    async fn execute(&self, _ctx: &WorkerContext) -> Result<WorkerReport> {
        let start = std::time::Instant::now();
        let paths = ZenPaths::detect()?;

        let journal_dir = paths.journal_entries();
        if !journal_dir.is_dir() {
            debug!("journal entries directory does not exist, skipping");
            return Ok(WorkerReport {
                worker_id: self.id().to_string(),
                success: true,
                fact_count: 0,
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }

        let mut all_facts: Vec<String> = Vec::new();
        let mut to_mark: Vec<std::path::PathBuf> = Vec::new();

        for entry in fs::read_dir(&journal_dir)
            .with_context(|| format!("failed to read journal entries: {}", journal_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();

            if !path.is_file() || !path.extension().is_some_and(|ext| ext == "md") {
                continue;
            }

            // Only process: journaled (ready) AND not yet memory-updated (fresh)
            if !is_journaled(&path) {
                continue;
            }
            if has_memory_updated_marker(&path) {
                continue;
            }

            match extract_facts_from_journal(&path) {
                Ok(facts) if !facts.is_empty() => {
                    debug!(path = %path.display(), facts = facts.len(), "facts extracted from journal entry");
                    all_facts.extend(facts);
                    to_mark.push(path);
                }
                Ok(_) => {
                    // No facts — still mark to avoid repeated scanning
                    to_mark.push(path);
                }
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "failed to extract facts from journal entry");
                }
            }
        }

        let total_facts = all_facts.len();

        if total_facts > 0 {
            update_memory_from_facts(&paths, &all_facts)?;
            info!(facts = total_facts, "MEMORY.md updated from journal entries");
        }

        for path in &to_mark {
            if let Err(e) = append_memory_updated_marker(path) {
                warn!(path = %path.display(), error = %e, "failed to mark journal entry as memory-updated");
            }
        }

        Ok(WorkerReport {
            worker_id: self.id().to_string(),
            success: true,
            fact_count: total_facts,
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }
}

fn extract_facts_from_journal(path: &std::path::Path) -> Result<Vec<String>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read journal entry: {}", path.display()))?;

    let mut facts = Vec::new();
    let mut in_facts_section = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed == "## Facts" {
            in_facts_section = true;
            continue;
        }

        if in_facts_section {
            if trimmed.starts_with("## ") {
                break;
            }
            if let Some(fact) = trimmed.strip_prefix("- ") {
                let fact = fact.trim().to_string();
                if !fact.is_empty() && fact != "_(no durable facts extracted)_" {
                    facts.push(fact);
                }
            }
        }
    }

    Ok(facts)
}

fn is_journaled(path: &std::path::Path) -> bool {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    for line in content.lines().take(15) {
        if line.trim().starts_with("journaled_at:") {
            return true;
        }
    }
    false
}

fn has_memory_updated_marker(path: &std::path::Path) -> bool {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    for line in content.lines().take(15) {
        if line.trim().starts_with("memory_updated_at:") {
            return true;
        }
    }
    false
}

fn append_memory_updated_marker(path: &std::path::Path) -> Result<()> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read journal entry: {}", path.display()))?;

    let now = chrono::Utc::now().to_rfc3339();
    let marker_line = format!("memory_updated_at: {now}\n");

    let new_content = if let Some(end) = content.find("\n---\n") {
        let insert_pos = end + 5;
        let (before, after) = content.split_at(insert_pos);
        format!("{}{}{}", before, marker_line, after)
    } else {
        format!("{marker_line}{content}")
    };

    fs::write(path, new_content)
        .with_context(|| format!("failed to write memory_updated marker: {}", path.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_is_journaled_found() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.md");
        fs::write(&path, "---\nsession_id: test\njournaled_at: 2026-06-20T14:30:00Z\n---\n\n").unwrap();
        assert!(is_journaled(&path));
    }

    #[test]
    fn test_is_journaled_not_found() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.md");
        fs::write(&path, "---\nsession_id: test\n---\n\ncontent\n").unwrap();
        assert!(!is_journaled(&path));
    }

    #[test]
    fn test_has_memory_updated_marker() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.md");
        fs::write(&path, "---\nsession_id: test\njournaled_at: 2026-06-20T14:30:00Z\nmemory_updated_at: 2026-06-20T14:35:00Z\n---\n\n").unwrap();
        assert!(has_memory_updated_marker(&path));
    }

    #[test]
    fn test_has_memory_updated_marker_not_found() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.md");
        fs::write(&path, "---\nsession_id: test\njournaled_at: 2026-06-20T14:30:00Z\n---\n\n").unwrap();
        assert!(!has_memory_updated_marker(&path));
    }

    #[test]
    fn test_extract_facts_from_journal() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.md");
        let content = "---\nsession_id: test\njournaled_at: 2026-06-20T14:30:00Z\n---\n\n## Facts\n\n- completed auth module\n- fixed login bug\n\n## Other\n\n- not a fact\n";
        fs::write(&path, content).unwrap();

        let facts = extract_facts_from_journal(&path).unwrap();
        assert_eq!(facts.len(), 2);
        assert!(facts.contains(&"completed auth module".to_string()));
        assert!(facts.contains(&"fixed login bug".to_string()));
    }

    #[test]
    fn test_extract_facts_empty_journal() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.md");
        fs::write(&path, "---\njournaled_at: 2026-06-20T14:30:00Z\n---\n\n## Facts\n\n_(no durable facts extracted)_\n").unwrap();

        let facts = extract_facts_from_journal(&path).unwrap();
        assert!(facts.is_empty());
    }
}
