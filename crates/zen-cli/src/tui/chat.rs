use std::sync::mpsc;
use std::time::Instant;

use ratatui::text::Line;

use super::app::{App, ECHO_SCRIPT, PendingCallKind, PendingLlmCallStream};
use super::cell::{OutputCell, PlainCell};
use crate::session::estimate_tokens;
use zen_core::types::SessionContext;
use zen_gateway::client::surface::TurnNotification;
use zen_gateway::server::hosting::{TOOL_COMPLETED_MARKER, TOOL_STARTED_MARKER};

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
        self.cancel_requested = false;
        self.stream_turn_seq += 1;
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

            // FR-009: mint the turn id up front so the event subscription
            // (which must predate the first emitted event) can filter this
            // turn out of the surface-wide broadcast; the same id rides
            // the turn RPC (opaque server-side).
            let turn_id = format!("turn-{}", uuid::Uuid::now_v7());
            let mut events = surface.subscribe_events();
            let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
            let forward_tx = tokens_tx.clone();
            let forward_turn_id = turn_id.clone();
            let forwarder = tokio::task::spawn(async move {
                // (saw_delta, saw_lag, deltas_concat): saw_delta drives the
                // no-event fallback gate; saw_lag + deltas_concat drive the
                // lag repair below — dropped broadcast deltas would otherwise
                // truncate the composed transcript with the authoritative
                // response suppressed by the same gate.
                let mut saw_delta = false;
                let mut saw_lag = false;
                let mut deltas_concat = String::new();
                let mut stop_fired = false;
                loop {
                    let received = if stop_fired {
                        // Bounded grace after the turn returns: the pump may
                        // still be fanning this turn's queued frames (the
                        // server emits the terminal event BEFORE the RPC
                        // response), so drain until idle.
                        match tokio::time::timeout(EVENT_DRAIN_GRACE, events.recv()).await {
                            Ok(r) => r,
                            Err(_) => break,
                        }
                    } else {
                        tokio::select! {
                            biased;
                            out = events.recv() => out,
                            _ = &mut stop_rx => {
                                stop_fired = true;
                                continue;
                            }
                        }
                    };
                    match received {
                        Ok(ev) => {
                            if ev.turn_id != forward_turn_id {
                                continue;
                            }
                            if ev.kind == "delta" {
                                saw_delta = true;
                                if let Some(text) = ev.payload["text"].as_str() {
                                    deltas_concat.push_str(text);
                                }
                            }
                            let terminal = ev.kind == "turn_completed" || ev.kind == "turn_error";
                            if let Some(token) = event_to_stream_token(&forward_turn_id, &ev)
                                && forward_tx.send(token).is_err()
                            {
                                break;
                            }
                            if terminal {
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            // Dropped frames are gone; record it so the caller
                            // repairs the composed text from the authoritative
                            // response instead of trusting the truncated prefix.
                            saw_lag = true;
                            tracing::warn!(
                                lagged = n,
                                "TUI gateway event stream lagged; dropped deltas will be repaired from the authoritative response"
                            );
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                (saw_delta, saw_lag, deltas_concat)
            });

            let outcome = surface
                .turn_with_recovery_with_id(&session_id, &turn_id, &query_owned, knowledge)
                .await;
            let _ = stop_tx.send(());
            let (saw_delta, saw_lag, deltas_prefix) = forwarder.await.unwrap_or_default();

            match outcome {
                Ok(response) => {
                    // Live deltas already composed the response in the
                    // collector; only the no-event fallback re-sends the full
                    // text as one token (byte-identical to the prior behavior).
                    if !saw_delta {
                        let _ = tokens_tx.send(response.clone());
                    } else if saw_lag && response.starts_with(deltas_prefix.as_str()) {
                        // Lag repair: dropped deltas left the composed prefix
                        // short of the authoritative response. Deltas arrive
                        // in order, so the received prefix aligns with the
                        // response for single-attempt turns; append only the
                        // missing suffix (token-compose contract intact).
                        let suffix = &response[deltas_prefix.len()..];
                        if !suffix.is_empty() {
                            let _ = tokens_tx.send(suffix.to_string());
                        }
                    } else if saw_lag {
                        // Prefix misalignment (recovery re-stream / multi-
                        // attempt turns) — completion is not derivable; the
                        // visible partial content is what the turn produced.
                        // Logged rather than silently trusting a wrong base.
                        tracing::warn!(
                            prefix_len = deltas_prefix.len(),
                            "lagged delta stream not prefix-aligned with the authoritative response; transcript may be incomplete"
                        );
                    }
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
                self.current_response_tokens = estimate_tokens(self.stream_collector.buffer());
                // T082 (NFR-009): stalls closed by a later delta emit once,
                // here — never per tick.
                self.emit_stream_stall_audits();

                if let Some(done_result) = result.done_result {
                    // T082: a trailing stall at turn end is the final episode
                    // (flush BEFORE the collector resets on the completion path).
                    self.stream_collector.stall_turn_end();
                    self.emit_stream_stall_audits();
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
                            self.current_response_tokens = estimate_tokens(&response);
                            let protocol =
                                zen_gateway::protocol::SERVER_PROTOCOL_VERSION.to_string();
                            // Phase 30 G3: the prewarm-recorded stale hint rides
                            // the Ok banner (visible one-row warn, viewport-safe).
                            self.gateway_banner = match super::prewarm::stale_hint() {
                                Some(hint) => super::banner::GatewayBannerState::Ok(format!(
                                    "{protocol} — {hint}"
                                )),
                                None => super::banner::GatewayBannerState::Ok(protocol),
                            };
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
                                if let Some(hint) = provider_error_hint(&e) {
                                    self.push_output(hint.to_string(), false);
                                }
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
            self.cancel_requested = false;
            while let Some(queued) = self.message_queue.pop_front() {
                // Echo happens at ENQUEUE time (commands.rs) — the drain
                // path must not re-render, or queued lines appear twice.
                // (T077's drain-time echo was superseded by echo-on-enqueue.)
                self.current_query = queued.clone();
                self.start_async_chat(&queued);
                if !self.pending_calls.is_empty() {
                    break;
                }
            }
        }
    }

    /// T082 (NFR-009): write pending `tui.stream.stall` audit lines — one
    /// per stall episode, fire-and-forget. `turn_id` is this TUI process's
    /// 1-based streaming-turn sequence; `session_id` is the TUI session uuid.
    fn emit_stream_stall_audits(&mut self) {
        let gaps = self.stream_collector.take_stall_gaps();
        if gaps.is_empty() {
            return;
        }
        let session_id = self
            .session
            .as_ref()
            .map(|s| s.session_id.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let turn_id = format!("tui-{}", self.stream_turn_seq);
        for gap in gaps {
            super::stall::append_stall_audit(&session_id, &turn_id, gap);
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

/// Phase 33 T229: provider unavailable/unconfigured turn errors get ONE
/// actionable hint appended (mirrors the `/search` Tip precedent in
/// `commands.rs`). Matching is a case-insensitive substring against the real
/// `LlmError` message forms — `provider unavailable: …`
/// (`crates/zen-provider/src/errors.rs`), `Provider '…' not configured`
/// (`router.rs`), and the transport-level `Connection refused`. Non-provider
/// errors return `None`, so their rendering stays byte-identical.
fn provider_error_hint(error: &str) -> Option<&'static str> {
    let lower = error.to_ascii_lowercase();
    if lower.contains("connection refused")
        || lower.contains("not configured")
        || lower.contains("unavailable")
    {
        Some(PROVIDER_UNAVAILABLE_HINT)
    } else {
        None
    }
}

/// Phase 33 T229: fixed first-run remediation copy (Constitution XV / T192 —
/// UX copy, not a config key). Kept consistent with `zen provider` and the
/// embedded `config/config.toml` sample.
const PROVIDER_UNAVAILABLE_HINT: &str = "Tip: configure a provider — run `zen provider` and set default_provider + [providers.<name>] in ~/.zen/config.toml (config/config.toml embedded sample included)";

/// FR-009: how long the hosted-turn forwarder keeps draining the gateway
/// event broadcast after the turn RPC returns, waiting for the notification
/// pump to catch up. The server emits the terminal event before the RPC
/// response, but the pump is a separate task, so a short bounded grace is
/// needed to avoid dropping the turn's trailing frames; the normal path
/// consumes the terminal event immediately and never waits the full budget.
const EVENT_DRAIN_GRACE: std::time::Duration = std::time::Duration::from_millis(200);

/// FR-009: translate one gateway `session/event` notification into the
/// delta-stream token the TUI `StreamCollector` already understands.
///
/// PURPOSE: A hosted turn's live progress arrives as structured
/// `session/event` notifications (`delta`, `tool_started`, `tool_completed`).
/// The collector renders assistant text and tool rounds from the existing
/// `🔧`/`✅` marker grammar (`stream.rs`), so this is the ONE mapping seam —
/// pure, channel-free, and unit-tested. The marker lines stop at the tool
/// name so the collector's `🔧 <tool>[ args…]` grammar parses it cleanly and
/// renders `🔧 <tool>: …` / `✅ <tool>: …` / `✅ <tool>: failed: <e>`.
///
/// USAGE: `event_to_stream_token(&turn_id, &ev)` where `turn_id` is the
/// caller-minted id. The surface broadcast fans out every surface's turns,
/// so notifications for any other turn return `None`.
///
/// EXPECTED: `Some(text)` for `delta`; `Some("🔧 <tool>\n")` /
/// `Some("✅ <tool>\n")` / `Some("✅ <tool> failed: <e>\n")` for the tool
/// kinds; `None` for foreign turns, terminal kinds
/// (`turn_completed`/`turn_error`), and any unrecognized kind.
///
/// ERRORS: None — a total function over the notification shape.
fn event_to_stream_token(turn_id: &str, ev: &TurnNotification) -> Option<String> {
    if ev.turn_id != turn_id {
        return None;
    }
    match ev.kind.as_str() {
        "delta" => ev.payload["text"].as_str().map(str::to_string),
        "tool_started" => {
            let tool = ev.payload["tool"].as_str().unwrap_or("tool");
            Some(format!("{TOOL_STARTED_MARKER} {tool}\n"))
        }
        "tool_completed" => {
            let tool = ev.payload["tool"].as_str().unwrap_or("tool");
            Some(match ev.payload["error"].as_str() {
                Some(error) => {
                    let error = error.replace('\n', " ");
                    format!("{TOOL_COMPLETED_MARKER} {tool} failed: {error}\n")
                }
                None => format!("{TOOL_COMPLETED_MARKER} {tool}\n"),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t229_hint_matches_connection_refused() {
        assert_eq!(
            provider_error_hint("Connection refused (os error 61)"),
            Some(PROVIDER_UNAVAILABLE_HINT)
        );
    }

    #[test]
    fn t229_hint_matches_provider_unavailable_and_not_configured() {
        assert!(provider_error_hint("provider unavailable: ollama — no route").is_some());
        assert!(
            provider_error_hint("Provider 'anthropic' not configured. Add to ~/.zen/config.toml")
                .is_some()
        );
    }

    #[test]
    fn t229_hint_absent_for_non_provider_errors() {
        assert!(
            provider_error_hint("gateway: offline — memory & agent features degraded").is_none()
        );
        assert!(provider_error_hint("gateway rpc error -32603: internal").is_none());
        assert!(provider_error_hint("disconnected").is_none());
    }

    fn ev(turn_id: &str, kind: &str, extra: serde_json::Value) -> TurnNotification {
        let mut payload = serde_json::json!({ "turnId": turn_id, "seq": 1, "kind": kind });
        if let (Some(obj), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
            for (k, v) in extra {
                obj.insert(k.clone(), v.clone());
            }
        }
        TurnNotification {
            turn_id: turn_id.to_string(),
            seq: 1,
            kind: kind.to_string(),
            payload,
        }
    }

    #[test]
    fn event_to_stream_token_delta_maps_to_text() {
        let token =
            event_to_stream_token("t1", &ev("t1", "delta", serde_json::json!({"text": "hi"})));
        assert_eq!(token.as_deref(), Some("hi"));
    }

    #[test]
    fn event_to_stream_token_tool_lifecycle_maps_to_markers() {
        let started = event_to_stream_token(
            "t1",
            &ev(
                "t1",
                "tool_started",
                serde_json::json!({"tool": "web.search"}),
            ),
        );
        assert_eq!(started.as_deref(), Some("🔧 web.search\n"));

        let done = event_to_stream_token(
            "t1",
            &ev(
                "t1",
                "tool_completed",
                serde_json::json!({"tool": "web.search"}),
            ),
        );
        assert_eq!(done.as_deref(), Some("✅ web.search\n"));
    }

    #[test]
    fn event_to_stream_token_tool_error_includes_failure() {
        let failed = event_to_stream_token(
            "t1",
            &ev(
                "t1",
                "tool_completed",
                serde_json::json!({"tool": "shell.exec", "error": "boom\nbad"}),
            ),
        );
        assert_eq!(failed.as_deref(), Some("✅ shell.exec failed: boom bad\n"));
    }

    #[test]
    fn event_to_stream_token_ignores_foreign_turn_and_terminal_kinds() {
        assert!(
            event_to_stream_token("t1", &ev("t2", "delta", serde_json::json!({"text": "x"})))
                .is_none()
        );
        assert!(
            event_to_stream_token(
                "t1",
                &ev("t1", "turn_completed", serde_json::json!({"response": "x"}))
            )
            .is_none()
        );
        assert!(
            event_to_stream_token(
                "t1",
                &ev("t1", "turn_error", serde_json::json!({"message": "x"}))
            )
            .is_none()
        );
        assert!(
            event_to_stream_token(
                "t1",
                &ev("t1", "agent_status", serde_json::json!({"status": "busy"}))
            )
            .is_none()
        );
    }
}
