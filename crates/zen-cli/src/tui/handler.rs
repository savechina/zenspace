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
        }
        (KeyCode::Char('d'), KeyModifiers::CONTROL) => return KeyAction::Quit,
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return KeyAction::Quit,
        (KeyCode::Up, KeyModifiers::NONE) => {
            app.history_up();
            app.autocomplete_suggestions.clear();
            app.show_autocomplete = false;
            return KeyAction::Continue;
        }
        (KeyCode::Down, KeyModifiers::NONE) => {
            app.history_down();
            app.autocomplete_suggestions.clear();
            app.show_autocomplete = false;
            return KeyAction::Continue;
        }
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
        }
        (KeyCode::Char(c), KeyModifiers::NONE) => {
            let is_cjk = ('\u{4E00}'..='\u{9FFF}').contains(&c)
                || ('\u{3040}'..='\u{30FF}').contains(&c)
                || ('\u{AC00}'..='\u{D7AF}').contains(&c);

            if app.ime_preedit.is_some() {
                if is_cjk {
                    let preedit = app.ime_preedit.take().unwrap();
                    let preedit_start = app.ime_preedit_start;
                    let preedit_end = preedit_start + preedit.len();

                    if preedit_end <= app.input.len()
                        && app.input[preedit_start..preedit_end] == preedit
                    {
                        app.input.replace_range(preedit_start..preedit_end, "");
                    }

                    app.input.insert_str(preedit_start, &preedit);
                    app.cursor_position = app.input.chars().count();

                    let byte_pos = app.input.len();
                    app.input.insert(byte_pos, c);
                    app.cursor_position = app.input.chars().count();

                    app.ime_preedit = None;
                } else if !c.is_ascii_lowercase() {
                    let preedit = app.ime_preedit.take().unwrap();
                    let preedit_start = app.ime_preedit_start;

                    app.input.insert_str(preedit_start, &preedit);
                    app.cursor_position = app.input.chars().count();
                    app.ime_preedit = None;

                    let byte_pos = app
                        .input
                        .char_indices()
                        .nth(app.cursor_position)
                        .map(|(i, _)| i)
                        .unwrap_or(app.input.len());
                    app.input.insert(byte_pos, c);
                    app.cursor_position += 1;
                    app.update_autocomplete();
                } else {
                    app.ime_preedit.as_mut().unwrap().push(c);
                }
            } else {
                let byte_pos = app
                    .input
                    .char_indices()
                    .nth(app.cursor_position)
                    .map(|(i, _)| i)
                    .unwrap_or(app.input.len());
                app.input.insert(byte_pos, c);
                app.cursor_position += 1;
                app.update_autocomplete();
            }
        }
        (KeyCode::Backspace, KeyModifiers::NONE) if app.cursor_position > 0 => {
            // Handle IME composition backspace
            if app.ime_preedit.is_some() {
                let preedit = app.ime_preedit.as_mut().unwrap();
                if !preedit.is_empty() {
                    preedit.pop();
                    if preedit.is_empty() {
                        app.ime_preedit = None;
                    }
                }
            } else {
                app.cursor_position -= 1;
                let byte_pos = app
                    .input
                    .char_indices()
                    .nth(app.cursor_position)
                    .map(|(i, _)| i)
                    .unwrap_or(app.input.len());
                app.input.remove(byte_pos);
                app.update_autocomplete();
            }
        }
        (KeyCode::Delete, KeyModifiers::NONE)
            if app.cursor_position < app.input.chars().count() =>
        {
            let byte_pos = app
                .input
                .char_indices()
                .nth(app.cursor_position)
                .map(|(i, _)| i)
                .unwrap_or(app.input.len());
            app.input.remove(byte_pos);
            app.update_autocomplete();
        }
        (KeyCode::Left, KeyModifiers::CONTROL) => {
            let char_count = app.cursor_position;
            let before: String = app.input.chars().take(char_count).collect();
            let new_char_pos = before
                .trim_end_matches(|c: char| !c.is_alphanumeric())
                .trim_end_matches(|c: char| c.is_alphanumeric())
                .chars()
                .count();
            app.cursor_position = new_char_pos;
        }
        (KeyCode::Right, KeyModifiers::CONTROL) => {
            let after: String = app.input.chars().skip(app.cursor_position).collect();
            let skip_non_alpha = after
                .find(|c: char| c.is_alphanumeric())
                .map(|i| i + 1)
                .unwrap_or(after.chars().count());
            let rest: String = after.chars().skip(skip_non_alpha).collect();
            let skip_alpha = rest
                .find(|c: char| !c.is_alphanumeric())
                .unwrap_or(rest.chars().count());
            app.cursor_position += skip_non_alpha + skip_alpha;
        }
        (KeyCode::Left, KeyModifiers::NONE) if app.cursor_position > 0 => {
            app.cursor_position -= 1;
        }
        (KeyCode::Right, KeyModifiers::NONE) if app.cursor_position < app.input.chars().count() => {
            app.cursor_position += 1;
        }
        (KeyCode::Home, _) => app.cursor_position = 0,
        (KeyCode::End, _) => app.cursor_position = app.input.chars().count(),
        (KeyCode::Esc, _) => {
            app.autocomplete_suggestions.clear();
            app.show_autocomplete = false;
            // Cancel IME composition on Escape
            app.ime_preedit = None;
        }
        _ => {}
    }
    KeyAction::Continue
}

/// Handle Event::Paste (IME commit from terminal)
pub fn handle_paste(pasted: &str, app: &mut super::app::App) {
    // Detect CJK characters in pasted text (IME commit)
    let has_cjk = pasted.chars().any(|c| {
        ('\u{4E00}'..='\u{9FFF}').contains(&c) // CJK Unified Ideographs
            || ('\u{3040}'..='\u{30FF}').contains(&c) // Hiragana/Katakana
            || ('\u{AC00}'..='\u{D7AF}').contains(&c) // Hangul
    });

    if has_cjk && app.ime_preedit.is_some() {
        // IME commit: replace preedit with committed text
        let preedit = app.ime_preedit.take().unwrap();
        let preedit_start = app.ime_preedit_start;

        // Remove preedit text if it was inserted
        let preedit_end = preedit_start + preedit.len();
        if preedit_end <= app.input.len() && app.input[preedit_start..preedit_end] == preedit {
            app.input.replace_range(preedit_start..preedit_end, "");
        }

        app.input.insert_str(preedit_start, pasted);

        app.cursor_position = app.input.chars().count();
    } else {
        // Normal paste: insert at cursor position
        let byte_pos = app
            .input
            .char_indices()
            .nth(app.cursor_position)
            .map(|(i, _)| i)
            .unwrap_or(app.input.len());
        app.input.insert_str(byte_pos, pasted);
        app.cursor_position += pasted.chars().count();
    }
}
