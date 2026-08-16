//! Layer-3 PTY end-to-end tests — docs/specs/002-agentic-tui/test-design.md.
//!
//! Spawns the real `zen` binary in a pseudo-terminal (portable-pty), feeds
//! its output into a vt100 screen model, drives the keyboard, and asserts
//! what a user would actually SEE. Covers what TestBackend cannot: real
//! escape sequences, raw mode, bracketed paste, viewport anchoring, and the
//! alternate-screen full TUI path.
//!
//! All tests are `#[ignore]`d — run via `bin/tui-pty-test` (needs a PTY and
//! the debug binary; skipped automatically in `bin/test-tui` otherwise).

#![allow(clippy::print_stdout)]

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use vt100::Parser;

const COLS: u16 = 100;
const ROWS: u16 = 30;
const TIMEOUT: Duration = Duration::from_secs(12);

type SharedWriter = Arc<Mutex<Box<dyn std::io::Write + Send>>>;

struct Tui {
    child: Box<dyn portable_pty::Child + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    writer: SharedWriter,
    parser: Arc<Mutex<Parser>>,
    _reader: std::thread::JoinHandle<()>,
    // Keep the isolated HOME alive for the child's lifetime.
    _home: tempfile::TempDir,
}

impl Tui {
    fn spawn(extra_env: &[(&str, &str)]) -> Self {
        Self::spawn_with(extra_env, None)
    }

    fn spawn_with(extra_env: &[(&str, &str)], config_toml: Option<&str>) -> Self {
        let bin =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/zen");
        assert!(
            bin.exists(),
            "debug binary missing — run `cargo build -p zen`: {bin:?}"
        );

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: ROWS,
                cols: COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let mut cmd = CommandBuilder::new(&bin);
        cmd.env("TERM", "xterm-256color");
        // Isolation: the child must not touch the developer's real ~/.zen
        // (logs, db, sessions) — and in sandboxed test environments the real
        // home is not writable at all.
        let home = tempfile::TempDir::new().expect("temp home");
        std::fs::create_dir_all(home.path().join(".zen")).expect("temp .zen");
        if let Some(toml) = config_toml {
            std::fs::write(home.path().join(".zen/config.toml"), toml).expect("temp config");
        }
        cmd.env("HOME", home.path());
        cmd.cwd(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let child = pair.slave.spawn_command(cmd).expect("spawn zen");
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().expect("pty reader");
        let writer: SharedWriter =
            Arc::new(Mutex::new(pair.master.take_writer().expect("pty writer")));
        let parser = Arc::new(Mutex::new(Parser::new(ROWS, COLS, 2000)));
        let feed = parser.clone();
        let reply_writer = writer.clone();
        let _reader = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                if let Ok(mut p) = feed.lock() {
                    p.process(&buf[..n]);
                }
                // Real terminals answer cursor-position queries (DSR). The
                // inline viewport anchors via `get_cursor_position`, so the
                // emulated terminal MUST reply or ratatui times out.
                if buf[..n].windows(4).any(|w| w == b"\x1b[6n")
                    && let Ok(p) = feed.lock()
                {
                    let (row, col) = p.screen().cursor_position();
                    let reply = format!("\x1b[{};{}R", row + 1, col + 1);
                    if let Ok(mut w) = reply_writer.lock() {
                        let _ = w.write_all(reply.as_bytes());
                        let _ = w.flush();
                    }
                }
            }
        });

        Self {
            child,
            master: pair.master,
            writer,
            parser,
            _reader,
            _home: home,
        }
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("pty resize");
    }

    fn send(&mut self, bytes: &[u8]) {
        let mut w = self.writer.lock().expect("pty writer");
        w.write_all(bytes).expect("pty write");
        w.flush().expect("pty flush");
    }

    fn screen(&self) -> String {
        self.parser.lock().expect("parser").screen().contents()
    }

    fn wait_for(&mut self, needle: &str, what: &str) {
        let start = Instant::now();
        loop {
            let screen = self.screen();
            if screen.contains(needle) {
                return;
            }
            if start.elapsed() > TIMEOUT {
                panic!(
                    "timeout waiting for {what:?} (needle {needle:?}); screen:\n{}",
                    screen
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn wait_exit(&mut self) -> portable_pty::ExitStatus {
        let start = Instant::now();
        loop {
            match self.child.try_wait().expect("try_wait") {
                Some(status) => return status,
                None => {
                    if start.elapsed() > TIMEOUT {
                        panic!(
                            "zen did not exit within {TIMEOUT:?}; screen:\n{}",
                            self.screen()
                        );
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }
}

/// E1 (SC-001/FR-004): startup shows the banner/intro above a bottom-anchored
/// inline viewport (input box + footer).
#[test]
#[ignore]
fn e1_inline_startup_banner_and_input() {
    let mut tui = Tui::spawn(&[]);
    tui.wait_for("Zen REPL", "inline intro line");
    tui.wait_for("Workspace:", "workspace hint");
    tui.wait_for("Input (Enter=send", "inline composer");
}

/// E2 (H1/FR): bracketed paste routes into the inline composer instead of
/// being swallowed by the event loop.
#[test]
#[ignore]
fn e2_inline_bracketed_paste_reaches_composer() {
    let mut tui = Tui::spawn(&[]);
    tui.wait_for("Input (Enter=send", "composer ready");
    tui.send(b"\x1b[200~pasted-xyz-123\x1b[201~");
    tui.wait_for("pasted-xyz-123", "pasted text visible in composer");
}

/// E3 (T059/FR-003): the slash popup opens without crushing the composer.
#[test]
#[ignore]
fn e3_inline_slash_popup_keeps_input() {
    let mut tui = Tui::spawn(&[]);
    tui.wait_for("Input (Enter=send", "composer ready");
    tui.send(b"/");
    tui.wait_for("Commands", "slash popup");
    tui.wait_for("Input (Enter=send", "composer intact under popup");
    tui.send(b"\x1b"); // Esc dismisses
}

/// E4 (SC-006): a local slash command flows through the real Enter path and
/// its output lands in native scrollback (no LLM involved).
#[test]
#[ignore]
fn e4_inline_slash_help_output_reaches_scrollback() {
    let mut tui = Tui::spawn(&[]);
    tui.wait_for("Input (Enter=send", "composer ready");
    tui.send(b"/help\r");
    tui.wait_for("Zen Agentic TUI - Commands", "/help output in scrollback");
}

/// E5 (FR-015): Ctrl+D exits cleanly.
#[test]
#[ignore]
fn e5_inline_ctrl_d_exits() {
    let mut tui = Tui::spawn(&[]);
    tui.wait_for("Zen REPL", "app started");
    tui.send(b"\x04");
    let status = tui.wait_exit();
    assert!(
        status.success(),
        "zen should exit 0 on Ctrl+D, got {status}"
    );
}

/// E6 (user report 2026-08-16, full-screen): typing the letter `v` must
/// insert `v` into the composer — previously the keypress was hijacked into
/// text-selection mode whenever output existed.
#[test]
#[ignore]
fn e6_fullscreen_typing_v_works() {
    let mut tui = Tui::spawn(&[("ZEN_TUI_FULLSCREEN", "1")]);
    tui.wait_for("Zen Agentic TUI", "fullscreen banner");
    // Some output exists (splash/intro cells) — the old bug's precondition.
    tui.send(b"vvvv");
    tui.wait_for("vvvv", "typed v's visible in composer");
    tui.send(b"\x04");
    tui.wait_exit();
}

/// E7 (T065, full-screen US3): slash popup opens in the alternate-screen mode
/// without breaking the composer.
#[test]
#[ignore]
fn e7_fullscreen_slash_popup_keeps_input() {
    let mut tui = Tui::spawn(&[("ZEN_TUI_FULLSCREEN", "1")]);
    tui.wait_for("Zen Agentic TUI", "fullscreen ready");
    tui.send(b"/");
    tui.wait_for("Commands", "fullscreen slash popup");
    // The composer still shows the typed slash.
    tui.wait_for("/", "composer visible under popup");
    tui.send(b"\x1b");
    // Let crossterm flush the lone Esc BEFORE the next byte — with
    // DISAMBIGUATE_ESCAPE_CODES, Esc+byte within the window parses as
    // Alt+<byte> (that is how terminals encode Alt), which would swallow
    // both keys.
    std::thread::sleep(Duration::from_millis(400));
    tui.send(b"\x04");
    tui.wait_exit();
}

/// E8 (T066, FR-014): live PTY resize — both modes must keep rendering and
/// accept input afterwards.
#[test]
#[ignore]
fn e8_inline_survives_resize() {
    let mut tui = Tui::spawn(&[]);
    tui.wait_for("Input (Enter=send", "composer ready");
    tui.resize(40, 36); // grow
    std::thread::sleep(Duration::from_millis(400));
    tui.resize(24, 14); // shrink
    std::thread::sleep(Duration::from_millis(400));
    tui.resize(ROWS, COLS); // restore
    tui.send(b"still-alive");
    tui.wait_for("still-alive", "input accepted after resizes");
    tui.send(b"\x04");
    tui.wait_exit();
}

const PICKER_CONFIG: &str = r#"
[providers.ollama]
provider_type = "ollama"
default_model = "qwen3-coder"

[providers.openai]
provider_type = "openai"
default_model = "gpt-4o-mini"
"#;

/// E9 (T066, FR-014): alternate-screen mode survives live resize and keeps
/// accepting input.
#[test]
#[ignore]
fn e9_fullscreen_survives_resize() {
    let mut tui = Tui::spawn(&[("ZEN_TUI_FULLSCREEN", "1")]);
    tui.wait_for("Zen Agentic TUI", "fullscreen ready");
    tui.resize(40, 36);
    std::thread::sleep(Duration::from_millis(400));
    tui.resize(24, 14);
    std::thread::sleep(Duration::from_millis(400));
    tui.resize(ROWS, COLS);
    tui.send(b"post-resize-input");
    tui.wait_for("post-resize-input", "input accepted after resizes");
    tui.send(b"\x04");
    tui.wait_exit();
}

/// E10 (T065): model picker opens from config providers, lists them, and
/// dismisses on Esc. (Stage advance is config-dependent — a provider without
/// a models catalog completes immediately — so it is not asserted here.)
#[test]
#[ignore]
fn e10_inline_model_picker_flow() {
    let mut tui = Tui::spawn_with(&[], Some(PICKER_CONFIG));
    tui.wait_for("Input (Enter=send", "composer ready");
    tui.send(b"/model\r");
    tui.wait_for("Select Provider", "model picker provider stage");
    tui.wait_for("ollama", "provider listed from config");
    tui.send(b"\x1b");
    std::thread::sleep(Duration::from_millis(400));
    tui.send(b"\x04");
    tui.wait_exit();
}

/// E11 (T065): session picker command opens the picker surface (isolated
/// HOME → empty list is the deterministic expectation) and dismisses.
#[test]
#[ignore]
fn e11_inline_session_picker_opens() {
    let mut tui = Tui::spawn_with(&[], Some(PICKER_CONFIG));
    tui.wait_for("Input (Enter=send", "composer ready");
    // The picker renders nothing when the list is empty, so create one
    // session first via `/new` (deterministic — no LLM round-trip; a plain
    // message would leave the app streaming and queue the next command).
    tui.send(b"/new\r");
    std::thread::sleep(Duration::from_millis(600));
    tui.send(b"/session\r");
    tui.wait_for("Sessions", "session picker surface");
    tui.send(b"\x1b");
    std::thread::sleep(Duration::from_millis(400));
    tui.send(b"\x04");
    tui.wait_exit();
}

/// E12 (T065): full-screen selection mode — Ctrl+X enters command mode,
/// `v` enters cell selection (border title changes), `y` yank path runs,
/// Esc returns to input.
#[test]
#[ignore]
fn e12_fullscreen_selection_mode_flow() {
    let mut tui = Tui::spawn(&[("ZEN_TUI_FULLSCREEN", "1")]);
    tui.wait_for("Zen Agentic TUI", "fullscreen ready");
    tui.send(b"\x18"); // Ctrl+X → command mode
    tui.wait_for("Command (v=select", "command mode border");
    tui.send(b"v"); // output exists (splash) → selection mode
    tui.wait_for("y yank", "selection mode border");
    tui.send(b"y"); // yank selected cell (clipboard may be unavailable in CI;
    // the mode must still be stable)
    std::thread::sleep(Duration::from_millis(300));
    tui.send(b"\x1b"); // exit selection
    std::thread::sleep(Duration::from_millis(400));
    tui.send(b"\x04");
    tui.wait_exit();
}
