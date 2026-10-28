mod app;
mod approval;
mod banner;
pub mod cell;
mod chat;
mod clipboard;
mod commands;
mod handler;
mod highlight;
mod history_search;
mod inline;
mod inline_handler;
mod inline_ui;
pub mod loop_panel;
pub mod markdown;
pub mod model_picker;
mod prewarm;
mod render;
pub(crate) mod resume;
mod scheduler_gate;
pub mod scrollback_inserter;
pub mod selection;
pub mod session_picker;
mod sessions;
pub mod slash;
mod stall;
pub mod stream;
pub mod theme;
mod transcript_source;
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

/// Blocking body of the TUI boot-time sweep (FR-040): sweeps the global
/// root (`~/.zen`) for stale `.bak`/`.tmp` artifacts left by crashed
/// `fs.edit` / wiki-writer operations. Returns the swept root, or `None`
/// when ZenPaths cannot resolve — fail-soft: the caller logs and startup
/// continues. The sweep itself is infallible (per-file failures log
/// internally and never abort).
fn sweep_global_root_blocking() -> Option<std::path::PathBuf> {
    let started = std::time::Instant::now();
    match zen_core::paths::ZenPaths::detect() {
        Ok(paths) => {
            let root = paths.global_root().clone();
            tracing::info!(
                root = %root.display(),
                "boot_time_sweep: sweeping global root for stale tempfiles"
            );
            zen_core::tempfile_lifecycle::boot_time_sweep(&root);
            tracing::debug!(
                root = %root.display(),
                ms = started.elapsed().as_millis() as u64,
                "boot_time_sweep: global-root sweep complete"
            );
            Some(root)
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "boot_time_sweep: ZenPaths unavailable; skipping tempfile sweep"
            );
            None
        }
    }
}

/// Fire-and-forget boot-time sweep on a detached background thread so
/// the TUI first paint never waits on the recursive walk. Called once
/// per process start (both full-screen `run` and `run_inline`).
pub(crate) fn spawn_boot_time_sweep() {
    let _ = std::thread::Builder::new()
        .name("zen-tui-boot-sweep".into())
        .spawn(sweep_global_root_blocking);
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

    // FR-040: sweep stale tempfiles left by crashed operations. Detached
    // background thread — the first paint never waits on the walk.
    spawn_boot_time_sweep();

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

#[cfg(test)]
mod boot_sweep_tests {
    use super::*;

    /// FR-040 pin: the sweep the TUI wires at startup is idempotent — a
    /// second pass over an already-swept tree changes nothing and never
    /// panics (missing files are the expected steady state).
    #[test]
    fn boot_time_sweep_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.md.bak"), b"x").unwrap();
        std::fs::write(root.join("b.md.tmp"), b"x").unwrap();
        std::fs::write(root.join("keep.md"), b"x").unwrap();

        for _ in 0..2 {
            zen_core::tempfile_lifecycle::boot_time_sweep(root);
        }

        assert!(!root.join("a.md.bak").exists(), "stale .bak must be swept");
        assert!(!root.join("b.md.tmp").exists(), "stale .tmp must be swept");
        assert!(root.join("keep.md").exists(), "regular files must survive");
    }

    /// FR-040 pin: the wiring helper exists, compiles, and resolves a
    /// sweep root in the test env (test-support build: ZEN_HOME or the
    /// per-process temp fallback — never the developer's real ~/.zen).
    #[test]
    fn sweep_global_root_blocking_resolves_or_fails_soft() {
        let swept = sweep_global_root_blocking();
        if let Some(root) = swept {
            assert!(
                root.is_absolute() || root.starts_with("/"),
                "root must be a real path"
            );
        }
        // None is the documented fail-soft outcome (paths unresolvable) —
        // both branches must be non-panicking, which reaching here proves.
    }
}
