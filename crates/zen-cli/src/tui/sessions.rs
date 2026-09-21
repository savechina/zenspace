use zen_core::paths::ZenPaths;
use zen_core::types::{MessageRole, SessionContext};
use zen_memory::conversation::ConversationStore;

use super::cell::{MarkdownCell, OutputCell};
use super::render::normalize_compact_markdown;

use zen_memory::session::SessionManager;

use super::app::App;

impl App {
    pub(crate) fn ensure_session(&mut self, first_message: &str) {
        if self.session_id.is_some() {
            return;
        }

        let manager = SessionManager::new();
        match manager.create_session("tui", ".") {
            Ok(mut session) => {
                let title: String = first_message.chars().take(60).collect();
                let title = if title.len() < first_message.len() {
                    format!("{}...", title)
                } else {
                    title
                };
                session.title = Some(title);
                if let Err(e) = session.save() {
                    tracing::warn!(error = %e, session_id = %session.id, "failed to save session metadata");
                }

                self.session_id = Some(session.id.clone());
                if let Ok(paths) = ZenPaths::detect() {
                    let date_dir = paths.session_dir_for_date(session.created_at);
                    self.conversation_store =
                        ConversationStore::with_dir(date_dir, &session.id).ok();
                }
                if let Some(ref mut ctx) = self.session {
                    ctx.session_id = session.id.parse().unwrap_or(ctx.session_id);
                }
                tracing::info!(
                    session_id = %session.id,
                    "auto-created session on first message"
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to auto-create session");
            }
        }
    }

    pub(crate) fn save_session_state(&mut self) {
        if let Some(ref id) = self.session_id
            && let Ok(mut notion) = zen_core::types::Session::load(id)
        {
            notion.updated_at = chrono::Utc::now();
            notion.status = zen_core::types::SessionStatus::Completed;
            if let Err(e) = notion.save() {
                tracing::warn!(error = %e, session_id = %id, "failed to save session notion status");
            }

            // Write daily log entry with conversation content
            if self.chat_history.is_empty() {
                return;
            }
            let turn_count = self.chat_history.len();

            let mut summary = if notion
                .title
                .as_ref()
                .map(|t| !t.is_empty())
                .unwrap_or(false)
            {
                format!(
                    "Agent session: {} agent ({} turns) — \"{}\"\n",
                    notion.agent_name,
                    turn_count,
                    notion.title.as_ref().unwrap()
                )
            } else {
                format!(
                    "Agent session: {} agent ({} turns)\n",
                    notion.agent_name, turn_count
                )
            };

            let start = turn_count.saturating_sub(10);
            for (role, content) in &self.chat_history[start..] {
                let preview: String = content.chars().take(200).collect();
                let ellipsis = if content.len() > 200 { "…" } else { "" };
                summary.push_str(&format!("  {role}: {preview}{ellipsis}\n"));
            }

            tracing::debug!(session_id = %id, turns = turn_count, "writing daily log entry for session end");
            if let Ok(paths) = ZenPaths::detect()
                && let Err(e) = zen_memory::journal::Journal::create_entry(&paths, &summary)
            {
                tracing::warn!(error = %e, session_id = %id, "failed to write daily journal entry for session end");
            }
        }
    }

    pub(crate) fn execute_new_session(&mut self) {
        let manager = SessionManager::new();
        match manager.create_session("default", ".") {
            Ok(session) => {
                self.session_id = Some(session.id.clone());
                if let Ok(paths) = ZenPaths::detect() {
                    let date_dir = paths.session_dir_for_date(session.created_at);
                    self.conversation_store =
                        ConversationStore::with_dir(date_dir, &session.id).ok();
                }
                self.session = Some(SessionContext::new(session.agent_name, String::new()));
                self.output.clear();
                self.invalidate_output_cache();
                self.chat_history.clear();
                self.push_output(format!("New session started: {}", session.id), false);
            }
            Err(e) => self.push_output(format!("Session error: {}", e), true),
        }
    }

    pub(crate) fn execute_fork_session(&mut self, title: Option<&str>) {
        let current_id = match &self.session_id {
            Some(id) => id.clone(),
            None => {
                self.push_output("No active session to fork".to_string(), true);
                return;
            }
        };

        self.save_session_state();

        let manager = SessionManager::new();
        match manager.fork_session(&current_id, title.map(String::from)) {
            Ok(forked) => {
                self.session_id = Some(forked.id.clone());
                if let Ok(paths) = ZenPaths::detect() {
                    let date_dir = paths.session_dir_for_date(forked.created_at);
                    if let Some(parent_store) = &self.conversation_store
                        && let Ok(new_store) =
                            parent_store.copy_to_dir(date_dir.clone(), &forked.id)
                    {
                        self.conversation_store = Some(new_store);
                    }
                    if self.conversation_store.is_none() {
                        self.conversation_store =
                            ConversationStore::with_dir(date_dir, &forked.id).ok();
                    }
                }
                self.session = Some(SessionContext::new(forked.agent_name, String::new()));
                self.output.clear();
                self.invalidate_output_cache();
                self.chat_history.clear();
                self.push_output(
                    format!("Session forked: {} (from {})", forked.id, current_id),
                    false,
                );
            }
            Err(e) => self.push_output(format!("Fork error: {}", e), true),
        }
    }

    pub(crate) fn execute_rename_session(&mut self, title: Option<&str>) {
        let current_id = match &self.session_id {
            Some(id) => id.clone(),
            None => {
                self.push_output("No active session to rename".to_string(), true);
                return;
            }
        };

        let title = match title {
            Some(t) if !t.is_empty() => t.to_string(),
            _ => {
                self.push_output("Usage: /rename <name>".to_string(), true);
                return;
            }
        };

        let manager = SessionManager::new();
        match manager.rename_session(&current_id, title.clone()) {
            Ok(()) => {
                self.push_output(format!("Session renamed to: {}", title), false);
            }
            Err(e) => self.push_output(format!("Rename error: {}", e), true),
        }
    }

    pub(crate) fn execute_archive_session(&mut self) {
        let current_id = match &self.session_id {
            Some(id) => id.clone(),
            None => {
                self.push_output("No active session to archive".to_string(), true);
                return;
            }
        };

        let manager = SessionManager::new();
        match manager.archive_session(&current_id) {
            Ok(()) => {
                self.push_output(format!("Session archived: {}", current_id), false);
                self.session_id = None;
                self.output.clear();
                self.invalidate_output_cache();
                self.chat_history.clear();
            }
            Err(e) => self.push_output(format!("Archive error: {}", e), true),
        }
    }

    pub fn resume_session(&mut self, session_id: &str) {
        self.save_session_state();

        // FR-025: When gateway is live, prefer/merge the gateway resume
        // so in-flight-turn state is recovered. Offline falls back to local.
        //
        // Spawn safety (item C): resume_session is called from sync key handlers
        // which may run outside a tokio runtime (headless tests). Use the
        // try_current / std::thread fallback pattern from persist_history (:1016-1046).
        let session_id_owned = session_id.to_string();
        if let Some(surface) = super::prewarm::take_client() {
            let surface = surface.clone();
            let sid = session_id_owned.clone();

            // Create the channel pair for resume events
            let (tx, rx) = std::sync::mpsc::sync_channel(32);
            self.resume_event_tx = Some(tx.clone());
            self.resume_event_rx = Some(rx);

            let producer = move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async move {
                    Self::run_gateway_resume(surface, &sid, &tx).await;
                });
            };

            // Inside a tokio runtime: spawn on blocking thread.
            // Outside (headless tests): spawn a dedicated OS thread.
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    std::mem::drop(handle.spawn_blocking(producer));
                }
                Err(_) => {
                    std::thread::Builder::new()
                        .name("resume-producer".into())
                        .spawn(producer)
                        .ok();
                }
            }
        }

        // Merge/dedup rule (documented per FR-025):
        // The local resume path renders archived turns synchronously and clears
        // output first. Gateway replay COMPLEMENTS it: only turns the local
        // store doesn't contain arrive via the channel. When local succeeds,
        // gateway events for the same turns are deduplicated because the local
        // store already rendered them. When gateway is offline, no producer is
        // spawned and the local path handles everything.

        let manager = SessionManager::new();
        match manager.resume_session(session_id) {
            Ok(session) => {
                self.session_id = Some(session.id.clone());
                if let Ok(paths) = ZenPaths::detect() {
                    let date_dir = paths.session_dir_for_date(session.created_at);
                    self.conversation_store =
                        ConversationStore::with_dir(date_dir, &session.id).ok();
                }
                self.output.clear();
                self.invalidate_output_cache();
                self.chat_history.clear();
                let mut session_ctx =
                    SessionContext::new(session.agent_name.clone(), String::new());

                if let Some(store) = &self.conversation_store
                    && let Ok(entries) = store.load()
                {
                    let mut i = 0;
                    while i < entries.len() {
                        if entries[i].0.parse::<MessageRole>() == Ok(MessageRole::User) {
                            let user_content = entries[i].1.clone();
                            self.push_output(format!("You: {}", user_content), false);
                            session_ctx.add_turn(MessageRole::User, &user_content);

                            if i + 1 < entries.len()
                                && entries[i + 1].0.parse::<MessageRole>()
                                    == Ok(MessageRole::Assistant)
                            {
                                let raw_assistant = entries[i + 1].1.clone();
                                let normalized = normalize_compact_markdown(&raw_assistant);
                                self.output.push(OutputCell::Markdown(MarkdownCell::new(
                                    normalized.clone(),
                                )));
                                self.invalidate_output_cache();
                                self.chat_history
                                    .push((user_content.clone(), normalized.clone()));
                                session_ctx.add_turn(MessageRole::Assistant, &normalized);

                                i += 2;
                            } else {
                                i += 1;
                            }
                        } else {
                            i += 1;
                        }
                    }
                }

                self.session = Some(session_ctx);

                let title = session.title.as_deref().unwrap_or("(untitled)");
                self.push_output(
                    format!("Resumed session: {} ({})", session.id, title),
                    false,
                );
                self.session_picker.dismiss();
            }
            Err(e) => self.push_output(format!("Resume error: {}", e), true),
        }
    }

    /// FR-025: Gateway resume producer — runs on a dedicated thread, parses
    /// replay events, buffers per turn, sends `ResumeMessage`s via channel.
    async fn run_gateway_resume(
        surface: std::sync::Arc<zen_gateway::client::SurfaceClient>,
        session_id: &str,
        tx: &std::sync::mpsc::SyncSender<super::resume::ResumeMessage>,
    ) {
        use zen_gateway::client::surface::SessionResumeResult;
        match surface.resume_session_rpc(session_id, None, None).await {
            Ok(SessionResumeResult::Replay(events)) => {
                let (parsed, parse_skipped) = super::resume::ResumeProcessor::parse_events(&events);
                tracing::info!(
                    session_id = %session_id,
                    event_count = parsed.len(),
                    parse_skipped,
                    "gateway resume: replayed events"
                );
                let mut proc = super::resume::ResumeProcessor::new();
                for ev in &parsed {
                    if let Some(msg) = proc.process_event(ev) {
                        let _ = tx.send(msg);
                    }
                }
                for msg in proc.flush() {
                    let _ = tx.send(msg);
                }
                if parse_skipped > 0 {
                    let _ = tx.send(super::resume::ResumeMessage::GapNotice {
                        skipped_count: parse_skipped,
                    });
                }
            }
            Ok(SessionResumeResult::Completed(response)) => {
                tracing::info!(
                    session_id = %session_id,
                    "gateway resume: turn already completed (-32004)"
                );
                if let Some(text) = response.get("response").and_then(|r| r.as_str()) {
                    let _ = tx.send(super::resume::ResumeMessage::CompletedResponse {
                        text: text.to_string(),
                    });
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    session_id = %session_id,
                    "gateway resume failed; local path handles rendering"
                );
            }
        }
    }

    pub fn archive_session(&mut self, session_id: &str) {
        let manager = SessionManager::new();
        match manager.archive_session(session_id) {
            Ok(()) => {
                self.push_output(format!("Session archived: {}", session_id), false);
                self.session_picker.load_sessions();
            }
            Err(e) => self.push_output(format!("Archive error: {}", e), true),
        }
    }

    pub fn rename_session(&mut self, session_id: &str, title: &str) {
        let manager = SessionManager::new();
        match manager.rename_session(session_id, title.to_string()) {
            Ok(()) => {
                self.push_output(format!("Session renamed to: {}", title), false);
                self.session_picker.load_sessions();
            }
            Err(e) => self.push_output(format!("Rename error: {}", e), true),
        }
    }
}
