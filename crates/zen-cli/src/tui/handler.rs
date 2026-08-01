use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use tui_textarea::{Input, Key};

use super::app::InputMode;
use super::selection::{Selection, TextPosition};

pub enum KeyAction {
    Submit,
    Quit,
    Continue,
}

pub fn handle_key(key: KeyEvent, app: &mut super::app::App) -> KeyAction {
    if app.text_selection.is_some() {
        return handle_text_selection_key(key, app);
    }

    if app.input.effective_mode() == InputMode::Command {
        return match (key.code, key.modifiers) {
            (KeyCode::Char('v'), KeyModifiers::NONE) => {
                app.input.exit_command_mode();
                if !app.output.is_empty() {
                    app.enter_selection();
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

    let input_before = app.input.lines().join("\n");

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

    if !app.output.is_empty()
        && key.code == KeyCode::Char('v')
        && key.modifiers == KeyModifiers::NONE
    {
        let all_lines = app.all_lines().to_vec();
        let last_line = all_lines.len().saturating_sub(1);
        app.text_selection = Some(Selection::new(
            TextPosition::new(last_line, 0),
            TextPosition::new(last_line, 0),
        ));
        app.auto_scroll = true;
        app.refresh_input_border();
        return KeyAction::Continue;
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
            return KeyAction::Continue;
        }
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            if key.modifiers.contains(KeyModifiers::SHIFT) && !app.output.is_empty() {
                app.enter_selection();
                app.yank_selected_cell();
                return KeyAction::Continue;
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
            if app.should_navigate_history() {
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
            if app.should_navigate_history() {
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
                app.slash_state.dismiss();
                return KeyAction::Continue;
            }
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
            if app.slash_state.visible {
                if let Some(cmd) = app.slash_state.selected_command(&app.slash_registry) {
                    let text = format!("/{} ", cmd);
                    app.input.select_all();
                    app.input.cut();
                    app.input.insert_str(&text);
                    app.slash_state.dismiss();
                }
                return KeyAction::Continue;
            }
            let text = app.input.lines().join("\n");
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
        app.slash_state
            .on_input_change(&input_after, &app.slash_registry);
        if app.input.effective_mode() == InputMode::History {
            app.input.exit_mode();
        }
    }

    action
}

pub fn handle_paste(pasted: &str, app: &mut super::app::App) {
    let pasted = pasted.replace('\r', "\n");
    app.input.insert_str(pasted);
    let input = app.input.lines().join("\n");
    app.slash_state.on_input_change(&input, &app.slash_registry);
    app.input.enter_paste_mode();
    app.refresh_input_border();
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
