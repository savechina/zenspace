use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::Instant;
use tui_textarea::{Input, Key};

use super::app::InputMode;

pub enum KeyAction {
    Submit,
    Quit,
    Continue,
}

pub fn handle_key(key: KeyEvent, app: &mut super::app::App) -> KeyAction {
    if app.input_mode == InputMode::Selection {
        return match (key.code, key.modifiers) {
            (KeyCode::Esc, KeyModifiers::NONE)
            | (KeyCode::Char('v'), KeyModifiers::NONE) => {
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
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return KeyAction::Quit,
        (KeyCode::Char('d'), KeyModifiers::CONTROL) => return KeyAction::Quit,
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
        (KeyCode::PageUp, KeyModifiers::NONE) => {
            app.auto_scroll = false;
            app.scroll_offset = app.scroll_offset.saturating_sub(10);
            return KeyAction::Continue;
        }
        (KeyCode::PageDown, KeyModifiers::NONE) => {
            app.scroll_offset = app.scroll_offset.saturating_add(10);
            return KeyAction::Continue;
        }
        (KeyCode::Char('v'), KeyModifiers::NONE) => {
            if app.input.lines().join("\n").trim().is_empty() && !app.output.is_empty() {
                app.enter_selection();
                return KeyAction::Continue;
            }
            app.input.input(Input {
                key: Key::Char('v'),
                ctrl: false,
                alt: false,
                shift: false,
            });
            KeyAction::Continue
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
        app.slash_state.on_input_change(&input_after, &app.slash_registry);
        if app.input_mode == InputMode::History {
            app.input_mode = InputMode::Default;
        }
    }

    action
}

pub fn handle_paste(pasted: &str, app: &mut super::app::App) {
    let pasted = pasted.replace('\r', "\n");
    app.input.insert_str(pasted);
    let input = app.input.lines().join("\n");
    app.slash_state.on_input_change(&input, &app.slash_registry);
    app.input_mode = InputMode::Paste;
    app.paste_timestamp = Some(Instant::now());
    app.refresh_input_border();
}
