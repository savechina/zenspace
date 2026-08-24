mod app;
pub mod cell;
mod clipboard;
mod handler;
mod highlight;
mod inline;
mod inline_handler;
mod inline_ui;
pub mod markdown;
pub mod model_picker;
mod prewarm;
mod render;
pub mod scrollback_inserter;
pub mod selection;
pub mod session_picker;
pub mod slash;
pub mod stream;
pub mod theme;
mod ui;

/// Drain window granted to in-flight work after SIGINT/SIGTERM before the TUI
/// is torn down and the process exits with 130 (128 + SIGINT).
const DRAIN_TIMEOUT_SECS: u64 = 5;

/// Install SIGINT/SIGTERM handlers. On signal: log the drain intent, wait out
/// the drain window (or exit immediately on a second Ctrl-C), restore the
/// terminal (leave alternate screen, disable raw mode) and exit 130.
fn install_signal_handler() {
    tokio::spawn(async {
        #[cfg(unix)]
        let mut sigterm = {
            use tokio::signal::unix::{SignalKind, signal};
            signal(SignalKind::terminate()).expect("failed to install SIGTERM handler")
        };
        #[cfg(not(unix))]
        let sigterm = std::future::pending::<()>();

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::warn!("SIGINT received — draining for {}s", DRAIN_TIMEOUT_SECS);
            }
            _ = sigterm.recv() => {
                tracing::warn!("SIGTERM received — draining for {}s", DRAIN_TIMEOUT_SECS);
            }
        }

        let drain = tokio::time::sleep(std::time::Duration::from_secs(DRAIN_TIMEOUT_SECS));
        tokio::select! {
            _ = drain => {}
            _ = tokio::signal::ctrl_c() => {
                tracing::warn!("second SIGINT received — exiting immediately");
            }
        }

        use crossterm::event::{
            DisableBracketedPaste, DisableMouseCapture, PopKeyboardEnhancementFlags,
        };
        use crossterm::execute;
        use crossterm::terminal::{LeaveAlternateScreen, disable_raw_mode};
        let _ = execute!(
            std::io::stdout(),
            LeaveAlternateScreen,
            DisableBracketedPaste,
            DisableMouseCapture,
            PopKeyboardEnhancementFlags,
        );
        let _ = disable_raw_mode();

        std::process::exit(130);
    });
}

pub fn run(config: &'static zen_core::config::ZenConfig) -> Result<(), anyhow::Error> {
    use crossterm::event::{
        DisableBracketedPaste, EnableBracketedPaste, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    };
    use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
    use crossterm::execute;
    use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
    use ratatui::Terminal;
    use ratatui::backend::CrosstermBackend;
    use std::io;

    install_signal_handler();

    if let Ok(paths) = zen_core::paths::ZenPaths::detect() {
        let _ = paths.ensure_identity_files();
        let _ = paths.ensure_runtime_dirs();
    }

    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
        ),
    )?;

    let result = (|| {
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        app::run_app(&mut terminal, config)
    })();

    execute!(
        io::stdout(),
        LeaveAlternateScreen,
        DisableBracketedPaste,
        DisableMouseCapture,
        PopKeyboardEnhancementFlags,
    )?;
    crossterm::terminal::disable_raw_mode()?;
    result
}

pub fn run_inline(config: &'static zen_core::config::ZenConfig) -> Result<(), anyhow::Error> {
    inline::run_inline(config)
}
