use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use tui_textarea::{Input, Key};

use super::app::InputMode;
use super::history_search;
use super::selection::{Selection, TextPosition};
use super::slash::{EnterAction, enter_decision};

#[derive(Debug, PartialEq, Eq)]
pub enum KeyAction {
    Submit,
    Quit,
    /// T228 parity with inline: Ctrl+C while streaming requests an in-flight
    /// turn cancel via the `session/cancel` RPC; the consumer in app.rs owns
    /// the RPC + toast side effects (handler must stay surface-agnostic).
    CancelTurn,
    Continue,
}

pub fn handle_key(key: KeyEvent, app: &mut super::app::App) -> KeyAction {
    if app.text_selection.is_some() {
        return handle_text_selection_key(key, app);
    }

    if app.loop_panel.visible
        && matches!(
            (key.code, key.modifiers),
            (KeyCode::Esc, KeyModifiers::NONE)
                | (KeyCode::Char('l'), KeyModifiers::NONE)
                | (KeyCode::Char('L'), KeyModifiers::NONE)
        )
    {
        app.loop_panel.visible = false;
        return KeyAction::Continue;
    }

    if app.input.effective_mode() == InputMode::Command {
        return match (key.code, key.modifiers) {
            (KeyCode::Char('v'), KeyModifiers::NONE) => {
                app.input.exit_command_mode();
                if !app.output.is_empty() {
                    app.enter_selection();
                } else {
                    // Nothing to select — `v` must stay typeable.
                    app.input.input(Input {
                        key: Key::Char('v'),
                        ctrl: false,
                        alt: false,
                        shift: false,
                    });
                }
                KeyAction::Continue
            }
            (KeyCode::Char('j'), KeyModifiers::NONE) => {
                app.auto_scroll = false;
                app.scroll_offset = app.scroll_offset.saturating_add(1);
                KeyAction::Continue
            }
            (KeyCode::Char('k'), KeyModifiers::NONE) => {
                app.auto_scroll = false;
                app.scroll_offset = app.scroll_offset.saturating_sub(1);
                KeyAction::Continue
            }
            (KeyCode::Char('G'), KeyModifiers::NONE) | (KeyCode::End, KeyModifiers::NONE) => {
                app.auto_scroll = true;
                KeyAction::Continue
            }
            (KeyCode::Char('g'), KeyModifiers::NONE) => {
                app.auto_scroll = false;
                app.scroll_offset = 0;
                KeyAction::Continue
            }
            (KeyCode::Home, KeyModifiers::NONE) => {
                app.auto_scroll = false;
                app.scroll_offset = 0;
                KeyAction::Continue
            }
            (KeyCode::Esc, KeyModifiers::NONE) | (KeyCode::Char('x'), KeyModifiers::CONTROL) => {
                app.input.exit_command_mode();
                KeyAction::Continue
            }
            _ => {
                app.input.exit_command_mode();
                // Exit-and-forward: do not swallow the key that left command
                // mode — the user's first typed character must land in the
                // input box.
                forward_key_to_input(key, app);
                KeyAction::Continue
            }
        };
    }

    if app.input.effective_mode() == InputMode::Selection {
        return match (key.code, key.modifiers) {
            (KeyCode::Char('v'), KeyModifiers::NONE) => {
                app.input.set_just_exited_selection(true);
                app.exit_selection();
                KeyAction::Continue
            }
            (KeyCode::Esc, KeyModifiers::NONE) => {
                app.exit_selection();
                KeyAction::Continue
            }
            (KeyCode::Char('y'), KeyModifiers::NONE) => {
                app.yank_selected_cell();
                KeyAction::Continue
            }
            (KeyCode::Up, KeyModifiers::NONE) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
                app.selection_up();
                KeyAction::Continue
            }
            (KeyCode::Down, KeyModifiers::NONE) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
                app.selection_down();
                KeyAction::Continue
            }
            (KeyCode::PageUp, KeyModifiers::NONE) => {
                app.auto_scroll = false;
                app.scroll_offset = app.scroll_offset.saturating_sub(10);
                KeyAction::Continue
            }
            (KeyCode::PageDown, KeyModifiers::NONE) => {
                app.scroll_offset = app.scroll_offset.saturating_add(10);
                KeyAction::Continue
            }
            _ => KeyAction::Continue,
        };
    }

    // B7: Reverse-i-search mode — all keys route to the SINGLE shared state
    // machine (the same one the inline prompt calls) before normal dispatch.
    if app.history_search.active {
        history_search::handle_key(key, app);
        return KeyAction::Continue;
    }

    let input_before = app.input.lines().join("\n");

    if matches!(
        (key.code, key.modifiers),
        (KeyCode::Char('l'), KeyModifiers::NONE) | (KeyCode::Char('L'), KeyModifiers::NONE)
    ) && !app.model_picker.visible
        && !app.session_picker.visible
        && !app.slash_state.visible
    {
        app.loop_panel.toggle();
        if app.loop_panel.visible && app.loop_panel.last_report.is_none() {
            app.show_toast("Loop panel — no cycle yet (L to close)");
        }
        return KeyAction::Continue;
    }

    if app.model_picker.visible {
        return match key.code {
            KeyCode::Up => {
                app.model_picker.move_up();
                KeyAction::Continue
            }
            KeyCode::Down => {
                app.model_picker.move_down();
                KeyAction::Continue
            }
            KeyCode::Enter => {
                if let Some((provider, model, variant)) = app.model_picker.advance(app.config) {
                    app.set_model(&provider, &model);
                    if let Some(v) = variant {
                        app.current_variant = Some(v.clone());
                    }
                }
                KeyAction::Continue
            }
            KeyCode::Left | KeyCode::Backspace => {
                app.model_picker.go_back();
                KeyAction::Continue
            }
            KeyCode::Esc => {
                app.model_picker.dismiss();
                KeyAction::Continue
            }
            _ => KeyAction::Continue,
        };
    }

    let action = match (key.code, key.modifiers) {
        (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
            if app.session_picker.visible && !app.session_picker.rename_mode {
                if app.session_picker.archive_pending.is_some() {
                    if let Some(session_id) = app.session_picker.confirm_archive() {
                        app.archive_session(&session_id);
                    }
                } else {
                    app.session_picker.start_archive();
                }
                return KeyAction::Continue;
            }
            return KeyAction::Continue;
        }
        (KeyCode::Char('r'), KeyModifiers::CONTROL) => {
            if app.session_picker.visible && !app.session_picker.rename_mode {
                app.session_picker.start_rename();
                return KeyAction::Continue;
            }
            // B7: enter reverse-i-search via the shared state machine, but
            // only when neither the session picker nor the slash popup owns
            // the keys (picker precedence above is preserved exactly).
            if !app.session_picker.visible && !app.slash_state.visible {
                history_search::handle_key(key, app);
            }
            return KeyAction::Continue;
        }
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            if key.modifiers.contains(KeyModifiers::SHIFT) && !app.output.is_empty() {
                app.enter_selection();
                app.yank_selected_cell();
                return KeyAction::Continue;
            }
            // T228 parity with inline_handler: while streaming, first Ctrl+C
            // cancels the in-flight turn (CancelTurn), a second consecutive
            // press quits. Idle Ctrl+C keeps quit semantics.
            if app.is_streaming {
                if app.cancel_requested {
                    return KeyAction::Quit;
                }
                app.cancel_requested = true;
                return KeyAction::CancelTurn;
            }
            return KeyAction::Quit;
        }
        (KeyCode::Char('d'), KeyModifiers::CONTROL) => return KeyAction::Quit,
        (KeyCode::Char('x'), KeyModifiers::CONTROL) => {
            app.input.enter_command_mode();
            return KeyAction::Continue;
        }
        (KeyCode::PageUp, KeyModifiers::NONE) => {
            app.auto_scroll = false;
            app.scroll_offset = app.scroll_offset.saturating_sub(10);
            return KeyAction::Continue;
        }
        (KeyCode::PageDown, KeyModifiers::NONE) => {
            app.auto_scroll = false;
            app.scroll_offset = app.scroll_offset.saturating_add(10);
            return KeyAction::Continue;
        }
        (KeyCode::Up, KeyModifiers::CONTROL) => {
            app.auto_scroll = false;
            app.scroll_offset = app.scroll_offset.saturating_sub(3);
            return KeyAction::Continue;
        }
        (KeyCode::Down, KeyModifiers::CONTROL) => {
            app.auto_scroll = false;
            app.scroll_offset = app.scroll_offset.saturating_add(3);
            return KeyAction::Continue;
        }
        (KeyCode::Up, KeyModifiers::NONE) => {
            if app.session_picker.visible {
                app.session_picker.move_up();
                return KeyAction::Continue;
            }
            if app.slash_state.visible {
                app.slash_state.move_up();
                return KeyAction::Continue;
            }
            if app.should_navigate_history_up() {
                app.history_up();
            } else {
                app.input.input(Input {
                    key: Key::Up,
                    ctrl: false,
                    alt: false,
                    shift: false,
                });
            }
            return KeyAction::Continue;
        }
        (KeyCode::Down, KeyModifiers::NONE) => {
            if app.session_picker.visible {
                app.session_picker.move_down();
                return KeyAction::Continue;
            }
            if app.slash_state.visible {
                app.slash_state.move_down();
                return KeyAction::Continue;
            }
            if app.should_navigate_history_down() {
                app.history_down();
            } else {
                app.input.input(Input {
                    key: Key::Down,
                    ctrl: false,
                    alt: false,
                    shift: false,
                });
            }
            return KeyAction::Continue;
        }
        (KeyCode::Tab, KeyModifiers::NONE) => {
            if app.slash_state.visible {
                if let Some(cmd) = app.slash_state.selected_command(&app.slash_registry) {
                    let text = format!("/{} ", cmd);
                    app.input.select_all();
                    app.input.cut();
                    app.input.insert_str(&text);
                    // T084(b): wholesale rewrite drops any pill tokens.
                    app.clear_paste_pills();
                    app.slash_state.dismiss();
                }
                return KeyAction::Continue;
            }
            app.input.input(Input {
                key: Key::Tab,
                ctrl: false,
                alt: false,
                shift: false,
            });
            return KeyAction::Continue;
        }
        (KeyCode::Esc, KeyModifiers::NONE) => {
            if app.session_picker.rename_mode {
                app.session_picker.cancel_rename();
                return KeyAction::Continue;
            }
            if app.session_picker.visible {
                app.session_picker.cancel_archive();
                app.session_picker.dismiss();
                return KeyAction::Continue;
            }
            if app.slash_state.visible {
                // T084(d): Esc records the dismissed token (popup stays shut
                // while the token is unchanged).
                app.dismiss_slash_popup();
                return KeyAction::Continue;
            }
            return KeyAction::Continue;
        }
        // T066: Shift+Enter always inserts a newline (never submits).
        (KeyCode::Enter, KeyModifiers::SHIFT) => {
            app.input.input(Input {
                key: Key::Enter,
                ctrl: false,
                alt: false,
                shift: true,
            });
            return KeyAction::Continue;
        }
        (KeyCode::Enter, KeyModifiers::NONE) => {
            if app.session_picker.rename_mode {
                if let Some((session_id, title)) = app.session_picker.confirm_rename() {
                    app.rename_session(&session_id, &title);
                }
                return KeyAction::Continue;
            }
            if app.session_picker.visible {
                if let Some(session) = app.session_picker.selected_session() {
                    let session_id = session.id.clone();
                    app.resume_session(&session_id);
                }
                return KeyAction::Continue;
            }
            let text = app.input.lines().join("\n");
            match enter_decision(
                app.slash_state.visible,
                app.slash_state.choice_locked,
                app.slash_state.selected_command(&app.slash_registry),
                &text,
            ) {
                EnterAction::Execute { rewrite } => {
                    if let Some(full) = rewrite {
                        app.input.select_all();
                        app.input.cut();
                        app.input.insert_str(&full);
                        app.clear_paste_pills();
                    }
                    app.slash_state.dismiss();
                    return KeyAction::Submit;
                }
                EnterAction::Complete(full) => {
                    app.input.select_all();
                    app.input.cut();
                    app.input.insert_str(&full);
                    app.clear_paste_pills();
                    app.slash_state.dismiss();
                    return KeyAction::Continue;
                }
                EnterAction::Submit => {}
            }
            let is_single_line = !text.contains('\n');
            let is_empty = text.trim().is_empty();
            app.input.input(Input {
                key: Key::Enter,
                ctrl: false,
                alt: false,
                shift: false,
            });
            if is_single_line && !is_empty {
                return KeyAction::Submit;
            }
            return KeyAction::Continue;
        }
        (KeyCode::Enter, KeyModifiers::CONTROL) => {
            let text = app.input.lines().join("\n");
            if !text.trim().is_empty() {
                return KeyAction::Submit;
            }
            return KeyAction::Continue;
        }
        (KeyCode::Char(c), KeyModifiers::NONE) => {
            if app.session_picker.rename_mode {
                app.session_picker.rename_input_char(c);
                return KeyAction::Continue;
            }
            app.input.input(Input {
                key: Key::Char(c),
                ctrl: false,
                alt: false,
                shift: false,
            });
            KeyAction::Continue
        }
        (KeyCode::Backspace, KeyModifiers::NONE) => {
            if app.session_picker.rename_mode {
                app.session_picker.rename_input_backspace();
                return KeyAction::Continue;
            }
            app.input.input(Input {
                key: Key::Backspace,
                ctrl: false,
                alt: false,
                shift: false,
            });
            KeyAction::Continue
        }
        _ => {
            app.input.input(Input {
                key: match key.code {
                    KeyCode::Char(c) => Key::Char(c),
                    KeyCode::Backspace => Key::Backspace,
                    KeyCode::Delete => Key::Delete,
                    KeyCode::Left => Key::Left,
                    KeyCode::Right => Key::Right,
                    KeyCode::Home => Key::Home,
                    KeyCode::End => Key::End,
                    KeyCode::Esc => Key::Esc,
                    _ => Key::Null,
                },
                ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
                alt: key.modifiers.contains(KeyModifiers::ALT),
                shift: key.modifiers.contains(KeyModifiers::SHIFT),
            });
            KeyAction::Continue
        }
    };

    let input_after = app.input.lines().join("\n");
    if input_before != input_after {
        // T084(d): dismissal-aware refresh (pure token equality).
        app.refresh_slash_popup();
        if app.input.effective_mode() == InputMode::History {
            app.input.exit_mode();
        }
    }

    action
}

pub fn handle_paste(pasted: &str, app: &mut super::app::App) {
    let pasted = pasted.replace('\r', "\n");
    // T084(b): pastes over threshold collapse to a `[Pasted N lines / M
    // chars]` pill (full text in the side map, expanded at submit).
    let insert = app.collapse_paste_for_insert(&pasted);
    app.input.insert_str(insert);
    app.refresh_slash_popup();
    app.input.enter_paste_mode();
    app.refresh_input_border();
}

/// Forward one crossterm key into the input textarea (used when leaving
/// command mode on a keystroke so the character is not swallowed).
fn forward_key_to_input(key: KeyEvent, app: &mut super::app::App) {
    let mapped = match key.code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        _ => return, // pure control keys are not text
    };
    app.input.input(Input {
        key: mapped,
        ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
        alt: key.modifiers.contains(KeyModifiers::ALT),
        shift: key.modifiers.contains(KeyModifiers::SHIFT),
    });
}

pub fn handle_mouse(mouse: MouseEvent, app: &mut super::app::App) {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            app.auto_scroll = false;
            app.scroll_offset = app.scroll_offset.saturating_sub(5);
        }
        MouseEventKind::ScrollDown => {
            app.auto_scroll = false;
            app.scroll_offset = app.scroll_offset.saturating_add(5);
        }
        MouseEventKind::Down(_) => {
            if !app.output.is_empty()
                && let Some(chat_area) = app.chat_area
            {
                let all_lines = app.all_lines().to_vec();
                let inner_width = chat_area.width.saturating_sub(2) as usize;
                if let Some(pos) = super::selection::mouse_to_position(
                    mouse.column,
                    mouse.row,
                    chat_area,
                    app.scroll_offset,
                    inner_width,
                    &all_lines,
                ) {
                    app.text_selection = Some(Selection::new(pos, pos));
                    app.auto_scroll = false;
                }
            }
        }
        MouseEventKind::Drag(_) => {
            let maybe_pos = app.chat_area.and_then(|chat_area| {
                let all_lines = app.all_lines().to_vec();
                let inner_width = chat_area.width.saturating_sub(2) as usize;
                super::selection::mouse_to_position(
                    mouse.column,
                    mouse.row,
                    chat_area,
                    app.scroll_offset,
                    inner_width,
                    &all_lines,
                )
            });
            if let (Some(pos), Some(sel)) = (maybe_pos, &mut app.text_selection) {
                sel.cursor = pos;
            }
        }
        MouseEventKind::Up(_) => {
            if let Some(sel) = &app.text_selection
                && sel.anchor == sel.cursor
            {
                app.text_selection = None;
            }
        }
        _ => {}
    }
}

fn handle_text_selection_key(key: KeyEvent, app: &mut super::app::App) -> KeyAction {
    let all_lines = app.all_lines().to_vec();
    let line_count = all_lines.len();

    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) | (KeyCode::Char('q'), KeyModifiers::NONE) => {
            app.text_selection = None;
            app.refresh_input_border();
            KeyAction::Continue
        }
        (KeyCode::Char('y'), KeyModifiers::NONE) => {
            if let Some(sel) = &app.text_selection {
                let text = sel.selected_text(&all_lines);
                if text.is_empty() {
                    app.show_toast("No text selected");
                } else {
                    let preview: String = text.chars().take(30).collect();
                    let suffix = if text.chars().count() > 30 { "…" } else { "" };
                    if crate::tui::clipboard::write_text(&text).is_ok() {
                        app.show_toast(format!("✓ Copied: {}{}", preview, suffix));
                    } else {
                        app.show_toast("✗ Clipboard unavailable");
                    }
                }
            }
            app.text_selection = None;
            app.refresh_input_border();
            KeyAction::Continue
        }
        (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
            if let Some(sel) = &mut app.text_selection {
                let cur = &mut sel.cursor;
                if cur.line_idx > 0 {
                    cur.line_idx -= 1;
                    let line_text = all_lines
                        .get(cur.line_idx)
                        .map(super::selection::line_text)
                        .unwrap_or_default();
                    cur.char_idx = cur.char_idx.min(line_text.chars().count());
                }
                ensure_visible(sel.cursor, app, &all_lines);
            }
            KeyAction::Continue
        }
        (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
            if let Some(sel) = &mut app.text_selection {
                let cur = &mut sel.cursor;
                if cur.line_idx + 1 < line_count {
                    cur.line_idx += 1;
                    let line_text = all_lines
                        .get(cur.line_idx)
                        .map(super::selection::line_text)
                        .unwrap_or_default();
                    cur.char_idx = cur.char_idx.min(line_text.chars().count());
                }
                ensure_visible(sel.cursor, app, &all_lines);
            }
            KeyAction::Continue
        }
        (KeyCode::Left, _) | (KeyCode::Char('h'), KeyModifiers::NONE) => {
            if let Some(sel) = &mut app.text_selection {
                let cur = &mut sel.cursor;
                if cur.char_idx > 0 {
                    cur.char_idx -= 1;
                } else if cur.line_idx > 0 {
                    cur.line_idx -= 1;
                    let line_text = all_lines
                        .get(cur.line_idx)
                        .map(super::selection::line_text)
                        .unwrap_or_default();
                    cur.char_idx = line_text.chars().count();
                }
                ensure_visible(sel.cursor, app, &all_lines);
            }
            KeyAction::Continue
        }
        (KeyCode::Right, _) | (KeyCode::Char('l'), KeyModifiers::NONE) => {
            if let Some(sel) = &mut app.text_selection {
                let cur = &mut sel.cursor;
                let line_text = all_lines
                    .get(cur.line_idx)
                    .map(super::selection::line_text)
                    .unwrap_or_default();
                let max_char = line_text.chars().count();
                if cur.char_idx < max_char {
                    cur.char_idx += 1;
                } else if cur.line_idx + 1 < line_count {
                    cur.line_idx += 1;
                    cur.char_idx = 0;
                }
                ensure_visible(sel.cursor, app, &all_lines);
            }
            KeyAction::Continue
        }
        (KeyCode::Char('v'), KeyModifiers::NONE) => {
            app.text_selection = None;
            app.refresh_input_border();
            KeyAction::Continue
        }
        _ => KeyAction::Continue,
    }
}

fn ensure_visible(
    pos: TextPosition,
    app: &mut super::app::App,
    all_lines: &[ratatui::text::Line<'static>],
) {
    use crate::tui::selection::line_text;
    use unicode_width::UnicodeWidthChar;

    let chat_area = app
        .chat_area
        .unwrap_or(ratatui::layout::Rect::new(0, 0, 82, 22));
    let inner_width = chat_area.width.saturating_sub(2) as usize;
    let visible_height = chat_area.height.saturating_sub(2) as usize;
    let mut visual_row: usize = 0;
    for (line_idx, line) in all_lines.iter().enumerate() {
        if line_idx == pos.line_idx {
            let text = line_text(line);
            let mut col: usize = 0;
            let mut visual_in_line: usize = 0;
            for (ci, ch) in text.chars().enumerate() {
                if ci == pos.char_idx {
                    break;
                }
                let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                col += cw;
                if col >= inner_width {
                    col = 0;
                    visual_in_line += 1;
                }
            }
            let target_visual = visual_row + visual_in_line;
            if target_visual < app.scroll_offset {
                app.scroll_offset = target_visual;
            } else if target_visual >= app.scroll_offset + visible_height {
                app.scroll_offset = target_visual.saturating_sub(visible_height - 1);
            }
            app.auto_scroll = false;
            return;
        }
        let line_width = line.width();
        let wrapped = if line_width == 0 || inner_width == 0 {
            1
        } else {
            line_width.div_ceil(inner_width)
        };
        visual_row += wrapped;
    }
}

#[cfg(test)]
mod tests {
    //! Full-screen mode input regression tests (user report 2026-08-16:
    //! typing `v` and other characters could not be entered normally).
    use super::*;
    use crate::tui::app::{App, InputMode};
    use crate::tui::cell::OutputCell;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn test_app() -> App {
        let config: &'static zen_core::config::ZenConfig = Box::leak(Box::default());
        let mut app = App::new(config);
        app.output.push(OutputCell::agent("previous answer", None));
        app
    }

    fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> KeyAction {
        handle_key(KeyEvent::new(code, modifiers), app)
    }

    /// Plain `v` in Default mode MUST type the letter — the old code hijacked
    /// it into text selection whenever output existed, making `v` untypeable
    /// in every message after the first response.
    #[test]
    fn default_mode_v_types_the_letter() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('v'), KeyModifiers::NONE);
        assert_eq!(app.input.lines().join(""), "v");
        assert!(
            app.text_selection.is_none(),
            "typing must not hijack into selection"
        );
    }

    /// Leaving command mode on a keystroke must forward that character into
    /// the input (previously the first character after Ctrl+X was swallowed).
    #[test]
    fn command_mode_exit_forwards_the_key() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert_eq!(app.input.effective_mode(), InputMode::Command);

        press(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
        assert_eq!(app.input.effective_mode(), InputMode::Default);
        assert_eq!(
            app.input.lines().join(""),
            "i",
            "the key that left command mode must be inserted"
        );
    }

    /// Command-mode `v` with no output must type `v` (nothing to select).
    #[test]
    fn command_mode_v_types_when_no_output() {
        let mut app = test_app();
        app.output.clear();
        press(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL);
        press(&mut app, KeyCode::Char('v'), KeyModifiers::NONE);
        assert_eq!(app.input.effective_mode(), InputMode::Default);
        assert_eq!(app.input.lines().join(""), "v");
    }

    /// Command-mode `v` with output still opens cell selection (the intended
    /// keyboard path — kept reachable).
    #[test]
    fn command_mode_v_selects_when_output_exists() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL);
        press(&mut app, KeyCode::Char('v'), KeyModifiers::NONE);
        assert_eq!(app.input.effective_mode(), InputMode::Selection);
    }

    #[test]
    fn shift_enter_inserts_newline_not_submit() {
        let mut app = test_app();
        app.input.insert_str("hello");
        let action = press(&mut app, KeyCode::Enter, KeyModifiers::SHIFT);
        assert_eq!(action, KeyAction::Continue);
        assert_eq!(app.input.lines(), vec!["hello", ""]);
    }

    /// T246 unification: a fully typed `/name` executes on the first Enter in
    /// fullscreen too (previously it only completed and needed a second Enter).
    #[test]
    fn slash_popup_enter_exact_command_submits() {
        let mut app = test_app();
        for c in "/help".chars() {
            press(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert!(app.slash_state.visible);
        let action = press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(action, KeyAction::Submit);
    }

    /// T246 unification: an arrow-locked selection executes on Enter in
    /// fullscreen too, rewriting the bare token into the selected command.
    #[test]
    fn slash_popup_enter_arrow_selected_command_submits() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
        assert!(app.slash_state.visible);
        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert!(app.slash_state.choice_locked);
        let action = press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(action, KeyAction::Submit);
        assert_eq!(app.input.lines().join("\n"), "/exit");
    }

    /// T246 unification: a partial token still completes without submitting.
    #[test]
    fn slash_popup_enter_partial_completes_without_submitting() {
        let mut app = test_app();
        for c in "/exi".chars() {
            press(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert!(app.slash_state.visible);
        let action = press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(action, KeyAction::Continue);
        assert_eq!(app.input.lines().join("\n"), "/exit ");
        assert!(!app.slash_state.visible);
    }

    #[test]
    fn enter_submits_single_line() {
        let mut app = test_app();
        app.input.insert_str("hello");
        let action = press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(action, KeyAction::Submit);
    }

    /// T228 parity: fullscreen Ctrl+C while streaming cancels (first press)
    /// and quits (second consecutive press) — the RPC side effect lives in
    /// the app.rs CancelTurn consumer, so this pins the state machine only.
    #[test]
    fn ctrl_c_streams_first_cancels_then_quits() {
        let mut app = test_app();
        app.is_streaming = true;
        let action = press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(action, KeyAction::CancelTurn);
        assert!(app.cancel_requested, "first Ctrl+C must flag cancel");

        let action = press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(action, KeyAction::Quit);
    }

    #[test]
    fn ctrl_c_idle_quits_immediately() {
        let mut app = test_app();
        app.is_streaming = false;
        app.cancel_requested = false;
        let action = press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(action, KeyAction::Quit);
    }

    #[test]
    fn ctrl_enter_submits_multiline() {
        let mut app = test_app();
        app.input.insert_str("line1\nline2");
        let action = press(&mut app, KeyCode::Enter, KeyModifiers::CONTROL);
        assert_eq!(action, KeyAction::Submit);
    }

    #[test]
    fn paste_multiline_preserves_newlines() {
        let mut app = test_app();
        handle_paste("line1\nline2\nline3", &mut app);
        assert_eq!(app.input.lines(), vec!["line1", "line2", "line3"]);
    }
    #[test]
    fn memory_nudge_poll_toasts_unseen_entries_once() {
        let temp = tempfile::tempdir().expect("tempdir");
        let logs = temp.path().join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(
            logs.join("memory-nudges.jsonl"),
            "{\"kind\":\"memory.nudge\",\"user_turns\":10,\"text\":\"nudge one\"}\n\
             {\"kind\":\"memory.nudge\",\"user_turns\":20,\"text\":\"nudge two\"}\n",
        )
        .unwrap();

        let mut app = test_app();
        app.poll_memory_nudges_in(&logs);
        assert_eq!(
            app.get_active_toast().as_deref(),
            Some("nudge one"),
            "first unseen nudge toasts"
        );
        // Marker advanced past both entries: a fresh app polls quiet.
        let marker = std::fs::read_to_string(logs.join(".last-nudge-shown")).unwrap();
        assert!(marker.contains("nudge two"), "marker covers all: {marker}");
        let mut app2 = test_app();
        app2.poll_memory_nudges_in(&logs);
        assert!(
            app2.get_active_toast().is_none(),
            "no re-toast after marker advance"
        );
    }

    #[test]
    fn memory_nudge_poll_missing_file_is_silent_noop() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut app = test_app();
        app.poll_memory_nudges_in(&temp.path().join("logs"));
        assert!(app.get_active_toast().is_none());
    }

    // === B7: Ctrl+R reverse-i-search (fullscreen) — exercises the SHARED
    // history_search::handle_key through the fullscreen entry points. ===

    /// (a) Ctrl+R enters the search state, previews the newest history entry,
    /// and snapshots the pre-search draft.
    #[test]
    fn ctrl_r_enters_reverse_i_search() {
        let mut app = test_app();
        app.command_history = vec!["alpha".into(), "beta".into(), "gamma".into()];
        app.input = App::create_input_textarea("current draft");
        let action = press(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(action, KeyAction::Continue);
        assert!(app.history_search.active);
        // Preview shows the newest match
        assert_eq!(app.input.lines().join("\n"), "gamma");
        // Draft snapshot preserved for Esc
        assert_eq!(app.history_search.draft_snapshot, "current draft");
    }

    /// (b) A typed character narrows the query while search is active (the
    /// key must reach the search, not the composer or the `l` panel toggle).
    #[test]
    fn reverse_search_typing_narrows_query() {
        let mut app = test_app();
        app.command_history = vec!["alpha".into(), "beta".into(), "gamma".into()];
        app.input = App::create_input_textarea("");
        press(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        let action = press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(action, KeyAction::Continue);
        assert_eq!(app.history_search.query, "a");
        // alpha / beta / gamma all contain 'a' — newest first
        assert_eq!(app.history_search.matches.len(), 3);
        assert_eq!(app.history_search.current_match(), Some("gamma"));
        assert_eq!(app.input.lines().join("\n"), "gamma");
    }

    /// (c) Ctrl+R again while active cycles to the older match.
    #[test]
    fn ctrl_r_cycles_older_match_while_active() {
        let mut app = test_app();
        app.command_history = vec!["alpha".into(), "beta".into(), "gamma".into()];
        app.input = App::create_input_textarea("");
        press(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(app.history_search.current_match(), Some("gamma"));
        let action = press(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(action, KeyAction::Continue);
        assert!(app.history_search.active, "cycling must stay in search");
        assert_eq!(app.history_search.match_index, 1);
        assert_eq!(app.history_search.current_match(), Some("beta"));
        assert_eq!(app.input.lines().join("\n"), "beta");
    }

    /// (d) Esc restores the draft snapshot and exits the search.
    #[test]
    fn reverse_search_esc_restores_draft_and_exits() {
        let mut app = test_app();
        app.command_history = vec!["alpha".into()];
        app.input = App::create_input_textarea("my draft");
        press(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        let action = press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(action, KeyAction::Continue);
        assert!(!app.history_search.active);
        assert_eq!(app.input.lines().join("\n"), "my draft");
        assert_eq!(app.input.effective_mode(), InputMode::Default);
    }

    /// Precedence pin: a visible session picker owns Ctrl+R — it must never
    /// start history search (rename handling is unchanged above it).
    #[test]
    fn ctrl_r_with_session_picker_never_starts_search() {
        let mut app = test_app();
        app.command_history = vec!["alpha".into()];
        app.session_picker.visible = true;
        let action = press(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(action, KeyAction::Continue);
        assert!(
            !app.history_search.active,
            "session picker owns Ctrl+R — no search entry"
        );
    }
}
