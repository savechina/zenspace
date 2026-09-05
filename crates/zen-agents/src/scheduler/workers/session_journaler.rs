use std::fs;

use anyhow::{Context, Result};
use chrono::{Datelike, Utc};
use tracing::{debug, info, warn};

use zen_core::config::load_config;
use zen_core::paths::ZenPaths;
use zen_provider::DefaultRouter;

use super::super::{WorkerContext, WorkerReport, ZenWorker};
use super::marker_state::SessionState;

mod session_journaler_signals;
use session_journaler_signals as sig;

const MIN_TURNS: usize = 1;

pub struct SessionJournaler {
    scheduled: Option<&'static str>,
    fresh_eyes_mode: bool,
}

impl SessionJournaler {
    pub fn new() -> Self {
        Self {
            scheduled: None,
            fresh_eyes_mode: false,
        }
    }

    pub fn with_schedule(mut self, expr: &str) -> Self {
        self.scheduled = Some(Box::leak(expr.to_string().into_boxed_str()));
        self
    }

    pub fn with_fresh_eyes(mut self, enabled: bool) -> Self {
        self.fresh_eyes_mode = enabled;
        self
    }
}

impl Default for SessionJournaler {
    fn default() -> Self {
        Self::new()
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
                llm_cost_usd: 0.0,
            });
        }

        let jsonl_files = scan_jsonl_files(&sessions_dir)?;
        if jsonl_files.is_empty() {
            return Ok(WorkerReport {
                worker_id: self.id().to_string(),
                success: true,
                fact_count: 0,
                duration_ms: start.elapsed().as_millis() as u64,
                llm_cost_usd: 0.0,
            });
        }

        let fresh_eyes_config = load_config()
            .ok()
            .and_then(|c| c.cron.fresh_eyes_mode)
            .unwrap_or(false);
        let fresh_eyes = self.fresh_eyes_mode || fresh_eyes_config || Utc::now().day() == 1;
        if fresh_eyes {
            info!("fresh eyes mode active — skipping prior context injection");
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
        let mut skipped_short = 0usize;
        let mut total_files = 0usize;

        for jsonl_path in &jsonl_files {
            if has_journaled_marker(jsonl_path) {
                continue;
            }

            total_files += 1;
            let session_id = extract_session_id(jsonl_path);
            match process_session(&paths, jsonl_path, &session_id, router.clone(), fresh_eyes).await
            {
                Ok(0) => {
                    skipped_short += 1;
                    debug!(
                        session_id = %session_id,
                        "session skipped (short conversation)"
                    );
                }
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

        if skipped_short > 0 && processed == 0 {
            warn!(
                skipped = skipped_short,
                min_turns = MIN_TURNS,
                "all {} unprocessed sessions too short for journaling — knowledge pipeline may be starved",
                total_files
            );
        } else if skipped_short > 0 {
            debug!(
                skipped = skipped_short,
                min_turns = MIN_TURNS,
                "skipped {} short sessions",
                skipped_short
            );
        }

        if processed > 0 {
            info!(
                processed = processed,
                facts = total_facts,
                skipped = skipped_short,
                "session-journaler tick complete"
            );
        }

        Ok(WorkerReport {
            worker_id: self.id().to_string(),
            success: true,
            fact_count: total_facts,
            duration_ms: start.elapsed().as_millis() as u64,
            llm_cost_usd: 0.0,
        })
    }
}

async fn process_session(
    paths: &ZenPaths,
    jsonl_path: &std::path::Path,
    session_id: &str,
    router: Option<DefaultRouter>,
    fresh_eyes: bool,
) -> Result<usize> {
    let store = zen_memory::conversation::ConversationStore::with_file(
        jsonl_path.to_path_buf(),
        session_id,
    )?;
    let turns = store.load()?;

    if turns.len() < MIN_TURNS {
        if turns.is_empty() {
            warn!(session_id = %session_id, "session has 0 turns, nothing to extract");
        } else {
            debug!(session_id = %session_id, turns = turns.len(), "skipping short session");
        }
        return Ok(0);
    }

    let conversation_text = sig::build_conversation_text(&turns);

    let matched_anti_patterns = if fresh_eyes {
        Vec::new()
    } else {
        let anti_patterns_dir = paths.vault().join("wiki/wisdom/anti-patterns");
        let matched = sig::check_anti_pattern_match(&conversation_text, &anti_patterns_dir);
        if !matched.is_empty() {
            info!(
                session_id = %session_id,
                anti_patterns = ?matched,
                "anti-patterns detected, forcing reflection extraction"
            );
        }
        matched
    };

    let prompt_context = if fresh_eyes {
        sig::PromptContext {
            commitments_section: String::new(),
            beliefs_section: String::new(),
            anti_patterns_section: String::new(),
        }
    } else {
        sig::load_prompt_context(paths).await
    };

    let (mut signals, source) = if let Some(router) = router {
        match sig::extract_signals_via_llm(
            &conversation_text,
            &prompt_context,
            router,
            &matched_anti_patterns,
            fresh_eyes,
        )
        .await
        {
            Ok(llm_signals) if !llm_signals.is_empty() => {
                info!(session_id = %session_id, total = llm_signals.total(), "LLM signal extraction succeeded");
                (llm_signals, "llm")
            }
            Ok(_) => {
                debug!(session_id = %session_id, "LLM returned no signals, falling back to keyword");
                (
                    sig::extract_signals_via_keyword(&conversation_text),
                    "keyword",
                )
            }
            Err(e) => {
                warn!(session_id = %session_id, error = %e, "LLM extraction failed, falling back to keyword");
                (
                    sig::extract_signals_via_keyword(&conversation_text),
                    "keyword",
                )
            }
        }
    } else {
        (
            sig::extract_signals_via_keyword(&conversation_text),
            "keyword",
        )
    };

    signals
        .preferences
        .extend(sig::derive_preferences(&conversation_text));

    let (signals, filtered) =
        sig::apply_quality_prefilter(&paths.vault(), signals, source == "llm");
    if filtered > 0 {
        debug!(
            session_id = %session_id,
            filtered = filtered,
            "quality pre-filter dropped low-grade signals"
        );
    }

    let journal_content = sig::build_journal_entry(session_id, turns.len(), &signals, source);
    write_journal_entry(paths, session_id, &journal_content)?;

    sig::save_typed_signals(paths, &signals);

    append_journaled_marker(jsonl_path, source)?;

    Ok(signals.total())
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_extract_session_id() {
        let path = std::path::PathBuf::from("/tmp/sessions/2026/06/20/test-session-id.jsonl");
        assert_eq!(extract_session_id(&path), "test-session-id");
    }

    #[test]
    fn test_fresh_eyes_mode_skips_context() {
        let journaler = SessionJournaler::new().with_fresh_eyes(true);
        assert!(journaler.fresh_eyes_mode);
    }

    #[test]
    fn test_fresh_eyes_mode_default_false() {
        let journaler = SessionJournaler::new();
        assert!(!journaler.fresh_eyes_mode);
    }
}
