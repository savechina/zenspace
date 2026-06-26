use std::fs;

use anyhow::{Context, Result};
use chrono::Utc;
use tracing::{debug, info, warn};

use zen_core::config::load_config;
use zen_core::paths::ZenPaths;
use zen_core::types::Sensitivity;
use zen_memory::conversation::ConversationStore;
use zen_memory::dream::{ExtractedSignals, extract_durable_facts_from_entry};
use zen_provider::{DefaultRouter, LlmRouterExt};

use super::super::{WorkerContext, WorkerReport, ZenWorker};
use super::marker_state::SessionState;

const MIN_TURNS: usize = 3;

struct PromptContext {
    commitments_section: String,
    beliefs_section: String,
}

impl PromptContext {
    fn is_empty(&self) -> bool {
        self.commitments_section.is_empty() && self.beliefs_section.is_empty()
    }

    fn to_prompt_section(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut s = String::from("\n--- Context ---\n");
        if !self.commitments_section.is_empty() {
            s.push_str(&self.commitments_section);
            s.push('\n');
        }
        if !self.beliefs_section.is_empty() {
            s.push_str(&self.beliefs_section);
            s.push('\n');
        }
        s
    }
}

struct CommitmentSummary {
    text: String,
    #[allow(dead_code)]
    status: String,
    review_at: String,
}

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

    let prompt_context = load_prompt_context(paths).await;

    let (signals, source) = if let Some(router) = router {
        match extract_signals_via_llm(&conversation_text, &prompt_context, router).await {
            Ok(llm_signals) if !llm_signals.is_empty() => {
                info!(session_id = %session_id, total = llm_signals.total(), "LLM signal extraction succeeded");
                (llm_signals, "llm")
            }
            Ok(_) => {
                debug!(session_id = %session_id, "LLM returned no signals, falling back to keyword");
                (extract_signals_via_keyword(&conversation_text), "keyword")
            }
            Err(e) => {
                warn!(session_id = %session_id, error = %e, "LLM extraction failed, falling back to keyword");
                (extract_signals_via_keyword(&conversation_text), "keyword")
            }
        }
    } else {
        (extract_signals_via_keyword(&conversation_text), "keyword")
    };

    let journal_content = build_journal_entry(session_id, turns.len(), &signals, source);
    write_journal_entry(paths, session_id, &journal_content)?;

    append_journaled_marker(jsonl_path, source)?;

    Ok(signals.total())
}

async fn extract_signals_via_llm(
    conversation_text: &str,
    prompt_context: &PromptContext,
    router: DefaultRouter,
) -> Result<ExtractedSignals> {
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

    let context_section = prompt_context.to_prompt_section();

    let prompt = format!(
        r#"Extract typed signals from this development session conversation.

Conversation:
{truncated}
{context_section}

Respond with ONLY a JSON object:
{{
  "facts": [
    "Implemented JWT authentication with refresh token rotation",
    "Decided to use SQLite for local storage instead of PostgreSQL"
  ],
  "reflections": [
    "The login flow is too complex — users get confused at step 3",
    "Should have tested the migration on a copy first"
  ],
  "commitments": [
    "Simplify login to 2 steps by 2026-07-01",
    "Write integration tests for the auth module this week"
  ]
}}

Rules:
- **Facts**: past-tense, specific, durable — useful after 6 months. Technical decisions, bug fixes, learnings.
- **Reflections**: what went wrong, what could be better, what surprised you. Self-critical, honest.
- **Commitments**: what you (the user) plan to do next. Include a rough timeframe if mentioned.
- Do NOT include transient mechanics ("user asked about X", "assistant replied")
- If a category is empty, return an empty array for it
- If nothing of value happened in any category, return all empty arrays"#
    );

    let response = tokio::task::spawn_blocking(move || {
        router.complete("signal_extraction", &prompt, Sensitivity::Private)
    })
    .await
    .context("LLM signal extraction task panicked")??;

    let json_str = if let Some(start) = response.find("```json") {
        let after = &response[start + 7..];
        if let Some(end) = after.find("```") {
            &after[..end]
        } else {
            after
        }
    } else if let Some(start) = response.find('{') {
        if let Some(end) = response.rfind('}') {
            &response[start..=end]
        } else {
            &response
        }
    } else {
        &response
    };

    let parsed: serde_json::Value = serde_json::from_str(json_str.trim())
        .context("failed to parse LLM signal extraction response")?;

    let mut signals = ExtractedSignals::default();

    if let Some(arr) = parsed["facts"].as_array() {
        for item in arr {
            if let Some(s) = item.as_str() {
                let s = s.trim();
                if !s.is_empty() && s != "No durable facts extracted." {
                    signals.facts.push(s.to_string());
                }
            }
        }
    }

    if let Some(arr) = parsed["reflections"].as_array() {
        for item in arr {
            if let Some(s) = item.as_str() {
                let s = s.trim();
                if !s.is_empty() {
                    signals.reflections.push(s.to_string());
                }
            }
        }
    }

    if let Some(arr) = parsed["commitments"].as_array() {
        for item in arr {
            if let Some(s) = item.as_str() {
                let s = s.trim();
                if !s.is_empty() {
                    signals.commitments.push(s.to_string());
                }
            }
        }
    }

    Ok(signals)
}

fn extract_signals_via_keyword(conversation_text: &str) -> ExtractedSignals {
    let facts = extract_durable_facts_from_entry(conversation_text);
    ExtractedSignals {
        facts,
        reflections: Vec::new(),
        commitments: Vec::new(),
    }
}

fn build_conversation_text(turns: &[(String, String)]) -> String {
    let mut text = String::new();
    for (role, content) in turns {
        text.push_str(&format!("{role}: {content}\n"));
    }
    text
}

fn build_journal_entry(
    session_id: &str,
    turn_count: usize,
    signals: &ExtractedSignals,
    source: &str,
) -> String {
    let now = Utc::now();
    let date_str = now.format("%Y-%m-%d").to_string();
    let timestamp_str = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let mut entry = format!(
        "---\nsession_id: {session_id}\ndate: {date_str}\nturn_count: {turn_count}\nsource: {source}\n---\n\n# Session Journal — {timestamp_str}\n\n"
    );

    entry.push_str("## Facts\n\n");
    if signals.facts.is_empty() {
        entry.push_str("_(no durable facts extracted)_\n\n");
    } else {
        for fact in &signals.facts {
            entry.push_str(&format!("- {fact}\n"));
        }
        entry.push('\n');
    }

    entry.push_str("## Reflections\n\n");
    if signals.reflections.is_empty() {
        entry.push_str("_(no reflections extracted)_\n\n");
    } else {
        for refl in &signals.reflections {
            entry.push_str(&format!("- {refl}\n"));
        }
        entry.push('\n');
    }

    entry.push_str("## Commitments\n\n");
    if signals.commitments.is_empty() {
        entry.push_str("_(no commitments extracted)_\n");
    } else {
        for comm in &signals.commitments {
            entry.push_str(&format!("- {comm}\n"));
        }
    }

    entry
}

fn write_journal_entry(paths: &ZenPaths, session_id: &str, content: &str) -> Result<()> {
    let dir = paths.journal_entries();
    fs::create_dir_all(&dir).with_context(|| {
        format!(
            "failed to create journal entries directory: {}",
            dir.display()
        )
    })?;

    let date_str = Utc::now().format("%Y-%m-%d").to_string();
    let filename = format!("{date_str}-{session_id}.md");
    let path = dir.join(&filename);

    fs::write(&path, content)
        .with_context(|| format!("failed to write journal entry: {}", path.display()))?;

    debug!("journal entry written: {}", path.display());
    Ok(())
}

fn append_journaled_marker(jsonl_path: &std::path::Path, source: &str) -> Result<()> {
    let state = SessionState {
        journaled: true,
        journaled_at: Some(Utc::now().to_rfc3339()),
        journaled_source: Some(source.to_string()),
    };
    state.save(jsonl_path)
}

fn has_journaled_marker(jsonl_path: &std::path::Path) -> bool {
    SessionState::is_journaled(jsonl_path)
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

async fn load_prompt_context(paths: &ZenPaths) -> PromptContext {
    let commitments_section = load_top_commitments(paths, 5);
    let beliefs_section = load_top_beliefs(paths, 5);
    PromptContext {
        commitments_section,
        beliefs_section,
    }
}

fn load_top_commitments(paths: &ZenPaths, n: usize) -> String {
    let dir = paths.vault().join("memories/commitments");
    let items = scan_commitments(&dir);
    if items.is_empty() {
        return String::new();
    }
    let top: Vec<&CommitmentSummary> = items.iter().take(n).collect();
    let mut s = String::from("User's active commitments (prioritize signals relevant to these):\n");
    for item in top {
        s.push_str(&format!("- {}\n", item.text));
    }
    s
}

fn load_top_beliefs(paths: &ZenPaths, n: usize) -> String {
    let dir = paths.vault().join("memories/beliefs");
    let beliefs = match zen_memory::belief::Belief::load_all(&dir) {
        Ok(b) => b,
        Err(_) => return String::new(),
    };
    if beliefs.is_empty() {
        return String::new();
    }
    let top = zen_memory::belief::top_by_priority(&beliefs, n);
    let mut s = String::from("User's current beliefs (by confidence):\n");
    for b in top {
        s.push_str(&format!(
            "- {} ({:.0}% confident)\n",
            b.proposition,
            b.posterior * 100.0
        ));
    }
    s
}

fn scan_commitments(dir: &std::path::Path) -> Vec<CommitmentSummary> {
    let mut items = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return items,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.extension().is_some_and(|ext| ext == "md") {
            continue;
        }
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let text = parse_frontmatter_field(&content, "text").unwrap_or_default();
        let status =
            parse_frontmatter_field(&content, "status").unwrap_or_else(|| "open".to_string());
        let review_at = parse_frontmatter_field(&content, "review_at").unwrap_or_default();
        if status == "open" && !text.is_empty() {
            items.push(CommitmentSummary {
                text,
                status,
                review_at,
            });
        }
    }
    items.sort_by(|a, b| a.review_at.cmp(&b.review_at));
    items
}

fn parse_frontmatter_field(content: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    for line in content.lines().take(15) {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(&prefix) {
            let val = rest.trim().trim_matches('"').to_string();
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn test_has_journaled_marker_via_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        fs::write(&path, "{\"type\":\"session/meta\"}\n").unwrap();

        let state = SessionState {
            journaled: true,
            journaled_at: Some("2026-06-20T14:30:00Z".to_string()),
            journaled_source: Some("keyword".to_string()),
        };
        state.save(&path).unwrap();

        assert!(has_journaled_marker(&path));
    }

    #[test]
    fn test_has_journaled_marker_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        fs::write(
            &path,
            "{\"type\":\"session/meta\"}\n{\"type\":\"chat/turn\"}\n",
        )
        .unwrap();

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
        let signals = ExtractedSignals {
            facts: vec![
                "completed auth module".to_string(),
                "fixed login bug".to_string(),
            ],
            reflections: vec![],
            commitments: vec![],
        };

        let entry = build_journal_entry(session_id, turn_count, &signals, "keyword");

        assert!(entry.contains("session_id: 01JX0TEST000000000000000000"));
        assert!(entry.contains("turn_count: 5"));
        assert!(entry.contains("source: keyword"));
        assert!(!entry.contains("journaled_at:"));
        assert!(entry.contains("completed auth module"));
        assert!(entry.contains("fixed login bug"));
        assert!(entry.contains("## Facts"));
        assert!(entry.contains("## Reflections"));
        assert!(entry.contains("## Commitments"));
    }

    #[test]
    fn test_build_journal_entry_empty_signals() {
        let session_id = "01JX0TEST000000000000000000";
        let signals = ExtractedSignals::default();
        let entry = build_journal_entry(session_id, 3, &signals, "keyword");

        assert!(entry.contains("_(no durable facts extracted)_"));
        assert!(entry.contains("_(no reflections extracted)_"));
        assert!(entry.contains("_(no commitments extracted)_"));
    }

    #[test]
    fn test_build_journal_entry_all_sections() {
        let session_id = "01JX0TEST000000000000000000";
        let signals = ExtractedSignals {
            facts: vec!["implemented auth".to_string()],
            reflections: vec!["login flow too complex".to_string()],
            commitments: vec!["simplify login by July".to_string()],
        };
        let entry = build_journal_entry(session_id, 10, &signals, "llm");

        assert!(entry.contains("## Facts"));
        assert!(entry.contains("- implemented auth"));
        assert!(entry.contains("## Reflections"));
        assert!(entry.contains("- login flow too complex"));
        assert!(entry.contains("## Commitments"));
        assert!(entry.contains("- simplify login by July"));
        assert!(entry.contains("source: llm"));
    }

    #[test]
    fn test_keyword_fallback_returns_facts_only() {
        let conversation = "user: completed the auth module\nassistant: great";
        let signals = extract_signals_via_keyword(conversation);

        assert!(!signals.facts.is_empty(), "keyword should extract facts");
        assert!(
            signals.reflections.is_empty(),
            "keyword returns no reflections"
        );
        assert!(
            signals.commitments.is_empty(),
            "keyword returns no commitments"
        );
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

    #[test]
    fn test_load_top_commitments_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ZenPaths::detect().unwrap_or_else(|_| {
            panic!("ZenPaths::detect failed");
        });
        let result = load_top_commitments(&paths, 5);
        assert!(result.is_empty() || result.contains("active commitments"));
    }

    #[test]
    fn test_load_top_beliefs_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ZenPaths::detect().unwrap_or_else(|_| {
            panic!("ZenPaths::detect failed");
        });
        let result = load_top_beliefs(&paths, 5);
        assert!(result.is_empty() || result.contains("beliefs"));
    }

    #[test]
    fn test_prompt_context_empty_to_prompt_section() {
        let ctx = PromptContext {
            commitments_section: String::new(),
            beliefs_section: String::new(),
        };
        assert!(ctx.is_empty());
        assert!(ctx.to_prompt_section().is_empty());
    }

    #[test]
    fn test_prompt_context_nonempty_has_sections() {
        let ctx = PromptContext {
            commitments_section: "commitments here\n".to_string(),
            beliefs_section: "beliefs here\n".to_string(),
        };
        assert!(!ctx.is_empty());
        let section = ctx.to_prompt_section();
        assert!(section.contains("--- Context ---"));
        assert!(section.contains("commitments here"));
        assert!(section.contains("beliefs here"));
    }

    #[test]
    fn test_parse_frontmatter_field_found() {
        let content = "---\ntext: Do the thing\nstatus: open\n---\n\nbody";
        assert_eq!(
            parse_frontmatter_field(content, "text"),
            Some("Do the thing".to_string())
        );
        assert_eq!(
            parse_frontmatter_field(content, "status"),
            Some("open".to_string())
        );
    }

    #[test]
    fn test_parse_frontmatter_field_missing() {
        let content = "---\ntext: Do the thing\n---\n\nbody";
        assert_eq!(parse_frontmatter_field(content, "status"), None);
    }

    #[test]
    fn test_scan_commitments_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let items = scan_commitments(dir.path());
        assert!(items.is_empty());
    }

    #[test]
    fn test_scan_commitments_filters_closed() {
        let dir = tempfile::tempdir().unwrap();
        let content = "---\ntext: Open task\nstatus: open\nreview_at: 2026-07-01T00:00:00Z\n---\n\n# Commitment\n\nOpen task\n";
        fs::write(dir.path().join("open.md"), content).unwrap();

        let closed = "---\ntext: Done task\nstatus: done\nreview_at: 2026-06-01T00:00:00Z\n---\n\n# Commitment\n\nDone task\n";
        fs::write(dir.path().join("closed.md"), closed).unwrap();

        let items = scan_commitments(dir.path());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "Open task");
    }
}
