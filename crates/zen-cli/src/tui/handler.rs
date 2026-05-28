use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub enum KeyAction {
    Submit,
    Quit,
    Continue,
}

pub fn handle_key(key: KeyEvent, app: &mut super::app::App) -> KeyAction {
    match (key.code, key.modifiers) {
        (KeyCode::Enter, _) => return KeyAction::Submit,
        (KeyCode::Char('d'), KeyModifiers::CONTROL) => return KeyAction::Quit,
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return KeyAction::Quit,
        (KeyCode::Char(c), KeyModifiers::NONE) => {
            app.input.insert(app.cursor_position, c);
            app.cursor_position += 1;
        },
        (KeyCode::Backspace, KeyModifiers::NONE) if app.cursor_position > 0 => {
            app.input.remove(app.cursor_position - 1);
            app.cursor_position -= 1;
        },
        (KeyCode::Delete, KeyModifiers::NONE) if app.cursor_position < app.input.len() => {
            app.input.remove(app.cursor_position);
        },
        (KeyCode::Left, KeyModifiers::CONTROL) => {
            // Jump to previous word boundary
            let before = &app.input[..app.cursor_position];
            let new_pos = before
                .trim_end_matches(|c: char| !c.is_alphanumeric())
                .trim_end_matches(|c: char| c.is_alphanumeric())
                .len();
            app.cursor_position = new_pos;
        },
        (KeyCode::Right, KeyModifiers::CONTROL) => {
            // Jump to next word boundary
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
        _ => {},
    }
    KeyAction::Continue
}
