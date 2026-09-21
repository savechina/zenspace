use ratatui::text::Line;

use super::app::{App, MAX_QUEUE_SIZE};

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
                self.message_queue.push_back(cmd.to_string());
            }
        } else {
            // T055/T061: instant path for BOTH inline and full-screen —
            // echo + pending call + streaming flag happen here (<1ms); the
            // heavy pipeline (orchestrator acquire, knowledge search, LLM
            // dispatch) runs in background tasks.
            if self.is_inline_mode() {
                let user_lines = self.render_user_lines_for_scrollback(cmd);
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

        let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
        let command_name = self
            .slash_registry
            .get_by_name_or_alias(parts[0])
            .map(|c| c.name.clone())
            .unwrap_or_else(|| parts[0].to_string());

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
                    self.enqueue_scrollback(vec![Line::from(ratatui::text::Span::styled(
                        "(screen cleared)",
                        self.theme.as_ref().text_muted(),
                    ))]);
                } else {
                    self.output.clear();
                    self.stream_collector.clear();
                }
                self.invalidate_output_cache();
            }
            "thinking" => {
                self.show_thinking = !self.show_thinking;
                self.push_output(
                    format!(
                        "Thinking display {}",
                        if self.show_thinking {
                            "enabled"
                        } else {
                            "hidden"
                        }
                    ),
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
            "note" => self.execute_note(parts.get(1).copied()),
            "search" => self.execute_search(parts.get(1).copied().unwrap_or("")),
            "session" => self.session_picker.show(),
            "new" => self.execute_new_session(),
            "fork" => self.execute_fork_session(parts.get(1).copied()),
            "rename" => self.execute_rename_session(parts.get(1).copied()),
            "archive" => self.execute_archive_session(),
            "serve" => self.execute_serve(parts.get(1).copied()),
            "config" => self.execute_config(),
            "model" => self.execute_model(parts.get(1).copied()),
            "variant" | "vc" | "variant_cycle" => self.execute_variant_cycle(),
            "distill" => self.execute_distill(),
            "lint" => self.execute_lint(),
            _ => self.push_output(
                format!("Unknown command: /{}. Type /help for commands.", parts[0]),
                true,
            ),
        }
    }

    fn show_help(&mut self) {
        let help = r#"Zen Agentic TUI - Commands:
  /help (/h)             Show this help
  /exit (/q)             Exit TUI
  /clear                 Clear output
  /thinking              Toggle showing thinking process (default: OFF)
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
  /serve                 Start gateway daemon
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
Use /thinking to show/hide thinking process."#;
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
                let surface = match super::prewarm::take_client() {
                    Some(surface) => Some(surface),
                    None => super::prewarm::resolve_client().await,
                };
                let Some(surface) = surface else {
                    return Err("gateway: offline — search unavailable (retrying)".to_string());
                };
                surface
                    .search_knowledge(&query_owned, None, 10)
                    .await
                    .map_err(|e| e.to_string())
            })
        });
        match outcome {
            Ok(notes) => {
                if notes.is_empty() {
                    self.push_output(format!("[gateway] No results for '{}'", query), false);
                } else {
                    self.push_output(format!("[gateway] Found {} results:", notes.len()), false);
                    for note in &notes {
                        self.push_output(format!("  {} {}", note.path, note.content), false);
                    }
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

    fn execute_serve(&mut self, args: Option<&str>) {
        match args {
            Some("start") => match self.spawn_gateway_daemon() {
                Ok(msg) => self.push_output(msg, false),
                Err(e) => self.push_output(format!("Gateway error: {}", e), true),
            },
            Some("stop") => match self.stop_gateway_daemon() {
                Ok(msg) => self.push_output(msg, false),
                Err(e) => self.push_output(format!("Gateway error: {}", e), true),
            },
            Some("status") => match self.check_gateway_status() {
                Ok(msg) => self.push_output(msg, false),
                Err(e) => self.push_output(format!("Gateway error: {}", e), true),
            },
            _ => self.push_output("Usage: /serve start|stop|status".into(), true),
        }
    }

    fn spawn_gateway_daemon(&self) -> Result<String, String> {
        use std::process::{Command, Stdio};

        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let pid_path = zen_core::paths::ZenPaths::detect()
            .map_err(|e| e.to_string())?
            .global_root()
            .join("daemon.pid");

        // Read through the shared zen-gateway parser: the daemon writes a
        // JSON record, and `read_pid` understands both that and the legacy
        // bare-pid format.
        if let Ok(pid) = zen_gateway::read_pid(&pid_path) {
            #[cfg(unix)]
            {
                if unsafe { libc::kill(pid as i32, 0) == 0 } {
                    return Err(format!("Gateway already running (pid: {})", pid));
                }
            }
        }

        let mut cmd = Command::new(&exe);
        cmd.arg("serve").arg("start").arg("--foreground");
        cmd.stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null());

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            unsafe {
                cmd.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
        }

        let child = cmd.spawn().map_err(|e| format!("Failed to spawn: {}", e))?;
        let child_pid = child.id();

        std::thread::sleep(std::time::Duration::from_millis(500));

        #[cfg(unix)]
        {
            if unsafe { libc::kill(child_pid as i32, 0) != 0 } {
                return Err("Gateway daemon failed to start".to_string());
            }
        }

        // Single-writer invariant (NFR-010): the daemon.pid record is owned
        // by the spawned child — `zen serve start --foreground` writes its
        // own JSON record `{"pid": N, "start": ...}` via `write_pid`
        // (tmp+rename, atomic) before entering the foreground loop. The TUI
        // must NOT write a bare pid here: a non-atomic write ~500ms after
        // spawn would clobber the child's record with a different format
        // (bare pid vs JSON) and race it. The child is the single writer.

        let config = zen_gateway::HttpConfig::default();
        Ok(format!(
            "Gateway started in background (pid: {}) on http://{}:{}",
            child_pid, config.bind_addr, config.port
        ))
    }

    fn stop_gateway_daemon(&self) -> Result<String, String> {
        let pid_path = zen_core::paths::ZenPaths::detect()
            .map_err(|e| e.to_string())?
            .global_root()
            .join("daemon.pid");

        if !pid_path.exists() {
            return Ok("Gateway not running (no PID file)".to_string());
        }

        let pid = zen_gateway::read_pid(&pid_path).map_err(|e| e.to_string())?;

        #[cfg(unix)]
        {
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };

            for _ in 0..20 {
                std::thread::sleep(std::time::Duration::from_millis(500));
                if unsafe { libc::kill(pid as i32, 0) != 0 } {
                    break;
                }
            }

            if unsafe { libc::kill(pid as i32, 0) == 0 } {
                unsafe { libc::kill(pid as i32, libc::SIGKILL) };
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }

        std::fs::remove_file(&pid_path).ok();
        Ok(format!("Gateway stopped (pid: {})", pid))
    }

    fn check_gateway_status(&self) -> Result<String, String> {
        let pid_path = zen_core::paths::ZenPaths::detect()
            .map_err(|e| e.to_string())?
            .global_root()
            .join("daemon.pid");

        if !pid_path.exists() {
            return Ok("Gateway not running".to_string());
        }

        let pid = zen_gateway::read_pid(&pid_path).map_err(|e| e.to_string())?;

        #[cfg(unix)]
        {
            if unsafe { libc::kill(pid as i32, 0) == 0 } {
                let config = zen_gateway::HttpConfig::default();
                return Ok(format!(
                    "Gateway running (pid: {}) on http://{}:{}",
                    pid, config.bind_addr, config.port
                ));
            }
        }

        Ok(format!("Gateway stale (pid: {} is dead)", pid))
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
            match Linter::new().run(&paths.wiki()) {
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
                }
                Err(e) => self.push_output(format!("Lint error: {}", e), true),
            }
        }
    }
}
