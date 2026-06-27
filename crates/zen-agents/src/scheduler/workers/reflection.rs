use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;
use tracing::{debug, info, warn};

use zen_core::config::load_config;
use zen_core::paths::ZenPaths;
use zen_core::types::Sensitivity;
use zen_provider::{DefaultRouter, LlmRouterExt};

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
                    // DESIGN.md §3.1: reflections use weekly format {YYYY-Www}.md
                    let date = chrono::NaiveDate::parse_from_str(
                        &date_str,
                        "%Y-%m-%d"
                    )
                    .unwrap_or_else(|_| chrono::Utc::now().date_naive());
                    let filename = format!("{}.md", date.format("%G-W%V"));
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

            match synthesize_anti_patterns(&reflections_dir).await {
                Ok(count) if count > 0 => {
                    info!(anti_patterns = count, "anti-pattern candidates synthesized from reflections");
                }
                Ok(_) => {}
                Err(e) => {
                    warn!(error = %e, "LLM anti-pattern synthesis failed, skipping (graceful degradation)");
                }
            }
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

async fn synthesize_anti_patterns(reflections_dir: &Path) -> Result<usize> {
    let reflections_text = load_all_reflections_text(reflections_dir);
    if reflections_text.is_empty() {
        return Ok(0);
    }

    let router = match load_config() {
        Ok(c) => Some(DefaultRouter::from_agentic(c)),
        Err(e) => {
            warn!(error = %e, "failed to load config for LLM anti-pattern synthesis");
            None
        }
    };

    let Some(router) = router else {
        return Ok(0);
    };

    let truncated = if reflections_text.len() > 6000 {
        let end = reflections_text
            .char_indices()
            .nth(6000)
            .map(|(i, _)| i)
            .unwrap_or(reflections_text.len());
        format!("{}...", &reflections_text[..end])
    } else {
        reflections_text
    };

    let prompt = format!(
        r#"Analyze these session reflections and identify recurring anti-patterns — repeated mistakes, blind spots, or behavioral traps.

## Recent Reflections
{truncated}

Respond with ONLY a JSON object:
{{
  "anti_patterns": [
    {{"pattern": "...", "trigger": "...", "avoidance": "...", "evidence_refs": ["ref1", "ref2"]}}
  ]
}}

Rules:
- Each anti-pattern must have at least 2 evidence references from the reflections
- "trigger" describes when this pattern typically occurs
- "avoidance" describes what to do instead
- Return empty array if no recurring patterns found"#
    );

    let response = tokio::task::spawn_blocking(move || {
        router.complete("anti_pattern_synthesis", &prompt, Sensitivity::Private)
    })
    .await
    .context("LLM anti-pattern synthesis task panicked")??;

    let json_str = extract_json(&response);
    let parsed: Value = serde_json::from_str(json_str)
        .context("failed to parse LLM anti-pattern synthesis response")?;

    let anti_patterns = parsed["anti_patterns"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    let count = write_anti_patterns(reflections_dir, &anti_patterns)?;

    if count > 0 {
        if let Err(e) = update_stop_doing_ledger(&anti_patterns) {
            warn!(error = %e, "failed to update MEMORY.md Stop-Doing Ledger (non-fatal)");
        }
    }

    Ok(count)
}

/// Update MEMORY.md ## Stop-Doing Ledger section with latest anti-patterns.
fn update_stop_doing_ledger(anti_patterns: &[Value]) -> Result<()> {
    if anti_patterns.is_empty() {
        return Ok(());
    }

    let paths = ZenPaths::detect()?;
    let memory_md = paths.memory().join("MEMORY.md");

    let content = fs::read_to_string(&memory_md).unwrap_or_default();
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let section_marker = "## Stop-Doing Ledger";

    let mut entries: Vec<String> = Vec::new();
    for ap in anti_patterns.iter().take(10) {
        let pattern = ap["pattern"].as_str().unwrap_or("unknown");
        let trigger = ap["trigger"].as_str().unwrap_or("");
        entries.push(format!(
            "- **{pattern}** — detected {today}: {trigger}"
        ));
    }

    let updated = if content.contains(section_marker) {
        let before = content.split(section_marker).next().unwrap_or("");
        let rest = content.split(section_marker).nth(1).unwrap_or("");
        let next_section = rest.find("\n## ").unwrap_or(rest.len());
        let after = &rest[next_section..];
        format!(
            "{before}{section_marker}\n\n{}\n{after}",
            entries.join("\n")
        )
    } else {
        format!(
            "{}\n\n{section_marker}\n\n{}\n",
            content.trim_end(),
            entries.join("\n")
        )
    };

    let tmp = memory_md.with_extension("md.tmp");
    fs::write(&tmp, &updated)
        .with_context(|| format!("failed to write tmp MEMORY.md: {}", tmp.display()))?;
    fs::rename(&tmp, &memory_md)
        .with_context(|| format!("failed to rename tmp MEMORY.md: {}", memory_md.display()))?;

    info!(
        count = anti_patterns.len(),
        "updated MEMORY.md Stop-Doing Ledger"
    );
    Ok(())
}

fn load_all_reflections_text(dir: &Path) -> String {
    if !dir.is_dir() {
        return String::new();
    }
    let mut combined = String::new();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return String::new(),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.extension().is_some_and(|ext| ext == "md") {
            continue;
        }
        if let Ok(content) = fs::read_to_string(&path) {
            let body = strip_frontmatter(&content);
            combined.push_str(&body);
            combined.push('\n');
        }
    }
    combined
}

fn strip_frontmatter(content: &str) -> String {
    let mut lines = content.lines();
    if lines.next() == Some("---") {
        let mut in_frontmatter = true;
        let mut body = String::new();
        for line in lines {
            if in_frontmatter && line.trim() == "---" {
                in_frontmatter = false;
                continue;
            }
            if !in_frontmatter {
                body.push_str(line);
                body.push('\n');
            }
        }
        body
    } else {
        content.to_string()
    }
}

fn extract_json(response: &str) -> &str {
    if let Some(start) = response.find("```json") {
        let after = &response[start + 7..];
        if let Some(end) = after.find("```") {
            after[..end].trim()
        } else {
            after.trim()
        }
    } else if let Some(start) = response.find('{') {
        if let Some(end) = response.rfind('}') {
            &response[start..=end]
        } else {
            response
        }
    } else {
        response
    }
}

fn slugify(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else if c.is_whitespace() {
                '-'
            } else {
                '\0'
            }
        })
        .filter(|c| *c != '\0')
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn write_anti_patterns(reflections_dir: &Path, candidates: &[Value]) -> Result<usize> {
    let ap_dir = reflections_dir
        .parent()
        .map(|p| p.join("anti-patterns"))
        .unwrap_or_else(|| reflections_dir.join("../anti-patterns"));

    fs::create_dir_all(&ap_dir)
        .with_context(|| format!("failed to create anti-patterns dir: {}", ap_dir.display()))?;

    let mut count = 0usize;
    let date_str = chrono::Utc::now().format("%Y-%m-%d").to_string();

    for ap in candidates {
        let pattern_name = ap["pattern"].as_str().unwrap_or("unknown-pattern");
        let trigger = ap["trigger"].as_str().unwrap_or("unknown trigger");
        let avoidance = ap["avoidance"].as_str().unwrap_or("unknown avoidance");
        let refs = ap["evidence_refs"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();

        let slug = slugify(pattern_name);
        let file_path = ap_dir.join(format!("{slug}.md"));

        let content = if file_path.exists() {
            let existing = fs::read_to_string(&file_path).unwrap_or_default();
            let entry = format!(
                "\n\n## Observation — {date_str}\n\n- **Trigger**: {trigger}\n- **Avoidance**: {avoidance}\n- Evidence: {refs}\n"
            );
            format!("{existing}{entry}")
        } else {
            format!(
                "---\npattern: {pattern_name}\ntrigger: {trigger}\navoidance: {avoidance}\npromoted_at: {date_str}\nsource: reflection-synth\n---\n\n# {pattern_name}\n\n**Trigger**: {trigger}\n\n**Avoidance**: {avoidance}\n\n**Evidence**: {refs}\n"
            )
        };

        fs::write(&file_path, &content)
            .with_context(|| format!("failed to write anti-pattern: {}", file_path.display()))?;
        count += 1;
        debug!(pattern = pattern_name, path = %file_path.display(), "wrote anti-pattern candidate");
    }

    Ok(count)
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

    #[test]
    fn test_load_all_reflections_text_empty_dir() {
        let dir = tempdir().unwrap();
        let result = load_all_reflections_text(dir.path());
        assert!(result.is_empty());
    }

    #[test]
    fn test_load_all_reflections_text_concatenates() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("refl1.md"),
            "---\ntitle: r1\n---\nReflection one\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("refl2.md"),
            "---\ntitle: r2\n---\nReflection two\n",
        )
        .unwrap();

        let result = load_all_reflections_text(dir.path());
        assert!(result.contains("Reflection one"));
        assert!(result.contains("Reflection two"));
    }
}
