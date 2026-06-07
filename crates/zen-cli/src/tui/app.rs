#![allow(dead_code)]

use anyhow::Result;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::symbols::border;
use ratatui::widgets::{Block, Borders};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Instant;
use tui_textarea::TextArea;
use zen_agents::AgentOrchestrator;
use zen_core::config::load_config;
use zen_core::types::SessionContext;
use zen_provider::DefaultRouter;

use super::cell::{BannerCell, ErrorCell, MarkdownCell, OutputCell, PlainCell};
use super::render::normalize_compact_markdown;
use super::session_picker::SessionPickerState;
use super::slash::{SlashCommandRegistry, SlashState, create_default_registry};
use super::stream::MarkdownStreamCollector;
use super::theme::{OutputTheme, ZenTheme, from_name as theme_from_name};
use zen_memory::conversation::ConversationStore;

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
const MAX_QUEUE_SIZE: usize = 10;
const TOAST_DURATION_SECS: u64 = 3;
const PASTE_MODE_SECS: u64 = 2;

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    #[default]
    Default,
    Paste,
    History,
    Selection,
}

// Full ZENSPACE logo for wide terminals (≥90 cols)
// const SPLASH_LOGO_FULL: &str = r#"
//  ████████  ████████  ███     ██   ████████  ████████   ████████   ████████  ████████
//       ██   ██        ████    ██   ██        ██    ██   ██    ██   ██        ██
//      ██    ██████    ██ ██   ██   ████████  ████████   ████████   ██        ██████
//     ██     ██        ██  ██  ██         ██  ██    ██   ██         ██        ██
//  ████████  ████████  ██   █████   ████████  ██    ██   ██         ████████  ████████
// "#;

// 3D Shadow ZENSPACE logo for wide terminals (≥90 cols)
const SPLASH_LOGO_FULL: &str = r#"
 ███████▒ ███████▒ ███▒   ██▒  ███████▒ ███████▒  ███████▒  ███████▒ ███████▒
   ▒▒▒██▒_██▒▒▒▒▒▒_████▒  ██▒__██▒▒▒▒▒▒_██▒▒▒▒██▒_██▒▒▒▒██▒_██▒▒▒▒▒▒_██▒▒▒▒▒▒
     ██▒  ██████▒  ██▒██▒ ██▒  ███████▒ ███████▒  ███████▒  ██▒      ██████▒
   ██▒    ██▒▒▒▒   ██▒▒██▒██▒  ▒▒▒▒▒██▒ ██▒▒▒▒██▒_██▒▒▒▒▒▒  ██▒      ██▒▒▒▒
 ███████▒ ███████▒ ██▒ ▒████▒  ███████▒ ██▒   ██▒_██▒       ▒██████▒ ███████▒
 ▒▒▒▒▒▒▒  ▒▒▒▒▒▒▒  ▒▒   ▒▒▒▒   ▒▒▒▒▒▒▒  ▒▒    ▒▒  ▒▒         ▒▒▒▒▒▒  ▒▒▒▒▒▒▒
"#;

// Optimized 3D ZENSPACE Logo for Ratatui (Strict 100% Alignment)
const SPLASH_LOGO_3D: &str = r#"
 ███████░ ███████░ ███░   ██░  ███████░ ███████░  ███████░  ███████░ ███████░
    ███░  ██░      ████░  ██░  ██░      ██░   ██░ ██░   ██░ ██░      ██░
   ███░   ██████░  ██░██░ ██░  ███████░ ███████░  ███████░  ██░      ██████░
  ███░    ██░      ██░ ██░██░       ██░ ██░   ██░ ██░       ██░      ██░
 ███████░ ███████░ ██░  ████░  ███████░ ██░   ██░ ██░       ░██████░ ███████░
 ░░░░░░░  ░░░░░░░  ░░    ░░░░   ░░░░░░░  ░░    ░░  ░░         ░░░░░░  ░░░░░░░
"#;

// Front Entity: RGB(43, 160, 152) | Drop Shadow: RGB(6, 106, 143)
const LOGO_ZENSPACE_HYBRID: &str = r#"
 ███████░ ███████░ ███░   ██░  ███████░ ███████░  ███████░  ███████░ ███████░
    ███░  ██░      ████░  ██░  ██░      ██░   ██░ ██░   ██░ ██░      ██░
   ███░   ██████░  ██░██░ ██░  ███████░ ███████░  ███████░  ██░      ██████░
  ███░    ██░      ██░ ██░██░       ██░ ██░   ██░ ██░       ██░      ██░
 ███████░ ███████░ ██░  ████░  ███████░ ██░   ██░ ██░       ░██████░ ███████░
 ░░░░░░░  ░░░░░░░  ░░    ░░░░   ░░░░░░░  ░░    ░░  ░░         ░░░░░░  ░░░░░░░
"#;

// const SPLASH_LOGO_FULL: &str = r#"
//  __________ _   _    _____ _____   ___   _____ _____
//  /___  /  __| \ | |  /  ___| ___ \ / _ \ /  __ \  ___|
//     / /| |__|  \| |  \ `--.| |_/ // /_\ \| /  \/| |__
//    / / |  __| . ` |   `--. \  __/ |  _  || |    |  __|
//  ./ /__| |__| |\  |  /\__/ / |    | | | || \__/\| |____
//  \_____/____\_| \_/  \____/\_|    \_| |_/ \____/\____/
// "#;

// Minimal ZEN for narrow terminals (50-69 cols)
// const SPLASH_LOGO_MINIMAL: &str = r#"
// ███████  ███████  ███    ██
//     ██   ██       ████   ██
//    ██    ███████  ██ ██  ██
//  ██      ██       ██  ██ ██
// ███████  ███████  ██   ████
// "#;

// Minimal ZEN for narrow terminals (50-69 cols)
const SPLASH_LOGO_MINIMAL: &str = r#"
 ███████▒ ███████▒ ███▒   ██▒
   ▒▒▒██▒_██▒▒▒▒▒▒_████▒  ██▒
     ██▒  ██████▒  ██▒██▒ ██▒
   ██▒    ██▒▒▒▒   ██▒▒██▒██▒
 ███████▒ ███████▒ ██▒ ▒████▒
 ▒▒▒▒▒▒▒  ▒▒▒▒▒▒▒  ▒▒   ▒▒▒▒
"#;

const SPLASH_PET: &str = "\
   /\\_/\\\n\
  ( o.o )\n\
  ( >^< )\n\
  /|   |\\\n\
 (_|   |_)\n";

const SPLASH_TAGLINE: &str = "\
  Zen Agentic Workspace\n";

const SPLASH_HELP: &str = "\
  Commands: /help  /exit  /clear  /model <name>\n\
            /note  /search  /session  /config\n\
  Keys:     Enter=send  Ctrl+D=quit  ↑/↓=history\n\
  Type `/` to see command suggestions.\n";

pub const SLASH_COMMANDS: &[&str] = &[
    "help",
    "h",
    "quit",
    "q",
    "exit",
    "clear",
    "cls",
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
    pub input: TextArea<'static>,
    pub output: Vec<OutputCell>,
    pub running: bool,
    pub workspace: String,
    pub session_id: Option<String>,
    pub model: String,
    pub memory_count: usize,
    pub show_thinking: bool,
    pub is_streaming: bool,
    pub chat_history: Vec<(String, String)>,
    pub pending_calls: Vec<PendingCallKind>,
    pub message_queue: VecDeque<String>,
    pub current_query: String,
    pub command_history: Vec<String>,
    pub history_position: Option<usize>,
    pub last_recalled_text: Option<String>,
    orchestrator: Option<Arc<AgentOrchestrator>>,
    session: Option<SessionContext>,
    pub scroll_offset: usize,
    pub auto_scroll: bool,
    pub stream_collector: MarkdownStreamCollector,
    pub theme: Box<dyn OutputTheme>,
    pub slash_state: SlashState,
    pub slash_registry: SlashCommandRegistry,
    pub session_picker: SessionPickerState,
    pub toast_queue: VecDeque<String>,
    pub current_toast: Option<(String, Instant)>,
    pub input_mode: InputMode,
    pub paste_timestamp: Option<Instant>,
    pub selected_cell_idx: usize,
    conversation_store: Option<ConversationStore>,
}

impl App {
    fn input_block() -> Block<'static> {
        Block::default()
            .borders(Borders::ALL)
            .border_set(border::Set {
                vertical_left: ">",
                ..border::PLAIN
            })
            .title(" Input (Enter=send, Ctrl+D=exit) ")
    }

    fn create_input_textarea(text: impl Into<String>) -> TextArea<'static> {
        let mut ta = TextArea::new(vec![text.into()]);
        ta.set_block(Self::input_block());
        ta
    }

    pub fn new() -> Self {
        let workspace = zen_core::paths::ZenPaths::detect()
            .ok()
            .and_then(|paths| paths.workspace_root().map(|p| p.display().to_string()))
            .unwrap_or_else(|| ".".into());
        let mut app = Self {
            input: Self::create_input_textarea(""),
            output: Vec::new(),
            running: true,
            workspace,
            session_id: None,
            model: "mock".to_string(),
            memory_count: 0,
            show_thinking: false,
            is_streaming: false,
            chat_history: Vec::new(),
            pending_calls: Vec::new(),
            message_queue: VecDeque::new(),
            current_query: String::new(),
            command_history: Vec::new(),
            history_position: None,
            last_recalled_text: None,
            orchestrator: None,
            session: None,
            scroll_offset: 0,
            auto_scroll: true,
            stream_collector: MarkdownStreamCollector::new(),
            theme: Box::new(ZenTheme),
            slash_state: SlashState::new(),
            slash_registry: create_default_registry(),
            session_picker: SessionPickerState::new(),
            toast_queue: VecDeque::new(),
            current_toast: None,
            input_mode: InputMode::Default,
            paste_timestamp: None,
            selected_cell_idx: 0,
            conversation_store: ConversationStore::open("tui").ok(),
        };
        app.load_command_history();
        app
    }

    pub fn with_theme(&mut self, name: &str) -> &mut Self {
        self.theme = theme_from_name(name);
        let bg_color = self.theme.bg();
        let bg_style = ratatui::style::Style::default().bg(bg_color);
        self.input.set_style(bg_style);
        self.refresh_input_border();
        self
    }

    pub fn show_toast(&mut self, msg: impl Into<String>) {
        self.toast_queue.push_back(msg.into());
    }

    pub fn get_active_toast(&mut self) -> Option<String> {
        if self.current_toast.is_none() {
            if let Some(msg) = self.toast_queue.pop_front() {
                self.current_toast = Some((msg, Instant::now()));
            }
        }
        
        if let Some((ref msg, timestamp)) = self.current_toast {
            if timestamp.elapsed().as_secs() < TOAST_DURATION_SECS {
                return Some(msg.clone());
            }
            self.current_toast = None;
            if let Some(msg) = self.toast_queue.pop_front() {
                self.current_toast = Some((msg, Instant::now()));
                return Some(self.current_toast.as_ref().unwrap().0.clone());
            }
        }
        None
    }

    pub fn effective_input_mode(&self) -> InputMode {
        match self.input_mode {
            InputMode::Paste => {
                if self.paste_timestamp.map_or(true, |t| t.elapsed().as_secs() >= PASTE_MODE_SECS) {
                    InputMode::Default
                } else {
                    InputMode::Paste
                }
            }
            other => other,
        }
    }

    pub fn refresh_input_border(&mut self) {
        let mode = self.effective_input_mode();
        let cell_info = if mode == InputMode::Selection && !self.output.is_empty() {
            format!(
                " Select: {}/{} · ↑↓/jk nav · y yank · Esc exit ",
                self.selected_cell_idx + 1,
                self.output.len()
            )
        } else {
            String::from(" Select: ↑↓/jk nav · y yank · Esc exit ")
        };
        let (border_char, title) = match mode {
            InputMode::Default => (">", String::from(" Input (Enter=send, Ctrl+D=exit) ")),
            InputMode::Paste => ("|", String::from(" Paste ")),
            InputMode::History => ("←", String::from(" History (↑↓ browse, Enter=load) ")),
            InputMode::Selection => ("▐", cell_info),
        };
        let bg_style = ratatui::style::Style::default().bg(self.theme.bg());
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(border::Set {
                vertical_left: border_char,
                ..border::PLAIN
            })
            .title(title)
            .style(bg_style);
        self.input.set_block(block);
    }

    pub fn enter_selection(&mut self) {
        if self.output.is_empty() {
            self.show_toast("Nothing to select — chat empty");
            return;
        }
        self.input_mode = InputMode::Selection;
        self.selected_cell_idx = self.output.len().saturating_sub(1);
        self.refresh_input_border();
    }

    pub fn exit_selection(&mut self) {
        self.input_mode = InputMode::Default;
        self.refresh_input_border();
    }

    pub fn selection_up(&mut self) {
        if self.selected_cell_idx > 0 {
            self.selected_cell_idx -= 1;
        }
        self.refresh_input_border();
    }

    pub fn selection_down(&mut self) {
        if !self.output.is_empty() && self.selected_cell_idx + 1 < self.output.len() {
            self.selected_cell_idx += 1;
        }
        self.refresh_input_border();
    }

    pub fn yank_selected_cell(&mut self) {
        if let Some(cell) = self.output.get(self.selected_cell_idx) {
            let text = cell.raw_text();
            if text.is_empty() {
                self.show_toast("Cell has no text to copy");
            } else {
                let prefix = text
                    .chars()
                    .take(30)
                    .map(|c| if c == '\n' { '⏎' } else { c })
                    .collect::<String>();
                let suffix = if text.chars().count() > 30 { "…" } else { "" };
                if crate::tui::clipboard::write_text(&text).is_ok() {
                    self.show_toast(format!("✓ Copied: {}{}", prefix, suffix));
                } else {
                    self.show_toast("✗ Clipboard unavailable");
                }
            }
        }
        self.exit_selection();
    }

    fn load_command_history(&mut self) {
        if let Some(store) = &self.conversation_store
            && let Ok(entries) = store.load()
        {
            for (role, content) in entries {
                if role == "user" && !content.is_empty() {
                    self.command_history.push(content);
                }
            }
            if self.command_history.len() > MAX_HISTORY {
                let start = self.command_history.len() - MAX_HISTORY;
                self.command_history = self.command_history[start..].to_vec();
            }
        }
    }

    fn push_splash(&mut self) {
        use crossterm::terminal::size;

        let width = size().map(|(w, _)| w).unwrap_or(80);
        let logo = match width {
            w if w >= 90 => SPLASH_LOGO_FULL,
            w if w >= 70 => SPLASH_LOGO_MINIMAL,
            w if w >= 50 => SPLASH_LOGO_MINIMAL,
            _ => "",
        };

        if !logo.is_empty() {
            let banner = OutputCell::Banner(BannerCell::new(logo, self.theme.as_ref()));
            self.output.push(banner);
        }

        let mut info = String::new();
        info.push_str(SPLASH_PET);
        info.push('\n');
        info.push_str(SPLASH_TAGLINE);
        info.push('\n');
        info.push_str(&format!("  Zen v{}\n", env!("CARGO_PKG_VERSION")));
        info.push('\n');
        info.push_str(SPLASH_HELP);
        self.output.push(OutputCell::Plain(PlainCell::new(info)));
    }

    pub fn init_orchestrator(&mut self) {
        let config = match load_config() {
            Ok(c) => c,
            Err(e) => {
                self.push_output(format!("Config load error (using defaults): {}", e), true);
                return;
            }
        };
        let router = DefaultRouter::from_agentic(&config);
        self.orchestrator = Some(Arc::new(AgentOrchestrator::new(router)));
        self.session = Some(SessionContext::new("default".into(), String::new()));
    }

    pub fn push_output(&mut self, text: String, is_error: bool) {
        if is_error {
            self.output
                .push(OutputCell::Error(ErrorCell::new(text, self.theme.as_ref())));
        } else {
            for line in text.lines() {
                self.output
                    .push(OutputCell::Plain(PlainCell::new(line.to_string())));
            }
        }
        while self.output.len() > 500 {
            self.output.remove(0);
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
        if let Some(store) = &self.conversation_store {
            let _ = store.append("user", cmd);
        }
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
        self.input_mode = InputMode::History;
        if let Some(entry) = self.command_history.get(new_pos) {
            self.last_recalled_text = Some(entry.clone());
            self.input = Self::create_input_textarea(entry.clone());
        }
    }

    pub fn history_down(&mut self) {
        match self.history_position {
            None => {}
            Some(p) if p + 1 >= self.command_history.len() => {
                self.history_position = None;
                self.last_recalled_text = None;
                self.input = Self::create_input_textarea("");
                self.input_mode = InputMode::Default;
            }
            Some(p) => {
                self.history_position = Some(p + 1);
                self.input_mode = InputMode::History;
                if let Some(entry) = self.command_history.get(p + 1) {
                    self.last_recalled_text = Some(entry.clone());
                    self.input = Self::create_input_textarea(entry.clone());
                }
            }
        }
    }

    /// Codex pattern: decide if Up/Down should navigate history or move cursor
    pub fn should_navigate_history(&self) -> bool {
        if self.command_history.is_empty() {
            return false;
        }
        let text = self.input.lines().join("\n");
        // Empty text → always history mode
        if text.is_empty() {
            return true;
        }
        // Cursor must be at line boundary (start or end)
        let cursor = self.input.cursor();
        let at_boundary = cursor == (0, 0) || {
            let lines = self.input.lines();
            let last_line_idx = lines.len().saturating_sub(1);
            let last_line_len = lines[last_line_idx].len();
            cursor == (last_line_idx, last_line_len)
        };
        if !at_boundary {
            return false;
        }
        // Text must match last recalled entry (user hasn't edited)
        self.last_recalled_text.as_deref() == Some(&text)
    }

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
            self.current_query = cmd.to_string();
            self.execute_chat(cmd);
        }
    }

    fn handle_slash_command(&mut self, cmd: &str) {
        let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
        let command_name = self
            .slash_registry
            .get_by_name_or_alias(parts[0])
            .map(|c| c.name.clone())
            .unwrap_or_else(|| parts[0].to_string());

        match command_name.as_str() {
            "exit" => self.running = false,
            "help" => self.show_help(),
            "clear" => {
                self.output.clear();
                self.stream_collector.clear();
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
  /help (/h)             Show this help
  /exit (/q)             Exit TUI
  /clear                 Clear output
  /thinking              Toggle showing thinking process (default: OFF)
  /model <p> [m]         Switch provider [model], show current if omitted
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
  /consolidate           Run consolidation pipeline
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
            self.stream_collector.clear();
        }

        let orchestrator = match &self.orchestrator {
            Some(o) => o.clone(),
            None => {
                let _ = done_tx.send(Err("Orchestrator not initialized".to_string()));
                return;
            }
        };

        let mut session = match &self.session {
            Some(s) => s.clone(),
            None => {
                let _ = done_tx.send(Err("Session not initialized".to_string()));
                return;
            }
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
                                        done_result: Some(Err("disconnected".into())),
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
                                    done_result: Some(result),
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
                                    done_result: Some(Err("disconnected".into())),
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

                let (completed_lines, raw_text) = self.stream_collector.commit_complete_lines();
                if !completed_lines.is_empty() {
                    self.output
                        .push(OutputCell::Markdown(super::cell::MarkdownCell::from_lines(
                            completed_lines,
                            raw_text,
                        )));
                }

                if let Some(done_result) = result.done_result {
                    if let Some(pos) = self.output.iter().position(|cell| {
                        matches!(cell, OutputCell::Plain(p) if p.text.contains("Thinking..."))
                    }) {
                        self.output.remove(pos);
                    }

                    match done_result {
                        Ok(response) => {
                            completed_indices.push(idx);
                            let (remaining, raw_text) = self.stream_collector.finalize_and_drain();
                            if !remaining.is_empty() {
                                self.output.push(OutputCell::Markdown(
                                    super::cell::MarkdownCell::from_lines(remaining, raw_text),
                                ));
                            }
                            if crate::tui::clipboard::write_text(&response).is_ok() {
                                self.show_toast("✓ Copied to clipboard");
                            }
                            self.chat_history.push((_query, response.clone()));
                            if let Some(store) = &self.conversation_store {
                                let _ = store.append("assistant", &response);
                            }
                        }
                        Err(e) => {
                            completed_indices.push(idx);
                            self.stream_collector.clear();
                            self.push_output(format!("[LLM] Error: {}", e), true);
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
            }
            Some(arg) => {
                let parts: Vec<&str> = arg.splitn(2, ' ').collect();
                let provider = parts[0];
                let model = parts.get(1).copied().unwrap_or("default");
                self.set_model(provider, model);
            }
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
        let output_text: Vec<String> = self
            .output
            .iter()
            .map(|cell| match cell {
                OutputCell::Banner(b) => b.text.clone(),
                OutputCell::Markdown(m) => m.content.clone(),
                OutputCell::Code(c) => format!("```{}\n{}\n```", c.lang, c.code),
                OutputCell::Error(e) => e.message.clone(),
                OutputCell::Streaming(s) => s.buffer.clone(),
                OutputCell::Plain(p) => p.text.clone(),
            })
            .collect();
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
            }
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
        use zen_knowledge::note::NoteService;
        match NoteService::new().create_note(content, vec![], "tui") {
            Ok(note) => self.push_output(
                format!("Note created: {} ({})", note.id, note.source),
                false,
            ),
            Err(e) => self.push_output(format!("Note error: {}", e), true),
        }
    }

    fn execute_new_session(&mut self) {
        use zen_memory::session_manager::SessionManager;
        let manager = SessionManager::new();
        match manager.create_session("default", ".") {
            Ok(session) => {
                self.session_id = Some(session.id.clone());
                self.conversation_store = ConversationStore::open(&session.id).ok();
                self.output.clear();
                self.chat_history.clear();
                self.push_output(format!("New session started: {}", session.id), false);
            }
            Err(e) => self.push_output(format!("Session error: {}", e), true),
        }
    }

    fn execute_fork_session(&mut self, title: Option<&str>) {
        use zen_memory::session_manager::SessionManager;
        let current_id = match &self.session_id {
            Some(id) => id.clone(),
            None => {
                self.push_output("No active session to fork".to_string(), true);
                return;
            }
        };

        let manager = SessionManager::new();
        match manager.fork_session(&current_id, title.map(String::from)) {
            Ok(forked) => {
                self.session_id = Some(forked.id.clone());
                if let Some(parent_store) = &self.conversation_store
                    && let Ok(new_store) = parent_store.copy_to(&forked.id)
                {
                    self.conversation_store = Some(new_store);
                }
                if self.conversation_store.is_none() {
                    self.conversation_store = ConversationStore::open(&forked.id).ok();
                }
                self.output.clear();
                self.chat_history.clear();
                self.push_output(
                    format!("Session forked: {} (from {})", forked.id, current_id),
                    false,
                );
            }
            Err(e) => self.push_output(format!("Fork error: {}", e), true),
        }
    }

    fn execute_rename_session(&mut self, title: Option<&str>) {
        use zen_memory::session_manager::SessionManager;
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

    fn execute_archive_session(&mut self) {
        use zen_memory::session_manager::SessionManager;
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
                self.chat_history.clear();
            }
            Err(e) => self.push_output(format!("Archive error: {}", e), true),
        }
    }

    pub fn resume_session(&mut self, session_id: &str) {
        use zen_memory::session_manager::SessionManager;
        let manager = SessionManager::new();
        match manager.resume_session(session_id) {
            Ok(session) => {
                self.session_id = Some(session.id.clone());
                self.conversation_store = ConversationStore::open(&session.id).ok();
                self.output.clear();
                self.chat_history.clear();
                if let Some(store) = &self.conversation_store
                    && let Ok(entries) = store.load()
                {
                    let mut i = 0;
                    while i < entries.len() {
                        if entries[i].0 == "user" {
                            let user_content = entries[i].1.clone();
                            self.push_output(format!("You: {}", user_content), false);
                            if i + 1 < entries.len() && entries[i + 1].0 == "assistant" {
                                let raw_assistant = entries[i + 1].1.clone();
                                let normalized = normalize_compact_markdown(&raw_assistant);
                                self.output.push(OutputCell::Markdown(MarkdownCell::new(
                                    normalized.clone(),
                                )));
                                self.chat_history.push((user_content, normalized));
                                i += 2;
                            } else {
                                i += 1;
                            }
                        } else {
                            i += 1;
                        }
                    }
                }
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

    pub fn archive_session(&mut self, session_id: &str) {
        use zen_memory::session_manager::SessionManager;
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
        use zen_memory::session_manager::SessionManager;
        let manager = SessionManager::new();
        match manager.rename_session(session_id, title.to_string()) {
            Ok(()) => {
                self.push_output(format!("Session renamed to: {}", title), false);
                self.session_picker.load_sessions();
            }
            Err(e) => self.push_output(format!("Rename error: {}", e), true),
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
            }
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
                }
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
                }
                Err(e) => self.push_output(format!("Lint error: {}", e), true),
            }
        }
    }
}

pub fn run_app(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>) -> Result<()> {
    let mut app = App::new();
    if let Ok(config) = load_config()
        && let Some(theme) = config.tui_theme()
    {
        app.with_theme(theme);
    }
    app.push_splash();
    app.init_orchestrator();
    app.push_output(
        "Zen Agentic TUI - type /help for commands, /thinking to show thinking, Ctrl+D to exit"
            .into(),
        false,
    );
    app.push_output(format!("Workspace: {}", app.workspace), false);

    loop {
        app.poll_llm_response();
        app.refresh_input_border();
        let active_toast = app.get_active_toast();
        terminal.draw(|frame| crate::tui::ui::render(frame, &app, active_toast.as_deref()))?;

        if crossterm::event::poll(std::time::Duration::from_millis(100))? {
            match crossterm::event::read()? {
                crossterm::event::Event::Key(key)
                    if key.kind == crossterm::event::KeyEventKind::Press =>
                {
                    match crate::tui::handler::handle_key(key, &mut app) {
                        crate::tui::handler::KeyAction::Submit => {
                            let cmd = app.input.lines().join("\n");
                            let cmd = cmd.trim().to_string();
                            if !cmd.is_empty() {
                                app.push_history(&cmd);
                            }
                            app.input_mode = InputMode::Default;
                            app.paste_timestamp = None;
                            app.input = App::create_input_textarea("");
                            app.handle_command(&cmd);
                        }
                        crate::tui::handler::KeyAction::Quit => {
                            app.running = false;
                        }
                        crate::tui::handler::KeyAction::Continue => {}
                    }
                    if !app.running {
                        break;
                    }
                }
                crossterm::event::Event::Paste(text) => {
                    crate::tui::handler::handle_paste(&text, &mut app);
                }
                _ => {} // Release, Repeat, Mouse, Focus — ignore
            }
        }
    }
    // Ensure clean terminal state on exit — print newline so shell prompt
    // appears on its own line.
    println!();
    Ok(())
}
