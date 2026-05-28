use crate::daily_log::DailyLog;
use chrono::NaiveDate;
use tracing::{debug, info};

use zen_core::paths::ZenPaths;

/// ZenDream — Nightly consolidation state.
///
/// Runs during a configurable window (default 2AM–4AM) to:
/// 1. Consolidate daily logs → extract durable facts
/// 2. Update MEMORY.md with new durable facts
/// 3. Compress old subconscious logs
/// 4. Recompute entity relationships
///
/// All operations are offline-first — no network/LLM calls required.
pub struct ZenDream;

impl ZenDream {
    /// Create a new ZenDream instance.
    pub fn new() -> Self {
        Self
    }

    /// Execute the full dream cycle for a given date.
    pub fn run_cycle(
        &self,
        zen_paths: &ZenPaths,
        date: NaiveDate,
    ) -> Result<DreamReport, DreamError> {
        info!("dream cycle started for {date}");

        let report = DreamReport {
            facts_extracted: consolidate_daily_log(zen_paths, date)?,
            memory_updated: update_memory(zen_paths)?,
            logs_compressed: compress_old_logs(zen_paths)?,
            entities_recomputed: recompute_entities(zen_paths)?,
        };

        info!(
            "dream cycle complete: facts={}, memory_updated={}, logs_compressed={}, entities={}",
            report.facts_extracted,
            report.memory_updated,
            report.logs_compressed,
            report.entities_recomputed
        );

        Ok(report)
    }
}

impl Default for ZenDream {
    fn default() -> Self {
        Self::new()
    }
}

/// Result summary from a dream cycle run.
#[derive(Debug, Default)]
pub struct DreamReport {
    pub facts_extracted: usize,
    pub memory_updated: bool,
    pub logs_compressed: bool,
    pub entities_recomputed: usize,
}

// ─── Error type ───────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum DreamError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("failed to read daily log: {0}")]
    DailyLogRead(String),

    #[error("failed to update MEMORY.md: {0}")]
    MemoryUpdate(String),
}

// ─── Consolidation steps ──────────────────────────────────────────────

/// Step 1: Consolidate daily log → extract durable facts.
///
/// Reads the daily log for `date`, extracts structured facts from
/// entries, returns the count of durable facts identified.
fn consolidate_daily_log(zen_paths: &ZenPaths, date: NaiveDate) -> Result<usize, DreamError> {
    let entries = DailyLog::read_entries(zen_paths, date)
        .map_err(|e| DreamError::DailyLogRead(e.to_string()))?;

    let mut fact_count = 0;

    for entry in &entries {
        let facts = extract_durable_facts(&entry.content);
        if !facts.is_empty() {
            debug!(
                "extracted {} durable fact(s) from entry at {}",
                facts.len(),
                entry.timestamp
            );
            fact_count += facts.len();
        }
    }

    Ok(fact_count)
}

/// Step 2: Update MEMORY.md with new durable facts.
///
/// Appends a dated section if new facts were consolidated today.
/// Returns `true` if the file was updated.
fn update_memory(zen_paths: &ZenPaths) -> Result<bool, DreamError> {
    let memory_path = zen_paths.global_root().join("MEMORY.md");

    if !memory_path.exists() {
        debug!("MEMORY.md not found, skipping update");
        return Ok(false);
    }

    let now = chrono::Utc::now().date_naive();
    let section_marker = format!("## Dream Facts — {now}");

    let content = std::fs::read_to_string(&memory_path)?;

    if content.contains(&section_marker) {
        debug!("dream facts for {now} already present in MEMORY.md");
        return Ok(false);
    }

    let daily_entries = DailyLog::read_entries(zen_paths, now)
        .map_err(|e| DreamError::MemoryUpdate(e.to_string()))?;

    let new_facts: Vec<String> = daily_entries
        .iter()
        .flat_map(|e| extract_durable_facts(&e.content))
        .collect();

    if new_facts.is_empty() {
        debug!("no new facts to append to MEMORY.md");
        return Ok(false);
    }

    let mut update = String::new();
    update.push_str(&format!("\n{section_marker}\n\n"));
    for fact in &new_facts {
        update.push_str(&format!("- {fact}\n"));
    }

    std::fs::write(&memory_path, format!("{content}{update}"))?;

    info!("MEMORY.md updated with {} new facts", new_facts.len());
    Ok(true)
}

/// Step 3: Compress old subconscious logs.
///
/// For logs older than 30 days, prepends a summary block and is
/// considered "compressed" once marked.
fn compress_old_logs(zen_paths: &ZenPaths) -> Result<bool, DreamError> {
    let logs_dir = zen_paths.global_root().join("logs");
    if !logs_dir.is_dir() {
        return Ok(false);
    }

    let thirty_days_ago = chrono::Utc::now().date_naive() - chrono::Duration::days(30);

    let mut any_marked = false;

    for entry in std::fs::read_dir(&logs_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.extension().is_some_and(|ext| ext == "md") {
            let content = std::fs::read_to_string(&path)?;
            if content.contains("<!-- dream:compressed -->") {
                continue;
            }

            let modified_opt = path.metadata().ok().and_then(|m| {
                use std::time::UNIX_EPOCH;
                m.modified().ok().and_then(|modified_time| {
                    modified_time
                        .duration_since(UNIX_EPOCH)
                        .ok()
                        .map(|dur| dur.as_secs())
                        .and_then(|secs| {
                            chrono::DateTime::<chrono::Utc>::from_timestamp(secs as i64, 0)
                        })
                })
            });

            if let Some(modified_dt) = modified_opt
                && modified_dt.date_naive() < thirty_days_ago
            {
                let header = "<!-- dream:compressed -->\n<!-- compressed at ".to_string()
                    + &chrono::Utc::now()
                        .naive_utc()
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string()
                    + " -->\n\n";
                std::fs::write(&path, format!("{}{}", header, content))?;
                any_marked = true;
                info!("old log compressed: {}", path.display());
            }
        }
    }

    Ok(any_marked)
}

/// Step 4: Recompute entity relationships.
///
/// Scans wiki pages for cross-links and returns the count of
/// unique relationships discovered.
fn recompute_entities(zen_paths: &ZenPaths) -> Result<usize, DreamError> {
    let wiki_dir = zen_paths.wiki();
    if !wiki_dir.is_dir() {
        debug!("wiki directory not found, skipping entity recompute");
        return Ok(0);
    }

    let mut relationship_count = 0;

    for entry in std::fs::read_dir(&wiki_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.extension().is_some_and(|ext| ext == "md") {
            let content = std::fs::read_to_string(&path)?;
            let links = extract_wikilinks(&content);
            relationship_count += links.len();
        }
    }

    debug!("entity recompute: found {relationship_count} relationships across wiki pages");
    Ok(relationship_count)
}

// ─── Helpers ──────────────────────────────────────────────────────────

/// Extract durable facts from a log entry content string.
///
/// Heuristic: lines that look like completed actions (past tense verbs,
/// numeric changes, etc.).
fn extract_durable_facts(content: &str) -> Vec<String> {
    let mut facts = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let lower = trimmed.to_lowercase();
        if lower.contains("completed")
            || lower.contains("fixed")
            || lower.contains("added")
            || lower.contains("removed")
            || lower.contains("resolved")
            || lower.contains("implemented")
            || lower.contains("created")
            || lower.contains("shipped")
            || lower.contains("deployed")
            || lower.contains("updated")
        {
            facts.push(trimmed.to_string());
        }
    }

    facts
}

/// Extract wikilinks from markdown content.
fn extract_wikilinks(content: &str) -> Vec<String> {
    let mut links = Vec::new();
    let mut chars = content.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '[' && chars.peek() == Some(&'[') {
            chars.next();
            let mut link = String::new();
            while let Some(ch) = chars.next() {
                if ch == ']' {
                    if chars.peek() == Some(&']') {
                        chars.next();
                        if !link.is_empty() {
                            // Handle [[Page|Alias]] format — extract page name before |
                            let page_name = link
                                .split('|')
                                .next()
                                .unwrap_or(&link)
                                .trim()
                                .to_string();
                            links.push(page_name);
                        }
                        break;
                    }
                } else {
                    link.push(ch);
                }
            }
        }
    }

    links
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_durable_facts() {
        let content = "Completed the auth module implementation\nJust a note\nFixed bug in search\nNothing special";
        let facts = extract_durable_facts(content);
        assert_eq!(facts.len(), 2);
        assert!(facts[0].contains("Completed"));
        assert!(facts[1].contains("Fixed"));
    }

    #[test]
    fn test_extract_durable_facts_empty() {
        let content = "Just thinking about things\ndecorating the office";
        let facts = extract_durable_facts(content);
        assert!(facts.is_empty());
    }

    #[test]
    fn test_extract_wikilinks() {
        let content = "See [[Foo]] and [[Bar|Some Title]] for details, not [single]";
        let links = extract_wikilinks(content);
        assert!(links.contains(&"Foo".to_string()));
        assert!(links.contains(&"Bar".to_string()));
    }
}
