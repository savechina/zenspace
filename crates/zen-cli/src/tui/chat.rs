use std::sync::mpsc;
use std::time::Instant;

use ratatui::text::Line;

use super::app::{App, ECHO_SCRIPT, PendingCallKind, PendingLlmCallStream};
use super::cell::{OutputCell, PlainCell};
use zen_core::types::SessionContext;

impl App {
    /// T055/T056: async chat dispatch for inline mode.
    ///
    /// The event loop only pays O(1) work: the pending call + streaming flag
    /// are created here, then the heavy pipeline (orchestrator acquire →
    /// knowledge search → LLM dispatch) runs in background tasks streaming
    /// into the pending call's channels. `poll_llm_response` consumes the
    /// result exactly as before — no consumer changes.
    pub(crate) fn start_async_chat(&mut self, query: &str) {
        if !self.is_inline_mode() {
            // Full-screen user echo lives in the chat output area.
            self.output.push(OutputCell::user(query));
            self.invalidate_output_cache();
        }
        let (tokens_tx, tokens_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        self.pending_calls
            .push(PendingCallKind::Streaming(PendingLlmCallStream {
                tokens_rx,
                done_rx,
                query: query.to_string(),
            }));
        self.is_streaming = true;
        if self.pending_calls.len() == 1 {
            self.stream_collector.clear();
        }
        self.turn_started_at = Some(Instant::now());
        self.tool_call_count = 0;
        self.current_response_tokens = 0;
        self.show_splash = false;
        self.status_hint = Some("preparing context…".to_string());

        // ZEN_TEST_ECHO_LLM: test-only seam streaming a deterministic script
        // through the real token channel (test-design.md §3 L3). Skips the
        // orchestrator/LLM; production is unaffected (env var absent).
        if std::env::var("ZEN_TEST_ECHO_LLM").is_ok() {
            let script = ECHO_SCRIPT.to_string();
            tokio::task::spawn(async move {
                for chunk in script.split_inclusive('\n') {
                    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                    let _ = tokens_tx.send(chunk.to_string());
                }
                let _ = done_tx.send((
                    Ok(script),
                    Some(SessionContext::new("echo".into(), String::new())),
                ));
            });
            return;
        }

        if self.session.is_none() {
            self.session = Some(SessionContext::new("default".into(), String::new()));
        }

        // US3 gateway producer (T023/T025): same channels, hosted turn.
        // The daemon owns the orchestrator; knowledge arrives via
        // knowledge/search plus the cheap local filename lookup, and a
        // dead link fails the turn with the degraded banner visible
        // (FR-011/012). Reconnect happens on the next turn.
        self.gateway_banner = super::banner::GatewayBannerState::Connecting;
        let session = self
            .session
            .clone()
            .unwrap_or_else(|| SessionContext::new("default".into(), String::new()));
        let session_id = session.session_id.to_string();
        let query_owned = query.to_string();
        let config = self.config;

        tokio::task::spawn(async move {
            let surface = match super::prewarm::take_client() {
                Some(surface) => Some(surface),
                None => super::prewarm::resolve_client().await,
            };
            let Some(surface) = surface else {
                // FR-023: a handshake refusal (e.g. version mismatch) recorded
                // during dialing surfaces VERBATIM; otherwise generic offline.
                let err = match super::prewarm::take_dial_refusal() {
                    Some((reason, recovery)) => super::banner::refusal_marker(&reason, &recovery),
                    None => {
                        "gateway: offline — memory & agent features degraded (retrying): no link"
                            .to_string()
                    }
                };
                let _ = done_tx.send((Err(err), None));
                return;
            };

            use zen_core::config::KnowledgeSearchMode;
            let mode = config.tui.knowledge_search;
            let mut knowledge: Vec<zen_core::types::RetrievedNote> = Vec::new();
            if mode != KnowledgeSearchMode::Off {
                let tiers = if mode == KnowledgeSearchMode::Fast {
                    Some(vec!["fts"])
                } else {
                    None
                };
                knowledge = surface
                    .search_knowledge(&query_owned, tiers.as_deref(), 5)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(
                            error = %e,
                            "gateway knowledge/search failed — continuing without context"
                        );
                        Vec::new()
                    });
            }

            let lookup_query = query_owned.clone();
            let direct = tokio::task::spawn_blocking(move || {
                zen_core::paths::ZenPaths::detect()
                    .map(|paths| App::direct_file_lookup_in_dirs(&paths, &lookup_query))
                    .unwrap_or_default()
            })
            .await
            .unwrap_or_default();
            for (i, r) in direct.into_iter().enumerate() {
                knowledge.push(zen_core::types::RetrievedNote {
                    path: r.file.display().to_string(),
                    content: r.content,
                    sensitivity: zen_core::types::Sensitivity::Public,
                    relevance: 1.0 - (i as f64 * 0.1),
                });
            }
            knowledge.truncate(5);

            tracing::info!(
                session_id,
                query_len = query_owned.len(),
                context_count = knowledge.len(),
                "TUI gateway chat: dispatching (hosted turn)"
            );

            match surface
                .turn_with_recovery(&session_id, &query_owned, knowledge)
                .await
            {
                Ok(response) => {
                    let _ = tokens_tx.send(response.clone());
                    let _ = done_tx.send((Ok(response), Some(session)));
                }
                Err(e) => {
                    if matches!(e, zen_gateway::client::SurfaceError::Cancelled) {
                        // Cancelled turn: send a sentinel so poll_llm_response
                        // handles it cleanly (no red error banner).
                        let _ = done_tx.send((Err("[[CANCELLED]]".into()), None));
                    } else if let zen_gateway::client::SurfaceError::Rpc(rpc) = &e
                        && rpc.code == -32001
                    {
                        // FR-023: handshake refusal incl. version mismatch —
                        // carry the server-provided reason+recovery verbatim.
                        let reason = rpc
                            .data
                            .as_ref()
                            .and_then(|d| d.get("reason"))
                            .and_then(|v| v.as_str())
                            .unwrap_or(rpc.message.as_str());
                        let recovery = rpc
                            .data
                            .as_ref()
                            .and_then(|d| d.get("recovery"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let _ = done_tx
                            .send((Err(super::banner::refusal_marker(reason, recovery)), None));
                    } else {
                        let _ = done_tx
                            .send((Err(format!("{}: {e}", surface.link_state().banner())), None));
                    }
                }
            }
        });
    }

    pub(crate) fn direct_file_lookup_in_dirs(
        paths: &zen_core::paths::ZenPaths,
        query: &str,
    ) -> Vec<zen_vault::search::SearchResult> {
        use std::fs;
        use zen_vault::search::SearchResult;

        let query_lower = query.to_lowercase();
        let keywords: Vec<&str> = query_lower
            .split_whitespace()
            .filter(|s| {
                s.len() >= 3
                    && ![
                        "the",
                        "and",
                        "for",
                        "with",
                        "about",
                        "summary",
                        "summarize",
                        "show",
                        "tell",
                        "me",
                        "this",
                        "that",
                        "above",
                    ]
                    .contains(s)
            })
            .collect();

        if keywords.is_empty() {
            return Vec::new();
        }

        let mut matches = Vec::new();

        for dir in [paths.inbox(), paths.wiki()] {
            let walker = match std::fs::read_dir(&dir) {
                Ok(w) => w,
                Err(_) => continue,
            };

            for entry in walker.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                let file_name = match path.file_stem().and_then(|s| s.to_str()) {
                    Some(n) => n.to_lowercase(),
                    None => continue,
                };

                let match_score = keywords.iter().filter(|kw| file_name.contains(*kw)).count();

                if match_score == 0 {
                    continue;
                }

                if let Ok(content) = fs::read_to_string(&path) {
                    let body = if content.starts_with("---") {
                        content
                            .splitn(3, "---")
                            .nth(2)
                            .unwrap_or(&content)
                            .trim_start()
                    } else {
                        &content
                    };
                    let snippet: String = body.chars().take(2000).collect();
                    matches.push((
                        match_score,
                        SearchResult {
                            file: path,
                            line: 0,
                            content: snippet,
                        },
                    ));
                }
            }
        }

        matches.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        matches.into_iter().map(|(_, r)| r).take(2).collect()
    }

    pub fn poll_llm_response(&mut self) {
        // FR-025: drain gateway resume events into scrollback
        self.drain_resume_events();

        struct StreamResult {
            done_result: Option<(
                Result<String, String>,
                Option<zen_core::types::SessionContext>,
            )>,
            tokens: Vec<String>,
        }

        let mut results: Vec<(usize, Option<(String, StreamResult)>)> = Vec::new();

        for (idx, call) in self.pending_calls.iter_mut().enumerate() {
            match call {
                PendingCallKind::Streaming(s) => {
                    let mut tokens = Vec::new();
                    while let Ok(token) = s.tokens_rx.try_recv() {
                        tokens.push(token);
                    }
                    match s.done_rx.try_recv() {
                        Ok(result) => {
                            results.push((
                                idx,
                                Some((
                                    s.query.clone(),
                                    StreamResult {
                                        done_result: Some(result),
                                        tokens,
                                    },
                                )),
                            ));
                        }
                        Err(mpsc::TryRecvError::Empty) => {
                            results.push((
                                idx,
                                Some((
                                    s.query.clone(),
                                    StreamResult {
                                        done_result: None,
                                        tokens,
                                    },
                                )),
                            ));
                        }
                        Err(mpsc::TryRecvError::Disconnected) => {
                            results.push((
                                idx,
                                Some((
                                    s.query.clone(),
                                    StreamResult {
                                        done_result: Some((Err("disconnected".into()), None)),
                                        tokens,
                                    },
                                )),
                            ));
                        }
                    }
                }
                PendingCallKind::SingleShot(ss) => match ss.rx.try_recv() {
                    Ok(result) => {
                        results.push((
                            idx,
                            Some((
                                ss.query.clone(),
                                StreamResult {
                                    done_result: Some((result, None)),
                                    tokens: Vec::new(),
                                },
                            )),
                        ));
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                    Err(mpsc::TryRecvError::Disconnected) => {
                        results.push((
                            idx,
                            Some((
                                ss.query.clone(),
                                StreamResult {
                                    done_result: Some((Err("disconnected".into()), None)),
                                    tokens: Vec::new(),
                                },
                            )),
                        ));
                    }
                },
            }
        }

        let mut completed_indices: Vec<usize> = Vec::new();

        for (idx, entry) in results {
            if let Some((_query, result)) = entry {
                for token in &result.tokens {
                    self.stream_collector.push_delta(token);
                }
                if !result.tokens.is_empty() {
                    self.status_hint = None; // T056: model started speaking
                }
                self.current_response_tokens = self.stream_collector.buffer().len() / 4;

                if let Some(done_result) = result.done_result {
                    match done_result {
                        (Ok(response), returned_session) => {
                            if let Some(s) = returned_session {
                                tracing::info!(
                                    session_id = %s.session_id,
                                    conversation_turns = s.conversation.len(),
                                    "poll_llm_response: updating session from returned session"
                                );
                                self.session = Some(s);
                            }
                            completed_indices.push(idx);
                            tracing::info!(
                                response_len = response.len(),
                                "TUI chat: LLM response complete"
                            );
                            // T057: flush tool-intermediate blocks the
                            // drain watermark missed (fullscreen never
                            // drains; keeps them above the separator).
                            let tool_lines = self.stream_collector.take_tool_lines();
                            if !tool_lines.is_empty() {
                                if self.is_inline_mode() {
                                    self.enqueue_scrollback(tool_lines);
                                } else {
                                    for line in tool_lines {
                                        let text: String =
                                            line.spans.iter().map(|s| s.content.as_ref()).collect();
                                        self.output.push(OutputCell::Plain(PlainCell::new(text)));
                                    }
                                    self.invalidate_output_cache();
                                }
                            }
                            let elapsed = self.turn_started_at.map(|t| t.elapsed());
                            let label = match (elapsed, self.tool_call_count) {
                                (Some(e), 0) => Some(format!("{:.1}s", e.as_secs_f64())),
                                (Some(e), n) => {
                                    Some(format!("{:.1}s • {} tool calls", e.as_secs_f64(), n))
                                }
                                (None, _) => None,
                            };
                            if self.is_inline_mode() {
                                let reasoning_style = self
                                    .theme
                                    .as_ref()
                                    .text_muted()
                                    .add_modifier(ratatui::style::Modifier::ITALIC);
                                let (committed, pending) = self
                                    .stream_collector
                                    .drain_and_tail_filtered(reasoning_style, self.show_thinking);
                                let mut remaining = committed;
                                remaining.extend(pending);
                                if !remaining.is_empty() {
                                    self.enqueue_scrollback(remaining);
                                }
                                if let Some(l) = &label {
                                    let separator_line = Line::from(ratatui::text::Span::styled(
                                        format!("── {} ──", l),
                                        self.theme.as_ref().separator(),
                                    ));
                                    self.enqueue_scrollback(vec![separator_line]);
                                }
                            }
                            let (raw_text, reasoning) = self.stream_collector.finalize_and_drain();
                            if !self.is_inline_mode() {
                                self.output.push(OutputCell::separator(label));
                                if !raw_text.is_empty() || reasoning.is_some() {
                                    self.output.push(OutputCell::agent(raw_text, reasoning));
                                }
                                self.invalidate_output_cache();
                            }
                            self.current_response_tokens = response.len() / 4;
                            self.gateway_banner = super::banner::GatewayBannerState::Ok(
                                zen_gateway::protocol::SERVER_PROTOCOL_VERSION.to_string(),
                            );
                            self.auto_scroll = true;
                            self.chat_history.push((_query.clone(), response.clone()));
                            if let Some(store) = &self.conversation_store {
                                if let Err(e) = store.append("user", &_query) {
                                    tracing::warn!(error = %e, "failed to persist user turn to conversation store");
                                }
                                if let Err(e) = store.append("assistant", &response) {
                                    tracing::warn!(error = %e, "failed to persist assistant turn to conversation store");
                                }
                            }
                        }
                        (Err(e), _) => {
                            completed_indices.push(idx);
                            if e == "[[CANCELLED]]" {
                                // Esc-to-interrupt: clean completion, not a failure.
                                // Drain any tokens that arrived before the cancel.
                                let remaining: Vec<_> = {
                                    let reasoning_style = self
                                        .theme
                                        .as_ref()
                                        .text_muted()
                                        .add_modifier(ratatui::style::Modifier::ITALIC);
                                    let (committed, pending) =
                                        self.stream_collector.drain_and_tail_filtered(
                                            reasoning_style,
                                            self.show_thinking,
                                        );
                                    let mut r = committed;
                                    r.extend(pending);
                                    r
                                };
                                if !remaining.is_empty() {
                                    self.enqueue_scrollback(remaining);
                                }
                                self.stream_collector.clear();
                                self.viewport_tail.clear();
                                // Render cancellation notice (not a red error)
                                let cancel_line = Line::from(ratatui::text::Span::styled(
                                    "\u{26a0} cancelled",
                                    self.theme.as_ref().text_muted(),
                                ));
                                self.enqueue_scrollback(vec![cancel_line]);
                                self.status_hint = None;
                                self.current_response_tokens = 0;
                            } else if let Some((reason, recovery)) =
                                super::banner::parse_refusal_marker(&e)
                            {
                                // FR-023: handshake refusal (incl. version
                                // mismatch) — banner renders the server-provided
                                // message VERBATIM; one mechanism, never
                                // status_hint.
                                self.gateway_banner =
                                    super::banner::GatewayBannerState::Refused { reason, recovery };
                                self.status_hint = None;
                                self.stream_collector.clear();
                                let text = self.gateway_banner.text();
                                self.push_output(format!("[LLM] Error: {text}"), true);
                            } else {
                                tracing::warn!(error = %e, "TUI chat: LLM response error");
                                // FR-011/012 + FR-023: gateway-class failures pin
                                // the degraded BANNER (single mechanism — the
                                // status_hint duplicate was removed).
                                if e.starts_with("gateway: offline") {
                                    self.gateway_banner =
                                        super::banner::GatewayBannerState::OfflineDegraded;
                                }
                                self.status_hint = None;
                                self.stream_collector.clear();
                                self.push_output(format!("[LLM] Error: {}", e), true);
                            }
                        }
                    }
                }
            }
        }

        for idx in completed_indices.into_iter().rev() {
            self.pending_calls.remove(idx);
        }

        if self.pending_calls.is_empty() {
            self.is_streaming = false;
            while let Some(queued) = self.message_queue.pop_front() {
                // Inline-mode user echo for QUEUED messages: the immediate
                // dispatch path in `handle_command` renders `> <query>` into
                // scrollback, but this deferred path skipped it, so a message
                // typed while the previous turn was finishing silently
                // vanished from the transcript (exposed by T077's faster
                // scrollback inserts shrinking the streaming→queued window;
                // e13 follow-up regression).
                if self.is_inline_mode() {
                    let user_lines = self.render_user_lines_for_scrollback(&queued);
                    self.enqueue_scrollback(user_lines);
                }
                self.current_query = queued.clone();
                self.start_async_chat(&queued);
                if !self.pending_calls.is_empty() {
                    break;
                }
            }
        }
    }

    /// FR-025: Drain resume events from the gateway channel and enqueue
    /// to scrollback. Called from `poll_llm_response` on each tick.
    fn drain_resume_events(&mut self) {
        let Some(rx) = self.resume_event_rx.as_ref() else {
            return;
        };
        // Clone receiver reference to avoid borrow conflict with enqueue_scrollback
        let mut messages = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            messages.push(msg);
        }
        // Process messages outside the borrow
        for msg in messages.drain(..) {
            match msg {
                super::resume::ResumeMessage::TurnText { text, .. } => {
                    let lines = super::resume::render_replay_turn(&text);
                    if !lines.is_empty() {
                        self.enqueue_scrollback(lines);
                    }
                }
                super::resume::ResumeMessage::GapNotice { skipped_count } => {
                    let lines = super::resume::render_gap_notice(skipped_count);
                    self.enqueue_scrollback(lines);
                }
                super::resume::ResumeMessage::CompletedResponse { text } => {
                    let lines = super::resume::render_completed_response(&text);
                    if !lines.is_empty() {
                        self.enqueue_scrollback(lines);
                    }
                }
            }
        }
    }
}
