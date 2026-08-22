#![allow(dead_code)]

use anyhow::Result;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders};
use std::collections::VecDeque;

pub struct ScrollbackEntry {
    pub lines: Vec<Line<'static>>,
    pub wrap: bool,
}

struct OutputCache {
    lines: Vec<Line<'static>>,
    cell_line_offsets: Vec<usize>,
    show_splash: bool,
    show_thinking: bool,
    theme_generation: u64,
}

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Instant;
use tui_textarea::TextArea;
use zen_agents::AgentOrchestrator;
use zen_core::constants::MEMVID_STORE_FILE;
use zen_core::paths::ZenPaths;
use zen_core::types::SessionContext;
use zen_provider::DefaultRouter;

use super::cell::{BannerCell, ErrorCell, MarkdownCell, OutputCell, PlainCell};
use super::model_picker::ModelPickerState;
use super::render::normalize_compact_markdown;
use super::selection::Selection;
use super::session_picker::SessionPickerState;
use super::slash::{SlashCommandRegistry, SlashState, create_default_registry};
use super::stream::StreamCollector;
use super::theme::{
    OutputTheme, ZenTheme, auto_select as theme_auto_select, from_name as theme_from_name,
    no_color as theme_no_color,
};
use zen_memory::conversation::ConversationStore;
use zen_memory::history::HistoryStore;

pub struct PendingLlmCall {
    pub query: String,
    pub rx: mpsc::Receiver<Result<String, String>>,
}

pub struct PendingLlmCallStream {
    pub query: String,
    pub tokens_rx: mpsc::Receiver<String>,
    pub done_rx: mpsc::Receiver<(
        Result<String, String>,
        Option<zen_core::types::SessionContext>,
    )>,
}

pub enum PendingCallKind {
    SingleShot(PendingLlmCall),
    Streaming(PendingLlmCallStream),
}

const MAX_HISTORY: usize = 100;

/// Deterministic markdown payload for the echo test seam (`ZEN_TEST_ECHO_LLM=1`,
/// test-design.md §3 L3). Exercises reasoning (`<think>`), heading, paragraph
/// (committed block), code fence (FR-012 highlight), list, link, and a trailing
/// partial line (viewport tail).
const ECHO_SCRIPT: &str = r##"<think>I should first understand the user's request.</think>
# Echo Heading

A paragraph with **bold** and *italic* inline text.

```rust
fn main() { println!("echo"); }
```

- bullet one
- bullet two

[link](https://example.com) and trailing"##;

/// Per-directory knowledge-search timeout for interactive chat context
/// injection (T054, input-display-plan.md). On expiry the chat continues
/// without knowledge context instead of blocking the event loop.
const KNOWLEDGE_SEARCH_TIMEOUT_MS: u64 = 1500;

/// Build the agent orchestrator (router + registry + wiring + memvid memory).
///
/// Extracted from `App::init_orchestrator` so background pre-warming can run
/// Build the agent orchestrator with memory wired (read-only mode).
///
/// Uses `with_memory_read_only()` to enable multi-process access:
/// - **Daemon (`zen serve start`)**: Opens with exclusive lock via `with_memory()`
/// - **TUI (`zen`)**: Opens with shared lock via `with_memory_read_only()`
/// - **CLI chat (`zen chat`)**: Opens with shared lock via `with_memory_read_only()`
///
/// This allows the TUI and CLI to coexist with the daemon without blocking.
/// The daemon handles all writes; TUI/CLI are read-only consumers.
///
/// If memvid initialization fails, continues without memory (graceful degradation).
pub(crate) fn build_orchestrator(
    config: &'static zen_core::config::ZenConfig,
) -> AgentOrchestrator {
    let router = DefaultRouter::from_agentic(config);
    let orch = AgentOrchestrator::new(router);

    let orch = match ZenPaths::detect() {
        Ok(paths) => {
            let memvid_dir = paths.memory();
            if let Err(e) = std::fs::create_dir_all(&memvid_dir) {
                tracing::warn!(path = ?memvid_dir, error = %e, "Failed to create memory directory");
            }
            let memvid_path = memvid_dir.join(MEMVID_STORE_FILE);
            match orch.with_memory_read_only(memvid_path) {
                Ok(o) => {
                    tracing::info!("Memvid store wired successfully (read-only)");
                    o
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to initialize memvid store, continuing without memory");
                    let router = DefaultRouter::from_agentic(config);
                    AgentOrchestrator::new(router)
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to detect Zen paths, continuing without memory");
            orch
        }
    };

    if let Ok(policy) = std::env::var("ZEN_ASK_FOR_APPROVAL")
        && !policy.is_empty()
    {
        use zen_core::sandbox::SandboxMode;
        let cb = super::approval_callback::create_approval_callback();
        return orch
            .with_sandbox_mode(SandboxMode::Ask)
            .with_approval_callback(cb);
    }
    orch
}

/// Effective knowledge-search tier for interactive chat (T054).
///
/// In [`KnowledgeSearchMode::Fast`] the heavy tiers 3–5 (embeddings, graph,
/// LLM synthesis) are capped to FTS5 (tier 2) so Enter → LLM dispatch never
/// waits on model loads or synthesis calls.
pub(crate) fn effective_search_tier(
    query: &str,
    mode: zen_core::config::KnowledgeSearchMode,
) -> u8 {
    use zen_core::config::KnowledgeSearchMode;
    let tier = zen_vault::search::TierSelector::select_tier(query);
    match mode {
        KnowledgeSearchMode::Fast => tier.min(2),
        KnowledgeSearchMode::Full | KnowledgeSearchMode::Off => tier,
    }
}

fn format_search_results(results: Vec<zen_vault::search::SearchResult>) -> Vec<String> {
    results
        .into_iter()
        .map(|r| format!("[{}]\n{}", r.file.display(), r.content))
        .collect()
}

/// Knowledge-context collection owning all its resources, so it can run off
/// the event-loop thread (T055 async submit). Mode gating (T054) and the
/// per-directory timeout apply exactly as in the legacy synchronous path.
/// Must be called with a tokio runtime handle available (spawn_blocking).
pub(crate) fn collect_knowledge_context(
    router: &DefaultRouter,
    config: &'static zen_core::config::ZenConfig,
    query: &str,
) -> Vec<String> {
    use zen_core::config::KnowledgeSearchMode;
    use zen_core::paths::ZenPaths;
    use zen_vault::search::SearchService;

    let paths = match ZenPaths::detect() {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };

    // Direct file lookup is cheap and local — always run.
    let mut results = App::direct_file_lookup_in_dirs(&paths, query);

    let mode = config.tui.knowledge_search;
    if mode == KnowledgeSearchMode::Off {
        return format_search_results(results);
    }

    let service = SearchService::new(router.clone());
    let tier = effective_search_tier(query, mode);

    let search_dirs = [paths.inbox(), paths.wiki()];
    let outcome = super::prewarm::with_db_client(move |client| {
        for dir in search_dirs {
            let search = service.search(query, &dir, client, Some(tier), None, None);
            let outcome = tokio::runtime::Handle::current().block_on(tokio::time::timeout(
                std::time::Duration::from_millis(KNOWLEDGE_SEARCH_TIMEOUT_MS),
                search,
            ));
            match outcome {
                Ok(Ok(r)) => results.extend(r),
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, dir = %dir.display(), "knowledge search failed")
                }
                Err(_) => tracing::warn!(
                    dir = %dir.display(),
                    timeout_ms = KNOWLEDGE_SEARCH_TIMEOUT_MS,
                    "knowledge search timed out — continuing without context"
                ),
            }
        }

        let mut seen = std::collections::HashSet::new();
        results.retain(|r| seen.insert(r.file.clone()));
        results.truncate(5);
        format_search_results(results)
    });

    // DB unavailable: fall back to direct file lookup only (cheap, local).
    let formatted = outcome
        .unwrap_or_else(|| format_search_results(App::direct_file_lookup_in_dirs(&paths, query)));

    tracing::info!(
        query_len = query.len(),
        tier,
        results_count = formatted.len(),
        "TUI collect_knowledge_context"
    );

    formatted
}
const MAX_QUEUE_SIZE: usize = 10;
const TOAST_DURATION_SECS: u64 = 3;
const PASTE_MODE_SECS: u64 = 2;

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum InputMode {
    #[default]
    Default,
    Paste,
    History,
    Selection,
    Command,
}

pub struct InputCell {
    textarea: TextArea<'static>,
    mode: InputMode,
    paste_timestamp: Option<Instant>,
    selected_cell_idx: usize,
    just_exited_selection: bool,
}

impl InputCell {
    pub fn new(text: impl Into<String>) -> Self {
        let mut textarea = TextArea::new(vec![text.into()]);
        textarea.set_block(Self::input_block());
        Self {
            textarea,
            mode: InputMode::Default,
            paste_timestamp: None,
            selected_cell_idx: 0,
            just_exited_selection: false,
        }
    }

    fn input_block() -> Block<'static> {
        Block::default()
            .borders(Borders::ALL)
            .border_set(border::Set {
                vertical_left: ">",
                ..border::PLAIN
            })
            .title(" Input (Enter=send, Ctrl+D=exit) ")
    }

    pub fn effective_mode(&self) -> InputMode {
        if self.mode == InputMode::Paste
            && let Some(ts) = self.paste_timestamp
            && ts.elapsed().as_secs() >= PASTE_MODE_SECS
        {
            return InputMode::Default;
        }
        self.mode
    }

    pub fn enter_paste_mode(&mut self) {
        self.mode = InputMode::Paste;
        self.paste_timestamp = Some(Instant::now());
    }

    pub fn enter_history_mode(&mut self) {
        self.mode = InputMode::History;
    }

    pub fn enter_command_mode(&mut self) {
        self.mode = InputMode::Command;
    }

    pub fn exit_command_mode(&mut self) {
        if self.mode == InputMode::Command {
            self.mode = InputMode::Default;
        }
    }

    pub fn enter_selection_mode(&mut self, cell_count: usize) {
        if cell_count > 0 {
            self.mode = InputMode::Selection;
            self.selected_cell_idx = cell_count - 1;
        }
    }

    pub fn exit_selection_mode(&mut self) {
        if self.mode == InputMode::Selection {
            self.mode = InputMode::Default;
        }
    }

    pub fn set_just_exited_selection(&mut self, val: bool) {
        self.just_exited_selection = val;
    }

    pub fn take_just_exited_selection(&mut self) -> bool {
        std::mem::take(&mut self.just_exited_selection)
    }

    pub fn exit_mode(&mut self) {
        self.mode = InputMode::Default;
        self.paste_timestamp = None;
    }

    pub fn selected_cell_idx(&self) -> usize {
        self.selected_cell_idx
    }

    pub fn set_selected_cell_idx(&mut self, idx: usize) {
        self.selected_cell_idx = idx;
    }

    pub fn textarea(&self) -> &TextArea<'static> {
        &self.textarea
    }

    pub fn textarea_mut(&mut self) -> &mut TextArea<'static> {
        &mut self.textarea
    }

    pub fn refresh_border(&mut self) {
        self.textarea.set_block(Self::input_block());
    }

    pub fn set_style(&mut self, style: ratatui::style::Style) {
        self.textarea.set_style(style);
    }
}

impl std::ops::Deref for InputCell {
    type Target = TextArea<'static>;

    fn deref(&self) -> &Self::Target {
        &self.textarea
    }
}

impl std::ops::DerefMut for InputCell {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.textarea
    }
}

// Full ZENSPACE logo for wide terminals (≥90 cols)
// const SPLASH_LOGO_FULL: &str = r#"
//  ████████  ████████  ███     ██   ████████  ████████   ████████   ████████  ████████
//       ██   ██        ████    ██   ██        ██    ██   ██    ██   ██        ██
//      ██    ██████    ██ ██   ██   ████████  ████████   ████████   ██        ██████
//     ██     ██        ██  ██  ██         ██  ██         ██    ██   ██        ██
//  ████████  ████████  ██   █████   ████████  ██         ██    ██   ████████  ████████
// "#;

// 3D Shadow ZENSPACE logo for wide terminals (≥90 cols)
const SPLASH_LOGO_FULL: &str = r#"
 ███████▒ ███████▒ ███▒   ██▒  ███████▒ ███████▒  ███████▒  ███████▒ ███████▒
   ▒▒▒██▒_██▒▒▒▒▒▒_████▒  ██▒__██▒▒▒▒▒▒_██▒▒▒▒██▒_██▒▒▒▒██▒_██▒▒▒▒▒▒_██▒▒▒▒▒▒
     ██▒  ██████▒  ██▒██▒ ██▒  ███████▒ ███████▒  ███████▒  ██▒      ██████▒
   ██▒    ██▒▒▒▒   ██▒▒██▒██▒  ▒▒▒▒▒██▒ ██▒▒▒▒▒▒ _██▒▒▒▒██▒ ██▒      ██▒▒▒▒
 ███████▒ ███████▒ ██▒ ▒████▒  ███████▒ ██▒      _██▒   ██▒ ▒██████▒ ███████▒
 ▒▒▒▒▒▒▒  ▒▒▒▒▒▒▒  ▒▒   ▒▒▒▒   ▒▒▒▒▒▒▒  ▒▒        ▒▒    ▒▒   ▒▒▒▒▒▒  ▒▒▒▒▒▒▒
"#;

// Optimized 3D ZENSPACE Logo for Ratatui (Strict 100% Alignment)
const SPLASH_LOGO_3D: &str = r#"
 ███████░ ███████░ ███░   ██░  ███████░ ███████░  ███████░  ███████░ ███████░
    ███░  ██░      ████░  ██░  ██░      ██░   ██░ ██░   ██░ ██░      ██░
   ███░   ██████░  ██░██░ ██░  ███████░ ███████░  ███████░  ██░      ██████░
  ███░    ██░      ██░ ██░██░       ██░ ██░       ██░   ██░ ██░      ██░
 ███████░ ███████░ ██░  ████░  ███████░ ██░       ██░   ██░ ░██████░ ███████░
 ░░░░░░░  ░░░░░░░  ░░    ░░░░   ░░░░░░░  ░░        ░░    ░░   ░░░░░░  ░░░░░░░
"#;

// Front Notion: RGB(43, 160, 152) | Drop Shadow: RGB(6, 106, 143)
const LOGO_ZENSPACE_HYBRID: &str = r#"
 ███████░ ███████░ ███░   ██░  ███████░ ███████░  ███████░  ███████░ ███████░
    ███░  ██░      ████░  ██░  ██░      ██░   ██░ ██░   ██░ ██░      ██░
   ███░   ██████░  ██░██░ ██░  ███████░ ███████░  ███████░  ██░      ██████░
  ███░    ██░      ██░ ██░██░       ██░ ██░       ██░   ██░ ██░      ██░
 ███████░ ███████░ ██░  ████░  ███████░ ██░       ██░   ██░ ░██████░ ███████░
 ░░░░░░░  ░░░░░░░  ░░    ░░░░   ░░░░░░░  ░░        ░░    ░░   ░░░░░░  ░░░░░░░
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
    "help", "h", "quit", "q", "exit", "clear", "cls", "thinking", "export", "note", "search",
    "session", "serve", "config", "model", "distill", "lint",
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
    "distill",
    "lint",
    "ingest",
    "routine",
    "task",
    "brief",
    "plugin",
];

pub struct App {
    pub input: InputCell,
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
    pub config: &'static zen_core::config::ZenConfig,
    pub router: DefaultRouter,
    orchestrator: Option<Arc<AgentOrchestrator>>,
    session: Option<SessionContext>,
    pub current_variant: Option<String>,
    pub scroll_offset: usize,
    pub auto_scroll: bool,
    pub stream_collector: StreamCollector,
    pub theme: Box<dyn OutputTheme>,
    pub slash_state: SlashState,
    pub slash_registry: SlashCommandRegistry,
    pub session_picker: SessionPickerState,
    pub model_picker: ModelPickerState,
    pub toast_queue: VecDeque<String>,
    pub current_toast: Option<(String, Instant)>,
    conversation_store: Option<ConversationStore>,
    history_store: HistoryStore,
    db_client: Option<zen_repo::SqliteClient>,
    pub turn_started_at: Option<Instant>,
    pub tool_call_count: u32,
    pub current_response_tokens: usize,
    /// Whether the welcome splash banner is still showing. Set to `false` once
    /// the first user or agent message arrives so the large banner does not
    /// permanently consume the chat area.
    pub show_splash: bool,
    /// Transient pre-LLM status shown in the inline footer (T056): set when
    /// the async chat pipeline starts, cleared on the first token / done.
    pub status_hint: Option<String>,
    /// T062: user is reading scrollback (PageUp). While set, committed
    /// blocks are DEFERRED instead of `insert_before`-ed, so the terminal
    /// view does not jump while they read history; the live tail keeps
    /// rendering the newest pending content.
    pub reading_mode: bool,
    /// T062: committed blocks held while `reading_mode` is active; flushed
    /// in order on return-to-bottom.
    pub deferred_scrollback: VecDeque<ScrollbackEntry>,
    /// Last rendered chat area rectangle, used to map mouse coordinates.
    pub chat_area: Option<ratatui::layout::Rect>,
    pub text_selection: Option<Selection>,
    pub scrollback_queue: VecDeque<ScrollbackEntry>,
    /// Single-source-of-truth pending tail. Written only by the event loop's
    /// drain, read non-consuming by the viewport. Prevents duplicate-drain race.
    pub viewport_tail: Vec<Line<'static>>,
    pub inline_mode: bool,
    output_cache: Option<OutputCache>,
    theme_generation: u64,
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

    pub(crate) fn create_input_textarea(text: impl Into<String>) -> InputCell {
        InputCell::new(text)
    }

    pub fn new(config: &'static zen_core::config::ZenConfig) -> Self {
        let workspace = zen_core::paths::ZenPaths::detect()
            .ok()
            .and_then(|paths| paths.workspace_root().map(|p| p.display().to_string()))
            .unwrap_or_else(|| ".".into());
        let router = DefaultRouter::from_agentic(config);
        let mut app = Self {
            input: InputCell::new(""),
            output: Vec::new(),
            running: true,
            workspace,
            session_id: None,
            model: format!(
                "{}/{}",
                config.default_provider.as_deref().unwrap_or("mock"),
                config.default_model.as_deref().unwrap_or("default")
            ),
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
            config,
            router,
            orchestrator: None,
            session: None,
            current_variant: None,
            scroll_offset: 0,
            auto_scroll: true,
            stream_collector: StreamCollector::new(),
            theme: Box::new(ZenTheme),
            slash_state: SlashState::new(),
            slash_registry: create_default_registry(),
            session_picker: SessionPickerState::new(),
            model_picker: ModelPickerState::new(),
            toast_queue: VecDeque::new(),
            current_toast: None,
            conversation_store: None,
            history_store: HistoryStore::open(config.history.max_bytes.map(|b| b as u64))
                .unwrap_or_else(|_| {
                    HistoryStore::with_path(
                        std::path::PathBuf::from("history.jsonl"),
                        Some(1_048_576),
                    )
                }),
            db_client: None,
            turn_started_at: None,
            tool_call_count: 0,
            current_response_tokens: 0,
            show_splash: true,
            status_hint: None,
            reading_mode: false,
            deferred_scrollback: VecDeque::new(),
            chat_area: None,
            text_selection: None,
            scrollback_queue: VecDeque::new(),
            viewport_tail: Vec::new(),
            inline_mode: false,
            output_cache: None,
            theme_generation: 0,
        };
        app.load_command_history();
        app
    }

    pub fn with_theme(&mut self, name: &str) -> &mut Self {
        self.theme = theme_from_name(name);
        self.theme_generation = self.theme_generation.wrapping_add(1);
        self.invalidate_output_cache();
        let bg_color = self.theme.bg();
        let bg_style = ratatui::style::Style::default().bg(bg_color);
        self.input.set_style(bg_style);
        self.refresh_input_border();
        self
    }

    pub fn invalidate_output_cache(&mut self) {
        self.output_cache = None;
    }

    fn ensure_output_cache(&mut self) {
        if self.output_cache.is_none() {
            self.output_cache = Some(self.build_output_cache());
        }
    }

    fn build_output_cache(&self) -> OutputCache {
        let theme = self.theme.as_ref();
        let bg_color = theme.bg();
        let blank_line = Line::styled("", ratatui::style::Style::default().bg(bg_color));
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut cell_line_offsets: Vec<usize> = Vec::with_capacity(self.output.len());

        for cell in &self.output {
            cell_line_offsets.push(lines.len());
            if !self.show_splash && matches!(cell, OutputCell::Banner(_)) {
                continue;
            }
            let cell_lines = cell.display_lines(theme, self.show_thinking);
            if !cell_lines.is_empty() {
                lines.extend(cell_lines.into_owned());
                lines.push(blank_line.clone());
            }
        }

        OutputCache {
            lines,
            cell_line_offsets,
            show_splash: self.show_splash,
            show_thinking: self.show_thinking,
            theme_generation: self.theme_generation,
        }
    }

    fn output_cache_is_stale(&self, cache: &OutputCache) -> bool {
        cache.show_splash != self.show_splash
            || cache.show_thinking != self.show_thinking
            || cache.theme_generation != self.theme_generation
    }

    pub fn all_lines(&mut self) -> &[Line<'static>] {
        let is_stale = self.output_cache.as_ref().is_none_or(|cache| {
            cache.show_splash != self.show_splash
                || cache.show_thinking != self.show_thinking
                || cache.theme_generation != self.theme_generation
        });
        if is_stale {
            self.output_cache = Some(self.build_output_cache());
        }
        &self.output_cache.as_ref().unwrap().lines
    }

    pub fn cell_line_offsets(&mut self) -> &[usize] {
        let is_stale = self.output_cache.as_ref().is_none_or(|cache| {
            cache.show_splash != self.show_splash
                || cache.show_thinking != self.show_thinking
                || cache.theme_generation != self.theme_generation
        });
        if is_stale {
            self.output_cache = Some(self.build_output_cache());
        }
        &self.output_cache.as_ref().unwrap().cell_line_offsets
    }

    pub fn show_toast(&mut self, msg: impl Into<String>) {
        self.toast_queue.push_back(msg.into());
    }

    pub fn get_active_toast(&mut self) -> Option<String> {
        if self.current_toast.is_none()
            && let Some(msg) = self.toast_queue.pop_front()
        {
            self.current_toast = Some((msg, Instant::now()));
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
        self.input.effective_mode()
    }

    pub fn refresh_input_border(&mut self) {
        let mode = self.input.effective_mode();
        let cell_info = if mode == InputMode::Selection && !self.output.is_empty() {
            format!(
                " Select: {}/{} · ↑↓/jk nav · y yank · Esc exit ",
                self.input.selected_cell_idx() + 1,
                self.output.len()
            )
        } else {
            String::from(" Select: ↑↓/jk nav · y yank · Esc exit ")
        };
        let (border_char, title) = match mode {
            InputMode::Default => (
                ">",
                String::from(" Input (Enter=send, Ctrl+D=exit, Ctrl+X=cmd) "),
            ),
            InputMode::Paste => ("|", String::from(" Paste ")),
            InputMode::History => ("←", String::from(" History (↑↓ browse, Enter=load) ")),
            InputMode::Selection => ("▐", cell_info),
            InputMode::Command => (
                "⌘",
                String::from(" Command (v=select · j/k=scroll · Esc=back) "),
            ),
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
        self.input.textarea_mut().set_block(block);
    }

    pub fn enter_selection(&mut self) {
        if self.output.is_empty() {
            self.show_toast("Nothing to select — chat empty");
            return;
        }
        self.input.enter_selection_mode(self.output.len());
        self.refresh_input_border();
    }

    pub fn exit_selection(&mut self) {
        self.input.exit_selection_mode();
        self.refresh_input_border();
    }

    pub fn selection_up(&mut self) {
        let idx = self.input.selected_cell_idx();
        if idx > 0 {
            self.input.set_selected_cell_idx(idx - 1);
        }
        self.refresh_input_border();
    }

    pub fn selection_down(&mut self) {
        let idx = self.input.selected_cell_idx();
        if !self.output.is_empty() && idx + 1 < self.output.len() {
            self.input.set_selected_cell_idx(idx + 1);
        }
        self.refresh_input_border();
    }

    pub fn yank_selected_cell(&mut self) {
        let idx = self.input.selected_cell_idx();
        if let Some(cell) = self.output.get(idx) {
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
        if let Ok(entries) = self.history_store.load_recent(MAX_HISTORY) {
            self.command_history = entries;
        }
    }

    pub(crate) fn push_splash(&mut self) {
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
        self.invalidate_output_cache();
    }

    pub fn is_inline_mode(&self) -> bool {
        self.inline_mode
    }

    /// T062: leave reading mode and move deferred blocks back into the
    /// flush queue (order preserved). `inline_tick` performs the insert.
    pub fn exit_reading_mode(&mut self) {
        self.reading_mode = false;
        if !self.deferred_scrollback.is_empty() {
            self.scrollback_queue
                .extend(self.deferred_scrollback.drain(..));
        }
    }

    pub fn enqueue_scrollback(&mut self, lines: Vec<Line<'static>>) {
        self.scrollback_queue
            .push_back(ScrollbackEntry { lines, wrap: true });
    }

    pub fn enqueue_scrollback_unwrapped(&mut self, lines: Vec<Line<'static>>) {
        self.scrollback_queue
            .push_back(ScrollbackEntry { lines, wrap: false });
    }

    pub fn enqueue_welcome_banner(&mut self) {
        use crossterm::terminal::size;

        let width = size().map(|(w, _)| w).unwrap_or(80);
        let logo = match width {
            w if w >= 90 => SPLASH_LOGO_FULL,
            w if w >= 70 => SPLASH_LOGO_MINIMAL,
            w if w >= 50 => SPLASH_LOGO_MINIMAL,
            _ => "",
        };

        let mut lines: Vec<Line<'static>> = Vec::new();

        if !logo.is_empty() {
            let banner = BannerCell::new(logo, self.theme.as_ref());
            lines.extend(banner.display_lines().iter().cloned());
        }

        let version_str = format!("  Zen v{}", env!("CARGO_PKG_VERSION"));
        let info = format!(
            "{}\n{}\n{}\n{}",
            SPLASH_PET.trim_end(),
            SPLASH_TAGLINE.trim_end(),
            version_str,
            SPLASH_HELP.trim_end()
        );
        for line in info.lines() {
            lines.push(Line::from(Span::styled(
                line.to_string(),
                self.theme.as_ref().text_muted(),
            )));
        }

        if !lines.is_empty() {
            self.scrollback_queue
                .push_back(ScrollbackEntry { lines, wrap: false });
        }
    }

    pub(crate) fn render_user_lines_for_scrollback(&self, text: &str) -> Vec<Line<'static>> {
        let theme = self.theme.as_ref();
        let bg = theme.user_bg();
        let prefix_style = theme.user_prefix();
        let text_style = ratatui::style::Style::default().bg(bg);

        let user_lines: Vec<&str> = text.lines().collect();
        let mut result = Vec::with_capacity(user_lines.len().max(1));

        for (i, line) in user_lines.iter().enumerate() {
            let mut spans: Vec<ratatui::text::Span<'static>> = Vec::new();
            if i == 0 {
                spans.push(ratatui::text::Span::styled(
                    "> ".to_string(),
                    prefix_style.bg(bg),
                ));
            } else {
                spans.push(ratatui::text::Span::styled(
                    "  ".to_string(),
                    ratatui::style::Style::default().bg(bg),
                ));
            }
            spans.push(ratatui::text::Span::styled(line.to_string(), text_style));
            result.push(Line::from(spans));
        }

        if result.is_empty() {
            result.push(Line::from(ratatui::text::Span::styled(
                "> ".to_string(),
                prefix_style.bg(bg),
            )));
        }

        result
    }

    pub fn init_orchestrator(&mut self, config: &'static zen_core::config::ZenConfig) {
        // Prefer the background pre-warmed orchestrator (T053); fall back to
        // synchronous construction so correctness never depends on the race.
        let arc = match super::prewarm::take_orchestrator() {
            Some(a) => a,
            None => Arc::new(build_orchestrator(config)),
        };
        self.orchestrator = Some(arc);
        self.session = Some(SessionContext::new("default".into(), String::new()));
    }

    pub fn push_output(&mut self, text: String, is_error: bool) {
        if self.is_inline_mode() {
            let theme = self.theme.as_ref();
            if is_error {
                let cell = ErrorCell::new(text, theme);
                let lines = cell.display_lines().to_vec();
                self.enqueue_scrollback(lines);
            } else {
                let lines = super::markdown::render_markdown(&text);
                if lines.is_empty() {
                    self.enqueue_scrollback(vec![Line::from("")]);
                } else {
                    self.enqueue_scrollback(lines);
                }
            }
            return;
        }
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
        self.invalidate_output_cache();
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
        self.persist_history(cmd);
    }

    /// Persist a submitted command to the history file OFF the event loop
    /// (T055/T058: the submit path must not block on file IO — appending
    /// re-reads the whole file for dedup, which is load-sensitive). Uses the
    /// blocking pool when a tokio runtime is present, else writes inline
    /// (headless tests without a runtime context).
    fn persist_history(&self, cmd: &str) {
        let store = self.history_store.clone();
        let session_id = self.session_id.clone();
        let cmd = cmd.to_string();
        let append = move || {
            if let Err(e) = store.append(&cmd, session_id.as_deref()) {
                tracing::warn!(error = %e, "failed to append command to history store");
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                // Dropping the JoinHandle detaches the blocking task — the
                // append still runs, the event loop doesn't await it.
                std::mem::drop(handle.spawn_blocking(append));
            }
            Err(_) => append(),
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
        self.input.enter_history_mode();
        if let Some(entry) = self.command_history.get(new_pos) {
            self.last_recalled_text = Some(entry.clone());
            self.input = Self::create_input_textarea(entry.clone());
            self.input.enter_history_mode();
        }
    }

    pub fn history_down(&mut self) {
        match self.history_position {
            None => {}
            Some(p) if p + 1 >= self.command_history.len() => {
                self.history_position = None;
                self.last_recalled_text = None;
                self.input = Self::create_input_textarea("");
                self.input.exit_mode();
            }
            Some(p) => {
                self.history_position = Some(p + 1);
                self.input.enter_history_mode();
                if let Some(entry) = self.command_history.get(p + 1) {
                    self.last_recalled_text = Some(entry.clone());
                    self.input = Self::create_input_textarea(entry.clone());
                    self.input.enter_history_mode();
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

    /// T055/T056: async chat dispatch for inline mode.
    ///
    /// The event loop only pays O(1) work: the pending call + streaming flag
    /// are created here, then the heavy pipeline (orchestrator acquire →
    /// knowledge search → LLM dispatch) runs in background tasks streaming
    /// into the pending call's channels. `poll_llm_response` consumes the
    /// result exactly as before — no consumer changes.
    fn start_async_chat(&mut self, query: &str) {
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

        let config = self.config;
        let router = self.router.clone();
        let orchestrator = self.orchestrator.clone();
        let session = self.session.clone();
        let query_owned = query.to_string();

        tokio::task::spawn(async move {
            // 1. Orchestrator: live instance → pre-warm → background build.
            let orch: Arc<AgentOrchestrator> = match orchestrator {
                Some(o) => o,
                None => match super::prewarm::take_orchestrator() {
                    Some(o) => o,
                    None => match super::prewarm::resolve(config).await {
                        Some(o) => o,
                        None => {
                            let _ = done_tx.send((Err("orchestrator build failed".into()), None));
                            return;
                        }
                    },
                },
            };

            // 2. Knowledge context — bounded, owns its own DB handle.
            let search_router = router.clone();
            let search_query = query_owned.clone();
            let context: Vec<String> = tokio::task::spawn_blocking(move || {
                collect_knowledge_context(&search_router, config, &search_query)
            })
            .await
            .unwrap_or_default();

            // 3. Inject context into the session.
            let mut session =
                session.unwrap_or_else(|| SessionContext::new("default".into(), String::new()));
            for (i, note_content) in context.iter().enumerate() {
                session.knowledge.push(zen_core::types::RetrievedNote {
                    path: format!("auto-search-{i}"),
                    content: note_content.clone(),
                    sensitivity: zen_core::types::Sensitivity::Public,
                    relevance: 1.0 - (i as f64 * 0.1),
                });
            }

            let session_id = session.session_id.to_string();
            tracing::info!(
                session_id,
                query_len = query_owned.len(),
                context_count = context.len(),
                "TUI inline chat: dispatching (async pipeline)"
            );

            // 4. Streaming LLM call.
            tokio::task::spawn_blocking(move || {
                let rt = tokio::runtime::Handle::current();
                let mut session = session;
                let result = rt.block_on(async {
                    orch.execute_stream(&mut session, &query_owned, |token| {
                        if let Err(e) = tokens_tx.send(token.to_string()) {
                            tracing::warn!(error = %e, "token channel closed during stream");
                        }
                    })
                    .await
                    .map_err(|e| e.to_string())
                });
                if let Err(e) = done_tx.send((result, Some(session))) {
                    tracing::warn!(error = %e, "done channel closed before result could be sent");
                }
            });
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
                tracing::warn!("TUI chat: orchestrator not initialized");
                if let Err(e) =
                    done_tx.send((Err("Orchestrator not initialized".to_string()), None))
                {
                    tracing::warn!(error = %e, "failed to send orchestrator error to done channel");
                }
                return;
            }
        };

        let mut session = match &self.session {
            Some(s) => {
                tracing::info!(
                    session_id = %s.session_id,
                    conversation_turns = s.conversation.len(),
                    "start_llm_call_via_orchestrator: cloning session"
                );
                s.clone()
            }
            None => {
                tracing::warn!("TUI chat: session not initialized");
                if let Err(e) = done_tx.send((Err("Session not initialized".to_string()), None)) {
                    tracing::warn!(error = %e, "failed to send session error to done channel");
                }
                return;
            }
        };

        let session_id = session.session_id.to_string();
        let context_count = context.len();

        tracing::info!(
            session_id,
            query_len = query.len(),
            context_count,
            "TUI chat: starting LLM call via orchestrator"
        );

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
                        if let Err(e) = tokens_tx.send(token.to_string()) {
                            tracing::warn!(error = %e, "token channel closed during stream");
                        }
                    })
                    .await
                    .map_err(|e| e.to_string())
            });
            if let Err(e) = done_tx.send((result, Some(session))) {
                tracing::warn!(error = %e, "done channel closed before result could be sent");
            }
        });
    }

    pub fn poll_llm_response(&mut self) {
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
                            self.status_hint = None;
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
                            tracing::warn!(error = %e, "TUI chat: LLM response error");
                            self.status_hint = None;
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
                self.start_async_chat(&queued);
                if !self.pending_calls.is_empty() {
                    break;
                }
            }
        }
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

        let router = DefaultRouter::from_config_override(self.config, provider, model);
        let new_model = format!("{}/{}", provider, model);
        let orchestrator = match ZenPaths::detect() {
            Ok(paths) => {
                let memvid_dir = paths.memory();
                std::fs::create_dir_all(&memvid_dir).ok();
                let memvid_path = memvid_dir.join(MEMVID_STORE_FILE);
                match AgentOrchestrator::new(router).with_memory_read_only(memvid_path) {
                    Ok(o) => o,
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to re-wire memory store after model switch");
                        AgentOrchestrator::new(DefaultRouter::from_config_override(
                            self.config,
                            provider,
                            model,
                        ))
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to detect Zen paths after model switch");
                AgentOrchestrator::new(router)
            }
        };
        let orchestrator = Arc::new(orchestrator);

        self.orchestrator = Some(orchestrator);
        self.model = new_model.clone();

        self.push_output(format!("Model switched to: {}", new_model), false);

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
        use zen_core::paths::ZenPaths;
        use zen_vault::search::{SearchService, TierSelector};

        let tier = TierSelector::select_tier(query);
        let paths = match ZenPaths::detect() {
            Ok(p) => p,
            Err(e) => {
                self.push_output(format!("Path error: {}", e), true);
                return;
            }
        };
        let base_dir = paths.inbox();

        if self.db_client.is_none() {
            let db_path = paths.data().join("state.db");
            self.db_client = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(zen_repo::SqliteClient::open_lazy(&db_path))
            })
            .ok();
        }
        let client = match self.db_client.as_ref() {
            Some(c) => c,
            None => {
                self.push_output("Database error: failed to open database".into(), true);
                return;
            }
        };

        match tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(
                SearchService::new(self.router.clone()).search(
                    query,
                    &base_dir,
                    client,
                    Some(tier),
                    None,
                    None,
                ),
            )
        }) {
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

    /// Auto-create a session on first chat message if none exists.
    /// Sets the session title from the first user message.
    pub(crate) fn ensure_session(&mut self, first_message: &str) {
        if self.session_id.is_some() {
            return;
        }

        use zen_memory::session::SessionManager;
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
            && let Ok(mut notion) = zen_core::types::SessionRecord::load(id)
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
    fn execute_new_session(&mut self) {
        use zen_memory::session::SessionManager;
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

    fn execute_fork_session(&mut self, title: Option<&str>) {
        use zen_memory::session::SessionManager;
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

    fn execute_rename_session(&mut self, title: Option<&str>) {
        use zen_memory::session::SessionManager;
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
        use zen_memory::session::SessionManager;
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
        use zen_memory::session::SessionManager;
        self.save_session_state();
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
                        if entries[i].0 == "user" {
                            let user_content = entries[i].1.clone();
                            self.push_output(format!("You: {}", user_content), false);
                            session_ctx.add_turn("user", &user_content);

                            if i + 1 < entries.len() && entries[i + 1].0 == "assistant" {
                                let raw_assistant = entries[i + 1].1.clone();
                                let normalized = normalize_compact_markdown(&raw_assistant);
                                self.output.push(OutputCell::Markdown(MarkdownCell::new(
                                    normalized.clone(),
                                )));
                                self.invalidate_output_cache();
                                self.chat_history
                                    .push((user_content.clone(), normalized.clone()));
                                session_ctx.add_turn("assistant", &normalized);

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

    pub fn archive_session(&mut self, session_id: &str) {
        use zen_memory::session::SessionManager;
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
        use zen_memory::session::SessionManager;
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
            match DistillationPipeline::new().run(&paths.inbox(), &paths.wiki()) {
                Ok(report) => {
                    self.push_output("Distillation complete:".into(), false);
                    self.push_output(
                        format!("  Notes processed: {}", report.notes_processed),
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

pub fn run_app(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    config: &'static zen_core::config::ZenConfig,
) -> Result<()> {
    let mut app = App::new(config);
    if std::env::var("NO_COLOR").is_ok() {
        app.theme = theme_no_color();
    } else if let Some(theme) = config.tui_theme() {
        app.with_theme(theme);
    } else {
        app.theme = theme_auto_select();
    }
    app.push_splash();
    app.push_output(
        "Zen Agentic TUI - type /help for commands, /thinking to show thinking, Ctrl+D to exit"
            .into(),
        false,
    );
    app.push_output(format!("Workspace: {}", app.workspace), false);

    // Spawn the background scheduler (auto-cancelled when TUI exits).
    let scheduler = zen_agents::scheduler::create_configured_scheduler(&config.cron);
    tokio::spawn(async move {
        scheduler.run().await;
    });
    // T061: pre-warm orchestrator/DB in the background exactly like the
    // inline path, so the first Enter does not pay the cold-start price here.
    super::prewarm::spawn(config);

    let mut dirty = true;
    loop {
        let prev_streaming = app.is_streaming;
        let prev_output_len = app.output.len();
        app.poll_llm_response();
        if app.is_streaming != prev_streaming || app.output.len() != prev_output_len {
            dirty = true;
        }

        app.refresh_input_border();
        let active_toast = app.get_active_toast();
        if dirty {
            terminal
                .draw(|frame| crate::tui::ui::render(frame, &mut app, active_toast.as_deref()))?;
            dirty = false;
        }

        if crossterm::event::poll(std::time::Duration::from_millis(30))? {
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
                            app.input.exit_mode();
                            app.input = App::create_input_textarea("");
                            app.auto_scroll = true;
                            app.handle_command(&cmd);
                        }
                        crate::tui::handler::KeyAction::Quit => {
                            app.save_session_state();
                            app.running = false;
                        }
                        crate::tui::handler::KeyAction::Continue => {}
                    }
                    dirty = true;
                    if !app.running {
                        break;
                    }
                }
                crossterm::event::Event::Paste(text) => {
                    crate::tui::handler::handle_paste(&text, &mut app);
                    dirty = true;
                }
                crossterm::event::Event::Mouse(mouse) => {
                    crate::tui::handler::handle_mouse(mouse, &mut app);
                    dirty = true;
                }
                crossterm::event::Event::Resize(_, _) => {
                    // FR-014: keep ratatui's buffer synced to the live terminal
                    // size. The inline path re-anchors its viewport on resize
                    // (inline.rs); the alternate-screen path must at least
                    // autoresize, or the stale buffer desyncs from the terminal
                    // and later keystrokes render to the wrong place (the e9
                    // "post-resize-input" flake).
                    terminal.autoresize()?;
                    dirty = true;
                }
                _ => {}
            }
        }
    }
    // Ensure clean terminal state on exit — print newline so shell prompt
    // appears on its own line.
    println!();
    Ok(())
}

#[cfg(test)]
mod search_tier_tests {
    use super::effective_search_tier;
    use zen_core::config::KnowledgeSearchMode;

    #[test]
    fn fast_mode_caps_heavy_tiers_to_fts() {
        for q in ["similar: rust", "graph: rust", "summarize: rust"] {
            assert_eq!(
                effective_search_tier(q, KnowledgeSearchMode::Fast),
                2,
                "query: {q}"
            );
        }
    }

    #[test]
    fn fast_mode_keeps_light_tiers() {
        assert_eq!(effective_search_tier("hello", KnowledgeSearchMode::Fast), 1);
        assert_eq!(
            effective_search_tier("hello world", KnowledgeSearchMode::Fast),
            2
        );
    }

    #[test]
    fn full_mode_keeps_selected_tier() {
        assert_eq!(
            effective_search_tier("similar: rust", KnowledgeSearchMode::Full),
            3
        );
        assert_eq!(
            effective_search_tier("graph: rust", KnowledgeSearchMode::Full),
            4
        );
    }
}
