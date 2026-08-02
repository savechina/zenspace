use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tui_textarea::{Input, Key};

use super::app::App;

#[derive(PartialEq, Eq)]
pub enum InlineKeyAction {
    Submit,
    Quit,
    Continue,
}

pub fn handle_key(key: KeyEvent, app: &mut App) -> InlineKeyAction {
    if app.model_picker.visible {
        return match key.code {
            KeyCode::Up => {
                app.model_picker.move_up();
                InlineKeyAction::Continue
            }
            KeyCode::Down => {
                app.model_picker.move_down();
                InlineKeyAction::Continue
            }
            KeyCode::Enter => {
                if let Some((provider, model, variant)) = app.model_picker.advance(app.config) {
                    app.set_model(&provider, &model);
                    if let Some(v) = variant {
                        app.current_variant = Some(v.clone());
                    }
                }
                InlineKeyAction::Continue
            }
            KeyCode::Left | KeyCode::Backspace => {
                app.model_picker.go_back();
                InlineKeyAction::Continue
            }
            KeyCode::Esc => {
                app.model_picker.dismiss();
                InlineKeyAction::Continue
            }
            _ => InlineKeyAction::Continue,
        };
    }

    if app.session_picker.visible {
        return match (key.code, key.modifiers) {
            (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                if !app.session_picker.rename_mode {
                    if app.session_picker.archive_pending.is_some() {
                        if let Some(session_id) = app.session_picker.confirm_archive() {
                            app.archive_session(&session_id);
                        }
                    } else {
                        app.session_picker.start_archive();
                    }
                }
                InlineKeyAction::Continue
            }
            (KeyCode::Char('r'), KeyModifiers::CONTROL) => {
                if !app.session_picker.rename_mode {
                    app.session_picker.start_rename();
                }
                InlineKeyAction::Continue
            }
            (KeyCode::Up, _) => {
                app.session_picker.move_up();
                InlineKeyAction::Continue
            }
            (KeyCode::Down, _) => {
                app.session_picker.move_down();
                InlineKeyAction::Continue
            }
            (KeyCode::Enter, _) => {
                if app.session_picker.rename_mode {
                    if let Some((session_id, title)) = app.session_picker.confirm_rename() {
                        app.rename_session(&session_id, &title);
                    }
                } else if let Some(session) = app.session_picker.selected_session() {
                    let session_id = session.id.clone();
                    app.resume_session(&session_id);
                }
                InlineKeyAction::Continue
            }
            (KeyCode::Esc, _) => {
                if app.session_picker.rename_mode {
                    app.session_picker.cancel_rename();
                } else if app.session_picker.archive_pending.is_some() {
                    app.session_picker.cancel_archive();
                } else {
                    app.session_picker.dismiss();
                }
                InlineKeyAction::Continue
            }
            (KeyCode::Backspace, _) => {
                if app.session_picker.rename_mode {
                    app.session_picker.rename_input_backspace();
                }
                InlineKeyAction::Continue
            }
            (KeyCode::Char(c), _) => {
                if app.session_picker.rename_mode {
                    app.session_picker.rename_input_char(c);
                }
                InlineKeyAction::Continue
            }
            _ => InlineKeyAction::Continue,
        };
    }

    match (key.code, key.modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            return InlineKeyAction::Quit;
        }
        (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
            return InlineKeyAction::Quit;
        }
        _ => {}
    }

    if key.code == KeyCode::Esc && app.slash_state.visible {
        app.slash_state.dismiss();
        return InlineKeyAction::Continue;
    }

    if key.code == KeyCode::Tab && app.slash_state.visible {
        if let Some(cmd) = app.slash_state.selected_command(&app.slash_registry) {
            let text = format!("/{} ", cmd);
            app.input.select_all();
            app.input.cut();
            app.input.insert_str(&text);
            app.slash_state.dismiss();
        }
        return InlineKeyAction::Continue;
    }

    if key.code == KeyCode::Up && key.modifiers == KeyModifiers::NONE {
        if app.slash_state.visible {
            if app.slash_state.at_first() {
                app.slash_state.dismiss();
                app.history_up();
            } else {
                app.slash_state.move_up();
            }
        } else if app.should_navigate_history() {
            app.history_up();
        } else {
            app.input.input(Input {
                key: Key::Up,
                ctrl: false,
                alt: false,
                shift: false,
            });
        }
        let input = app.input.lines().join("\n");
        app.slash_state.on_input_change(&input, &app.slash_registry);
        return InlineKeyAction::Continue;
    }
    if key.code == KeyCode::Down && key.modifiers == KeyModifiers::NONE {
        if app.slash_state.visible {
            if app.slash_state.at_last() {
                app.slash_state.dismiss();
                app.history_down();
            } else {
                app.slash_state.move_down();
            }
        } else if app.should_navigate_history() {
            app.history_down();
        } else {
            app.input.input(Input {
                key: Key::Down,
                ctrl: false,
                alt: false,
                shift: false,
            });
        }
        let input = app.input.lines().join("\n");
        app.slash_state.on_input_change(&input, &app.slash_registry);
        return InlineKeyAction::Continue;
    }

    if key.code == KeyCode::PageUp {
        return InlineKeyAction::Continue;
    }
    if key.code == KeyCode::PageDown {
        return InlineKeyAction::Continue;
    }
    if key.code == KeyCode::Up && key.modifiers.contains(KeyModifiers::CONTROL) {
        return InlineKeyAction::Continue;
    }
    if key.code == KeyCode::Down && key.modifiers.contains(KeyModifiers::CONTROL) {
        return InlineKeyAction::Continue;
    }

    if key.code == KeyCode::Enter && key.modifiers == KeyModifiers::NONE {
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
            return InlineKeyAction::Submit;
        }
        return InlineKeyAction::Continue;
    }

    if key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::CONTROL) {
        let text = app.input.lines().join("\n");
        if !text.trim().is_empty() {
            return InlineKeyAction::Submit;
        }
        return InlineKeyAction::Continue;
    }

    let input_before = app.input.lines().join("\n");
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

    let input_after = app.input.lines().join("\n");
    if input_before != input_after {
        app.slash_state
            .on_input_change(&input_after, &app.slash_registry);
    }

    InlineKeyAction::Continue
}
