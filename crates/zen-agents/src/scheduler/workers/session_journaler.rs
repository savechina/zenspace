use std::fs;
use std::io::Write;

use anyhow::{Context, Result};
use chrono::Utc;
use tracing::{debug, info, warn};

use zen_core::config::load_config;
use zen_core::paths::ZenPaths;
use zen_core::types::Sensitivity;
use zen_memory::conversation::ConversationStore;
use zen_memory::dream::extract_durable_facts_from_entry;
use zen_provider::{DefaultRouter, LlmRouterExt};

use super::super::{WorkerContext, WorkerReport, ZenWorker};

const MARKER_PREFIX: &str = r#"{"type":"system/journaled""#;
const MARKER_SEARCH_BYTES: usize = 200;
const MIN_TURNS: usize = 3;

pub struct SessionJournaler {
    scheduled: Option<&'static str>,
}

impl SessionJournaler {
    pub fn new() -> Self {
        Self { scheduled: None }
    }

    pub fn with_schedule(mut self, expr: &str) -> Self {
        self.scheduled = Some(Box::leak(expr.to_string().into_boxed_str()));
        self
    }
}

#[async_trait::async_trait]
impl ZenWorker for SessionJournaler {
    fn id(&self) -> &'static str {
        "session-journaler"
    }

    fn description(&self) -> &'static str {
        "Scan session conversations, extract durable facts, write journal entries"
    }

    fn schedule(&self) -> &'static str {
        self.scheduled.unwrap_or("0 */5 * * * *")
    }

    async fn execute(&self, _ctx: &WorkerContext) -> Result<WorkerReport> {
        let start = std::time::Instant::now();
        let paths = ZenPaths::detect()?;

        let sessions_dir = paths.sessions();
        if !sessions_dir.is_dir() {
            debug!("sessions directory does not exist, skipping");
            return Ok(WorkerReport {
                worker_id: self.id().to_string(),
                success: true,
                fact_count: 0,
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }

        let jsonl_files = scan_jsonl_files(&sessions_dir)?;
        if jsonl_files.is_empty() {
            return Ok(WorkerReport {
                worker_id: self.id().to_string(),
                success: true,
                fact_count: 0,
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }

        let router = match load_config() {
            Ok(c) => Some(DefaultRouter::from_agentic(c)),
            Err(e) => {
                warn!(error = %e, "failed to load config for LLM journaling, falling back to keyword-only");
                None
            }
        };

        let mut processed = 0usize;
        let mut total_facts = 0usize;

        for jsonl_path in &jsonl_files {
            if has_journaled_marker(jsonl_path) {
                continue;
            }

            let session_id = extract_session_id(jsonl_path);
            match process_session(&paths, jsonl_path, &session_id, router.clone()).await {
                Ok(facts) => {
                    total_facts += facts;
                    processed += 1;
                    info!(
                        session_id = %session_id,
                        facts = facts,
                        "journal entry written from session conversation"
                    );
                }
                Err(e) => {
                    warn!(
                        session_id = %session_id,
                        error = %e,
                        "failed to journal session conversation"
                    );
                }
            }
        }

        if processed > 0 {
            info!(
                processed = processed,
                facts = total_facts,
                "session-journaler tick complete"
            );
        }

        Ok(WorkerReport {
            worker_id: self.id().to_string(),
            success: true,
            fact_count: total_facts,
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }
}

async fn process_session(
    paths: &ZenPaths,
    jsonl_path: &std::path::Path,
    session_id: &str,
    router: Option<DefaultRouter>,
) -> Result<usize> {
    let store = ConversationStore::with_file(jsonl_path.to_path_buf(), session_id)?;
    let turns = store.load()?;

    if turns.len() < MIN_TURNS {
        debug!(session_id = %session_id, turns = turns.len(), "skipping short session");
        return Ok(0);
    }

    let conversation_text = build_conversation_text(&turns);

    let (facts, source) = if let Some(router) = router {
        match extract_facts_via_llm(&conversation_text, router).await {
            Ok(llm_facts) if !llm_facts.is_empty() => {
                info!(session_id = %session_id, count = llm_facts.len(), "LLM fact extraction succeeded");
                (llm_facts, "llm")
            }
            Ok(_) => {
                debug!(session_id = %session_id, "LLM returned no facts, falling back to keyword");
                (extract_durable_facts_from_entry(&conversation_text), "keyword")
            }
            Err(e) => {
                warn!(session_id = %session_id, error = %e, "LLM extraction failed, falling back to keyword");
                (extract_durable_facts_from_entry(&conversation_text), "keyword")
            }
        }
    } else {
        (extract_durable_facts_from_entry(&conversation_text), "keyword")
    };

    let journal_content = build_journal_entry(session_id, turns.len(), &facts, source);
    write_journal_entry(paths, session_id, &journal_content)?;

    append_journaled_marker(jsonl_path, source)?;

    Ok(facts.len())
}

async fn extract_facts_via_llm(
    conversation_text: &str,
    router: DefaultRouter,
) -> Result<Vec<String>> {
    let truncated = if conversation_text.len() > 12000 {
        let end = conversation_text
            .char_indices()
            .nth(12000)
            .map(|(i, _)| i)
            .unwrap_or(conversation_text.len());
        format!("{}...", &conversation_text[..end])
    } else {
        conversation_text.to_string()
    };

    let prompt = format!(
        r#"Extract durable facts from this development session conversation. Durable facts are insights, decisions, problems solved, techniques learned, or architectural choices that remain useful after 6 months.

Conversation:
{truncated}

Respond with ONLY a JSON object:
{{
  "facts": [
    "Implemented JWT authentication with refresh token rotation",
    "Decided to use SQLite for local storage instead of PostgreSQL",
    "Fixed race condition in the session journaler marker check",
    "Learned that floor_char_boundary() prevents UTF-8 slice panics"
  ]
}}

Rules:
- Facts MUST be past-tense, specific, and durable — useful after 6 months
- Do NOT include transient mechanics ("user asked about X", "assistant replied")
- Include technical decisions, bug fixes, architectural choices, and learnings
- If nothing durable happened, return empty array"#
    );

    let response = tokio::task::spawn_blocking(move || {
        router.complete("fact_extraction", &prompt, Sensitivity::Private)
    })
    .await
    .context("LLM fact extraction task panicked")??;

    let json_str = if let Some(start) = response.find("```json") {
        let after = &response[start + 7..];
        if let Some(end) = after.find("```") { &after[..end] } else { after }
    } else if let Some(start) = response.find('{') {
        if let Some(end) = response.rfind('}') { &response[start..=end] } else { &response }
    } else {
        &response
    };

    let parsed: serde_json::Value = serde_json::from_str(json_str.trim())
        .context("failed to parse LLM fact extraction response")?;

    let mut facts = Vec::new();
    if let Some(arr) = parsed["facts"].as_array() {
        for item in arr {
            if let Some(fact) = item.as_str() {
                let f = fact.trim().to_string();
                if !f.is_empty() && f != "No durable facts extracted." {
                    facts.push(f);
                }
            }
        }
    }

    Ok(facts)
}

fn build_conversation_text(turns: &[(String, String)]) -> String {
    let mut text = String::new();
    for (role, content) in turns {
        text.push_str(&format!("{role}: {content}\n"));
    }
    text
}

fn build_journal_entry(session_id: &str, turn_count: usize, facts: &[String], source: &str) -> String {
    let now = Utc::now();
    let date_str = now.format("%Y-%m-%d").to_string();
    let timestamp_str = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let mut entry = format!(
        "---\nsession_id: {session_id}\ndate: {date_str}\nturn_count: {turn_count}\njournaled_at: {timestamp_str}\nsource: {source}\n---\n\n# Session Journal — {timestamp_str}\n\n"
    );

    entry.push_str("## Facts\n\n");
    if facts.is_empty() {
        entry.push_str("_(no durable facts extracted)_\n");
    } else {
        for fact in facts {
            entry.push_str(&format!("- {fact}\n"));
        }
    }

    entry
}

fn write_journal_entry(paths: &ZenPaths, session_id: &str, content: &str) -> Result<()> {
    let dir = paths.journal_entries();
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create journal entries directory: {}", dir.display()))?;

    let date_str = Utc::now().format("%Y-%m-%d").to_string();
    let filename = format!("{date_str}-{session_id}.md");
    let path = dir.join(&filename);

    fs::write(&path, content)
        .with_context(|| format!("failed to write journal entry: {}", path.display()))?;

    debug!("journal entry written: {}", path.display());
    Ok(())
}

fn append_journaled_marker(jsonl_path: &std::path::Path, source: &str) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    let marker = format!(
        r#"{{"type":"system/journaled","payload":{{"timestamp":"{now}","source":"{source}"}}}}"#
    );

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(jsonl_path)
        .with_context(|| format!("failed to open session file for marker: {}", jsonl_path.display()))?;

    writeln!(file, "{marker}")?;
    Ok(())
}

fn has_journaled_marker(jsonl_path: &std::path::Path) -> bool {
    let content = match fs::read_to_string(jsonl_path) {
        Ok(c) => c,
        Err(_) => return false,
    };

    if content.len() <= MARKER_SEARCH_BYTES {
        return content.contains(MARKER_PREFIX);
    }

    let tail_start = content
        .floor_char_boundary(content.len().saturating_sub(MARKER_SEARCH_BYTES));
    content[tail_start..].contains(MARKER_PREFIX)
}

fn scan_jsonl_files(dir: &std::path::Path) -> Result<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();
    scan_dir(dir, &mut files)?;
    Ok(files)
}

fn scan_dir(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }

    for entry in fs::read_dir(dir)
        .with_context(|| format!("failed to read sessions directory: {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            scan_dir(&path, files)?;
        } else if path.extension().is_some_and(|ext| ext == "jsonl") {
            files.push(path);
        }
    }

    Ok(())
}

fn extract_session_id(jsonl_path: &std::path::Path) -> String {
    jsonl_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use std::io::Write as _;

    fn write_marker(path: &std::path::Path) {
        let marker = r#"{"type":"system/journaled","payload":{"timestamp":"2026-06-20T14:30:00Z","source":"keyword"}}"#;
        let mut file = fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
        writeln!(file, "{marker}").unwrap();
    }

    #[test]
    fn test_has_journaled_marker_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");

        fs::write(&path, "{\"type\":\"session/meta\"}\n").unwrap();
        write_marker(&path);

        assert!(has_journaled_marker(&path));
    }

    #[test]
    fn test_has_journaled_marker_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");

        fs::write(&path, "{\"type\":\"session/meta\"}\n{\"type\":\"chat/turn\"}\n").unwrap();

        assert!(!has_journaled_marker(&path));
    }

    #[test]
    fn test_has_journaled_marker_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");

        fs::write(&path, "").unwrap();

        assert!(!has_journaled_marker(&path));
    }

    #[test]
    fn test_has_journaled_marker_missing_file() {
        let path = std::path::PathBuf::from("/nonexistent/path/test.jsonl");
        assert!(!has_journaled_marker(&path));
    }

    #[test]
    fn test_build_journal_entry() {
        let session_id = "01JX0TEST000000000000000000";
        let turn_count = 5;
        let facts = vec!["completed auth module".to_string(), "fixed login bug".to_string()];

        let entry = build_journal_entry(session_id, turn_count, &facts, "keyword");

        assert!(entry.contains("session_id: 01JX0TEST000000000000000000"));
        assert!(entry.contains("turn_count: 5"));
        assert!(entry.contains("source: keyword"));
        assert!(entry.contains("completed auth module"));
        assert!(entry.contains("fixed login bug"));
        assert!(entry.contains("## Facts"));
    }

    #[test]
    fn test_build_journal_entry_empty_facts() {
        let session_id = "01JX0TEST000000000000000000";
        let entry = build_journal_entry(session_id, 3, &[], "keyword");

        assert!(entry.contains("_(no durable facts extracted)_"));
    }

    #[test]
    fn test_build_conversation_text() {
        let turns = vec![
            ("user".to_string(), "Hello".to_string()),
            ("assistant".to_string(), "Hi there!".to_string()),
        ];

        let text = build_conversation_text(&turns);

        assert_eq!(text, "user: Hello\nassistant: Hi there!\n");
    }

    #[test]
    fn test_extract_session_id() {
        let path = std::path::PathBuf::from("/tmp/sessions/2026/06/20/test-session-id.jsonl");
        assert_eq!(extract_session_id(&path), "test-session-id");
    }
}
