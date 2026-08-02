use std::io;
use std::time::Instant;

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::{TerminalOptions, Viewport};

use super::app::App;
use super::inline_handler::InlineKeyAction;
use super::theme::auto_select as theme_auto_select;
use super::theme::no_color as theme_no_color;

pub fn run_inline(config: &'static zen_core::config::ZenConfig) -> Result<()> {
    if let Ok(paths) = zen_core::paths::ZenPaths::detect() {
        let _ = paths.ensure_identity_files();
        let _ = paths.ensure_runtime_dirs();
    }

    let mut app = App::new(config);
    app.inline_mode = true;
    if std::env::var("NO_COLOR").is_ok() {
        app.theme = theme_no_color();
    } else if let Some(theme) = config.tui_theme() {
        app.with_theme(theme);
    } else {
        app.theme = theme_auto_select();
    }
    app.enqueue_welcome_banner();
    app.push_output(
        "Zen REPL — type a message or /help for commands, Ctrl+D to exit".into(),
        false,
    );
    app.push_output(format!("Workspace: {}", app.workspace), false);

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
                viewport: Viewport::Inline(8),
            },
        )?;
        let _ = super::scrollback_inserter::insert_scrollback_queue(
            &mut terminal,
            &mut app.scrollback_queue,
        );
        run_inline_app(&mut terminal, app)
    })();

    execute!(
        io::stdout(),
        DisableBracketedPaste,
        PopKeyboardEnhancementFlags,
    )?;
    crossterm::terminal::disable_raw_mode()?;
    result
}

fn run_inline_app(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    mut app: App,
) -> Result<()> {
    let config = app.config;

    let scheduler = zen_agents::scheduler::create_configured_scheduler(&config.cron);
    tokio::spawn(async move {
        scheduler.run().await;
    });

    let mut dirty = true;
    let mut last_streaming_render = Instant::now();
    const STREAMING_RENDER_INTERVAL_MS: u128 = 33;
    const POLL_INTERVAL_ACTIVE_MS: u64 = 16;
    const POLL_INTERVAL_IDLE_MS: u64 = 50;

    loop {
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
                .add_modifier(ratatui::style::Modifier::ITALIC);
            let (committed, pending) = app.stream_collector.drain_and_tail(reasoning_style);
            if !committed.is_empty() {
                app.enqueue_scrollback(committed);
            }
            app.viewport_tail = pending;

            let now = Instant::now();
            if now.duration_since(last_streaming_render).as_millis() >= STREAMING_RENDER_INTERVAL_MS
            {
                dirty = true;
                last_streaming_render = now;
            }
        }

        if response_just_completed {
            let reasoning_style = app
                .theme
                .as_ref()
                .text_muted()
                .add_modifier(ratatui::style::Modifier::ITALIC);
            let (committed, pending) = app.stream_collector.drain_and_tail(reasoning_style);
            if !committed.is_empty() {
                app.enqueue_scrollback(committed);
            }
            if !pending.is_empty() {
                app.enqueue_scrollback(pending);
            }
            app.stream_collector.clear();
            app.viewport_tail.clear();
            dirty = true;
        }

        if !app.scrollback_queue.is_empty() {
            super::scrollback_inserter::insert_scrollback_queue(
                terminal,
                &mut app.scrollback_queue,
            )?;
            dirty = true;
        }

        if dirty {
            terminal.draw(|frame| super::inline_ui::render(frame, &mut app))?;
            dirty = false;
        }

        let poll_ms = if app.is_streaming {
            POLL_INTERVAL_ACTIVE_MS
        } else {
            POLL_INTERVAL_IDLE_MS
        };
        if crossterm::event::poll(std::time::Duration::from_millis(poll_ms))? {
            match crossterm::event::read()? {
                crossterm::event::Event::Key(key)
                    if key.kind == crossterm::event::KeyEventKind::Press =>
                {
                    match super::inline_handler::handle_key(key, &mut app) {
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
                    dirty = true;
                    if !app.running {
                        break;
                    }
                }
                crossterm::event::Event::Resize(_, _) => {
                    dirty = true;
                }
                _ => {}
            }
        }
    }
    if !app.scrollback_queue.is_empty() {
        let _ = super::scrollback_inserter::insert_scrollback_queue(
            terminal,
            &mut app.scrollback_queue,
        );
    }
    println!();
    Ok(())
}
