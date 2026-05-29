use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub enum KeyAction {
    Submit,
    Quit,
    Continue,
}

pub fn handle_key(key: KeyEvent, app: &mut super::app::App) -> KeyAction {
    match (key.code, key.modifiers) {
        (KeyCode::Enter, _) => {
            if app.show_autocomplete {
                app.autocomplete_accept();
                return KeyAction::Continue;
            }
            return KeyAction::Submit;
        },
        (KeyCode::Char('d'), KeyModifiers::CONTROL) => return KeyAction::Quit,
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return KeyAction::Quit,
        (KeyCode::Up, KeyModifiers::NONE) => {
            app.history_up();
            app.autocomplete_suggestions.clear();
            app.show_autocomplete = false;
            return KeyAction::Continue;
        },
        (KeyCode::Down, KeyModifiers::NONE) => {
            app.history_down();
            app.autocomplete_suggestions.clear();
            app.show_autocomplete = false;
            return KeyAction::Continue;
        },
        (KeyCode::Tab, KeyModifiers::NONE) => {
            if app.show_autocomplete {
                app.autocomplete_cycle();
            } else {
                app.update_autocomplete();
                if app.autocomplete_suggestions.len() == 1 {
                    app.autocomplete_accept();
                }
            }
            return KeyAction::Continue;
        },
        (KeyCode::Char(c), KeyModifiers::NONE) => {
            app.input.insert(app.cursor_position, c);
            app.cursor_position += 1;
            app.update_autocomplete();
        },
        (KeyCode::Backspace, KeyModifiers::NONE) if app.cursor_position > 0 => {
            app.input.remove(app.cursor_position - 1);
            app.cursor_position -= 1;
            app.update_autocomplete();
        },
        (KeyCode::Delete, KeyModifiers::NONE) if app.cursor_position < app.input.len() => {
            app.input.remove(app.cursor_position);
            app.update_autocomplete();
        },
        (KeyCode::Left, KeyModifiers::CONTROL) => {
            let before = &app.input[..app.cursor_position];
            let new_pos = before
                .trim_end_matches(|c: char| !c.is_alphanumeric())
                .trim_end_matches(|c: char| c.is_alphanumeric())
                .len();
            app.cursor_position = new_pos;
        },
        (KeyCode::Right, KeyModifiers::CONTROL) => {
            let after = &app.input[app.cursor_position..];
            let skip_non_alpha = after
                .find(|c: char| c.is_alphanumeric())
                .map(|i| i + 1)
                .unwrap_or(after.len());
            let rest = &after[skip_non_alpha..];
            let skip_alpha = rest
                .find(|c: char| !c.is_alphanumeric())
                .unwrap_or(rest.len());
            app.cursor_position += skip_non_alpha + skip_alpha;
        },
        (KeyCode::Left, KeyModifiers::NONE) if app.cursor_position > 0 => {
            app.cursor_position -= 1;
        },
        (KeyCode::Right, KeyModifiers::NONE) if app.cursor_position < app.input.len() => {
            app.cursor_position += 1;
        },
        (KeyCode::Home, _) => app.cursor_position = 0,
        (KeyCode::End, _) => app.cursor_position = app.input.len(),
        (KeyCode::Esc, _) => {
            app.autocomplete_suggestions.clear();
            app.show_autocomplete = false;
        },
        _ => {},
    }
    KeyAction::Continue
}
