use anyhow::{Context, Result};
use std::fs;
use tracing::{debug, info, warn};

use zen_core::paths::ZenPaths;
use zen_memory::dream::{ExtractedSignals, update_memory_from_facts};

use super::super::{WorkerContext, WorkerReport, ZenWorker};
use super::marker_state::JournalEntryState;

pub struct MemoryCurator {
    scheduled: Option<&'static str>,
}

impl MemoryCurator {
    pub fn new() -> Self {
        Self { scheduled: None }
    }

    pub fn with_schedule(mut self, expr: &str) -> Self {
        self.scheduled = Some(Box::leak(expr.to_string().into_boxed_str()));
        self
    }
}

#[async_trait::async_trait]
impl ZenWorker for MemoryCurator {
    fn id(&self) -> &'static str {
        "memory-curator"
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

        let mut all_signals = ExtractedSignals::default();
        let mut to_mark: Vec<std::path::PathBuf> = Vec::new();

        for entry in fs::read_dir(&journal_dir)
            .with_context(|| format!("failed to read journal entries: {}", journal_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();

            if !path.is_file() || !path.extension().is_some_and(|ext| ext == "md") {
                continue;
            }

            if !is_journaled(&path) {
                continue;
            }
            if has_memory_updated_marker(&path) {
                continue;
            }

            match extract_signals_from_journal(&path) {
                Ok(signals) if !signals.is_empty() => {
                    debug!(
                        path = %path.display(),
                        facts = signals.facts.len(),
                        reflections = signals.reflections.len(),
                        commitments = signals.commitments.len(),
                        "signals extracted from journal entry"
                    );
                    all_signals.facts.extend(signals.facts);
                    all_signals.reflections.extend(signals.reflections);
                    all_signals.commitments.extend(signals.commitments);
                    to_mark.push(path);
                }
                Ok(_) => {
                    to_mark.push(path);
                }
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "failed to extract signals from journal entry");
                }
            }
        }

        let total = all_signals.total();

        if !all_signals.facts.is_empty() {
            update_memory_from_facts(&paths, &all_signals.facts, "Session")?;
        }
        if !all_signals.reflections.is_empty() {
            update_memory_from_facts(&paths, &all_signals.reflections, "Reflection")?;
        }
        if !all_signals.commitments.is_empty() {
            update_memory_from_facts(&paths, &all_signals.commitments, "Commitment")?;
        }

        if total > 0 {
            info!(
                facts = all_signals.facts.len(),
                reflections = all_signals.reflections.len(),
                commitments = all_signals.commitments.len(),
                "MEMORY.md updated from journal entries"
            );
        }

        for path in &to_mark {
            if let Err(e) = append_memory_updated_marker(path) {
                warn!(path = %path.display(), error = %e, "failed to mark journal entry as memory-updated");
            }
        }

        Ok(WorkerReport {
            worker_id: self.id().to_string(),
            success: true,
            fact_count: total,
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }
}

fn extract_signals_from_journal(path: &std::path::Path) -> Result<ExtractedSignals> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read journal entry: {}", path.display()))?;

    let mut signals = ExtractedSignals::default();
    let mut current_section: Option<&str> = None;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed == "## Facts" {
            current_section = Some("facts");
            continue;
        } else if trimmed == "## Reflections" {
            current_section = Some("reflections");
            continue;
        } else if trimmed == "## Commitments" {
            current_section = Some("commitments");
            continue;
        } else if trimmed.starts_with("## ") {
            current_section = None;
            continue;
        }

        if let Some(section) = current_section {
            if let Some(item) = trimmed.strip_prefix("- ") {
                let item = item.trim().to_string();
                let is_placeholder = item.starts_with("_(no ");
                if !item.is_empty() && !is_placeholder {
                    match section {
                        "facts" => signals.facts.push(item),
                        "reflections" => signals.reflections.push(item),
                        "commitments" => signals.commitments.push(item),
                        _ => {}
                    }
                }
            }
        }
    }

    Ok(signals)
}

fn is_journaled(path: &std::path::Path) -> bool {
    if JournalEntryState::is_journaled(path) {
        return true;
    }
    JournalEntryState::migrate_from_frontmatter(path) && JournalEntryState::is_journaled(path)
}

fn has_memory_updated_marker(path: &std::path::Path) -> bool {
    if JournalEntryState::has_memory_updated(path) {
        return true;
    }
    JournalEntryState::migrate_from_frontmatter(path) && JournalEntryState::has_memory_updated(path)
}

fn append_memory_updated_marker(path: &std::path::Path) -> Result<()> {
    let state = JournalEntryState {
        memory_updated_at: Some(chrono::Utc::now().to_rfc3339()),
        ..Default::default()
    };
    state.save(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_is_journaled_found() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.md");
        fs::write(&path, "---\nsession_id: test\n---\n\n").unwrap();

        let state = JournalEntryState {
            journaled_at: Some("2026-06-20T14:30:00Z".to_string()),
            ..Default::default()
        };
        state.save(&path).unwrap();

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
        fs::write(&path, "---\nsession_id: test\n---\n\n").unwrap();

        let state = JournalEntryState {
            journaled_at: Some("2026-06-20T14:30:00Z".to_string()),
            memory_updated_at: Some("2026-06-20T14:35:00Z".to_string()),
            ..Default::default()
        };
        state.save(&path).unwrap();

        assert!(has_memory_updated_marker(&path));
    }

    #[test]
    fn test_has_memory_updated_marker_not_found() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.md");
        fs::write(
            &path,
            "---\nsession_id: test\njournaled_at: 2026-06-20T14:30:00Z\n---\n\n",
        )
        .unwrap();
        assert!(!has_memory_updated_marker(&path));
    }

    #[test]
    fn test_extract_signals_from_journal() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.md");
        let content = "---\nsession_id: test\njournaled_at: 2026-06-20T14:30:00Z\n---\n\n## Facts\n\n- completed auth module\n- fixed login bug\n\n## Other\n\n- not a fact\n";
        fs::write(&path, content).unwrap();

        let signals = extract_signals_from_journal(&path).unwrap();
        assert_eq!(signals.facts.len(), 2);
        assert!(signals.facts.contains(&"completed auth module".to_string()));
        assert!(signals.facts.contains(&"fixed login bug".to_string()));
    }

    #[test]
    fn test_extract_signals_empty_journal() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.md");
        fs::write(&path, "---\njournaled_at: 2026-06-20T14:30:00Z\n---\n\n## Facts\n\n_(no durable facts extracted)_\n").unwrap();

        let signals = extract_signals_from_journal(&path).unwrap();
        assert!(signals.is_empty());
    }

    #[test]
    fn test_extract_signals_all_three_sections() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.md");
        let content = "---\nsession_id: test\njournaled_at: 2026-06-20T14:30:00Z\n---\n\n## Facts\n\n- implemented JWT auth\n- fixed race condition\n\n## Reflections\n\n- login flow too complex\n- should have tested migration first\n\n## Commitments\n\n- simplify login by July\n- write integration tests this week\n";
        fs::write(&path, content).unwrap();

        let signals = extract_signals_from_journal(&path).unwrap();
        assert_eq!(signals.facts.len(), 2);
        assert_eq!(signals.reflections.len(), 2);
        assert_eq!(signals.commitments.len(), 2);
        assert!(signals.facts.contains(&"implemented JWT auth".to_string()));
        assert!(
            signals
                .reflections
                .contains(&"login flow too complex".to_string())
        );
        assert!(
            signals
                .commitments
                .contains(&"simplify login by July".to_string())
        );
    }
}
