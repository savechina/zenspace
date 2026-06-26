use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use zen_core::paths::ZenPaths;

use super::super::{WorkerContext, WorkerReport, ZenWorker};
use super::marker_state::JournalEntryState;

pub struct ReflectionWorker {
    scheduled: Option<&'static str>,
}

impl ReflectionWorker {
    pub fn new() -> Self {
        Self { scheduled: None }
    }

    pub fn with_schedule(mut self, expr: &str) -> Self {
        self.scheduled = Some(Box::leak(expr.to_string().into_boxed_str()));
        self
    }
}

#[async_trait::async_trait]
impl ZenWorker for ReflectionWorker {
    fn id(&self) -> &'static str {
        "reflection-worker"
    }

    fn description(&self) -> &'static str {
        "Aggregate session reflections into wiki/wisdom/reflections/ for synthesis"
    }

    fn schedule(&self) -> &'static str {
        self.scheduled.unwrap_or("0 0 6 * * *")
    }

    async fn execute(&self, _ctx: &WorkerContext) -> Result<WorkerReport> {
        let start = std::time::Instant::now();
        let paths = ZenPaths::detect()?;

        let journal_dir = paths.journal_entries();
        if !journal_dir.is_dir() {
            debug!("journal entries directory does not exist, skipping reflection extraction");
            return Ok(WorkerReport {
                worker_id: self.id().to_string(),
                success: true,
                fact_count: 0,
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }

        let reflections_dir = paths.vault().join("wiki/wisdom/reflections");
        let mut total_reflections = 0usize;

        for entry in fs::read_dir(&journal_dir)
            .with_context(|| format!("failed to read journal dir: {}", journal_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();

            if !path.is_file() || !path.extension().is_some_and(|ext| ext == "md") {
                continue;
            }

            if !is_journaled(&path) {
                continue;
            }
            if JournalEntryState::has_reflection_extracted(&path) {
                continue;
            }

            match extract_reflections_from_journal(&path) {
                Ok(reflections) if !reflections.is_empty() => {
                    let (date_str, session_id) = read_frontmatter_meta(&path);
                    let filename = format!("{date_str}.md");
                    let file_path = reflections_dir.join(&filename);

                    fs::create_dir_all(&reflections_dir).with_context(|| {
                        format!(
                            "failed to create reflections dir: {}",
                            reflections_dir.display()
                        )
                    })?;

                    let session_header = format!("## From Session {session_id}");
                    let mut existing_content = String::new();
                    if file_path.exists() {
                        existing_content = fs::read_to_string(&file_path).with_context(|| {
                            format!(
                                "failed to read existing reflections: {}",
                                file_path.display()
                            )
                        })?;
                        if existing_content.contains(&session_header) {
                            let now = chrono::Utc::now();
                            let mark_state = JournalEntryState {
                                reflection_extracted_at: Some(now.to_rfc3339()),
                                ..Default::default()
                            };
                            if let Err(e) = mark_state.save(&path) {
                                warn!(path = %path.display(), error = %e, "failed to mark journal entry as reflection-extracted");
                            }
                            continue;
                        }
                    }

                    let content = if existing_content.is_empty() {
                        format!(
                            "---\ndate: {date_str}\nsource: journal\n---\n\n# Reflections — {date_str}\n\n{session_header}\n{}\n",
                            reflections
                                .iter()
                                .map(|r| format!("- {r}"))
                                .collect::<Vec<_>>()
                                .join("\n")
                        )
                    } else {
                        format!(
                            "{existing_content}\n{session_header}\n{}\n",
                            reflections
                                .iter()
                                .map(|r| format!("- {r}"))
                                .collect::<Vec<_>>()
                                .join("\n")
                        )
                    };

                    fs::write(&file_path, content).with_context(|| {
                        format!("failed to write reflections: {}", file_path.display())
                    })?;
                    total_reflections += reflections.len();
                }
                Ok(_) => {}
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "failed to extract reflections from journal entry");
                }
            }

            let now = chrono::Utc::now();
            let final_state = JournalEntryState {
                reflection_extracted_at: Some(now.to_rfc3339()),
                ..Default::default()
            };
            if let Err(e) = final_state.save(&path) {
                warn!(path = %path.display(), error = %e, "failed to mark journal entry as reflection-extracted");
            }
        }

        if total_reflections > 0 {
            info!(
                reflections = total_reflections,
                "reflections aggregated from journal entries"
            );
        }

        Ok(WorkerReport {
            worker_id: self.id().to_string(),
            success: true,
            fact_count: total_reflections,
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }
}

fn extract_reflections_from_journal(path: &Path) -> Result<Vec<String>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read journal entry: {}", path.display()))?;

    let mut reflections = Vec::new();
    let mut in_reflections = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed == "## Reflections" {
            in_reflections = true;
            continue;
        } else if trimmed.starts_with("## ") {
            in_reflections = false;
            continue;
        }

        if in_reflections {
            if let Some(item) = trimmed.strip_prefix("- ") {
                let text = item.trim().to_string();
                if !text.is_empty() && !text.starts_with("_(no ") {
                    reflections.push(text);
                }
            }
        }
    }

    Ok(reflections)
}

fn read_frontmatter_meta(path: &Path) -> (String, String) {
    let content = fs::read_to_string(path).unwrap_or_default();
    let mut date_str = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let mut session_id = "unknown".to_string();

    for line in content.lines().take(15) {
        let trimmed = line.trim();
        if let Some(val) = trimmed.strip_prefix("date:") {
            let val = val.trim().to_string();
            if !val.is_empty() {
                date_str = val;
            }
        }
        if let Some(val) = trimmed.strip_prefix("session_id:") {
            let val = val.trim().to_string();
            if !val.is_empty() {
                session_id = val;
            }
        }
    }

    (date_str, session_id)
}

fn is_journaled(path: &Path) -> bool {
    if JournalEntryState::is_journaled(path) {
        return true;
    }
    JournalEntryState::migrate_from_frontmatter(path) && JournalEntryState::is_journaled(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_extract_reflections_from_journal() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.md");
        let content = "---\nsession_id: 01JX001\ndate: 2026-06-26\n---\n\n# Session Journal\n\n## Reflections\n\n- login flow too complex\n- should have tested migration first\n";
        fs::write(&path, content).unwrap();

        let reflections = extract_reflections_from_journal(&path).unwrap();
        assert_eq!(reflections.len(), 2);
        assert_eq!(reflections[0], "login flow too complex");
        assert_eq!(reflections[1], "should have tested migration first");
    }

    #[test]
    fn test_extract_reflections_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.md");
        let content =
            "---\nsession_id: test\n---\n\n## Reflections\n\n_(no reflections extracted)_\n";
        fs::write(&path, content).unwrap();

        let reflections = extract_reflections_from_journal(&path).unwrap();
        assert!(reflections.is_empty());
    }

    #[test]
    fn test_extract_reflections_no_section() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.md");
        let content = "---\nsession_id: test\n---\n\n## Facts\n\n- some fact\n";
        fs::write(&path, content).unwrap();

        let reflections = extract_reflections_from_journal(&path).unwrap();
        assert!(reflections.is_empty());
    }

    #[test]
    fn test_aggregate_idempotent() {
        let dir = tempdir().unwrap();
        let journal_dir = dir.path().join("journal");
        fs::create_dir_all(&journal_dir).unwrap();

        let journal_path = journal_dir.join("2026-06-26-test.md");
        let content = "---\nsession_id: 01JX001\ndate: 2026-06-26\njournaled_at: 2026-06-26T14:30:00Z\n---\n\n# Session Journal\n\n## Reflections\n\n- login flow too complex\n";
        fs::write(&journal_path, content).unwrap();

        let reflections_dir = dir.path().join("wiki/wisdom/reflections");
        fs::create_dir_all(&reflections_dir).unwrap();
        let file_path = reflections_dir.join("2026-06-26.md");

        let session_header = "## From Session 01JX001";
        let existing = format!(
            "---\ndate: 2026-06-26\nsource: journal\n---\n\n# Reflections — 2026-06-26\n\n{session_header}\n- login flow too complex\n"
        );
        fs::write(&file_path, existing).unwrap();

        let reflections = extract_reflections_from_journal(&journal_path).unwrap();
        assert!(!reflections.is_empty());

        let existing_content = fs::read_to_string(&file_path).unwrap();
        assert!(existing_content.contains(session_header));

        let (date_str, session_id) = read_frontmatter_meta(&journal_path);
        let session_header = format!("## From Session {session_id}");
        assert_eq!(date_str, "2026-06-26");
        assert!(existing_content.contains(&session_header));
    }
}
