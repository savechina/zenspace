mod app;
mod handler;
mod ui;

pub fn run() -> Result<(), anyhow::Error> {
    use crossterm::execute;
    use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
    use ratatui::Terminal;
    use ratatui::backend::CrosstermBackend;
    use std::io;

    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    let result = (|| {
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        app::run_app(&mut terminal)
    })();

    execute!(io::stdout(), LeaveAlternateScreen)?;
    crossterm::terminal::disable_raw_mode()?;
    result
}
