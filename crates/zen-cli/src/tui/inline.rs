use std::io;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use ratatui::Terminal;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::{Position, Rect};
use ratatui::style::Modifier;
use ratatui::{TerminalOptions, Viewport};

use super::app::App;
use super::inline_handler::InlineKeyAction;
use super::theme::auto_select as theme_auto_select;
use super::theme::no_color as theme_no_color;

/// Height (rows) of the bottom inline viewport. Kept fixed; the tail region
/// inside it is dynamic (2–4 rows, see `inline_ui::dynamic_tail_height`).
pub(crate) const INLINE_VIEWPORT_ROWS: u16 = 8;

const STREAMING_RENDER_INTERVAL_MS: u128 = 33;
const POLL_INTERVAL_ACTIVE_MS: u64 = 16;
const POLL_INTERVAL_IDLE_MS: u64 = 50;

pub fn run_inline(config: &'static zen_core::config::ZenConfig) -> Result<()> {
    if let Ok(paths) = zen_core::paths::ZenPaths::detect() {
        let _ = paths.ensure_identity_files();
        let _ = paths.ensure_runtime_dirs();
    }

    let app = prepare_inline_app(config);

    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnableBracketedPaste,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
        ),
    )?;

    let result = (|| {
        // Anchor the inline viewport to the bottom of the screen instead of the
        // current cursor row; otherwise rows below the viewport stay blank.
        if let Ok((_, rows)) = crossterm::terminal::size() {
            execute!(
                io::stdout(),
                crossterm::cursor::MoveTo(0, rows.saturating_sub(1)),
            )?;
        }
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(INLINE_VIEWPORT_ROWS),
            },
        )?;
        run_inline_session(&mut terminal, app)
    })();

    execute!(
        io::stdout(),
        DisableBracketedPaste,
        PopKeyboardEnhancementFlags,
    )?;
    crossterm::terminal::disable_raw_mode()?;
    result
}

/// Build the inline-mode `App`: theme selection, welcome banner, intro lines.
///
/// Extracted from `run_inline` so headless tests can drive the exact same
/// startup sequence against `TestBackend` (test-design.md Layer 2, S1/S6).
pub(crate) fn prepare_inline_app(config: &'static zen_core::config::ZenConfig) -> App {
    let mut app = App::new(config);
    app.inline_mode = true;
    apply_inline_theme(&mut app, config, std::env::var("NO_COLOR").is_ok());
    app.enqueue_welcome_banner();
    app.push_output(
        "Zen REPL — type a message or /help for commands, Ctrl+D to exit".into(),
        false,
    );
    app.push_output(format!("Workspace: {}", app.workspace), false);
    app
}

/// Inline-mode theme selection. `no_color` forces the plain `NO_COLOR` theme
/// (FR-011); otherwise the configured theme, else terminal auto-detection.
pub(crate) fn apply_inline_theme(
    app: &mut App,
    config: &zen_core::config::ZenConfig,
    no_color: bool,
) {
    if no_color {
        app.theme = theme_no_color();
    } else if let Some(theme) = config.tui_theme() {
        app.with_theme(theme);
    } else {
        app.theme = theme_auto_select();
    }
}

/// Mutable state carried across inline event-loop iterations.
pub(crate) struct InlineLoopState {
    pub dirty: bool,
    pub last_streaming_render: Instant,
}

impl InlineLoopState {
    pub fn new() -> Self {
        Self {
            dirty: true,
            last_streaming_render: Instant::now(),
        }
    }
}

/// Re-anchor the inline viewport to the bottom of a resized screen (H4 fix,
/// analyze-report.md).
///
/// ratatui's `Terminal::resize` recomputes an inline viewport relative to the
/// live backend cursor position, preserving the cursor's row offset inside the
/// viewport (ratatui-core `compute_inline_size`). Without intervention the
/// viewport keeps its old absolute row after the terminal grows, floating
/// mid-screen with a blank band below (Codex issue #16134 class).
///
/// Sequence: (1) record the cursor on the last row of the current viewport so
/// the preserved offset is `viewport_height - 1`; (2) move the backend cursor
/// (bypassing ratatui's tracking) to the bottom row of the resized screen;
/// (3) `Terminal::resize` then recomputes the viewport bottom-anchored.
pub(crate) fn reanchor_viewport<B: Backend>(
    terminal: &mut Terminal<B>,
    width: u16,
    height: u16,
) -> io::Result<()> {
    let viewport = terminal.get_frame().area();
    terminal
        .set_cursor_position(Position::new(0, viewport.bottom().saturating_sub(1)))
        .map_err(|e| io::Error::other(e.to_string()))?;
    terminal
        .backend_mut()
        .set_cursor_position(Position::new(0, height.saturating_sub(1)))
        .map_err(|e| io::Error::other(e.to_string()))?;
    terminal
        .resize(Rect::new(0, 0, width, height))
        .map_err(|e| io::Error::other(e.to_string()))?;
    Ok(())
}

/// One iteration of the inline event loop: advance streaming state, flush
/// committed blocks into native scrollback, process one terminal event (if
/// any), and redraw the bottom viewport. Returns `true` when the session
/// should exit.
///
/// Decoupled from crossterm's blocking `poll`/`read` and generic over the
/// backend so scripted headless sessions can drive it with `TestBackend`
/// (test-design.md Layer 2).
pub(crate) fn inline_tick<B: Backend>(
    app: &mut App,
    terminal: &mut Terminal<B>,
    state: &mut InlineLoopState,
    event: Option<Event>,
) -> Result<bool> {
    let prev_streaming = app.is_streaming;
    let prev_buffer_len = app.stream_collector.buffer().len();

    app.poll_llm_response();

    let tokens_pushed = app.stream_collector.buffer().len() > prev_buffer_len;
    let response_just_completed = !app.is_streaming && prev_streaming;

    if tokens_pushed {
        let reasoning_style = app
            .theme
            .as_ref()
            .text_muted()
            .add_modifier(Modifier::ITALIC);
        let (committed, pending) = app
            .stream_collector
            .drain_and_tail_filtered(reasoning_style, app.show_thinking);
        if !committed.is_empty() {
            app.enqueue_scrollback(committed);
        }
        app.viewport_tail = pending;

        let now = Instant::now();
        if now.duration_since(state.last_streaming_render).as_millis()
            >= STREAMING_RENDER_INTERVAL_MS
        {
            state.dirty = true;
            state.last_streaming_render = now;
        }
    }

    if response_just_completed {
        let reasoning_style = app
            .theme
            .as_ref()
            .text_muted()
            .add_modifier(Modifier::ITALIC);
        let (committed, pending) = app
            .stream_collector
            .drain_and_tail_filtered(reasoning_style, app.show_thinking);
        if !committed.is_empty() {
            app.enqueue_scrollback(committed);
        }
        if !pending.is_empty() {
            app.enqueue_scrollback(pending);
        }
        app.stream_collector.clear();
        app.viewport_tail.clear();
        state.dirty = true;
    }

    if let Some(ev) = event {
        match ev {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                match super::inline_handler::handle_key(key, app) {
                    InlineKeyAction::Submit => {
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
                    InlineKeyAction::Quit => {
                        app.save_session_state();
                        app.running = false;
                    }
                    InlineKeyAction::Continue => {}
                }
                state.dirty = true;
                if !app.running {
                    return Ok(true);
                }
            }
            Event::Resize(width, height) => {
                reanchor_viewport(terminal, width, height)?;
                state.dirty = true;
            }
            // H1 fix (analyze-report.md): bracketed paste is ENABLED at
            // startup but was previously swallowed by the catch-all, so
            // pasting into the inline composer silently did nothing.
            // Route it through the same handler the full-screen path uses.
            Event::Paste(text) => {
                super::handler::handle_paste(&text, app);
                state.dirty = true;
            }
            _ => {}
        }
    }

    if !app.scrollback_queue.is_empty() {
        if app.reading_mode {
            // T062: the user is reading scrollback — inserting now would jump
            // the view. Hold committed blocks in order; the live tail still
            // shows the newest content.
            app.deferred_scrollback
                .extend(app.scrollback_queue.drain(..));
        } else {
            super::scrollback_inserter::insert_scrollback_queue(
                terminal,
                &mut app.scrollback_queue,
            )?;
        }
        state.dirty = true;
    }

    // T070: toast lifecycle is tick-driven — keep redrawing while a toast is
    // active/pending so it appears immediately and expires on time instead
    // of waiting for an unrelated dirty tick.
    if !app.toast_queue.is_empty() || app.current_toast.is_some() {
        state.dirty = true;
    }

    if state.dirty {
        terminal
            .draw(|frame| super::inline_ui::render(frame, app))
            .map_err(|e| io::Error::other(e.to_string()))?;
        state.dirty = false;
    }

    Ok(false)
}

/// Production session runner: initial scrollback flush, then the
/// crossterm poll/read loop around `inline_tick`, then the exit flush.
fn run_inline_session(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    mut app: App,
) -> Result<()> {
    let config = app.config;

    let scheduler = zen_agents::scheduler::create_configured_scheduler(&config.cron);
    tokio::spawn(async move {
        scheduler.run().await;
    });

    // T053: pre-warm orchestrator + knowledge DB in the background so the
    // first Enter does not pay the ~10s cold-start price on this thread.
    super::prewarm::spawn(config);

    super::scrollback_inserter::insert_scrollback_queue(terminal, &mut app.scrollback_queue)?;

    let mut state = InlineLoopState::new();
    loop {
        let poll_ms = if app.is_streaming {
            POLL_INTERVAL_ACTIVE_MS
        } else {
            POLL_INTERVAL_IDLE_MS
        };
        let event = if crossterm::event::poll(Duration::from_millis(poll_ms))? {
            Some(crossterm::event::read()?)
        } else {
            None
        };
        if inline_tick(&mut app, terminal, &mut state, event)? {
            break;
        }
    }

    // Reading-mode exit flush: deferred blocks (T062) must reach scrollback
    // before teardown, otherwise Ctrl+D while reading silently drops them.
    app.exit_reading_mode();
    if !app.scrollback_queue.is_empty() {
        let _ = super::scrollback_inserter::insert_scrollback_queue(
            terminal,
            &mut app.scrollback_queue,
        );
    }
    println!();
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Layer-2 headless session tests (docs/specs/002-agentic-tui/test-design.md).
    //!
    //! These drive the real inline startup + event-loop code paths
    //! (`prepare_inline_app` / `inline_tick`) against `TestBackend` with
    //! scripted token channels and key events — no LLM, no real terminal.

    use super::*;
    use crate::tui::app::{PendingCallKind, PendingLlmCallStream};
    use crate::tui::scrollback_inserter::insert_scrollback_queue;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use std::sync::mpsc;

    type TokenSender = mpsc::Sender<String>;
    type DoneSender = mpsc::Sender<(
        Result<String, String>,
        Option<zen_core::types::SessionContext>,
    )>;

    fn test_config() -> &'static zen_core::config::ZenConfig {
        Box::leak(Box::default())
    }

    /// Mimic zen's inline startup: cursor anchored to the bottom row, then an
    /// Inline viewport created from there.
    fn anchored_terminal(width: u16, height: u16) -> Terminal<TestBackend> {
        let mut backend = TestBackend::new(width, height);
        backend
            .set_cursor_position(Position::new(0, height - 1))
            .expect("cursor position");
        Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(INLINE_VIEWPORT_ROWS),
            },
        )
        .expect("terminal")
    }

    fn row_text(buf: &Buffer, y: u16) -> String {
        let mut s = String::new();
        for x in 0..buf.area.width {
            s.push_str(buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "));
        }
        s.trim_end().to_string()
    }

    /// Scrollback + visible screen, top to bottom, blanks preserved.
    fn raw_history(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let sb = terminal.backend().scrollback();
        let mut all: Vec<String> = (0..sb.area.height).map(|y| row_text(sb, y)).collect();
        let buf = terminal.backend().buffer();
        all.extend((0..buf.area.height).map(|y| row_text(buf, y)));
        all
    }

    fn history(terminal: &Terminal<TestBackend>) -> Vec<String> {
        raw_history(terminal)
            .into_iter()
            .filter(|r| !r.is_empty())
            .collect()
    }

    /// Rows committed to native scrollback: the backend scrollback buffer
    /// plus visible rows ABOVE the viewport (excludes the viewport itself,
    /// where the live tail/input render).
    fn flushed_rows(terminal: &mut Terminal<TestBackend>) -> Vec<String> {
        let vp_top = terminal.get_frame().area().top();
        let sb = terminal.backend().scrollback();
        let mut all: Vec<String> = (0..sb.area.height).map(|y| row_text(sb, y)).collect();
        let buf = terminal.backend().buffer();
        let limit = vp_top.min(buf.area.height);
        all.extend((0..limit).map(|y| row_text(buf, y)));
        all.into_iter().filter(|r| !r.is_empty()).collect()
    }

    fn history_contains(terminal: &Terminal<TestBackend>, needle: &str) -> bool {
        history(terminal).iter().any(|r| r.contains(needle))
    }

    /// Force the streaming render throttle open so the tick under test draws.
    fn unthrottle(state: &mut InlineLoopState) {
        state.last_streaming_render =
            Instant::now() - Duration::from_millis(STREAMING_RENDER_INTERVAL_MS as u64 + 10);
    }

    /// Scripted streaming call: returns the senders plus an app holding the
    /// pending call, ready for `inline_tick`.
    fn scripted_stream(query: &str) -> (App, TokenSender, DoneSender) {
        let mut app = prepare_inline_app(test_config());
        app.scrollback_queue.clear(); // isolate streaming content from banner
        let (tokens_tx, tokens_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        app.is_streaming = true;
        app.turn_started_at = Some(Instant::now());
        app.pending_calls
            .push(PendingCallKind::Streaming(PendingLlmCallStream {
                query: query.to_string(),
                tokens_rx,
                done_rx,
            }));
        (app, tokens_tx, done_tx)
    }

    /// S1 (SC-001, FR-004): startup puts banner + intro + workspace into
    /// scrollback, and the viewport is bottom-anchored.
    #[test]
    fn s1_startup_scrollback_and_bottom_anchor() {
        let mut app = prepare_inline_app(test_config());
        let mut terminal = anchored_terminal(80, 24);

        // Viewport must hug the bottom of the screen.
        let area = terminal.get_frame().area();
        assert_eq!(area.bottom(), 24, "viewport must be bottom-anchored");

        insert_scrollback_queue(&mut terminal, &mut app.scrollback_queue).expect("flush");

        assert!(
            history_contains(&terminal, "Zen REPL"),
            "intro line missing"
        );
        assert!(
            history_contains(&terminal, "Workspace:"),
            "workspace hint missing"
        );
        // The banner (SPLASH_LOGO_MINIMAL at the 80-col fallback) produced
        // scrollback rows above the intro.
        let hist = history(&terminal);
        let intro_pos = hist.iter().position(|r| r.contains("Zen REPL")).unwrap();
        assert!(intro_pos > 0, "banner rows must precede the intro line");
    }

    /// S2 (SC-006, FR-001): submitting `/help` through the real key path
    /// routes the command output into native scrollback — no LLM involved.
    #[test]
    fn s2_slash_help_lands_in_scrollback() {
        let mut app = prepare_inline_app(test_config());
        app.scrollback_queue.clear();
        let mut terminal = anchored_terminal(80, 24);
        let mut state = InlineLoopState::new();

        app.input = App::create_input_textarea("/help");
        let exit = inline_tick(
            &mut app,
            &mut terminal,
            &mut state,
            Some(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))),
        )
        .expect("tick");
        assert!(!exit);

        assert!(
            history_contains(&terminal, "/help"),
            "help output must reach scrollback"
        );
        assert!(
            app.scrollback_queue.is_empty(),
            "queue must drain into scrollback"
        );
    }

    /// S2b (FR-001): the user-message echo uses the `> ` prefix and reaches
    /// scrollback (the exact routing `handle_command` performs for plain text
    /// in inline mode, minus the LLM dispatch).
    #[test]
    fn s2b_user_message_echo_lands_in_scrollback() {
        let mut app = prepare_inline_app(test_config());
        app.scrollback_queue.clear();
        let mut terminal = anchored_terminal(80, 24);

        let echo = app.render_user_lines_for_scrollback("hello world");
        app.enqueue_scrollback(echo);
        insert_scrollback_queue(&mut terminal, &mut app.scrollback_queue).expect("flush");

        assert!(history_contains(&terminal, "> hello world"));
    }

    /// S3 (SC-004, FR-005): while streaming, completed blocks drain into
    /// scrollback and the partial block renders as the viewport tail.
    #[test]
    fn s3_streaming_splits_committed_and_tail() {
        let (mut app, tokens_tx, _done_tx) = scripted_stream("hello");
        let mut terminal = anchored_terminal(80, 24);
        let mut state = InlineLoopState::new();

        tokens_tx
            .send("First paragraph.\n\nSecond para".to_string())
            .expect("send");
        unthrottle(&mut state);
        inline_tick(&mut app, &mut terminal, &mut state, None).expect("tick");

        let flushed = flushed_rows(&mut terminal);
        assert!(
            flushed.iter().any(|r| r.contains("First paragraph.")),
            "committed block must reach scrollback: {flushed:?}"
        );
        let tail: String = app
            .viewport_tail
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(
            tail.contains("Second para"),
            "pending block must stay in the viewport tail, got {tail:?}"
        );
        assert!(
            !flushed.iter().any(|r| r.contains("Second para")),
            "pending block must not be flushed yet: {flushed:?}"
        );
    }

    /// S4 (US2-3): on completion the tail flushes to scrollback, streaming
    /// state clears, and the turn separator is emitted.
    #[test]
    fn s4_completion_flushes_tail_and_clears() {
        let (mut app, tokens_tx, done_tx) = scripted_stream("hello");
        let mut terminal = anchored_terminal(80, 24);
        let mut state = InlineLoopState::new();

        tokens_tx.send("Partial answer".to_string()).expect("send");
        unthrottle(&mut state);
        inline_tick(&mut app, &mut terminal, &mut state, None).expect("tick");

        // The completion carries the SAME full text as the accumulated
        // tokens (real streaming protocol: tokens compose the response).
        tokens_tx.send(", now complete.".to_string()).expect("send");
        done_tx
            .send((Ok("Partial answer, now complete.".to_string()), None))
            .expect("send");
        unthrottle(&mut state);
        let exit = inline_tick(&mut app, &mut terminal, &mut state, None).expect("tick");
        assert!(!exit);

        assert!(!app.is_streaming, "streaming must end");
        assert!(app.pending_calls.is_empty(), "pending call must clear");
        assert!(app.viewport_tail.is_empty(), "tail must clear");
        let flushed = flushed_rows(&mut terminal);
        assert!(
            flushed.iter().any(|r| r.contains("now complete.")),
            "final content must reach scrollback: {flushed:?}"
        );
        assert!(
            history_contains(&terminal, "\u{2500}\u{2500}"),
            "turn separator line expected"
        );
    }

    /// S5 (FR-014, H4): resize re-anchors the viewport to the bottom of the
    /// new screen — both growth and shrink.
    #[test]
    fn s5_resize_reanchors_viewport_to_bottom() {
        let mut app = prepare_inline_app(test_config());
        let mut terminal = anchored_terminal(60, 12);
        let mut state = InlineLoopState::new();

        // Grow 12 → 20 rows via the real Resize event path (the backend
        // reports the new size, as a real terminal would).
        terminal.backend_mut().resize(60, 20);
        let exit = inline_tick(
            &mut app,
            &mut terminal,
            &mut state,
            Some(Event::Resize(60, 20)),
        )
        .expect("tick");
        assert!(!exit);
        let area = terminal.get_frame().area();
        assert_eq!(area.bottom(), 20, "viewport must re-anchor to new bottom");
        assert_eq!(area.height, INLINE_VIEWPORT_ROWS);

        // Shrink 20 → 6 rows: viewport must gracefully fill the screen.
        terminal.backend_mut().resize(60, 6);
        reanchor_viewport(&mut terminal, 60, 6).expect("reanchor");
        let area = terminal.get_frame().area();
        assert_eq!(area.bottom(), 6, "viewport must clamp to shrunken screen");
        assert_eq!(area.height, 6);
    }

    /// S6 (FR-011): NO_COLOR forces the accent-less theme regardless of
    /// config.
    #[test]
    fn s6_no_color_theme_selection() {
        let config = test_config();
        let mut app = App::new(config);
        apply_inline_theme(&mut app, config, true);
        assert_eq!(
            app.theme.info_accent(),
            ratatui::style::Color::Reset,
            "NO_COLOR theme must carry no accent color"
        );
    }

    /// S8 (T058, guards T055): Submit must be fast on the event loop — the
    /// pending call and streaming state appear instantly while the heavy
    /// pipeline (orchestrator/knowledge/LLM) runs in background tasks.
    /// The 500ms gate tolerates first-message `ensure_session` file IO
    /// (~150ms cold) while catching any second-scale synchronous work —
    /// the pre-T055 behaviour blocked the loop for up to ~10s. Subsequent
    /// submits return in single-digit milliseconds.
    #[test]
    fn s8_submit_dispatch_is_instant() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let (elapsed, app, mut terminal) = rt.block_on(async {
            let mut app = prepare_inline_app(test_config());
            app.scrollback_queue.clear();
            let mut terminal = anchored_terminal(80, 24);
            let mut state = InlineLoopState::new();
            app.input = App::create_input_textarea("hello async world");

            let start = Instant::now();
            let exit = inline_tick(
                &mut app,
                &mut terminal,
                &mut state,
                Some(Event::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE,
                ))),
            )
            .expect("tick");
            let elapsed = start.elapsed();
            assert!(!exit);
            (elapsed, app, terminal)
        });

        assert!(
            elapsed < Duration::from_millis(500),
            "submit path must not block the event loop, took {elapsed:?}"
        );
        assert!(app.is_streaming, "streaming state must flip instantly");
        assert_eq!(
            app.pending_calls.len(),
            1,
            "pending call must exist instantly"
        );
        assert!(
            app.status_hint.is_some(),
            "progress hint must be visible while the pipeline prepares"
        );
        let flushed = flushed_rows(&mut terminal);
        assert!(
            flushed.iter().any(|r| r.contains("hello async world")),
            "user echo must reach scrollback: {flushed:?}"
        );
    }

    /// H1 regression: bracketed paste routes into the composer instead of
    /// being swallowed by the inline event loop.
    #[test]
    fn s9_paste_reaches_inline_composer() {
        let mut app = prepare_inline_app(test_config());
        app.scrollback_queue.clear();
        let mut terminal = anchored_terminal(80, 24);
        let mut state = InlineLoopState::new();

        inline_tick(
            &mut app,
            &mut terminal,
            &mut state,
            Some(Event::Paste("pasted content".to_string())),
        )
        .expect("tick");

        assert!(
            app.input.lines().join("\n").contains("pasted content"),
            "pasted text must land in the composer"
        );
    }

    /// T062 (C2): while the user reads scrollback (PageUp), committed blocks
    /// are DEFERRED — nothing is inserted, the view cannot jump — and the
    /// live tail keeps updating. PageDown flushes in order.
    #[test]
    fn s10_reading_mode_defers_then_flushes_in_order() {
        let (mut app, tokens_tx, _done) = scripted_stream("history read");
        let mut terminal = anchored_terminal(80, 24);
        let mut state = InlineLoopState::new();

        // Enter reading mode BEFORE the stream produces committed blocks.
        app.reading_mode = true;

        tokens_tx
            .send("Block one.\n\nBlock two".to_string())
            .expect("send");
        unthrottle(&mut state);
        inline_tick(&mut app, &mut terminal, &mut state, None).expect("tick");

        let flushed = flushed_rows(&mut terminal);
        assert!(
            !flushed.iter().any(|r| r.contains("Block one.")),
            "nothing may be inserted while reading: {flushed:?}"
        );
        assert!(
            app.deferred_scrollback.iter().any(|e| e
                .lines
                .iter()
                .any(|l| { l.spans.iter().any(|sp| sp.content.contains("Block one.")) })),
            "committed block must be deferred, not dropped"
        );

        // PageDown → exit reading + ordered flush.
        inline_tick(
            &mut app,
            &mut terminal,
            &mut state,
            Some(Event::Key(KeyEvent::new(
                KeyCode::PageDown,
                KeyModifiers::NONE,
            ))),
        )
        .expect("tick");

        assert!(!app.reading_mode);
        assert!(app.deferred_scrollback.is_empty());
        let flushed = flushed_rows(&mut terminal);
        let one = flushed.iter().position(|r| r.contains("Block one."));
        // "Block two" is still the pending tail at this point; block one MUST
        // have flushed after leaving reading mode.
        assert!(
            one.is_some(),
            "deferred block must flush on PageDown: {flushed:?}"
        );
        assert!(app.deferred_scrollback.is_empty());
    }

    /// P1 regression (review-agent 2026-08-16): quitting while in reading
    /// mode must flush deferred blocks, not drop them.
    #[test]
    fn s11_quit_in_reading_mode_flushes_deferred() {
        let (mut app, tokens_tx, _done) = scripted_stream("read then quit");
        let mut terminal = anchored_terminal(80, 24);
        let mut state = InlineLoopState::new();

        tokens_tx
            .send("Deferred answer.".to_string())
            .expect("send");
        unthrottle(&mut state);
        inline_tick(&mut app, &mut terminal, &mut state, None).expect("tick");

        // Enter reading mode AFTER the block was already flushed? No — defer
        // requires reading BEFORE the tick that commits. Re-do properly:
        // (the first tick above flushed; start a second deferred block)
        app.reading_mode = true;
        tokens_tx
            .send("\n\nSecond deferred body.\n\nTail".to_string())
            .expect("send");
        unthrottle(&mut state);
        inline_tick(&mut app, &mut terminal, &mut state, None).expect("tick");
        assert!(
            !app.deferred_scrollback.is_empty(),
            "second block must be deferred"
        );

        // Ctrl+D quit path runs the same exit flush as run_inline_session.
        app.save_session_state();
        app.running = false;
        app.exit_reading_mode();
        crate::tui::scrollback_inserter::insert_scrollback_queue(
            &mut terminal,
            &mut app.scrollback_queue,
        )
        .expect("final flush");
        let flushed = flushed_rows(&mut terminal);
        assert!(
            flushed.iter().any(|r| r.contains("Second deferred body.")),
            "deferred block must survive quit: {flushed:?}"
        );
    }

    /// S7 (FR-008 precondition): consecutive turns both land in scrollback in
    /// order.
    #[test]
    fn s7_multi_turn_history_order() {
        let (mut app, tokens_tx, done_tx) = scripted_stream("first");
        let mut terminal = anchored_terminal(80, 24);
        let mut state = InlineLoopState::new();

        tokens_tx.send("Turn one body.".to_string()).expect("send");
        unthrottle(&mut state);
        inline_tick(&mut app, &mut terminal, &mut state, None).expect("tick");
        done_tx
            .send((Ok("Turn one body.".to_string()), None))
            .expect("send");
        unthrottle(&mut state);
        inline_tick(&mut app, &mut terminal, &mut state, None).expect("tick");

        // Second turn on the same scripted channels.
        app.is_streaming = true;
        app.turn_started_at = Some(Instant::now());
        app.pending_calls
            .push(PendingCallKind::Streaming(PendingLlmCallStream {
                query: "second".to_string(),
                tokens_rx: {
                    let (tx, rx) = mpsc::channel::<String>();
                    tx.send("Turn two body.".to_string()).expect("send");
                    // Keep the sender alive until done arrives so the channel
                    // is not reported disconnected early.
                    std::mem::forget(tx);
                    rx
                },
                done_rx: {
                    let (tx, rx) = mpsc::channel();
                    tx.send((Ok("Turn two body.".to_string()), None))
                        .expect("send");
                    std::mem::forget(tx);
                    rx
                },
            }));
        unthrottle(&mut state);
        inline_tick(&mut app, &mut terminal, &mut state, None).expect("tick");

        let hist = history(&terminal);
        let one = hist
            .iter()
            .position(|r| r.contains("Turn one body."))
            .unwrap();
        let two = hist
            .iter()
            .position(|r| r.contains("Turn two body."))
            .unwrap();
        assert!(one < two, "turns must keep chronological order");
        assert!(!app.is_streaming);
    }
}
