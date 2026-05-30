use anyhow::Result;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::mpsc;
use zen_agents::AgentOrchestrator;
use zen_core::config::load_config;
use zen_core::types::SessionContext;
use zen_provider::DefaultRouter;

#[derive(Debug)]
pub struct OutputLine {
    pub text: String,
    pub is_error: bool,
}

pub struct PendingLlmCall {
    pub query: String,
    pub rx: mpsc::Receiver<Result<String, String>>,
}

pub struct PendingLlmCallStream {
    pub query: String,
    pub tokens_rx: mpsc::Receiver<String>,
    pub done_rx: mpsc::Receiver<Result<String, String>>,
}

pub enum PendingCallKind {
    SingleShot(PendingLlmCall),
    Streaming(PendingLlmCallStream),
}

const MAX_HISTORY: usize = 100;

pub const SLASH_COMMANDS: &[&str] = &[
    "help",
    "quit",
    "clear",
    "thinking",
    "export",
    "note",
    "search",
    "session",
    "serve",
    "config",
    "model",
    "consolidate",
    "lint",
];

pub const CLI_COMMANDS: &[&str] = &[
    "hello",
    "clean",
    "starter",
    "wps",
    "version",
    "session",
    "serve",
    "agent",
    "workspace",
    "config",
    "provider",
    "audit",
    "note",
    "search",
    "similar",
    "graph",
    "reindex",
    "research",
    "consolidate",
    "lint",
    "ingest",
    "routine",
    "task",
    "brief",
    "plugin",
];

pub struct App {
    pub input: String,
    pub cursor_position: usize,
    pub output: VecDeque<OutputLine>,
    pub running: bool,
    pub workspace: String,
    pub session_id: Option<String>,
    pub model: String,
    pub memory_count: usize,
    pub show_thinking: bool,
    pub streaming_buffer: String,
    pub is_streaming: bool,
    pub chat_history: Vec<(String, String)>,
    pub pending_calls: Vec<PendingCallKind>,
    pub message_queue: VecDeque<String>,
    pub current_query: String,
    pub command_history: Vec<String>,
    pub history_position: Option<usize>,
    pub autocomplete_suggestions: Vec<String>,
    pub autocomplete_selected: usize,
    pub autocomplete_scroll_offset: usize,
    pub show_autocomplete: bool,
    orchestrator: Option<Arc<AgentOrchestrator>>,
    session: Option<SessionContext>,
}

impl App {
    pub fn new() -> Self {
        let workspace = std::env::current_dir()
            .ok()
            .and_then(|p| p.to_str().map(String::from))
            .unwrap_or_else(|| ".".into());
        Self {
            input: String::new(),
            cursor_position: 0,
            output: VecDeque::new(),
            running: true,
            workspace,
            session_id: None,
            model: "ollama/qwen3.6:35b-mlx".into(),
            memory_count: 0,
            show_thinking: false,
            streaming_buffer: String::new(),
            is_streaming: false,
            chat_history: Vec::new(),
            pending_calls: Vec::new(),
            message_queue: VecDeque::new(),
            current_query: String::new(),
            command_history: Vec::new(),
            history_position: None,
            autocomplete_suggestions: Vec::new(),
            autocomplete_selected: 0,
            autocomplete_scroll_offset: 0,
            show_autocomplete: false,
            orchestrator: None,
            session: None,
        }
    }

    pub fn init_orchestrator(&mut self) {
        let config = match load_config() {
            Ok(c) => c,
            Err(e) => {
                self.push_output(format!("Config load error (using defaults): {}", e), true);
                return;
            },
        };
        let router = DefaultRouter::from_agentic(&config);
        self.orchestrator = Some(Arc::new(AgentOrchestrator::new(router)));
        self.session = Some(SessionContext::new("default".into(), String::new()));
    }

    pub fn push_output(&mut self, text: String, is_error: bool) {
        for line in text.lines() {
            self.output.push_back(OutputLine {
                text: line.to_string(),
                is_error,
            });
        }
        while self.output.len() > 500 {
            self.output.pop_front();
        }
    }

    pub fn push_history(&mut self, cmd: &str) {
        if cmd.is_empty() || self.command_history.last().map(|h| h.as_str()) == Some(cmd) {
            return;
        }
        self.command_history.push(cmd.to_string());
        if self.command_history.len() > MAX_HISTORY {
            self.command_history.remove(0);
        }
        self.history_position = None;
    }

    pub fn history_up(&mut self) {
        if self.command_history.is_empty() {
            return;
        }
        let new_pos = match self.history_position {
            None => self.command_history.len() - 1,
            Some(0) => 0,
            Some(p) => p - 1,
        };
        self.history_position = Some(new_pos);
        if let Some(entry) = self.command_history.get(new_pos) {
            self.input = entry.clone();
            self.cursor_position = self.input.len();
        }
    }

    pub fn history_down(&mut self) {
        match self.history_position {
            None => {},
            Some(p) if p + 1 >= self.command_history.len() => {
                self.history_position = None;
                self.input.clear();
                self.cursor_position = 0;
            },
            Some(p) => {
                self.history_position = Some(p + 1);
                if let Some(entry) = self.command_history.get(p + 1) {
                    self.input = entry.clone();
                    self.cursor_position = self.input.len();
                }
            },
        }
    }

    pub fn update_autocomplete(&mut self) {
        let input = self.input.trim_start();
        if input.is_empty() || (!input.starts_with('/') && input.len() < 2) {
            self.autocomplete_suggestions.clear();
            self.show_autocomplete = false;
            return;
        }
        let prefix = input.strip_prefix('/').unwrap_or(input);
        let mut suggestions: Vec<String> = SLASH_COMMANDS
            .iter()
            .filter(|c| c.starts_with(prefix))
            .map(|c| format!("/{}", c))
            .collect();
        if !input.starts_with('/') {
            suggestions.extend(
                CLI_COMMANDS
                    .iter()
                    .filter(|c| c.starts_with(prefix))
                    .map(|c| c.to_string()),
            );
        }
        suggestions.sort();
        suggestions.dedup();
        if suggestions.len() > 1 && suggestions != self.autocomplete_suggestions {
            self.autocomplete_suggestions = suggestions;
            self.autocomplete_selected = 0;
            self.autocomplete_scroll_offset = 0;
            self.show_autocomplete = true;
        } else {
            self.autocomplete_suggestions.clear();
            self.show_autocomplete = false;
        }
    }

    pub fn autocomplete_cycle(&mut self) {
        if self.autocomplete_suggestions.is_empty() {
            return;
        }
        self.autocomplete_selected =
            (self.autocomplete_selected + 1) % self.autocomplete_suggestions.len();
        let max_visible = 5;
        if self.autocomplete_selected >= self.autocomplete_scroll_offset + max_visible {
            self.autocomplete_scroll_offset =
                self.autocomplete_selected.saturating_sub(max_visible - 1);
        } else if self.autocomplete_selected < self.autocomplete_scroll_offset {
            self.autocomplete_scroll_offset = self.autocomplete_selected;
        }
    }

    pub fn autocomplete_accept(&mut self) {
        if let Some(s) = self
            .autocomplete_suggestions
            .get(self.autocomplete_selected)
        {
            self.input = s.clone();
            self.cursor_position = self.input.len();
        }
        self.autocomplete_suggestions.clear();
        self.show_autocomplete = false;
    }

    pub fn handle_command(&mut self, cmd: &str) {
        if cmd.is_empty() {
            return;
        }
        if let Some(stripped) = cmd.strip_prefix('/') {
            self.handle_slash_command(stripped);
        } else if self.is_streaming {
            self.message_queue.push_back(cmd.to_string());
        } else {
            self.current_query = cmd.to_string();
            self.execute_chat(cmd);
        }
    }

    fn handle_slash_command(&mut self, cmd: &str) {
        let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
        match parts[0] {
            "quit" | "q" | "exit" => self.running = false,
            "help" | "h" => self.show_help(),
            "clear" | "cls" => {
                self.output.clear();
                self.streaming_buffer.clear();
            },
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
            },
            "export" => self.execute_export(),
            "note" => self.execute_note(parts.get(1).copied()),
            "search" => self.execute_search(parts.get(1).copied().unwrap_or("")),
            "session" => self.execute_session(parts.get(1).copied()),
            "serve" => self.execute_serve(parts.get(1).copied()),
            "config" => self.execute_config(),
            "model" => self.execute_model(parts.get(1).copied()),
            "consolidate" => self.execute_consolidate(),
            "lint" => self.execute_lint(),
            _ => self.push_output(
                format!("Unknown command: /{}. Type /help for commands.", parts[0]),
                true,
            ),
        }
    }

    fn show_help(&mut self) {
        let help = r#"Zen Agentic TUI - Commands:
  /help          Show this help
  /quit          Exit TUI
  /clear         Clear output
  /thinking      Toggle showing thinking process (default: OFF)
  /model <p> [m] Switch provider [model], show current if omitted
  /export        Export chat to Markdown
  /note <text>   Create a note
  /search <q>    Search knowledge base
  /session       Start a new session
  /serve start   Start gateway daemon
  /config        Show configuration
  /consolidate   Run consolidation pipeline
  /lint          Run knowledge lint

Chat mode is default — type questions to get LLM responses.
Use /thinking to show/hide thinking process."#;
        self.push_output(help.to_string(), false);
    }

    fn execute_chat(&mut self, query: &str) {
        let specialist = self
            .orchestrator
            .as_ref()
            .map(|o| o.route(query))
            .unwrap_or_else(|| "unknown".to_string());

        let context = self.auto_search_knowledge(query);
        if !context.is_empty() {
            self.push_output(
                format!("[Knowledge] Found {} relevant notes", context.len()),
                false,
            );
        }

        self.push_output(format!("You: {}", query), false);
        self.push_output(format!("[{}] Thinking...", specialist), false);
        self.start_llm_call_via_orchestrator(query, &context);
    }

    fn auto_search_knowledge(&self, query: &str) -> Vec<String> {
        use zen_core::paths::ZenPaths;
        use zen_knowledge::search::{SearchService, TierSelector};

        let tier = TierSelector::select_tier(query);
        let paths = match ZenPaths::detect() {
            Ok(p) => p,
            Err(_) => return Vec::new(),
        };
        let base_dir = paths.inbox();

        match SearchService::new().search(query, &base_dir, Some(tier)) {
            Ok(results) => results
                .into_iter()
                .take(3)
                .map(|r| format!("[{}]\n{}", r.file.display(), r.content))
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    fn start_llm_call_via_orchestrator(&mut self, query: &str, context: &[String]) {
        let (tokens_tx, tokens_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let query_owned = query.to_string();
        self.pending_calls
            .push(PendingCallKind::Streaming(PendingLlmCallStream {
                tokens_rx,
                done_rx,
                query: query_owned.clone(),
            }));
        self.is_streaming = true;
        if self.pending_calls.len() == 1 {
            self.streaming_buffer.clear();
        }

        let orchestrator = match &self.orchestrator {
            Some(o) => o.clone(),
            None => {
                let _ = done_tx.send(Err("Orchestrator not initialized".to_string()));
                return;
            },
        };

        let mut session = match &self.session {
            Some(s) => s.clone(),
            None => {
                let _ = done_tx.send(Err("Session not initialized".to_string()));
                return;
            },
        };

        if !context.is_empty() {
            for (i, note_content) in context.iter().enumerate() {
                session.knowledge.push(zen_core::types::RetrievedNote {
                    path: format!("auto-search-{}", i),
                    content: note_content.clone(),
                    sensitivity: zen_core::types::Sensitivity::Public,
                    relevance: 1.0 - (i as f64 * 0.1),
                });
            }
        }

        tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Handle::current();
            let mut session = session;
            let result = rt.block_on(async {
                orchestrator
                    .execute_stream(&mut session, &query_owned, |token| {
                        let _ = tokens_tx.send(token.to_string());
                    })
                    .await
                    .map_err(|e| e.to_string())
            });
            let _ = done_tx.send(result);
        });
    }

    pub fn poll_llm_response(&mut self) {
        struct StreamResult {
            done_result: Option<Result<String, String>>,
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
                        },
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
                        },
                        Err(mpsc::TryRecvError::Disconnected) => {
                            results.push((
                                idx,
                                Some((
                                    s.query.clone(),
                                    StreamResult {
                                        done_result: Some(Err("disconnected".into())),
                                        tokens,
                                    },
                                )),
                            ));
                        },
                    }
                },
                PendingCallKind::SingleShot(ss) => match ss.rx.try_recv() {
                    Ok(result) => {
                        results.push((
                            idx,
                            Some((
                                ss.query.clone(),
                                StreamResult {
                                    done_result: Some(result),
                                    tokens: Vec::new(),
                                },
                            )),
                        ));
                    },
                    Err(mpsc::TryRecvError::Empty) => {},
                    Err(mpsc::TryRecvError::Disconnected) => {
                        results.push((
                            idx,
                            Some((
                                ss.query.clone(),
                                StreamResult {
                                    done_result: Some(Err("disconnected".into())),
                                    tokens: Vec::new(),
                                },
                            )),
                        ));
                    },
                },
            }
        }

        let mut completed_indices: Vec<usize> = Vec::new();

        for (idx, entry) in results {
            if let Some((_query, result)) = entry {
                for token in &result.tokens {
                    self.streaming_buffer.push_str(token);
                }

                if let Some(done_result) = result.done_result {
                    if let Some(pos) = self
                        .output
                        .iter()
                        .position(|line| line.text.contains("Thinking..."))
                    {
                        self.output.remove(pos);
                    }

                    match done_result {
                        Ok(response) => {
                            completed_indices.push(idx);
                            self.streaming_buffer.clear();
                            self.chat_history.push((_query, response.clone()));
                            for line in response.lines() {
                                self.push_output(line.to_string(), false);
                            }
                        },
                        Err(e) => {
                            completed_indices.push(idx);
                            self.streaming_buffer.clear();
                            self.push_output(format!("[LLM] Error: {}", e), true);
                        },
                    }
                }
            }
        }

        for idx in completed_indices.into_iter().rev() {
            self.pending_calls.remove(idx);
        }

        if self.pending_calls.is_empty() {
            self.is_streaming = false;
            self.streaming_buffer.clear();
            while let Some(queued) = self.message_queue.pop_front() {
                self.current_query = queued.clone();
                self.execute_chat(&queued);
                if !self.pending_calls.is_empty() {
                    break;
                }
            }
        }
    }

    fn execute_model(&mut self, args: Option<&str>) {
        match args {
            None => {
                self.push_output(format!("Current model: {}", self.model), false);
            },
            Some(arg) => {
                let parts: Vec<&str> = arg.splitn(2, ' ').collect();
                let provider = parts[0];
                let model = parts.get(1).copied().unwrap_or("default");
                self.set_model(provider, model);
            },
        }
    }

    fn set_model(&mut self, provider: &str, model: &str) {
        use zen_core::constants::SUPPORTED_LLM_PROVIDERS;

        let valid_providers = SUPPORTED_LLM_PROVIDERS;
        let is_valid = valid_providers.contains(&provider) || provider == "mock";

        if !is_valid {
            self.push_output(
                format!(
                    "Unknown provider: {}. Supported: {}",
                    provider,
                    "openai, anthropic, deepseek, aliyun, mistral, groq, moonshot, xai, perplexity, gemini, ollama, qqbot, mock"
                ),
                true,
            );
            return;
        }

        if provider != "ollama" && provider != "mock" {
            let agentic = zen_core::config::load_embedded_config()
                .unwrap_or_else(|_| zen_core::config::AgenticConfig::default());

            let env_var = agentic
                .providers
                .get(provider)
                .and_then(|cfg| {
                    cfg.api_key
                        .as_ref()
                        .and_then(|sr| match sr {
                            zen_core::secrets::SecretRef::Env { env } => Some(env.clone()),
                            _ => None,
                        })
                        .or_else(|| cfg.api_key_env.clone())
                })
                .unwrap_or_else(|| format!("{}_API_KEY", provider.to_uppercase()));

            if std::env::var(&env_var).is_err() {
                self.push_output(
                    format!(
                        "Provider '{}' needs API key. Set {} or use: /model ollama (running)",
                        provider, env_var
                    ),
                    true,
                );
                return;
            }
        }

        let router = DefaultRouter::new_for_provider(provider, model);
        let new_model = format!("{}/{}", provider, model);
        let orchestrator = Arc::new(AgentOrchestrator::new(router));

        self.orchestrator = Some(orchestrator);
        self.model = new_model.clone();

        self.push_output(format!("Model switched to: {}", new_model), false);
    }

    fn execute_export(&mut self) {
        let output_text: Vec<String> = self.output.iter().map(|l| l.text.clone()).collect();
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
        use zen_core::paths::ZenPaths;
        use zen_knowledge::search::{SearchService, TierSelector};

        let tier = TierSelector::select_tier(query);
        let paths = match ZenPaths::detect() {
            Ok(p) => p,
            Err(e) => {
                self.push_output(format!("Path error: {}", e), true);
                return;
            },
        };
        let base_dir = paths.inbox();

        match SearchService::new().search(query, &base_dir, Some(tier)) {
            Ok(results) => {
                if results.is_empty() {
                    self.push_output(format!("[tier {}] No results for '{}'", tier, query), false);
                } else {
                    self.push_output(
                        format!("[tier {}] Found {} results:", tier, results.len()),
                        false,
                    );
                    for r in &results {
                        self.push_output(
                            format!("  {}:{} {}", r.file.display(), r.line, r.content),
                            false,
                        );
                    }
                }
            },
            Err(e) => self.push_output(format!("Search error: {}", e), true),
        }
    }

    fn execute_note(&mut self, content: Option<&str>) {
        let content = match content {
            Some(c) if !c.is_empty() => c,
            _ => {
                self.push_output("Usage: /note <content>".into(), true);
                return;
            },
        };
        use zen_knowledge::note::NoteService;
        match NoteService::new().create_note(content, vec![], "tui") {
            Ok(note) => self.push_output(
                format!("Note created: {} ({})", note.id, note.source),
                false,
            ),
            Err(e) => self.push_output(format!("Note error: {}", e), true),
        }
    }

    fn execute_session(&mut self, _args: Option<&str>) {
        use zen_memory::session_manager::SessionManager;
        let manager = SessionManager::new();
        match manager.create_session("default", ".") {
            Ok(session) => {
                self.session_id = Some(session.id.clone());
                self.push_output(
                    format!(
                        "Session started: {} (agent: {})",
                        session.id, session.agent_name
                    ),
                    false,
                );
            },
            Err(e) => self.push_output(format!("Session error: {}", e), true),
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

        if let Ok(pid_str) = std::fs::read_to_string(&pid_path)
            && let Ok(pid) = pid_str.trim().parse::<u32>()
        {
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

        if let Some(parent) = pid_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&pid_path, child_pid.to_string()).ok();

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

        let pid_str = std::fs::read_to_string(&pid_path).map_err(|e| e.to_string())?;
        let pid = pid_str.trim().parse::<u32>().map_err(|e| e.to_string())?;

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

        let pid_str = std::fs::read_to_string(&pid_path).map_err(|e| e.to_string())?;
        let pid = pid_str.trim().parse::<u32>().map_err(|e| e.to_string())?;

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
        use zen_core::config::load_config;
        match load_config() {
            Ok(cfg) => {
                self.push_output("Configuration:".into(), false);
                self.push_output(format!("  LLM default: {:?}", cfg.default_provider), false);
                self.push_output(format!("  Cron: {:?}", cfg.cron.consolidation_time), false);
            },
            Err(e) => self.push_output(format!("Config error: {}", e), true),
        }
    }

    fn execute_consolidate(&mut self) {
        use zen_core::paths::ZenPaths;
        use zen_knowledge::consolidate::ConsolidationPipeline;
        if let Ok(paths) = ZenPaths::detect() {
            match ConsolidationPipeline::new().run(&paths.inbox(), &paths.wiki()) {
                Ok(report) => {
                    self.push_output("Consolidation complete:".into(), false);
                    self.push_output(
                        format!("  Notes processed: {}", report.notes_processed),
                        false,
                    );
                },
                Err(e) => self.push_output(format!("Consolidation error: {}", e), true),
            }
        }
    }

    fn execute_lint(&mut self) {
        use zen_core::paths::ZenPaths;
        use zen_knowledge::maintenance::Linter;
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
                },
                Err(e) => self.push_output(format!("Lint error: {}", e), true),
            }
        }
    }
}

pub fn run_app(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>) -> Result<()> {
    let mut app = App::new();
    app.init_orchestrator();
    app.push_output(
        "Zen Agentic TUI - type /help for commands, /thinking to show thinking, Ctrl+D to exit"
            .into(),
        false,
    );
    app.push_output(format!("Workspace: {}", app.workspace), false);

    loop {
        app.poll_llm_response();
        terminal.draw(|frame| crate::tui::ui::render(frame, &app))?;

        if crossterm::event::poll(std::time::Duration::from_millis(100))?
            && let crossterm::event::Event::Key(key) = crossterm::event::read()?
            && key.kind == crossterm::event::KeyEventKind::Press
        {
            match crate::tui::handler::handle_key(key, &mut app) {
                crate::tui::handler::KeyAction::Submit => {
                    let cmd = app.input.clone();
                    app.push_history(&cmd);
                    app.input.clear();
                    app.cursor_position = 0;
                    app.handle_command(&cmd);
                },
                crate::tui::handler::KeyAction::Quit => {
                    app.running = false;
                },
                crate::tui::handler::KeyAction::Continue => {},
            }
            if !app.running {
                break;
            }
        }
    }
    // Ensure clean terminal state on exit — print newline so shell prompt
    // appears on its own line.
    println!();
    Ok(())
}
