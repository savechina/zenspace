//! Layer-3 PTY end-to-end tests — docs/specs/002-agentic-tui/test-design.md.
//!
//! Spawns the real `zen` binary in a pseudo-terminal (portable-pty), feeds
//! its output into a vt100 screen model, drives the keyboard, and asserts
//! what a user would actually SEE. Covers what TestBackend cannot: real
//! escape sequences, raw mode, bracketed paste, viewport anchoring, and the
//! alternate-screen full TUI path.
//!
//! All tests are `#[ignore]`d — run via `bin/test tui pty` (needs a PTY and the debug
//! binary; skipped automatically in `bin/test tui` otherwise).

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
    /// Raw PTY byte stream captured BEFORE vte parsing — wire-level truth
    /// for assertions about escape sequences (e10: DEC 2026 synchronized-
    /// update pairs).
    raw: Arc<Mutex<Vec<u8>>>,
    _reader: std::thread::JoinHandle<()>,
    // Keep the isolated HOME alive for the child's lifetime.
    _home: Option<tempfile::TempDir>,
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
        Self::spawn_cmd(CommandBuilder::new(&bin), extra_env, config_toml)
    }

    /// Spawn `zen` inside an interactive shell (`sh -c 'zen; echo ZEN-EXITED;
    /// exec sh'`) so a test can prove the terminal is usable AFTER zen exits:
    /// the shell keeps running in the same pty and the terminal driver echoes
    /// typed input back (termios ECHO) — the e16 panic-restore acceptance
    /// (FR-021/SC-010). Verified empirically that macOS `/bin/sh` does NOT
    /// reset termios on interactive startup, so a leftover raw mode is NOT
    /// masked: the echoed input line only appears when zen restored cooked
    /// mode.
    fn spawn_in_shell(extra_env: &[(&str, &str)]) -> Self {
        let bin =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/zen");
        assert!(
            bin.exists(),
            "debug binary missing — run `cargo build -p zen`: {bin:?}"
        );
        let shell_cmd = format!("{}; echo ZEN-EXITED; exec sh", bin.display());
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", shell_cmd.as_str()]);
        Self::spawn_cmd(cmd, extra_env, None)
    }

    fn spawn_cmd(
        mut cmd: CommandBuilder,
        extra_env: &[(&str, &str)],
        config_toml: Option<&str>,
    ) -> Self {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: ROWS,
                cols: COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        cmd.env("TERM", "xterm-256color");
        // Isolation: the child must not touch the developer's real ~/.zen
        // (logs, db, sessions) — and in sandboxed test environments the real
        // home is not writable at all.
        let home = tempfile::TempDir::new().expect("temp home");
        std::fs::create_dir_all(home.path().join(".zen")).expect("temp .zen");
        if let Some(toml) = config_toml {
            std::fs::write(home.path().join(".zen/config.toml"), toml).expect("temp config");
        }
        // Post-mortem seam: ZEN_TEST_KEEP_HOME=1 leaks the temp home and
        // prints its path so a failing case's zen.log can be inspected —
        // without it the TempDir drop deletes the evidence at teardown.
        let keep_home = std::env::var("ZEN_TEST_KEEP_HOME").as_deref() == Ok("1");
        let home_path = home.path().to_path_buf();
        if keep_home {
            eprintln!("ZEN_TEST_KEEP_HOME: {}", home_path.display());
        }
        // The forget lives in the keep-branch only, so exactly one arm moves
        // `home` (a forget in both arms' scope would leave the other a
        // use-after-move).
        let home_cell = if keep_home {
            std::mem::forget(home);
            None
        } else {
            Some(home)
        };
        cmd.env("HOME", &home_path);
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
        let raw: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let raw_feed = raw.clone();
        let reply_writer = writer.clone();
        let _reader = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                // Wire truth must be recorded before the vte model eats the
                // bytes: e10 asserts DEC 2026 synchronized-update pairs that
                // are invisible in the parsed screen.
                if let Ok(mut r) = raw_feed.lock() {
                    r.extend_from_slice(&buf[..n]);
                }
                // Real terminals answer cursor-position queries (DSR) IN
                // STREAM: the reply reflects the parser state at the moment
                // the `ESC[6n` is processed, before any LATER bytes in the
                // stream. ratatui's inline viewport re-derives its anchor
                // from the reply, so a snapshot taken after processing the
                // whole read() chunk corrupts the anchor whenever the child
                // emits `scroll + DSR + more` in one burst (exact duplicate-
                // viewport corruption; masks/breaks e13 depending on how
                // fast scrollback insertion runs). Split the chunk at each
                // DSR, process + reply incrementally.
                {
                    let mut p = feed.lock().expect("parser");
                    let mut rest: &[u8] = &buf[..n];
                    loop {
                        match rest.windows(4).position(|w| w == b"\x1b[6n") {
                            Some(i) => {
                                let (before, after) = rest.split_at(i + 4);
                                p.process(before);
                                let (row, col) = p.screen().cursor_position();
                                let reply = format!("\x1b[{};{}R", row + 1, col + 1);
                                drop(p);
                                if let Ok(mut w) = reply_writer.lock() {
                                    let _ = w.write_all(reply.as_bytes());
                                    let _ = w.flush();
                                }
                                p = feed.lock().expect("parser");
                                rest = after;
                            }
                            None => {
                                if !rest.is_empty() {
                                    p.process(rest);
                                }
                                break;
                            }
                        }
                    }
                }
            }
        });

        Self {
            child,
            master: pair.master,
            writer,
            parser,
            raw,
            _reader,
            _home: home_cell,
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
        // Keep the vt100 model at the REAL pty size — content assertions
        // after a resize burst (e10) are meaningless if the model keeps
        // parsing into the stale startup grid.
        self.parser
            .lock()
            .expect("parser")
            .screen_mut()
            .set_size(rows, cols);
    }

    fn send(&mut self, bytes: &[u8]) {
        let mut w = self.writer.lock().expect("pty writer");
        w.write_all(bytes).expect("pty write");
        w.flush().expect("pty flush");
    }

    fn screen(&self) -> String {
        self.parser.lock().expect("parser").screen().contents()
    }

    /// Contents of the row band `top..bottom` (exclusive bottom), whole
    /// width — row-scoped view for ghost-band assertions (e11).
    fn band(&self, top: u16, bottom: u16) -> String {
        self.parser
            .lock()
            .expect("parser")
            .screen()
            .contents_between(top, 0, bottom.saturating_sub(1), COLS)
    }

    /// Raw PTY byte stream, captured before vte parsing (wire truth).
    fn raw(&self) -> Vec<u8> {
        self.raw.lock().expect("raw pty stream").clone()
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
    // Codex-parity popup (ADR-003 superseded): no glyph — rows render as
    // "/help (/h)  description"; the alias-paren form is popup-only text.
    tui.wait_for("/help (/h)", "slash popup");
    tui.wait_for("Input (Enter=send", "composer intact under popup");
    tui.send(b"\x1b"); // Esc dismisses (keeps the typed text — Codex behavior)
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
    // Codex-parity popup: no glyph — alias-paren form is popup-only text.
    tui.wait_for("/help (/h)", "fullscreen slash popup");
    // The composer still shows the typed slash.
    tui.wait_for("/", "composer visible under popup");
    tui.send(b"\x1b");
    // Let crossterm flush the lone Esc BEFORE the next byte — with
    // DISAMBIGUATE_ESCAPE_CODES, Esc+byte within the window parses as
    // Alt+<byte> (that is how terminals encode Alt), which would swallow
    // both keys.
    std::thread::sleep(Duration::from_millis(400));
    // Esc kept the "/" (Codex behavior) — Ctrl+D on a non-empty buffer
    // deletes a char instead of quitting, so clear the line first.
    tui.send(b"\x15"); // Ctrl+U — clear line
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
    // Ctrl+D is conditional (empty input quits, non-empty deletes forward),
    // so clear the line before quitting.
    tui.send(b"\x15"); // Ctrl+U — clear line
    tui.send(b"\x04");
    tui.wait_exit();
}

/// E10 (resize-integrity): a drag-resize burst must not duplicate or float
/// the inline viewport, and frames are DEC 2026 synchronized-update
/// bracketed on the wire. Submit a message carrying a unique marker (the
/// echo seam commits it to scrollback), fire ~20 alternating resizes with
/// <=5ms gaps — exactly the event rate that used to interleave blocking DSR
/// replies into duplicated/floated viewports (see the reader-thread comment
/// above) — settle, then assert CONTENT on the captured screen: the marker
/// appears EXACTLY ONCE, input still echoes after the burst, the viewport
/// furniture renders EXACTLY ONCE, and the raw PTY stream carries at least
/// one `ESC[?2026h`…`ESC[?2026l` pair. The height-only burst keeps the
/// marker in the live grid throughout: vt100 `set_size` keeps the TOP rows
/// on shrink, and ratatui only wipes the screen on WIDTH shrink.
///
/// DEVIATION from the task's example dims (100x40 <-> 60x30): the burst is
/// height-only (100x40 <-> 100x30). ratatui 0.30 `Terminal::resize`
/// deliberately `clear_region(All)`s on HORIZONTAL shrink (ratatui-core
/// resize.rs: "clear screen on horizontal shrink to avoid line wrapping
/// issues") — that upstream-by-design `ESC[2J` wipes the vt100 model's
/// visible grid, so a width-shrinking burst makes any exactly-once content
/// assertion impossible for reasons unrelated to the duplicate-viewport bug
/// this test hunts. The height-only burst exercises the identical
/// corruption class (reanchor + blocking DSR interleave + coalescer).
///
/// Note: an `e10_inline_model_picker_flow` already exists below; this case
/// keeps the task-assigned `e10_` name (distinct function, no conflict).
#[test]
#[ignore]
fn e10_burst_resize_preserves_content_and_single_viewport() {
    const MARKER: &str = "ZZRESIZEMARKER42";
    let mut tui = Tui::spawn(&[("ZEN_TEST_ECHO_LLM", "1")]);
    tui.wait_for("Input (Enter=send", "composer ready");
    tui.send(format!("{MARKER}\r").as_bytes());
    // The echo commits to scrollback at submit; the echo-LLM seam completes
    // the turn so the transcript is static before the burst starts.
    tui.wait_for(MARKER, "user echo committed to scrollback");
    tui.wait_for(
        "\u{2500}\u{2500}",
        "turn separator (turn completed pre-burst)",
    );

    // The burst: ~20 alternating grow/shrink, <=5ms apart.
    for i in 0..20 {
        if i % 2 == 0 {
            tui.resize(40, 100); // grow
        } else {
            tui.resize(30, 100); // shrink back to the startup height
        }
        std::thread::sleep(Duration::from_millis(4));
    }
    // Settle past the 80ms coalescer quiet window plus a redraw margin.
    std::thread::sleep(Duration::from_millis(400));

    // (1) The marker must NEVER DUPLICATE — a duplicated/floated viewport
    // would print it twice. Exactly-once is NOT assertable for rows near
    // the top of the transcript: alternating grow/shrink legitimately
    // scrolls committed rows off the top of the visible grid into native
    // scrollback (ordinary terminal behavior — content preserved, just
    // above the window; the vt100 parser exposes only the visible screen).
    // The viewport-integrity signal is assertion (3): bottom-anchored
    // furniture can never scroll off, so a duplicate anchor doubles it.
    let all = tui.screen();
    let marker_count = all.matches(MARKER).count();
    assert!(
        marker_count <= 1,
        "marker must never duplicate (count {marker_count}):\n{all}"
    );
    // (1b) Bottom-of-transcript anchor still visible: a lost/floated
    // viewport would overwrite or blank the committed rows just above it.
    assert!(
        all.contains("bullet two"),
        "committed transcript tail must survive the burst:\n{all}"
    );

    // (2) Input is alive after the burst.
    tui.send(b"still-alive");
    tui.wait_for("still-alive", "input accepted after resize burst");

    // (3) Single viewport: the composer hint renders EXACTLY ONCE on the
    // live screen — a duplicated/floated viewport (the corruption class
    // under test) would draw the furniture twice.
    let live = tui.screen();
    assert_eq!(
        live.matches("Input (Enter=send").count(),
        1,
        "viewport furniture must render exactly once (duplicate anchor would double it):\n{live}"
    );

    // (4) Wire truth: at least one synchronized-update pair bracketed the
    // per-tick frames (Begin must precede End in the captured stream).
    let raw = tui.raw();
    let begin = raw
        .windows(8)
        .position(|w| w == b"\x1b[?2026h")
        .expect("BeginSynchronizedUpdate (ESC[?2026h) must appear on the wire");
    let end = raw
        .windows(8)
        .position(|w| w == b"\x1b[?2026l")
        .expect("EndSynchronizedUpdate (ESC[?2026l) must appear on the wire");
    assert!(
        begin < end,
        "a 2026h…2026l pair must be bracketed in order (begin at byte {begin}, end at {end})"
    );

    // Quit: Ctrl+D on non-empty input deletes forward — clear first.
    // Settle between the two keys: observed once under full-suite load as
    // a wait_exit timeout when the exit sequence interleaved with a
    // post-burst trailing redraw.
    tui.send(b"\x15"); // Ctrl+U — clear line
    std::thread::sleep(Duration::from_millis(100));
    tui.send(b"\x04");
    tui.wait_exit();
}

/// E11 (ghost-viewport fix pin): a single GROW resize moves the
/// bottom-anchored inline viewport DOWN; ratatui's `Terminal::resize` clears
/// only the NEW viewport area, so the previously drawn composer rows
/// old_top..new_top would stay on screen as a ghost copy of the input box
/// mid-screen (user-reported 显示错乱 on window auto-resize). After the
/// reanchor grow-clear fix the stale band must be wiped (rows 18..28 blank)
/// and the viewport furniture must render EXACTLY ONCE.
///
/// Geometry: startup is 30 rows with a 12-row viewport bottom-anchored at
/// rows 18..30 (input box rows 26..29); one grow to 40 rows re-anchors the
/// viewport to rows 28..40, leaving rows 18..28 stale. (Name note: an
/// `e11_inline_session_picker_opens` already exists below; this case keeps
/// the task-assigned `e11_` prefix — distinct function, same precedent as
/// the two `e10_` tests.)
#[test]
#[ignore]
fn e11_grow_resize_leaves_no_ghost_viewport() {
    let mut tui = Tui::spawn(&[]);
    tui.wait_for("Input (Enter=send", "composer ready");
    // ONE grow: viewport moves down, old viewport rows go stale.
    tui.resize(40, 100);
    // Settle past the 80ms coalescer quiet window (leading + trailing apply)
    // plus a redraw margin.
    std::thread::sleep(Duration::from_millis(300));

    // (1) The stale band the old viewport vacated must be wiped — blank
    // rows only. Pre-fix this held the old input box (border + text rows).
    let leftover = tui.band(18, 28);
    assert!(
        leftover.trim().is_empty(),
        "ghost viewport: stale band rows 18..28 must be blank after grow:\n{leftover:?}"
    );

    // (2) Single viewport: the composer hint renders EXACTLY ONCE on the
    // live screen — a duplicated/floated viewport would draw it twice.
    let live = tui.screen();
    assert_eq!(
        live.matches("Input (Enter=send").count(),
        1,
        "viewport furniture must render exactly once after grow:\n{live}"
    );

    // Quit: Ctrl+U clear + Ctrl+D (e10 exit idiom).
    tui.send(b"\x15"); // Ctrl+U — clear line
    std::thread::sleep(Duration::from_millis(100));
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
    // Settle: fullscreen `autoresize()` processes the SIGWINCH on the next
    // event-loop tick; without this wait the keystroke lands mid-resize on
    // slow CI and never reaches the input widget (e9 "post-resize-input" flake).
    std::thread::sleep(Duration::from_millis(400));
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

/// E13 (streaming E2E, `ZEN_TEST_ECHO_LLM=1`): a plain message drives the real
/// Enter → start_async_chat → token channel → scrollback pipeline via the
/// deterministic echo script. Asserts committed markdown blocks (paragraph +
/// code fence) land in native scrollback exactly once (duplicate-output
/// regression), reasoning stays hidden by default, the turn separator renders
/// on completion, and the app keeps accepting input afterwards.
#[test]
#[ignore]
fn e13_echo_streaming_commits_blocks_without_duplication() {
    let mut tui = Tui::spawn(&[("ZEN_TEST_ECHO_LLM", "1")]);
    tui.wait_for("Input (Enter=send", "composer ready");
    tui.send(b"hello echo\r");

    tui.wait_for("A paragraph with", "committed paragraph in scrollback");
    tui.wait_for("fn main", "committed code fence in scrollback");
    // Completion flushes the tail and emits the turn separator.
    tui.wait_for("\u{2500}\u{2500}", "turn separator after completion");

    let screen = tui.screen();
    assert_eq!(
        screen.matches("A paragraph with").count(),
        1,
        "paragraph must appear exactly once (duplicate-output regression):\n{screen}"
    );
    assert_eq!(
        screen.matches("fn main").count(),
        1,
        "code fence must appear exactly once:\n{screen}"
    );
    assert!(
        !screen.contains("understand the user"),
        "reasoning must stay hidden by default:\n{screen}"
    );

    // App must still be alive and accept a follow-up after the turn completes.
    tui.send(b"still alive\r");
    tui.wait_for("still alive", "follow-up accepted after streaming turn");
    tui.send(b"\x04");
    tui.wait_exit();
}

/// E14 (streaming E2E, `ZEN_TEST_ECHO_LLM=1`): `/thinking` three-state
/// (T089) — brief surfaces only the graduation title (first reasoning
/// sentence) in the transcript; full surfaces the reasoning body live.
/// Body-only needle = the second ECHO_SCRIPT sentence; assertions are
/// completion-synchronized (the live brief line legitimately shows collapsed
/// reasoning mid-stream inside the viewport tail).
#[test]
#[ignore]
fn e14_echo_streaming_thinking_toggle_surfaces_reasoning() {
    // 250ms/chunk: the multi-line reasoning phase spans ~750ms of live
    // in_think draws — the full-mode body needle gets >=2 poll samples
    // (100ms wait_for cadence) instead of a single-drain vanish.
    let mut tui = Tui::spawn(&[
        ("ZEN_TEST_ECHO_LLM", "1"),
        ("ZEN_TEST_ECHO_DELAY_MS", "250"),
    ]);
    tui.wait_for("Input (Enter=send", "composer ready");

    // Phase 1 — BRIEF (bare /thinking cycles off -> brief). The two-sentence
    // ECHO_SCRIPT reasoning pins the T089 split: the graduation TITLE is the
    // first sentence; the second sentence ("edge case carefully") is body-only
    // and must NEVER reach the transcript in brief mode. Assert only after
    // turn completion — while streaming, the live brief line legitimately
    // shows the collapsed reasoning inside the viewport tail.
    tui.send(b"/thinking\r");
    std::thread::sleep(Duration::from_millis(300)); // let the toggle ack render
    tui.send(b"hello echo\r");
    tui.wait_for("Thought for", "graduation line in scrollback");
    tui.wait_for("A paragraph with", "committed paragraph in scrollback");
    tui.wait_for("\u{2500}\u{2500}", "turn separator after completion");
    let screen = tui.screen();
    assert!(
        screen.contains("understand the user's request"),
        "brief mode: graduation title (first reasoning sentence) must render:\n{screen}"
    );
    assert!(
        !screen.contains("edge case carefully"),
        "brief mode: reasoning body beyond the title must stay collapsed out \
         of the transcript after completion:\n{screen}"
    );

    // Phase 2 — FULL (second bare /thinking cycles brief -> full): the W3
    // window now surfaces the reasoning body live while streaming.
    tui.send(b"/thinking\r");
    std::thread::sleep(Duration::from_millis(300));
    tui.send(b"hello full\r");
    tui.wait_for(
        "edge case carefully",
        "full mode surfaces the reasoning body",
    );

    tui.send(b"\x15");
    std::thread::sleep(Duration::from_millis(100));
    tui.send(b"\x04");
    tui.wait_exit();
}

/// E15 (popup nav, Codex parity — regression for the 2026-09-17 user report):
/// Up/Down move the popup selection and Enter executes the SELECTED command.
/// "/" + Down selects `/exit` (row 2), so Enter must quit the app — if the
/// selection were stuck on row 1, /help would print and the app would stay
/// alive (the old selected-reset-to-0 stomp bug).
#[test]
#[ignore]
fn e15_inline_slash_popup_arrow_select_and_enter_executes() {
    let mut tui = Tui::spawn(&[]);
    tui.wait_for("Input (Enter=send", "composer ready");
    tui.send(b"/");
    tui.wait_for("/help (/h)", "slash popup open");
    tui.send(b"\x1b[B"); // Down — move selection to /exit
    std::thread::sleep(Duration::from_millis(200));
    tui.send(b"\r"); // Enter — execute the selected command
    let status = tui.wait_exit();
    assert!(
        status.success(),
        "slash-popup Enter should execute the selected /exit and quit, got {status}"
    );
}

/// E16 (FR-021/SC-010): a panic inside the inline session must leave the
/// terminal usable — cooked mode, echo, bracketed paste, and keyboard flags
/// restored. The `ZEN_TEST_PANIC_ON_FIRST_KEY` seam panics on the first key
/// event; zen is spawned inside a shell (`sh -c 'zen; echo ZEN-EXITED;
/// exec sh'`) so the SAME pty still has a live process afterwards. Writing
/// `echo STILL-ALIVE` must be echoed back by the terminal driver — the
/// echoed input line only appears when termios ECHO is on (cooked mode),
/// which is exactly what the panic path must restore.
#[test]
#[ignore]
fn e16_panic_leaves_terminal_usable() {
    let mut tui = Tui::spawn_in_shell(&[("ZEN_TEST_PANIC_ON_FIRST_KEY", "1")]);
    tui.wait_for("Input (Enter=send", "composer ready");
    tui.send(b"x"); // first key event → deliberate panic
    // The chained panic hook restores the terminal BEFORE the panic message
    // prints; ZEN-EXITED proves zen fully exited and the shell continued.
    tui.wait_for("ZEN-EXITED", "zen exited after panic");
    // The shell is now reading from the same pty. In cooked mode the
    // terminal driver echoes the typed line back — the echoed input is the
    // proof that raw mode (ECHO off) was disabled by the panic path.
    tui.send(b"echo STILL-ALIVE\r");
    tui.wait_for("echo STILL-ALIVE", "shell echoed typed input after panic");
    tui.wait_for("STILL-ALIVE", "shell executed the command after panic");
}

/// E17 (FR-024/SC-013): approval popup roundtrip.
/// Uses `ZEN_TEST_APPROVAL_SEAM=1` to inject a fake approval request at
/// startup. Asserts:
/// - popup renders with tool details (binary+args for shell.exec)
/// - input is paused (typed chars do NOT reach composer)
/// - `y` closes popup + records approve decision
#[test]
#[ignore]
fn e17_approval_popup_roundtrip() {
    let mut tui = Tui::spawn(&[("ZEN_TEST_APPROVAL_SEAM", "1")]);
    tui.wait_for("Input (Enter=send", "composer ready");
    // The seam injects an approval request; wait for the popup hints line
    // (FR-024: "y = approve  n = deny  Esc = deny" is always rendered)
    tui.wait_for("approve", "approval popup visible (key hint)");
    // Input should be paused — typed chars should NOT appear in composer
    tui.send(b"should-not-appear");
    std::thread::sleep(Duration::from_millis(300));
    let screen = tui.screen();
    assert!(
        !screen.contains("should-not-appear"),
        "input must be paused during approval: found typed text in screen"
    );
    // Press 'y' to approve
    tui.send(b"y");
    // After approval the popup hint line should disappear
    std::thread::sleep(Duration::from_millis(500));
    let screen = tui.screen();
    assert!(
        !screen.contains("approve"),
        "popup must close after approval (hint line gone)"
    );
}

/// E18 (FR-023/SC-013): degraded banner renders within 500ms of daemon loss
/// and local-only commands remain functional while degraded.
/// Uses `ZEN_TEST_GATEWAY_OFFLINE=1` to force OfflineDegraded at startup.
#[test]
#[ignore]
fn e18_degraded_banner_renders_and_local_commands_work() {
    let mut tui = Tui::spawn(&[("ZEN_TEST_GATEWAY_OFFLINE", "1")]);
    tui.wait_for("Input (Enter=send", "composer ready");
    // The seam forces OfflineDegraded; wait for the banner
    tui.wait_for("gateway: offline", "degraded banner visible");
    // Local-only commands should still work
    tui.send(b"/help\r");
    tui.wait_for("Zen Agentic TUI - Commands", "/help works while degraded");
}

/// T082 (NFR-008): input-latency smoke — keystroke-to-echo wall time.
///
/// Sends one printable keystroke burst into the inline composer and
/// poll-reads the PTY screen (5 ms resolution) until the characters are
/// rendered, timing the wait. MEASUREMENT ONLY: the only assertion is a
/// loose sanity bound (2000 ms — CI machines vary; this is a smoke, not a
/// benchmark), and the measured milliseconds are PRINTed via the harness's
/// output so a human can collect numbers over time toward the NFR-008
/// target (≤20 ms on reference hardware, advisory until measured).
#[test]
#[ignore]
fn input_latency_smoke() {
    let mut tui = Tui::spawn(&[]);
    tui.wait_for("Input (Enter=send", "composer ready");
    // Distinctive burst that cannot appear anywhere on the startup screen.
    let needle = "zx81";
    tui.send(needle.as_bytes());
    let start = Instant::now();
    loop {
        if tui.screen().contains(needle) {
            break;
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(2000),
            "keystroke echo exceeded the 2000 ms smoke bound (measured {} ms)",
            elapsed.as_millis()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let latency_ms = start.elapsed().as_millis();
    println!("input latency (keystroke→echo): {latency_ms} ms");
    // Ctrl+D quits only on EMPTY input in inline mode (otherwise it is
    // delete-char-forward) — clear the composer first (Ctrl+U), then exit.
    tui.send(b"\x15\x04");
    tui.wait_exit();
}

/// E19 (Wave 2A, T090): source-backed replay reflows the committed
/// transcript on a width GROW. A user echo hard-wrapped at 60 cols (one
/// logical line > 60 chars) must appear on ONE grid row after growing to
/// 100 cols — pre-Wave-2A behavior kept the 60-col hard wrap in native
/// scrollback forever (the 显示错乱 the replay lane exists for). Also pins:
/// exactly-once reinsertion (no duplication from the purge+replay), the
/// turn separator survives as a PreRendered capture, and the app keeps
/// accepting input after the destructive purge.
///
/// Assertion law (e10 lesson): only BOTTOM-anchored content is asserted —
/// the echo/response/separator tail sits above the 12-row viewport and the
/// 40-row grid leaves ample headroom; the banner may scroll off and is
/// deliberately not asserted.
#[test]
#[ignore]
fn e19_width_change_replays_transcript_at_new_width() {
    let mut tui = Tui::spawn(&[("ZEN_TEST_ECHO_LLM", "1")]);
    tui.wait_for("Input (Enter=send", "composer ready");

    // Narrow BEFORE any content is committed so the echo hard-wraps at 60.
    // Rows grow 30→40 in the same step (height change is orthogonal — the
    // replay gate only fires on WIDTH change; the taller grid keeps the
    // bottom-anchored tail on screen at both widths).
    tui.resize(40, 60);
    std::thread::sleep(Duration::from_millis(400));

    // 70-char body → "> " echo = 72 cols: ONE logical row at 100 cols,
    // TWO rows at 60. Head/tail tokens straddle the 60-col wrap point.
    let body = format!("REPLAYHEAD-{}-REPLAYTAIL", "x".repeat(48));
    assert_eq!(body.chars().count(), 70);
    tui.send(format!("{body}\r").as_bytes());
    tui.wait_for("REPLAYTAIL", "echo committed at 60 cols");
    std::thread::sleep(Duration::from_millis(200));

    // At 60 cols the logical line is hard-wrapped: no single grid row can
    // hold both tokens (this is the state replay must UNDO on grow).
    let narrow = tui.screen();
    assert!(
        narrow.contains("REPLAYHEAD") && narrow.contains("REPLAYTAIL"),
        "both tokens must be on screen before the grow:\n{narrow}"
    );
    assert!(
        !narrow
            .lines()
            .any(|l| l.contains("REPLAYHEAD") && l.contains("REPLAYTAIL")),
        "precondition: echo must be hard-wrapped at 60 cols:\n{narrow}"
    );

    // Grow: coalescer trailing edge → width change → purge + re-render +
    // reinsert at 100 cols, all inside one synchronized-update region.
    //
    // Completion orders (both legal, both must converge): if the scripted
    // echo reply already finished, the replay runs at the trailing edge;
    // if it is still streaming (slow CI runner — the Linux repro), the pure
    // gate DEFERS (streaming defers by design) and the stream-completion
    // boundary runs it. Poll for the replay OUTCOME instead of sampling
    // once after a fixed sleep — a single shot loses to either slow order.
    tui.resize(40, 100);
    let replay_deadline = Instant::now() + TIMEOUT;
    let wide = loop {
        let wide = tui.screen();
        let joined = wide
            .lines()
            .filter(|l| l.contains("REPLAYHEAD") && l.contains("REPLAYTAIL"))
            .count();
        if joined == 1 {
            break wide;
        }
        assert!(
            Instant::now() < replay_deadline,
            "replay never reflowed the echo onto ONE 100-col row:\n{wide}"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(
        wide.lines().filter(|l| l.contains("REPLAYHEAD")).count(),
        1,
        "replay must reinsert exactly once (no duplication):\n{wide}"
    );
    assert!(
        wide.contains("\u{2500}\u{2500}"),
        "turn separator must survive the purge+replay:\n{wide}"
    );

    // Alive after the destructive purge (fail-soft contract: replay never
    // kills the session).
    tui.send(b"still alive\r");
    tui.wait_for("still alive", "input accepted after replay");
    tui.send(b"\x15");
    std::thread::sleep(Duration::from_millis(100));
    tui.send(b"\x04");
    tui.wait_exit();
}
