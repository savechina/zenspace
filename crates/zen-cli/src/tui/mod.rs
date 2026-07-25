mod app;
pub mod cell;
mod clipboard;
mod handler;
mod highlight;
pub mod model_picker;
pub mod render;
pub mod session_picker;
pub mod slash;
pub mod stream;
pub mod theme;
mod ui;

pub fn run(config: &'static zen_core::config::ZenConfig) -> Result<(), anyhow::Error> {
    use crossterm::event::{
        DisableBracketedPaste, EnableBracketedPaste, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    };
    use crossterm::execute;
    use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
    use ratatui::Terminal;
    use ratatui::backend::CrosstermBackend;
    use std::io;

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
        PopKeyboardEnhancementFlags,
    )?;
    crossterm::terminal::disable_raw_mode()?;
    result
}
