use ratatui::text::{Line, Span};

use super::app::{App, MAX_QUEUE_SIZE};
use super::theme::OutputTheme;

/// Echo-on-enqueue marker (T247): a message queued mid-stream renders with a
/// dim `⇅` prefix in inline scrollback. Applied to render-only line clones
/// after the queue accepts the raw text, so the marker never reaches
/// `message_queue`, the session jsonl, or the model payload.
/// Wave 2A: pub(crate) so the replay path (`App::build_replay_entries`)
/// reproduces the queued-submission marker on UserEcho entries.
pub(crate) fn mark_queued_echo(
    mut lines: Vec<Line<'static>>,
    theme: &dyn OutputTheme,
) -> Vec<Line<'static>> {
    if let Some(first) = lines.first_mut() {
        let mut spans = vec![Span::styled("⇅ ", theme.text_muted())];
        spans.append(&mut first.spans);
        first.spans = spans;
    }
    lines
}

/// Truthful offline detail for `/search` (G4-02): the previous string
/// promised a retry this path never performs.
const SEARCH_OFFLINE_MSG: &str = "gateway: offline — search unavailable (daemon unreachable)";

/// Display caps for one `/search` result's note content (G4-03). `/search`
/// renders note bodies inline into the scrollback; a long note (or a fusion
/// hit carrying a whole document) would flood the viewport. DISPLAY-only:
/// the full content is untouched in the daemon response and the store.
/// Fixed constants, no config key (T192: no dormant knobs).
const SEARCH_CONTENT_MAX_LINES: usize = 40;
const SEARCH_CONTENT_MAX_CHARS: usize = 2000;
const SEARCH_TRUNCATION_MARKER: &str = " … [truncated]";

/// Char-boundary-safe display truncation of a `/search` note body.
fn truncate_search_content(content: &str) -> String {
    let total_lines = content.lines().count();
    let limited: String = content
        .lines()
        .take(SEARCH_CONTENT_MAX_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    let truncated = if limited.chars().count() > SEARCH_CONTENT_MAX_CHARS {
        limited.chars().take(SEARCH_CONTENT_MAX_CHARS).collect()
    } else {
        limited
    };
    if total_lines > SEARCH_CONTENT_MAX_LINES || content.chars().count() > SEARCH_CONTENT_MAX_CHARS
    {
        format!("{truncated}{SEARCH_TRUNCATION_MARKER}")
    } else {
        truncated
    }
}

/// Renders the G4-01 degraded-tier warning line, or `None` when every tier
/// answered. `names` are the contract tier names from the RPC `degraded`
/// array (`fts`, `vec`, `graph`, ...).
fn degraded_tiers_line(names: &[String]) -> Option<String> {
    if names.is_empty() {
        None
    } else {
        Some(format!("⚠ degraded tiers: {}", names.join(", ")))
    }
}

/// Advisory for the `search_knowledge` fallback: the relayed path discards
/// the O16 `degraded` array, so absent diagnostics there must be surfaced
/// as uncertainty rather than silence. Split out for unit testing.
fn relayed_degraded_advisory(relayed: bool, degraded: &[String]) -> Option<String> {
    if relayed && degraded.is_empty() {
        Some("⚠ degraded tiers unavailable (relayed search)".to_string())
    } else {
        None
    }
}

/// Parses a raw `knowledge/search` result into `(notes, degraded tier
/// names)`. Split out from [`App::execute_search`] for unit testing.
fn parse_search_response(result: &serde_json::Value) -> (Vec<(String, String)>, Vec<String>) {
    let notes = result
        .get("notes")
        .and_then(serde_json::Value::as_array)
        .map(|notes| {
            notes
                .iter()
                .map(|note| {
                    let path = note
                        .get("path")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let content = note
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    (path, content)
                })
                .collect()
        })
        .unwrap_or_default();
    let degraded = result
        .get("degraded")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    entry
                        .get("tier")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    (notes, degraded)
}

/// Raw `knowledge/search` RPC so the additive `degraded` array is visible —
/// [`zen_gateway::client::SurfaceClient::search_knowledge`] discards it.
async fn raw_search_request(query: &str) -> Result<(Vec<(String, String)>, Vec<String>), String> {
    let client = zen_gateway::client::GatewayClient::connect(
        zen_gateway::transport::uds::default_socket_path(),
    )
    .await
    .map_err(|e| format!("gateway: search unavailable ({e})"))?;
    let result = client
        .request(
            "knowledge/search",
            serde_json::json!({ "query": query, "limit": 10 }),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(parse_search_response(&result))
}

/// Split a stripped slash input into (command token, trimmed remainder).
/// A double-space typo (`/search  letter`) must not leak a leading-space
/// remainder (' letter') into the query shown to the user and sent to
/// the search tiers; a trailing-space-only remainder yields `None`.
pub(crate) fn split_slash_input(cmd: &str) -> (&str, Option<&str>) {
    let mut parts = cmd.splitn(2, ' ');
    let token = parts.next().unwrap_or("");
    let rest = parts.next().map(str::trim).filter(|r| !r.is_empty());
    (token, rest)
}

/// (c) Thinking-display mode decoded from the canonical field pair:
/// `show_thinking` is the "mode != off" bool, `thinking_brief` selects the
/// hermes `truncated` single-line pending tail.
fn decode_thinking_mode(show: bool, brief: bool) -> &'static str {
    match (show, brief) {
        (false, _) => "off",
        (true, true) => "brief",
        (true, false) => "full",
    }
}

/// (c) Bare `/thinking` cycle: off -> brief -> full -> off.
fn cycle_thinking_mode(show: bool, brief: bool) -> (bool, bool) {
    match (show, brief) {
        (false, _) => (true, true),
        (true, true) => (true, false),
        (true, false) => (false, false),
    }
}

/// (c) Explicit `/thinking <arg>` -> mode field pair; `None` = unknown arg.
fn parse_thinking_mode(arg: &str) -> Option<(bool, bool)> {
    match arg.trim().to_ascii_lowercase().as_str() {
        "off" => Some((false, false)),
        "brief" => Some((true, true)),
        "full" => Some((true, false)),
        _ => None,
    }
}

impl App {
    pub fn handle_command(&mut self, cmd: &str) {
        let cmd = cmd.trim();
        if cmd.is_empty() {
            return;
        }
        if let Some(stripped) = cmd.strip_prefix('/') {
            self.handle_slash_command(stripped);
        } else if self.is_streaming {
            if self.message_queue.len() >= MAX_QUEUE_SIZE {
                self.push_output(
                    "Queue full. Please wait for current response to complete.".to_string(),
                    true,
                );
            } else {
                // Echo-on-enqueue: the queued message renders into inline
                // scrollback the moment it lands, so a submission during a
                // live stream is visibly accepted (silent enqueue previously
                // read as lost input; the echo at drain time duplicated
                // nothing because only ONE of the two paths ever rendered).
                if self.is_inline_mode() {
                    let user_lines = self.render_user_lines_for_scrollback(cmd);
                    let user_lines = mark_queued_echo(user_lines, self.theme.as_ref());
                    // Wave 2A: capture the echo source (queued variant keeps
                    // the marker through width-change replay).
                    self.transcript
                        .push(super::transcript_source::TranscriptSource::UserEcho {
                            text: cmd.to_string(),
                            queued: true,
                        });
                    self.enqueue_scrollback(user_lines);
                }
                self.message_queue.push_back(cmd.to_string());
            }
        } else {
            // T055/T061: instant path for BOTH inline and full-screen —
            // echo + pending call + streaming flag happen here (<1ms); the
            // heavy pipeline (orchestrator acquire, knowledge search, LLM
            // dispatch) runs in background tasks.
            if self.is_inline_mode() {
                let user_lines = self.render_user_lines_for_scrollback(cmd);
                // Wave 2A: capture the echo source for width-change replay.
                self.transcript
                    .push(super::transcript_source::TranscriptSource::UserEcho {
                        text: cmd.to_string(),
                        queued: false,
                    });
                self.enqueue_scrollback(user_lines);
            }
            self.ensure_session(cmd);
            self.current_query = cmd.to_string();
            self.start_async_chat(cmd);
        }
    }

    pub(crate) fn handle_slash_command(&mut self, cmd: &str) {
        // A submitted command must close the slash popup — otherwise the
        // popup stays visible (the composer was already cleared, so
        // on_input_change never fires) and, in inline mode, masks the very
        // picker the command opened (render order: slash → session → model).
        self.slash_state.dismiss();

        let (token, rest) = split_slash_input(cmd);
        let command_name = self
            .slash_registry
            .get_by_name_or_alias(token)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| token.to_string());

        match command_name.as_str() {
            "exit" => {
                self.save_session_state();
                self.running = false;
            }
            "help" => self.show_help(),
            "clear" => {
                if self.is_inline_mode() {
                    self.scrollback_queue.clear();
                    self.output.clear();
                    self.stream_collector.clear();
                    // Wave 2A: /clear restarts the transcript — the replay
                    // registry must restart with it (the notice below is the
                    // only history that remains).
                    self.transcript.clear();
                    let cleared = vec![Line::from(ratatui::text::Span::styled(
                        "(screen cleared)",
                        self.theme.as_ref().text_muted(),
                    ))];
                    self.transcript
                        .push(super::transcript_source::TranscriptSource::PreRendered(
                            cleared.clone(),
                        ));
                    self.enqueue_scrollback(cleared);
                } else {
                    self.output.clear();
                    self.stream_collector.clear();
                }
                self.invalidate_output_cache();
            }
            "thinking" => {
                // (c) Three-state thinking display (hermes off/brief/full).
                // Bare `/thinking` cycles off -> brief -> full -> off; an
                // explicit arg sets the mode. `show_thinking` stays the
                // canonical "mode != off" bool (every existing consumer
                // compiles untouched); `thinking_brief` is true ONLY in
                // brief mode. The stream_collector mirror feeds the render
                // path (same shape as /tools -> tools_expanded, so the
                // frozen inline.rs/chat.rs drain call sites keep their
                // signature).
                let mode = match rest {
                    None => cycle_thinking_mode(self.show_thinking, self.thinking_brief),
                    Some(arg) => match parse_thinking_mode(arg) {
                        Some(mode) => mode,
                        None => {
                            self.push_output(
                                "Usage: /thinking [off|brief|full] (bare cycles off -> brief -> full)"
                                    .to_string(),
                                true,
                            );
                            return;
                        }
                    },
                };
                self.show_thinking = mode.0;
                self.thinking_brief = mode.1;
                self.stream_collector.set_thinking_brief(mode.1);
                self.push_output(
                    format!("Thinking display: {}", decode_thinking_mode(mode.0, mode.1)),
                    false,
                );
            }
            "tools" => {
                // T057: collapse/expand 🔧/✅ tool intermediate blocks.
                self.stream_collector.toggle_tools_expanded();
                self.push_output(
                    format!(
                        "Tool intermediates {}",
                        if self.stream_collector.tools_expanded() {
                            "expanded"
                        } else {
                            "collapsed"
                        }
                    ),
                    false,
                );
            }
            "export" => self.execute_export(),
            "note" => self.execute_note(rest),
            "search" => self.execute_search(rest.unwrap_or("")),
            "session" => self.session_picker.show(),
            "new" => self.execute_new_session(),
            "fork" => self.execute_fork_session(rest),
            "rename" => self.execute_rename_session(rest),
            "archive" => self.execute_archive_session(),
            "config" => self.execute_config(),
            "model" => self.execute_model(rest),
            "variant" | "vc" | "variant_cycle" => self.execute_variant_cycle(),
            "distill" => self.execute_distill(),
            "lint" => self.execute_lint(),
            _ => self.push_output(
                format!("Unknown command: /{}. Type /help for commands.", token),
                true,
            ),
        }
    }

    fn show_help(&mut self) {
        let help = r#"Zen Agentic TUI - Commands:
  /help (/h)             Show this help
  /exit (/q)             Exit TUI
  /clear                 Clear output
  /thinking [off|brief|full]
                         Thinking display mode (bare cycles; default OFF)
  /model <p> [m]         Switch provider [model], show current if omitted
  /variant (/vc)          Cycle through model variants (reasoning levels)
  /export (/e)           Export chat to Markdown
  /note <text>           Create a note
  /search <q> (/s <q>)   Search knowledge base
  /session               List and select sessions
  /new                   Create new session
  /fork [name]           Fork current session
  /rename <name>         Rename current session
  /archive               Archive current session
  /config                Show configuration
  /distill            Run distillation pipeline
  /lint                  Run knowledge lint

Keyboard shortcuts:
  Tab          Switch between input modes
  Ctrl+V       Paste from clipboard
  Ctrl+L       Clear output (alias: /clear)
  Ctrl+D       Exit TUI (alias: /exit)

Aliases: Commands shown with (/<alias>) can be typed with the shorter form.
Example: '/h' = '/help', '/q' = '/exit', '/s <query>' = '/search <query>'

Chat mode is default — type questions to get LLM responses.
Use /thinking [off|brief|full] to control the thinking display (bare cycles)."#;
        self.push_output(help.to_string(), false);
    }

    fn execute_model(&mut self, args: Option<&str>) {
        match args {
            None => {
                self.model_picker.show(self.config);
            }
            Some(arg) => {
                let parts: Vec<&str> = arg.splitn(3, ' ').collect();
                let provider = parts[0];
                match self.config.providers.get(provider) {
                    None => {
                        self.push_output(format!("Unknown provider: {provider}"), true);
                    }
                    Some(pc) if parts.len() == 1 => {
                        self.push_output(format!("Provider: {provider}"), false);
                        if pc.models.is_empty() {
                            let d = pc.default_model.as_deref().unwrap_or("-");
                            self.push_output(format!("  (no catalog; default: {d})"), false);
                            self.push_output(format!("  Use: /model {provider} {d}"), false);
                        } else {
                            for (mid, e) in &pc.models {
                                let tag = if Some(mid.as_str()) == pc.default_model.as_deref() {
                                    " (default)"
                                } else {
                                    ""
                                };
                                let vi = if !e.variants.is_empty() {
                                    let ns: Vec<_> = e.variants.keys().cloned().collect();
                                    format!("  variants: [{}]", ns.join(", "))
                                } else {
                                    String::new()
                                };
                                self.push_output(format!("  {mid}{tag}{vi}"), false);
                            }
                            self.push_output(
                                format!("Usage: /model {provider} <model> [variant]"),
                                false,
                            );
                        }
                    }
                    _ => {
                        let model_name = parts[1];
                        let variant = parts.get(2).copied();
                        self.set_model(provider, model_name);
                        if let Some(v) = variant {
                            self.current_variant = Some(v.to_string());
                            self.push_output(format!("Variant: {v}"), false);
                        }
                    }
                }
            }
        }
    }

    /// Switch the active LLM provider/model and re-wire memory store.
    ///
    /// Re-wires the memvid store in read-only mode after model switch,
    /// maintaining multi-process compatibility with the daemon.
    pub fn set_model(&mut self, provider: &str, model: &str) {
        // Accept any provider that exists in config, is a known built-in, or is ollama/mock.
        // Custom providers (e.g. personal model gateways, third-party proxies) are configured
        // in ~/.zen/config.toml with type = "openai-compatible" or "anthropic-compatible".
        let known = zen_core::constants::SUPPORTED_LLM_PROVIDERS;
        let in_config = self.config.providers.contains_key(provider);
        let is_valid = in_config || known.contains(&provider) || provider == "mock";

        if !is_valid {
            self.push_output(
                format!(
                    "Provider '{provider}' not found. Add to ~/.zen/config.toml first, e.g.:\n\
                     [providers.{provider}]\n\
                     type = \"openai-compatible\"\n\
                     base_url = \"https://...\"\n\
                     api_key = {{ env = \"{}_API_KEY\" }}",
                    provider.to_uppercase(),
                ),
                true,
            );
            return;
        }

        if provider != "ollama" && provider != "mock" {
            let provider_cfg = self.config.providers.get(provider);
            let default_env = format!("{}_API_KEY", provider.to_uppercase());
            let hint = provider_cfg
                .and_then(|cfg| {
                    cfg.api_key_env.clone().or_else(|| {
                        cfg.api_key.as_ref().and_then(|sr| match sr {
                            zen_core::secrets::SecretRef::Env { env } => Some(env.clone()),
                            _ => None,
                        })
                    })
                })
                .unwrap_or_else(|| default_env.clone());

            let has_key = provider_cfg.and_then(|cfg| {
                // 1. Try Keychain → Env via SecretResolver (Keychain first)
                let kc_name = format!("zen-{provider}-api-key");
                zen_auth::SecretResolver::new(&kc_name, &default_env)
                    .resolve()
                    .ok()
                    // 2. Try explicitly configured SecretRef (Keychain or Env)
                    .or_else(|| {
                        cfg.api_key
                            .as_ref()
                            .and_then(|sr| zen_auth::resolve_secret_ref(sr).ok())
                    })
                    // 3. Try legacy api_key_env
                    .or_else(|| {
                        cfg.api_key_env
                            .as_ref()
                            .and_then(|name| std::env::var(name).ok())
                    })
            });

            if has_key.is_none() {
                self.push_output(
                    format!(
                        "Provider '{provider}' needs API key. Set {hint} or use: /model ollama (running)",
                    ),
                    true,
                );
                return;
            }
        }

        // Hosted turns build their router daemon-side; persisting the
        // selection here means the next daemon start (and its
        // orchestrator) picks the switched model up. A server-side
        // model/list method lands with P3 (contracts/04 `/model`).
        let new_model = format!("{}/{}", provider, model);
        self.model = new_model.clone();

        self.push_output(format!("Model switched to: {}", new_model), false);
        self.push_output(
            "Note: running daemon keeps its current model until restarted.".to_string(),
            false,
        );

        if let Err(e) = zen_core::config::save_model_selection(provider, model) {
            self.push_output(format!("Warning: failed to persist model: {e}"), true);
        }
    }

    fn execute_variant_cycle(&mut self) {
        let (provider, model_name) = match self.model.split_once('/') {
            Some((p, m)) => (p.to_string(), m.to_string()),
            None => {
                // Single-word model — treat as provider, use its default_model
                let provider = self.model.clone();
                let model_name = self
                    .config
                    .providers
                    .get(&provider)
                    .and_then(|p| p.default_model.clone())
                    .unwrap_or_else(|| "default".into());
                (provider, model_name)
            }
        };

        let variants = self
            .config
            .providers
            .get(&provider)
            .and_then(|p| p.models.get(&model_name))
            .map(|m| m.variants.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();

        if variants.is_empty() {
            self.push_output(
                format!(
                    "Model {}/{} has no variants configured.",
                    provider, model_name
                ),
                false,
            );
            return;
        }

        let current = self.current_variant.as_deref().unwrap_or("");
        let idx = variants
            .iter()
            .position(|v| v == current)
            .unwrap_or(variants.len() - 1);
        let next = variants[(idx + 1) % variants.len()].clone();

        self.current_variant = Some(next.clone());
        self.push_output(
            format!("Variant: {} ({}/{})", next, next, variants.join(", ")),
            false,
        );
    }

    fn execute_export(&mut self) {
        let output_text: Vec<String> = self.output.iter().map(|cell| cell.raw_text()).collect();
        let content = output_text.join("\n");
        let timestamp = chrono::Utc::now().format("%Y-%m-%d-%H%M%S");
        let filename = format!("chat-{}.md", timestamp);
        let path = std::env::temp_dir().join(&filename);

        match std::fs::write(
            &path,
            format!("# Chat Export - {}\n\n```\n{}\n```\n", timestamp, content),
        ) {
            Ok(_) => self.push_output(format!("Chat exported to {}", path.display()), false),
            Err(e) => self.push_output(format!("Export error: {}", e), true),
        }
    }

    fn execute_search(&mut self, query: &str) {
        if query.is_empty() {
            self.push_output("Usage: /search <query> or just type text".into(), true);
            return;
        }
        // contracts/04 P2: /search rides knowledge/search on the daemon.
        let query_owned = query.to_string();
        let outcome = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                // Ensure a gateway is up (connect-or-embed / spawn),
                // preserving the pre-existing connection policy. The surface
                // is then used only as a fallback: search_knowledge discards
                // the additive `degraded` array this command must surface.
                let surface = match super::prewarm::take_client() {
                    Some(surface) => Some(surface),
                    None => super::prewarm::resolve_client().await,
                };
                let Some(surface) = surface else {
                    return Err(SEARCH_OFFLINE_MSG.to_string());
                };
                match raw_search_request(&query_owned).await {
                    Ok(parsed) => Ok((parsed.0, parsed.1, false)),
                    Err(raw_err) => match surface.search_knowledge(&query_owned, None, 10).await {
                        Ok(notes) => Ok((
                            notes.into_iter().map(|n| (n.path, n.content)).collect(),
                            Vec::new(),
                            true,
                        )),
                        Err(e) => Err(format!("{raw_err}; {e}")),
                    },
                }
            })
        });
        match outcome {
            Ok((notes, degraded, relayed)) => {
                if notes.is_empty() {
                    self.push_output(format!("[gateway] No results for '{}'", query), false);
                    self.push_output(
                        "  Tip: `zen wiki reindex` refreshes FTS + semantic indexes over the vault."
                            .to_string(),
                        false,
                    );
                } else {
                    self.push_output(format!("[gateway] Found {} results:", notes.len()), false);
                    for (path, content) in &notes {
                        self.push_output(
                            format!("  {} {}", path, truncate_search_content(content)),
                            false,
                        );
                    }
                }
                if let Some(line) = relayed_degraded_advisory(relayed, &degraded) {
                    self.push_output(line, true);
                }
                if let Some(line) = degraded_tiers_line(&degraded) {
                    self.push_output(line, true);
                }
            }
            Err(e) => self.push_output(format!("Search error: {}", e), true),
        }
    }

    fn execute_note(&mut self, content: Option<&str>) {
        let content = match content {
            Some(c) if !c.is_empty() => c,
            _ => {
                self.push_output("Usage: /note <content>".into(), true);
                return;
            }
        };
        use zen_vault::note::NoteService;
        match tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(NoteService::new().create_note(
                content,
                vec![],
                "tui",
            ))
        }) {
            Ok(note) => self.push_output(
                format!("Note created: {} ({})", note.id, note.source),
                false,
            ),
            Err(e) => self.push_output(format!("Note error: {}", e), true),
        }
    }

    fn execute_config(&mut self) {
        self.push_output("Configuration:".into(), false);
        self.push_output(
            format!("  LLM default: {:?}", self.config.default_provider),
            false,
        );
        self.push_output(
            format!("  Cron: {:?}", self.config.cron.consolidation_time),
            false,
        );
    }

    fn execute_distill(&mut self) {
        use zen_core::paths::ZenPaths;
        use zen_vault::distill::DistillationPipeline;
        if let Ok(paths) = ZenPaths::detect() {
            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(DistillationPipeline::new().run(&paths.inbox(), &paths.wiki()))
            });
            match result {
                Ok(report) => {
                    self.push_output("Distillation complete:".into(), false);
                    self.push_output(
                        format!("  Notes processed: {}", report.notes_processed),
                        false,
                    );
                    self.push_output(
                        format!("  Archived: {}", report.migrated_files.len()),
                        false,
                    );
                }
                Err(e) => self.push_output(format!("Distillation error: {}", e), true),
            }
        }
    }

    fn execute_lint(&mut self) {
        use zen_core::paths::ZenPaths;
        use zen_vault::tindy::Linter;
        if let Ok(paths) = ZenPaths::detect() {
            let okf_enabled = zen_core::config::load_config()
                .map(|c| c.agentic.compile.okf_lint_or_default())
                .unwrap_or(true);
            match Linter::new().run_with_okf(&paths.wiki(), okf_enabled) {
                Ok(result) => {
                    self.push_output("Lint complete:".into(), false);
                    self.push_output(
                        format!("  Orphan pages: {}", result.orphan_pages.len()),
                        false,
                    );
                    self.push_output(
                        format!("  Broken wikilinks: {}", result.broken_wikilinks.len()),
                        false,
                    );
                    self.push_output(
                        format!("  OKF missing fields: {}", result.okf_missing.len()),
                        false,
                    );
                    self.push_output(
                        format!("  Sources missing: {}", result.sources_missing.len()),
                        false,
                    );
                }
                Err(e) => self.push_output(format!("Lint error: {}", e), true),
            }
        }
    }
}

#[cfg(test)]
mod search_display_tests {
    use super::*;

    #[test]
    fn truncate_passes_short_content_through() {
        let short = "first line\nsecond line";
        assert_eq!(truncate_search_content(short), short);
    }

    #[test]
    fn truncate_caps_line_count_with_marker() {
        let long: String = (0..100)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = truncate_search_content(&long);
        assert!(out.ends_with(SEARCH_TRUNCATION_MARKER));
        assert_eq!(
            out.lines().count(),
            SEARCH_CONTENT_MAX_LINES,
            "line cap is a hard display bound"
        );
    }

    #[test]
    fn truncate_caps_char_count_on_a_single_long_line() {
        let long = "x".repeat(SEARCH_CONTENT_MAX_CHARS * 2);
        let out = truncate_search_content(&long);
        assert!(out.ends_with(SEARCH_TRUNCATION_MARKER));
        assert_eq!(
            out.chars().count(),
            SEARCH_CONTENT_MAX_CHARS + SEARCH_TRUNCATION_MARKER.chars().count()
        );
    }

    #[test]
    fn degraded_line_none_when_all_tiers_healthy() {
        assert!(degraded_tiers_line(&[]).is_none());
    }

    #[test]
    fn degraded_line_names_failing_tiers() {
        let line = degraded_tiers_line(&["fts".to_string(), "vec".to_string()]).unwrap();
        assert_eq!(line, "⚠ degraded tiers: fts, vec");
    }

    #[test]
    fn relayed_search_warns_on_absent_diagnostics() {
        assert!(relayed_degraded_advisory(true, &[]).is_some());
        assert!(relayed_degraded_advisory(false, &[]).is_none());
    }

    #[test]
    fn parse_search_response_reads_notes_and_degraded() {
        let result = serde_json::json!({
            "notes": [{"path": "/a.md", "content": "hi"}],
            "degraded": [{"tier": "vec", "error": "no model"}],
        });
        let (notes, degraded) = parse_search_response(&result);
        assert_eq!(notes, vec![("/a.md".to_string(), "hi".to_string())]);
        assert_eq!(degraded, vec!["vec".to_string()]);
    }

    #[test]
    fn parse_search_response_absent_degraded_is_empty() {
        let result = serde_json::json!({"notes": []});
        let (notes, degraded) = parse_search_response(&result);
        assert!(notes.is_empty());
        assert!(degraded.is_empty());
    }

    #[test]
    fn split_trims_double_space_typo() {
        assert_eq!(
            split_slash_input("search  letter"),
            ("search", Some("letter"))
        );
    }

    #[test]
    fn split_preserves_internal_spaces() {
        assert_eq!(
            split_slash_input("fork my copied name"),
            ("fork", Some("my copied name"))
        );
    }

    #[test]
    fn split_trailing_space_yields_none() {
        assert_eq!(split_slash_input("clear "), ("clear", None));
        assert_eq!(split_slash_input("search  "), ("search", None));
    }

    #[test]
    fn split_bare_token_has_no_rest() {
        assert_eq!(split_slash_input("exit"), ("exit", None));
    }
}

#[cfg(test)]
mod enqueue_echo_tests {
    use super::*;
    use crate::tui::app::App;

    fn test_config() -> &'static zen_core::config::ZenConfig {
        Box::leak(Box::default())
    }

    fn rendered_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn enqueue_echo_carries_scrollback_only_queue_marker() {
        let mut app = App::new(test_config());
        app.inline_mode = true;
        app.is_streaming = true;
        app.scrollback_queue.clear();

        app.handle_command("queued later");

        assert_eq!(
            app.message_queue
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["queued later"],
            "the queue must hold the raw text (session/model payload)"
        );
        assert!(
            !app.message_queue.iter().any(|queued| queued.contains('⇅')),
            "the scrollback marker must never enter the submitted payload"
        );

        let echo = app
            .scrollback_queue
            .iter()
            .flat_map(|entry| entry.lines.iter())
            .map(rendered_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            echo.contains('⇅'),
            "enqueue echo must carry the queue marker: {echo:?}"
        );
        assert!(
            echo.contains("> queued later"),
            "enqueue echo must still render the user line: {echo:?}"
        );
    }
}

/// (c) Three-state /thinking command pins.
#[cfg(test)]
mod thinking_mode_tests {
    use crate::tui::app::App;
    use crate::tui::cell::OutputCell;

    fn test_app() -> App {
        let config: &'static zen_core::config::ZenConfig = Box::leak(Box::default());
        App::new(config)
    }

    fn mode(app: &App) -> (bool, bool) {
        (app.show_thinking, app.thinking_brief)
    }

    fn last_plain_output(app: &App) -> String {
        for cell in app.output.iter().rev() {
            if let OutputCell::Plain(cell) = cell {
                return cell.text.clone();
            }
        }
        panic!("no plain output cell found");
    }

    #[test]
    fn thinking_command_cycles_off_brief_full() {
        let mut app = test_app();
        assert_eq!(mode(&app), (false, false), "default is off");

        app.handle_slash_command("thinking"); // off -> brief
        assert_eq!(mode(&app), (true, true), "first bare press lands on brief");
        assert_eq!(last_plain_output(&app), "Thinking display: brief");

        app.handle_slash_command("thinking"); // brief -> full
        assert_eq!(mode(&app), (true, false));
        assert_eq!(last_plain_output(&app), "Thinking display: full");

        app.handle_slash_command("thinking"); // full -> off
        assert_eq!(mode(&app), (false, false));
        assert_eq!(last_plain_output(&app), "Thinking display: off");
    }

    #[test]
    fn thinking_command_explicit_arg_sets_mode() {
        let mut app = test_app();

        app.handle_slash_command("thinking full");
        assert_eq!(mode(&app), (true, false));
        assert_eq!(last_plain_output(&app), "Thinking display: full");

        app.handle_slash_command("thinking brief");
        assert_eq!(mode(&app), (true, true));
        assert_eq!(last_plain_output(&app), "Thinking display: brief");

        app.handle_slash_command("thinking off");
        assert_eq!(mode(&app), (false, false));
        assert_eq!(last_plain_output(&app), "Thinking display: off");

        // Unknown arg: usage error, mode unchanged, NO plain ack appended.
        let plains_before = app
            .output
            .iter()
            .filter(|c| matches!(c, OutputCell::Plain(_)))
            .count();
        app.handle_slash_command("thinking loud");
        assert_eq!(mode(&app), (false, false), "bad arg must not flip the mode");
        let plains_after = app
            .output
            .iter()
            .filter(|c| matches!(c, OutputCell::Plain(_)))
            .count();
        assert_eq!(
            plains_after, plains_before,
            "error path appends no Plain ack"
        );
    }
}
