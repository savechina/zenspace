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

use std::sync::mpsc;
use std::time::Instant;
use tui_textarea::{Input, Key, TextArea};
use zen_core::types::SessionContext;

use super::cell::{BannerCell, ErrorCell, OutputCell, PlainCell};
use super::history_search::HistorySearch;
use super::model_picker::ModelPickerState;
use super::selection::Selection;
use super::session_picker::SessionPickerState;
use super::slash::{DismissedToken, SlashCommandRegistry, SlashState, create_default_registry};
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
    #[allow(dead_code)] // consumed in poll_llm_response; construction pending future wiring
    SingleShot(PendingLlmCall),
    Streaming(PendingLlmCallStream),
}

const MAX_HISTORY: usize = 100;

/// FR-016: maximum number of scrollback blocks held in the deferred queue
/// (reading mode). Overflow flushes oldest entries first (ADR-001 Option A).
pub(crate) const DEFERRED_QUEUE_CAP: usize = 256;

/// Deterministic markdown payload for the echo test seam (`ZEN_TEST_ECHO_LLM=1`,
/// test-design.md §3 L3). Exercises reasoning (`<think>`), heading, paragraph
/// (committed block), code fence (FR-012 highlight), list, link, and a trailing
/// partial line (viewport tail).
pub(crate) const ECHO_SCRIPT: &str = r##"<think>I should first understand the user's request.</think>
# Echo Heading

A paragraph with **bold** and *italic* inline text.

```rust
fn main() { println!("echo"); }
```

- bullet one
- bullet two

[link](https://example.com) and trailing"##;

pub(crate) const MAX_QUEUE_SIZE: usize = 10;
const TOAST_DURATION_SECS: u64 = 3;
const PASTE_MODE_SECS: u64 = 2;

/// T084(b): large-paste collapse thresholds (FR-020 backlog). A bracketed paste
/// with MORE than this many lines OR chars collapses to a `[Pasted N lines /
/// M chars]` pill; smaller pastes insert verbatim.
pub(crate) const PASTE_COLLAPSE_LINES: usize = 3;
pub(crate) const PASTE_COLLAPSE_CHARS: usize = 200;

/// T084(b): a collapsed large paste. DESIGN (fallback per the task text): the
/// input buffer stores the human-readable pill text verbatim and this side map
/// holds the full text; `expanded_input_text` restores it at submit. Chosen
/// because tui_textarea owns its render path — true render-substitution would
/// mean replacing the textarea widget with a custom paragraph + manual cursor,
/// churning the whole input pipeline; the literal-token design leaves insert /
/// cut / history / search byte-identical, with atomicity enforced at the key
/// layer (Backspace/Delete/Left/Right interception) and expansion at submit.
/// Pairing is positional (nth buffer occurrence ↔ nth vec entry, kept ordered
/// at insert); wholesale input replacements must call `clear_paste_pills`
/// (submit, history recall), and tokens absent from the buffer are ignored at
/// expand. KNOWN LIMIT: cutting a pill across lines and re-pasting it elsewhere
/// can mis-pair duplicate pills — same class as the codex-rs occurrence
/// heuristic, accepted at our scale.
#[derive(Debug, Clone)]
pub struct PastePill {
    pub display: String,
    pub full_text: String,
}
const INPUT_HINT: &str = "Input (Enter=send, Shift+Enter=newline, Ctrl+R=search, Ctrl+D=exit)";

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

    /// BUG-2+L3: Create an InputCell with cursor at the END of the text.
    /// This is the correct builder for history recall and search accept,
    /// where the cursor should be at the end of the recalled text.
    pub fn new_at_end(text: impl Into<String>) -> Self {
        let text: String = text.into();
        let lines: Vec<String> = if text.is_empty() {
            vec![String::new()]
        } else {
            text.split('\n').map(String::from).collect()
        };
        let mut textarea = TextArea::new(lines);
        textarea.set_block(Self::input_block());
        // Move cursor to the end of the text (bottom row, end of line).
        textarea.move_cursor(tui_textarea::CursorMove::Bottom);
        textarea.move_cursor(tui_textarea::CursorMove::End);
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
            .title(format!(" {} ", INPUT_HINT))
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

pub struct App {
    pub input: InputCell,
    pub output: Vec<OutputCell>,
    pub running: bool,
    pub workspace: String,
    pub session_id: Option<String>,
    pub model: String,
    #[allow(dead_code)] // live-seam: memory surface wiring pending
    pub memory_count: usize,
    pub show_thinking: bool,
    pub is_streaming: bool,
    pub chat_history: Vec<(String, String)>,
    pub pending_calls: Vec<PendingCallKind>,
    pub message_queue: VecDeque<String>,
    pub current_query: String,
    pub command_history: Vec<String>,
    pub history_position: Option<usize>,
    pub history_draft: Option<String>,
    pub config: &'static zen_core::config::ZenConfig,
    pub(crate) session: Option<SessionContext>,
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
    pub(crate) conversation_store: Option<ConversationStore>,
    pub(crate) history_store: Option<HistoryStore>,
    pub turn_started_at: Option<Instant>,
    /// T084(b): full text behind each collapsed-paste pill in the input.
    pub paste_pills: Vec<PastePill>,
    /// T084(d): Esc-dismissed slash token (codex-rs `DismissedToken`
    /// mechanics); while the input token equals this, the popup stays shut.
    pub dismissed_slash_token: Option<DismissedToken>,
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
    /// Last memory-nudge poll (FR-040 TUI surface): file IO throttled to
    /// [`NUDGE_POLL_SECS`]; `None` means never polled (first loop polls).
    last_nudge_poll: Option<Instant>,
    output_cache: Option<OutputCache>,
    theme_generation: u64,
    pub loop_panel: crate::tui::loop_panel::LoopPanelState,
    pub history_search: HistorySearch,
    /// FR-023: gateway lifecycle state rendered in the one-row banner slot.
    /// Replaces ad-hoc status_hint strings for gateway state display.
    pub gateway_banner: super::banner::GatewayBannerState,
    /// FR-024: approval popup state. One-at-a-time FIFO, input paused while pending.
    pub approval: super::approval::ApprovalState,
    /// FR-024: channel for sending approval decisions back to the gateway pump.
    /// Uses tokio unbounded sender (sync non-blocking send) — approvals are
    /// low-rate (server-serialized) and the TUI key handler is sync.
    pub approval_tx: Option<
        tokio::sync::mpsc::UnboundedSender<zen_gateway::client::surface::ApprovalResponsePayload>,
    >,
    /// FR-024: channel for receiving approval requests from the gateway pump.
    /// Uses tokio unbounded receiver — drained via try_recv in `inline_tick`
    /// (sync crossterm poll loop). Populated from `prewarm::take_approval_bridge()`.
    pub approval_rx: Option<
        tokio::sync::mpsc::UnboundedReceiver<zen_gateway::client::surface::ApprovalRequestPayload>,
    >,
    /// FR-025: channel for delivering gateway resume events to the TUI main thread.
    pub resume_event_tx: Option<std::sync::mpsc::SyncSender<super::resume::ResumeMessage>>,
    pub resume_event_rx: Option<std::sync::mpsc::Receiver<super::resume::ResumeMessage>>,
    /// T082 (NFR-009): 1-based sequence of streaming turns in this TUI
    /// process; identifies `tui.stream.stall` audit lines (`turn_id`).
    pub(crate) stream_turn_seq: u64,
}

impl App {
    pub(crate) fn create_input_textarea(text: impl Into<String>) -> InputCell {
        InputCell::new(text)
    }

    /// BUG-2+L3: Create an InputCell with cursor at the END of the text.
    /// Use this for history recall and search accept operations.
    pub(crate) fn create_input_textarea_at_end(text: impl Into<String>) -> InputCell {
        InputCell::new_at_end(text)
    }

    pub fn new(config: &'static zen_core::config::ZenConfig) -> Self {
        let workspace = zen_core::paths::ZenPaths::detect()
            .ok()
            .and_then(|paths| paths.workspace_root().map(|p| p.display().to_string()))
            .unwrap_or_else(|| ".".into());
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
            history_draft: None,
            config,
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
            history_store: match HistoryStore::open(config.history.max_bytes.map(|b| b as u64)) {
                Ok(store) => Some(store),
                Err(e) => {
                    // NFR-010: history MUST NOT fall back to CWD. Fail-loud: toast
                    // at startup, run without persistence.
                    tracing::warn!(error = %e, "history store unavailable; running without persistence");
                    None
                }
            },
            turn_started_at: None,
            paste_pills: Vec::new(),
            dismissed_slash_token: None,
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
            history_search: HistorySearch::new(),
            loop_panel: crate::tui::loop_panel::LoopPanelState::default(),
            last_nudge_poll: None,
            stream_turn_seq: 0,
            gateway_banner: super::banner::GatewayBannerState::default(),
            approval: super::approval::ApprovalState::default(),
            approval_tx: None,
            approval_rx: None,
            resume_event_tx: None,
            resume_event_rx: None,
        };
        app.load_command_history();

        // ZEN_TEST_APPROVAL_SEAM: test-only seam injecting a fake approval request
        // at startup (FR-024 acceptance test). Production is unaffected.
        if std::env::var("ZEN_TEST_APPROVAL_SEAM").is_ok() {
            let request = super::approval::ApprovalRequest {
                turn_id: "test-turn-1".to_string(),
                request_id: "test-req-1".to_string(),
                tool_name: "shell.exec".to_string(),
                invocation: serde_json::json!({
                    "binary": "/bin/sh",
                    "args": ["-c", "echo hello"]
                }),
                reason: "test approval request".to_string(),
                received_at: std::time::Instant::now(),
            };
            app.approval.push(request);
        }

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
        let is_stale = self
            .output_cache
            .as_ref()
            .is_none_or(|cache| self.output_cache_is_stale(cache));
        if is_stale {
            self.output_cache = Some(self.build_output_cache());
        }
        &self.output_cache.as_ref().unwrap().lines
    }

    pub fn cell_line_offsets(&mut self) -> &[usize] {
        let is_stale = self
            .output_cache
            .as_ref()
            .is_none_or(|cache| self.output_cache_is_stale(cache));
        if is_stale {
            self.output_cache = Some(self.build_output_cache());
        }
        &self.output_cache.as_ref().unwrap().cell_line_offsets
    }

    pub fn show_toast(&mut self, msg: impl Into<String>) {
        self.toast_queue.push_back(msg.into());
    }

    /// Surface pending memory nudges as toasts (FR-040 TUI surface).
    ///
    /// Reads `logs/memory-nudges.jsonl`, toasts entries newer than the
    /// `logs/.last-nudge-shown` marker (cap 3 per poll), and advances the
    /// marker. File IO runs at most every [`NUDGE_POLL_SECS`]; all failures
    /// are silent no-ops (a missed toast is never an error).
    pub fn poll_memory_nudges(&mut self) {
        let Ok(paths) = zen_core::paths::ZenPaths::detect() else {
            return;
        };
        self.poll_memory_nudges_in(paths.logs().as_path());
    }

    /// Testable core of [`Self::poll_memory_nudges`]: all state lives under
    /// `logs_dir`, so tests pass a tempdir instead of mutating `ZEN_HOME`
    /// (which [`zen_core::paths::user_root`] caches process-wide outside
    /// `cfg(test)` builds — env mutation is order-dependent and flaky).
    pub(crate) fn poll_memory_nudges_in(&mut self, logs_dir: &std::path::Path) {
        const NUDGE_POLL_SECS: u64 = 60;
        const MAX_TOASTS_PER_POLL: usize = 3;
        let now = Instant::now();
        if let Some(last) = self.last_nudge_poll
            && now.duration_since(last).as_secs() < NUDGE_POLL_SECS
        {
            return;
        }
        self.last_nudge_poll = Some(now);
        let Ok(content) = std::fs::read_to_string(logs_dir.join("memory-nudges.jsonl")) else {
            return;
        };
        let marker_path = logs_dir.join(".last-nudge-shown");
        let seen = std::fs::read_to_string(&marker_path).unwrap_or_default();
        let all: Vec<&str> = content.lines().collect();
        let seen_idx = if seen.trim().is_empty() {
            None
        } else {
            all.iter().rposition(|l| l.trim() == seen.trim())
        };
        let fresh: Vec<&str> = all
            .into_iter()
            .skip(seen_idx.map(|i| i + 1).unwrap_or(0))
            .filter(|l| !l.trim().is_empty())
            .collect();
        if fresh.is_empty() {
            return;
        }
        for line in fresh.iter().take(MAX_TOASTS_PER_POLL) {
            let msg = serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .and_then(|v| v.get("text").and_then(|t| t.as_str()).map(str::to_string))
                .unwrap_or_else(|| (*line).to_string());
            self.show_toast(msg);
        }
        if let Some(last) = fresh.last() {
            let _ = std::fs::write(&marker_path, last);
        }
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
            InputMode::Default => (">", format!(" {} ", INPUT_HINT)),
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
        if let Some(store) = &self.history_store
            && let Ok(entries) = store.load_recent(MAX_HISTORY)
        {
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

    /// NFR-010: check if the history store is available (for startup toast).
    pub fn has_history_store(&self) -> bool {
        self.history_store.is_some()
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

    /// FR-016: push a scrollback entry into the deferred queue, flushing
    /// the oldest entries when the cap is reached. Used by ALL defer sites
    /// in inline.rs so overflow handling is centralized.
    pub(crate) fn defer_scrollback(&mut self, entry: ScrollbackEntry) -> VecDeque<ScrollbackEntry> {
        self.deferred_scrollback.push_back(entry);
        let mut overflow = VecDeque::new();
        while self.deferred_scrollback.len() > DEFERRED_QUEUE_CAP {
            if let Some(oldest) = self.deferred_scrollback.pop_front() {
                overflow.push_back(oldest);
            }
        }
        overflow
    }

    pub fn enqueue_scrollback(&mut self, lines: Vec<Line<'static>>) {
        self.scrollback_queue
            .push_back(ScrollbackEntry { lines, wrap: true });
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
        // W1: clear draft and position so next Up starts fresh.
        self.history_position = None;
        self.history_draft = None;
        if self.history_store.is_some() {
            self.persist_history(cmd);
        }
    }

    /// Persist a submitted command to the history file OFF the event loop
    /// (T055/T058: the submit path must not block on file IO — appending
    /// re-reads the whole file for dedup, which is load-sensitive). Uses the
    /// blocking pool when a tokio runtime is present, else writes inline
    /// (headless tests without a runtime context).
    fn persist_history(&self, cmd: &str) {
        let Some(store) = self.history_store.clone() else {
            return;
        };
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

    // === T084(b): paste-collapse pills ===

    /// T084(b): display text for a collapsed paste — the `[Pasted N lines /
    /// M chars]` copy comes verbatim from the task text.
    pub fn paste_pill_display(lines: usize, chars: usize) -> String {
        format!("[Pasted {lines} lines / {chars} chars]")
    }

    /// T084(b): collapse hook for bracketed pastes. Pastes over threshold
    /// collapse to a pill (full text recorded in `paste_pills`); smaller
    /// pastes insert verbatim. Returns the text the caller inserts at cursor.
    pub fn collapse_paste_for_insert(&mut self, pasted: &str) -> String {
        let lines = pasted.lines().count();
        let chars = pasted.chars().count();
        if lines <= PASTE_COLLAPSE_LINES && chars <= PASTE_COLLAPSE_CHARS {
            return pasted.to_string();
        }
        let display = Self::paste_pill_display(lines, chars);
        let pill = PastePill {
            display: display.clone(),
            full_text: pasted.to_string(),
        };
        // Keep vec order == buffer occurrence order (positional pairing at
        // expand/delete): count existing pill spans at or before the cursor.
        let (crow, ccol) = self.input.cursor();
        let idx = self
            .paste_pill_spans()
            .iter()
            .filter(|span| span.0 < crow || (span.0 == crow && span.2 <= ccol))
            .count()
            .min(self.paste_pills.len());
        self.paste_pills.insert(idx, pill);
        display
    }

    /// T084(b): current input with every pill occurrence expanded back to its
    /// full text (positional pairing; stale pills whose token no longer occurs
    /// are skipped). Submit + history paths MUST use this, never the raw
    /// buffer — history.jsonl records the EXPANDED text.
    pub fn expanded_input_text(&self) -> String {
        if self.paste_pills.is_empty() {
            return self.input.lines().join("\n");
        }
        let mut text = self.input.lines().join("\n");
        for pill in &self.paste_pills {
            if let Some(pos) = text.find(pill.display.as_str()) {
                text.replace_range(pos..pos + pill.display.len(), &pill.full_text);
            }
        }
        text
    }

    /// T084(b): drop all pill mappings (wholesale input replacement sites:
    /// submit reset, history recall).
    pub fn clear_paste_pills(&mut self) {
        self.paste_pills.clear();
    }

    /// T084(b): pill token spans as (line, char_start, char_end) in buffer
    /// order. Cursor units are chars on both sides (tui-textarea clamps Jump
    /// by `chars().count()`), so ASCII pill tokens align exactly.
    pub(crate) fn paste_pill_spans(&self) -> Vec<(usize, usize, usize)> {
        let mut spans = Vec::new();
        for (li, line) in self.input.lines().iter().enumerate() {
            let mut occupied: Vec<(usize, usize)> = Vec::new();
            for pill in &self.paste_pills {
                let token = pill.display.as_str();
                if token.is_empty() {
                    continue;
                }
                let mut from = 0;
                while let Some(rel) = line[from..].find(token) {
                    let byte_start = from + rel;
                    let char_start = line[..byte_start].chars().count();
                    let char_end = char_start + token.chars().count();
                    if !occupied
                        .iter()
                        .any(|(a, b)| char_start < *b && *a < char_end)
                    {
                        occupied.push((char_start, char_end));
                        spans.push((li, char_start, char_end));
                    }
                    from = byte_start + token.len().max(1);
                }
            }
        }
        spans.sort();
        spans
    }

    /// T084(b): Backspace with the cursor at a pill's right edge deletes the
    /// whole pill (positional pairing: the k-th occurrence drops the k-th
    /// pill). Returns true when it consumed the key.
    pub fn paste_backspace(&mut self) -> bool {
        let (row, col) = self.input.cursor();
        let spans = self.paste_pill_spans();
        let found = spans
            .iter()
            .enumerate()
            .find(|(_, span)| span.0 == row && span.2 == col);
        let Some((k, _)) = found else {
            return false;
        };
        let (_, cs, ce) = spans[k];
        // Reuse the exact per-char Backspace path (ASCII token: one key per
        // char) so deletion semantics match normal editing; the cursor lands
        // at the pill's start.
        for _ in cs..ce {
            self.input.input(Input {
                key: Key::Backspace,
                ctrl: false,
                alt: false,
                shift: false,
            });
        }
        if k < self.paste_pills.len() {
            self.paste_pills.remove(k);
        }
        true
    }

    /// T084(b): Delete with the cursor at a pill's left edge deletes the whole
    /// pill. Returns true when it consumed the key.
    pub fn paste_delete_forward(&mut self) -> bool {
        let (row, col) = self.input.cursor();
        let spans = self.paste_pill_spans();
        let found = spans
            .iter()
            .enumerate()
            .find(|(_, span)| span.0 == row && span.1 == col);
        let Some((k, _)) = found else {
            return false;
        };
        let (_, cs, ce) = spans[k];
        for _ in cs..ce {
            self.input.input(Input {
                key: Key::Delete,
                ctrl: false,
                alt: false,
                shift: false,
            });
        }
        if k < self.paste_pills.len() {
            self.paste_pills.remove(k);
        }
        true
    }

    /// T084(b): Left/Right skip over a pill as one unit — when the move would
    /// start at (Right) or end inside (Left) a pill span, jump across it.
    /// Returns the Jump target, or None for a normal single-char move.
    pub fn pill_jump_for_horizontal(&self, left: bool) -> Option<(u16, u16)> {
        let (row, col) = self.input.cursor();
        for (li, cs, ce) in self.paste_pill_spans() {
            if li != row {
                continue;
            }
            if left {
                if col > cs && col <= ce {
                    return Some((row as u16, cs as u16));
                }
            } else if col >= cs && col < ce {
                return Some((row as u16, ce as u16));
            }
        }
        None
    }

    // === T084(d): dismissed-token memory ===

    /// T084(d): recompute slash visibility, then enforce dismissed-token
    /// memory: while the input's current command token EQUALS the dismissed
    /// token the popup stays suppressed (pure token equality — never cleared
    /// on edit, so deleting back to the dismissed token keeps it closed, the
    /// codex-rs mechanic). Any differing token (or none) re-arms normally.
    /// Fresh-query sites (submit reset, history recall) clear the dismissal.
    pub fn refresh_slash_popup(&mut self) {
        let input = self.input.lines().join("\n");
        self.slash_state
            .on_input_change(&input, &self.slash_registry);
        if self.slash_state.visible
            && let Some(token) = super::slash::slash_command_token(&input)
            && let Some(dismissed) = &self.dismissed_slash_token
            && dismissed.token == token
        {
            self.slash_state.visible = false;
        }
    }

    /// T084(d): Esc with the popup visible records the current command token
    /// (replacing any previous dismissal) and hides the popup. Esc elsewhere
    /// is untouched — callers keep gating on `slash_state.visible` as before.
    pub fn dismiss_slash_popup(&mut self) {
        if self.slash_state.visible
            && let Some(token) = super::slash::slash_command_token(&self.input.lines().join("\n"))
        {
            self.dismissed_slash_token = Some(DismissedToken { token });
        }
        self.slash_state.dismiss();
    }

    pub fn history_up(&mut self) {
        if self.command_history.is_empty() {
            return;
        }
        // W1: snapshot the user's unsent draft on first Up into history.
        if self.history_position.is_none() {
            // T084(b): stash the EXPANDED text so a pill-bearing draft
            // restores its full content (pills are cleared below with the
            // wholesale input replacement).
            let draft = self.expanded_input_text();
            self.history_draft = Some(draft);
        }
        // T084(b/d): history recall replaces the whole buffer — a fresh query
        // surface, so pill mappings and any dismissal go with the old buffer.
        self.clear_paste_pills();
        self.dismissed_slash_token = None;
        let new_pos = match self.history_position {
            None => self.command_history.len() - 1,
            Some(0) => 0,
            Some(p) => p - 1,
        };
        self.history_position = Some(new_pos);
        self.input.enter_history_mode();
        if let Some(entry) = self.command_history.get(new_pos) {
            // BUG-2+L3: Use new_at_end to position cursor at end of recalled text.
            self.input = Self::create_input_textarea_at_end(entry.clone());
            self.input.enter_history_mode();
        }
    }

    pub fn history_down(&mut self) {
        match self.history_position {
            None => {}
            Some(p) if p + 1 >= self.command_history.len() => {
                // W1: restore the user's draft instead of clearing.
                let draft = self.history_draft.take().unwrap_or_default();
                self.history_position = None;
                // T084(b/d): wholesale replacement — see history_up.
                self.clear_paste_pills();
                self.dismissed_slash_token = None;
                // BUG-2+L3: Use new_at_end to position cursor at end of restored draft.
                self.input = Self::create_input_textarea_at_end(draft);
                self.input.exit_mode();
            }
            Some(p) => {
                self.history_position = Some(p + 1);
                // T084(b/d): wholesale replacement — see history_up.
                self.clear_paste_pills();
                self.dismissed_slash_token = None;
                self.input.enter_history_mode();
                if let Some(entry) = self.command_history.get(p + 1) {
                    // BUG-2+L3: Use new_at_end to position cursor at end of recalled text.
                    self.input = Self::create_input_textarea_at_end(entry.clone());
                    self.input.enter_history_mode();
                }
            }
        }
    }

    /// Codex pattern: decide if Up/Down should navigate history or move cursor
    /// BUG-3: Up navigates history iff cursor is on the first row (row 0).
    /// This follows Codex/bash semantics: Up while composing stashes draft
    /// and recalls history; multi-line editing moves cursor up unless on
    /// first line.
    pub fn should_navigate_history_up(&self) -> bool {
        if self.command_history.is_empty() {
            return false;
        }
        let cursor = self.input.cursor();
        cursor.0 == 0
    }

    /// BUG-3: Down navigates history iff history_position is Some AND cursor
    /// is on the last row. This allows Down-past-end to restore draft.
    pub fn should_navigate_history_down(&self) -> bool {
        if self.history_position.is_none() {
            return false;
        }
        let cursor = self.input.cursor();
        let last_row = self.input.lines().len().saturating_sub(1);
        cursor.0 == last_row
    }
}

pub fn run_app(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    config: &'static zen_core::config::ZenConfig,
) -> Result<()> {
    let mut app = App::new(config);
    // NFR-010: fail-loud — show a visible toast if history persistence is unavailable.
    if app.history_store.is_none() {
        app.show_toast(
            "history unavailable: could not open history file — running without persistence",
        );
    }
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

    // In-app learning scheduler: probe-gated (skips when a daemon
    // already hosts one), learning-core workers only. Auto-cancelled
    // when the TUI exits.
    super::scheduler_gate::spawn(config);
    // T061: pre-warm the gateway link in the background exactly like the
    // inline path, so the first Enter does not pay the cold-start price here.
    super::prewarm::spawn();

    let mut dirty = true;
    loop {
        let prev_streaming = app.is_streaming;
        let prev_output_len = app.output.len();
        app.poll_llm_response();
        if app.is_streaming != prev_streaming || app.output.len() != prev_output_len {
            dirty = true;
        }

        app.refresh_input_border();
        app.poll_memory_nudges();
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
                            // T084(b): submit + history record the EXPANDED
                            // text, never the pill.
                            let cmd = app.expanded_input_text();
                            let cmd = cmd.trim().to_string();
                            if !cmd.is_empty() {
                                app.push_history(&cmd);
                            }
                            app.input.exit_mode();
                            app.input = App::create_input_textarea("");
                            // T084(b/d): fresh query — drop pill mappings and
                            // any dismissal with the old buffer.
                            app.clear_paste_pills();
                            app.dismissed_slash_token = None;
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
mod t084_tests {
    //! T084(b)/(d): paste-collapse pill + dismissed-token memory, unit-level
    //! (App state machines only — no terminal).

    use super::*;

    fn test_app() -> App {
        let config: &'static zen_core::config::ZenConfig = Box::leak(Box::default());
        App::new(config)
    }

    fn big_paste() -> String {
        (1..=5)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn type_char(app: &mut App, c: char) {
        app.input.input(Input {
            key: Key::Char(c),
            ctrl: false,
            alt: false,
            shift: false,
        });
    }

    fn backspace(app: &mut App) {
        app.input.input(Input {
            key: Key::Backspace,
            ctrl: false,
            alt: false,
            shift: false,
        });
    }

    // === T084(b): paste-collapse pills ===

    /// Insertion threshold (FR-020): ≤3 lines AND ≤200 chars insert verbatim;
    /// more lines OR more chars collapse to the `[Pasted N lines / M chars]`
    /// pill (copy verbatim from the task text).
    #[test]
    fn t084_pill_threshold_small_verbatim_large_collapses() {
        let mut app = test_app();
        // Under both thresholds → verbatim, no pill.
        let small = "a\nb\nc";
        assert_eq!(app.collapse_paste_for_insert(small), small);
        assert!(app.paste_pills.is_empty());

        // 5 lines (> 3) → pill.
        let full = big_paste();
        let insert = app.collapse_paste_for_insert(&full);
        assert_eq!(
            insert,
            App::paste_pill_display(5, full.chars().count()),
            "pill display must be `[Pasted N lines / M chars]`"
        );
        assert!(insert.starts_with("[Pasted 5 lines / "));
        assert_eq!(app.paste_pills.len(), 1);

        // Char threshold alone also collapses (1 line, > 200 chars).
        let long_line = "x".repeat(PASTE_COLLAPSE_CHARS + 1);
        let insert = app.collapse_paste_for_insert(&long_line);
        assert_eq!(insert, "[Pasted 1 lines / 201 chars]");
        assert_eq!(app.paste_pills.len(), 2);
    }

    /// Backspace at a pill's right edge deletes the WHOLE pill (restoring
    /// nothing); away from a pill it falls through to normal editing.
    #[test]
    fn t084_pill_backspace_deletes_whole_pill() {
        let mut app = test_app();
        let insert = app.collapse_paste_for_insert(&big_paste());
        app.input.insert_str(&insert);
        assert_eq!(app.input.cursor(), (0, insert.chars().count()));

        assert!(app.paste_backspace(), "backspace at pill edge is consumed");
        assert_eq!(
            app.input.lines().join(
                "
"
            ),
            "",
            "whole pill removed"
        );
        assert!(
            app.paste_pills.is_empty(),
            "pill mapping drops with the pill"
        );

        // No pill involved → normal single-char backspace.
        app.input.insert_str("ab");
        assert!(!app.paste_backspace(), "plain backspace must fall through");
        backspace(&mut app);
        assert_eq!(
            app.input.lines().join(
                "
"
            ),
            "a"
        );
    }

    /// Submit expansion: `expanded_input_text` restores every pill to its
    /// full text, and the history (submit path) records the EXPANDED text —
    /// never the pill (task: "History.jsonl records the EXPANDED text").
    #[test]
    fn t084_pill_expands_at_submit_and_history_stores_expanded_text() {
        let mut app = test_app();
        let full = big_paste();
        app.input.insert_str("before ");
        let insert = app.collapse_paste_for_insert(&full);
        app.input.insert_str(&insert);
        app.input.insert_str(" after");

        // Buffer renders the pill; expansion restores full text.
        assert!(
            app.input
                .lines()
                .join(
                    "
"
                )
                .contains("[Pasted 5 lines /")
        );
        let expanded = app.expanded_input_text();
        assert_eq!(expanded, format!("before {full} after"));

        app.push_history(&app.expanded_input_text());
        assert_eq!(
            app.command_history.last().map(String::as_str),
            Some(expanded.as_str()),
            "history must store the expanded text"
        );
    }

    /// Left/Right skip over a pill as one atomic unit.
    #[test]
    fn t084_pill_horizontal_movement_skips_atomically() {
        let mut app = test_app();
        let insert = app.collapse_paste_for_insert(&big_paste());
        app.input.insert_str(&insert);
        let end = insert.chars().count();
        // Left from the right edge jumps to the pill start.
        assert_eq!(app.pill_jump_for_horizontal(true), Some((0, 0)));
        app.input
            .textarea_mut()
            .move_cursor(tui_textarea::CursorMove::Jump(0, 0));
        // Right from the left edge jumps past the pill end.
        assert_eq!(app.pill_jump_for_horizontal(false), Some((0, end as u16)));
    }

    // === T084(d): dismissed-token memory ===

    /// Full codex-rs lifecycle: Esc→suppressed at the same token; an edit
    /// that changes the token re-arms; deleting back to EXACTLY the
    /// dismissed token keeps it closed; a new Esc replaces the old token.
    #[test]
    fn t084_dismissed_token_lifecycle() {
        let mut app = test_app();
        app.input = App::create_input_textarea_at_end("/mo");
        app.refresh_slash_popup();
        assert!(app.slash_state.visible);

        app.dismiss_slash_popup();
        assert_eq!(
            app.dismissed_slash_token.as_ref().map(|d| d.token.as_str()),
            Some("mo")
        );
        assert!(!app.slash_state.visible);
        app.refresh_slash_popup();
        assert!(!app.slash_state.visible, "same token must stay suppressed");

        // Edit changes the token → popup re-arms.
        type_char(&mut app, 'd');
        app.refresh_slash_popup();
        assert!(app.slash_state.visible, "changed token re-arms the popup");

        // Deleting back to EXACTLY the dismissed token keeps it closed.
        backspace(&mut app);
        app.refresh_slash_popup();
        assert!(
            !app.slash_state.visible,
            "delete-back-to-dismissed-token stays closed (codex mechanic)"
        );

        // A new Esc REPLACES the old token: at /mod the dismissal becomes
        // "mod", so deleting back to "mo" now re-arms (old "mo" is gone).
        type_char(&mut app, 'd');
        app.refresh_slash_popup();
        assert!(app.slash_state.visible);
        app.dismiss_slash_popup();
        assert_eq!(
            app.dismissed_slash_token.as_ref().map(|d| d.token.as_str()),
            Some("mod"),
            "new Esc must replace the previous dismissal"
        );
        backspace(&mut app);
        app.refresh_slash_popup();
        assert!(
            app.slash_state.visible,
            "old dismissal token must be gone after replacement"
        );
    }
}
