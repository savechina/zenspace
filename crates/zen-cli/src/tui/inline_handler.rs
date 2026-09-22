use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tui_textarea::{Input, Key};

use super::app::App;

#[derive(Debug, PartialEq, Eq)]
pub enum InlineKeyAction {
    Submit,
    Quit,
    Continue,
    CancelTurn,
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

    // W2: Reverse-i-search mode — ALL keys route here before normal dispatch.
    // Pickers (model_picker, session_picker) are already guarded above.
    if app.history_search.active {
        match (key.code, key.modifiers) {
            // Ctrl+R or Up: cycle to older match
            (KeyCode::Char('r'), KeyModifiers::CONTROL) | (KeyCode::Up, KeyModifiers::NONE) => {
                app.history_search.cycle_older();
                if let Some(m) = app.history_search.current_match() {
                    let text = m.to_string();
                    // BUG-2+L3: Use new_at_end to position cursor at end of match.
                    app.input = App::create_input_textarea_at_end(text);
                    app.input.enter_history_mode();
                }
            }
            // Down: cycle to newer match
            (KeyCode::Down, KeyModifiers::NONE) => {
                app.history_search.cycle_newer();
                if let Some(m) = app.history_search.current_match() {
                    let text = m.to_string();
                    // BUG-2+L3: Use new_at_end to position cursor at end of match.
                    app.input = App::create_input_textarea_at_end(text);
                    app.input.enter_history_mode();
                } else {
                    // No match — restore draft
                    let draft = app.history_search.draft_snapshot.clone();
                    // BUG-2+L3: Use new_at_end to position cursor at end of draft.
                    app.input = App::create_input_textarea_at_end(draft);
                    app.input.exit_mode();
                }
            }
            // Enter: ACCEPT — set textarea to match, exit search.
            // BUG-4: If no match, restore draft (same as Esc/cancel).
            (KeyCode::Enter, _) => {
                if let Some(m) = app.history_search.current_match().map(|s| s.to_string()) {
                    // BUG-2+L3: Use new_at_end to position cursor at end of match.
                    app.input = App::create_input_textarea_at_end(m);
                    app.input.exit_mode();
                } else {
                    // No match — restore draft (bash semantics: failed search leaves line unchanged).
                    let draft = app.history_search.draft_snapshot.clone();
                    app.input = App::create_input_textarea_at_end(draft);
                    app.input.exit_mode();
                }
                app.history_search.exit();
                // Refresh slash state with the RESULTING textarea text.
                let input = app.input.lines().join("\n");
                app.slash_state.on_input_change(&input, &app.slash_registry);
            }
            // Esc: CANCEL — restore draft, exit search
            (KeyCode::Esc, _) => {
                let draft = app.history_search.draft_snapshot.clone();
                app.history_search.exit();
                // BUG-2+L3: Use new_at_end to position cursor at end of draft.
                app.input = App::create_input_textarea_at_end(draft);
                app.input.exit_mode();
            }
            // Ctrl+C: exit search only (do not quit app)
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                let draft = app.history_search.draft_snapshot.clone();
                app.history_search.exit();
                // BUG-2+L3: Use new_at_end to position cursor at end of draft.
                app.input = App::create_input_textarea_at_end(draft);
                app.input.exit_mode();
            }
            // Ctrl+D: exit search (do not quit app)
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                let draft = app.history_search.draft_snapshot.clone();
                app.history_search.exit();
                // BUG-2+L3: Use new_at_end to position cursor at end of draft.
                app.input = App::create_input_textarea_at_end(draft);
                app.input.exit_mode();
            }
            // Backspace: remove char from query; empty query + Backspace → exit
            (KeyCode::Backspace, _) => {
                if app.history_search.query.is_empty() {
                    let draft = app.history_search.draft_snapshot.clone();
                    app.history_search.exit();
                    // BUG-2+L3: Use new_at_end to position cursor at end of draft.
                    app.input = App::create_input_textarea_at_end(draft);
                    app.input.exit_mode();
                } else {
                    app.history_search.backspace(&app.command_history);
                    if let Some(m) = app.history_search.current_match().map(|s| s.to_string()) {
                        // BUG-2+L3: Use new_at_end to position cursor at end of match.
                        app.input = App::create_input_textarea_at_end(m);
                        app.input.enter_history_mode();
                    } else {
                        // No matches — show query in textarea
                        let q = app.history_search.query.clone();
                        // BUG-2+L3: Use new_at_end to position cursor at end of query.
                        app.input = App::create_input_textarea_at_end(q);
                        app.input.enter_history_mode();
                    }
                }
            }
            // Printable char: push to query
            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                app.history_search.push_char(c, &app.command_history);
                if let Some(m) = app.history_search.current_match().map(|s| s.to_string()) {
                    // BUG-2+L3: Use new_at_end to position cursor at end of match.
                    app.input = App::create_input_textarea_at_end(m);
                    app.input.enter_history_mode();
                } else {
                    let q = app.history_search.query.clone();
                    // BUG-2+L3: Use new_at_end to position cursor at end of query.
                    app.input = App::create_input_textarea_at_end(q);
                    app.input.enter_history_mode();
                }
            }
            // Everything else: swallow while search is active
            _ => {}
        }
        return InlineKeyAction::Continue;
    }

    // W2 entry: Ctrl+R in normal context → start reverse-i-search.
    // L2: Guard against slash popup — swallow Ctrl+R while popup owns keys.
    if key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL) {
        if !app.slash_state.visible {
            let current_text = app.input.lines().join("\n");
            app.history_search.enter(&current_text);
            app.history_search.recompute_matches(&app.command_history);
            // Preview the first match (or full history if no query yet)
            if let Some(m) = app.history_search.current_match().map(|s| s.to_string()) {
                // BUG-2+L3: Use new_at_end to position cursor at end of match.
                app.input = App::create_input_textarea_at_end(m);
                app.input.enter_history_mode();
            }
        }
        return InlineKeyAction::Continue;
    }

    match (key.code, key.modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            return InlineKeyAction::Quit;
        }
        (KeyCode::Char('l'), KeyModifiers::CONTROL) => {
            // Ctrl+L: clear screen (same as /clear)
            app.handle_slash_command("clear");
            return InlineKeyAction::Continue;
        }
        (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
            // Ctrl+U: kill line to HEAD (Codex editor.kill_line_start)
            app.input.textarea_mut().delete_line_by_head();
            let input_after = app.input.lines().join("\n");
            app.slash_state
                .on_input_change(&input_after, &app.slash_registry);
            return InlineKeyAction::Continue;
        }
        (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
            // Ctrl+D: conditional — empty input → quit, non-empty → delete char forward
            let text = app.input.lines().join("\n");
            if text.is_empty() {
                return InlineKeyAction::Quit;
            }
            app.input.textarea_mut().delete_next_char();
            let input_after = app.input.lines().join("\n");
            app.slash_state
                .on_input_change(&input_after, &app.slash_registry);
            return InlineKeyAction::Continue;
        }
        _ => {}
    }

    // FR-024: approval popup swallows all keys except y/n/Esc
    if app.approval.is_pending() {
        return match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some((request, _decision)) = app
                    .approval
                    .resolve_current(crate::tui::approval::ApprovalDecision::Approve)
                {
                    // FR-024 production wiring: send approval decision via the gateway
                    // bridge channel. UnboundedSender::send is sync non-blocking.
                    if let Some(tx) = &app.approval_tx {
                        let _ = tx.send(zen_gateway::client::surface::ApprovalResponsePayload {
                            request_id: request.request_id,
                            decision: "approve".to_string(),
                        });
                    }
                    app.show_toast(format!("Approved: {}", request.tool_name));
                }
                InlineKeyAction::Continue
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                if let Some((request, _decision)) = app
                    .approval
                    .resolve_current(crate::tui::approval::ApprovalDecision::Deny)
                {
                    // FR-024 production wiring: send deny decision via gateway bridge.
                    if let Some(tx) = &app.approval_tx {
                        let _ = tx.send(zen_gateway::client::surface::ApprovalResponsePayload {
                            request_id: request.request_id,
                            decision: "deny".to_string(),
                        });
                    }
                    app.show_toast(format!("Denied: {}", request.tool_name));
                }
                InlineKeyAction::Continue
            }
            _ => {
                // All other keys are swallowed while approval is pending
                InlineKeyAction::Continue
            }
        };
    }

    if key.code == KeyCode::Esc {
        if app.slash_state.visible {
            app.slash_state.dismiss();
            return InlineKeyAction::Continue;
        }
        if app.is_streaming {
            return InlineKeyAction::CancelTurn;
        }
        // Esc while idle — no-op (fall through to catch-all which forwards to textarea)
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

    // A1: Up and Ctrl+P — slash popup navigation (wrap, no stomp)
    if (key.code == KeyCode::Up && key.modifiers == KeyModifiers::NONE)
        || (key.code == KeyCode::Char('p') && key.modifiers.contains(KeyModifiers::CONTROL))
    {
        if app.slash_state.visible {
            app.slash_state.move_up();
            return InlineKeyAction::Continue;
        }
        // Plain Up: history navigation; Ctrl+P without popup: no-op
        if key.modifiers == KeyModifiers::NONE {
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
            let input = app.input.lines().join("\n");
            app.slash_state.on_input_change(&input, &app.slash_registry);
        }
        return InlineKeyAction::Continue;
    }
    // A2: Down and Ctrl+N — slash popup navigation (wrap, no stomp)
    if (key.code == KeyCode::Down && key.modifiers == KeyModifiers::NONE)
        || (key.code == KeyCode::Char('n') && key.modifiers.contains(KeyModifiers::CONTROL))
    {
        if app.slash_state.visible {
            app.slash_state.move_down();
            return InlineKeyAction::Continue;
        }
        // Plain Down: history navigation; Ctrl+N without popup: no-op
        if key.modifiers == KeyModifiers::NONE {
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
            let input = app.input.lines().join("\n");
            app.slash_state.on_input_change(&input, &app.slash_registry);
        }
        return InlineKeyAction::Continue;
    }

    // T062: keyboard reading mode. While active, scrollback inserts are
    // deferred so the view does not jump mid-read (see inline_tick).
    if key.code == KeyCode::PageUp && key.modifiers == KeyModifiers::NONE {
        app.reading_mode = true;
        return InlineKeyAction::Continue;
    }
    if (key.code == KeyCode::PageDown || key.code == KeyCode::End)
        && key.modifiers == KeyModifiers::NONE
        && app.reading_mode
    {
        app.exit_reading_mode();
        return InlineKeyAction::Continue;
    }
    if key.code == KeyCode::PageDown {
        return InlineKeyAction::Continue;
    }

    if key.code == KeyCode::Enter && key.modifiers == KeyModifiers::SHIFT {
        app.input.input(Input {
            key: Key::Enter,
            ctrl: false,
            alt: false,
            shift: true,
        });
        return InlineKeyAction::Continue;
    }

    if key.code == KeyCode::Enter && key.modifiers == KeyModifiers::NONE {
        let text = app.input.lines().join("\n");
        let is_empty = text.trim().is_empty();
        if is_empty {
            app.input.input(Input {
                key: Key::Enter,
                ctrl: false,
                alt: false,
                shift: false,
            });
            return InlineKeyAction::Continue;
        }
        // A4: Execute selected slash command on Enter
        if app.slash_state.visible
            && let Some(cmd) = app.slash_state.selected_command(&app.slash_registry)
        {
            let text = format!("/{}", cmd);
            app.input.select_all();
            app.input.cut();
            app.input.insert_str(&text);
            app.slash_state.dismiss();
            return InlineKeyAction::Submit;
        }
        return InlineKeyAction::Submit;
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

#[cfg(test)]
mod tests {
    //! Inline mode keybinding regression tests (mirrors the full-screen
    //! handler tests in `handler.rs`, adapted to `InlineKeyAction`).
    use super::*;
    use crate::tui::app::App;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn test_app() -> App {
        let config: &'static zen_core::config::ZenConfig = Box::leak(Box::default());
        App::new(config)
    }

    fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> InlineKeyAction {
        handle_key(KeyEvent::new(code, modifiers), app)
    }

    /// Slash popup: Down moves the selection and it PERSISTS (regression —
    /// the old code re-ran on_input_change after every arrow press, stomping
    /// selected back to 0, so arrows could never select).
    #[test]
    fn slash_popup_arrow_keys_move_selection_and_persist() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
        assert!(app.slash_state.visible, "popup visible for /");
        assert_eq!(app.slash_state.selected, 0);
        let len = app.slash_state.filtered_indices.len();
        assert!(len > 1, "registry must have multiple commands");

        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.slash_state.selected, 1, "Down must move and persist");
        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.slash_state.selected, 2);
        press(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.slash_state.selected, 1, "Up must move and persist");

        // Codex semantics: wrap at both ends.
        app.slash_state.selected = len - 1;
        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.slash_state.selected, 0, "Down wraps to first");
        press(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.slash_state.selected, len - 1, "Up wraps to last");
    }

    /// Ctrl+N / Ctrl+P are popup-navigation aliases (Codex parity).
    #[test]
    fn slash_popup_ctrl_n_ctrl_p_move_selection() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
        assert_eq!(app.slash_state.selected, 1);
        press(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert_eq!(app.slash_state.selected, 0);
    }

    /// Enter with a visible popup executes the SELECTED command (Codex
    /// semantics) — the buffer is rewritten to the full command and submitted.
    #[test]
    fn slash_popup_enter_executes_selected_command() {
        let mut app = test_app();
        for c in "/exi".chars() {
            press(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert!(app.slash_state.visible);
        let action = press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(action, InlineKeyAction::Submit);
        assert_eq!(
            app.input.lines().join("\n"),
            "/exit",
            "Enter must execute the selected command"
        );
    }

    /// Enter with a visible but empty popup falls through: raw buffer is
    /// submitted unchanged (handle_command reports the unknown command).
    #[test]
    fn slash_popup_enter_no_match_submits_raw_buffer() {
        let mut app = test_app();
        for c in "/zzz".chars() {
            press(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert!(app.slash_state.visible);
        assert!(app.slash_state.filtered_indices.is_empty());
        let action = press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(action, InlineKeyAction::Submit);
        assert_eq!(app.input.lines().join("\n"), "/zzz");
    }

    #[test]
    fn shift_enter_inserts_newline_not_submit() {
        let mut app = test_app();
        app.input.insert_str("hello");
        let action = press(&mut app, KeyCode::Enter, KeyModifiers::SHIFT);
        assert_eq!(action, InlineKeyAction::Continue);
        assert_eq!(app.input.lines(), vec!["hello", ""]);
    }

    #[test]
    fn enter_submits_single_line() {
        let mut app = test_app();
        app.input.insert_str("hello");
        let action = press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(action, InlineKeyAction::Submit);
    }

    #[test]
    fn ctrl_enter_submits_multiline() {
        let mut app = test_app();
        app.input.insert_str("line1\nline2");
        let action = press(&mut app, KeyCode::Enter, KeyModifiers::CONTROL);
        assert_eq!(action, InlineKeyAction::Submit);
    }

    // === W1: History Draft Preservation ===

    #[test]
    fn w1_draft_preserved_on_history_down_past_end() {
        let mut app = test_app();
        app.command_history = vec!["first".into(), "second".into()];
        // Start with empty input (clean state), Up into history
        press(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.input.lines().join("\n"), "second");
        // Down past end — should restore the empty draft
        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.input.lines().join("\n"), "");
        assert!(app.history_position.is_none());
    }

    #[test]
    fn w1_draft_preserved_with_nonempty_initial_text() {
        let mut app = test_app();
        app.command_history = vec!["first".into(), "second".into()];
        // Simulate: user types "hello", submits (clears), types "world",
        // presses Up. In the real flow, after submit the input is empty.
        // But if user is mid-compose and hits Up, the should_navigate_history
        // gate blocks unless text matches last_recalled. So we test the path
        // where Up is reached via empty input.
        app.input = App::create_input_textarea("");
        press(&mut app, KeyCode::Up, KeyModifiers::NONE);
        // history_draft should have been snapshot (empty string)
        assert_eq!(app.history_draft.as_deref(), Some(""));
        assert_eq!(app.input.lines().join("\n"), "second");
        // Down past end
        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
        // Draft (empty) restored
        assert_eq!(app.input.lines().join("\n"), "");
        assert!(app.history_position.is_none());
        assert!(app.history_draft.is_none());
    }

    #[test]
    fn w1_submit_clears_draft_and_position() {
        let mut app = test_app();
        app.command_history = vec!["old cmd".into()];
        // Start empty, Up into history
        press(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert!(app.history_position.is_some());
        assert!(app.history_draft.is_some());
        // Submit via push_history
        app.push_history("submitted");
        assert!(app.history_position.is_none());
        assert!(app.history_draft.is_none());
    }

    #[test]
    fn w1_empty_draft_when_no_history_browse() {
        let mut app = test_app();
        app.command_history = vec!["old".into()];
        // Down without Up first — history_position is None, nothing happens
        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert!(app.history_position.is_none());
        // Draft is None (never set)
        assert!(app.history_draft.is_none());
    }

    // === W2: Ctrl+R Reverse History Search ===

    #[test]
    fn w2_ctrl_r_enters_search_mode() {
        let mut app = test_app();
        app.command_history = vec!["alpha".into(), "beta".into(), "gamma".into()];
        app.input = App::create_input_textarea("current draft");
        press(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert!(app.history_search.active);
        // Preview should show newest match (gamma)
        assert_eq!(app.input.lines().join("\n"), "gamma");
        // Draft snapshot preserved
        assert_eq!(app.history_search.draft_snapshot, "current draft");
    }

    #[test]
    fn w2_search_char_filters_matches() {
        let mut app = test_app();
        app.command_history = vec!["alpha".into(), "beta".into(), "gamma".into()];
        app.input = App::create_input_textarea("");
        press(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        // Type 'a' — should filter to entries containing 'a'
        press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(app.history_search.query, "a");
        // gamma (newest), beta, alpha all contain 'a'
        assert_eq!(app.history_search.matches.len(), 3);
        assert_eq!(app.history_search.current_match(), Some("gamma"));
    }

    #[test]
    fn w2_search_enter_accepts_match() {
        let mut app = test_app();
        app.command_history = vec!["alpha".into(), "beta".into()];
        app.input = App::create_input_textarea("draft");
        press(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        press(&mut app, KeyCode::Char('b'), KeyModifiers::NONE);
        // Accept
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(!app.history_search.active);
        assert_eq!(app.input.lines().join("\n"), "beta");
    }

    /// EDGE-4: Enter on no-match search restores draft (bash semantics).
    #[test]
    fn w2_search_enter_no_match_restores_draft() {
        let mut app = test_app();
        app.command_history = vec!["alpha".into()];
        app.input = App::create_input_textarea("my draft");
        press(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        // Type 'z' — no match
        press(&mut app, KeyCode::Char('z'), KeyModifiers::NONE);
        assert!(app.history_search.current_match().is_none());
        // Enter — should restore draft, not accept query
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(!app.history_search.active);
        assert_eq!(app.input.lines().join("\n"), "my draft");
        // Input mode should be Default (not History)
        assert_eq!(
            app.input.effective_mode(),
            crate::tui::app::InputMode::Default
        );
    }

    #[test]
    fn w2_search_esc_cancels_restores_draft() {
        let mut app = test_app();
        app.command_history = vec!["alpha".into()];
        app.input = App::create_input_textarea("my draft");
        press(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        // Cancel
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.history_search.active);
        assert_eq!(app.input.lines().join("\n"), "my draft");
    }

    #[test]
    fn w2_ctrl_c_in_search_exits_search_not_app() {
        let mut app = test_app();
        app.command_history = vec!["alpha".into()];
        app.input = App::create_input_textarea("draft");
        let action = press(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(action, InlineKeyAction::Continue);
        assert!(app.history_search.active);
        let action = press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(action, InlineKeyAction::Continue); // NOT Quit
        assert!(!app.history_search.active);
    }

    #[test]
    fn w2_ctrl_d_in_search_exits_search_not_app() {
        let mut app = test_app();
        app.command_history = vec!["alpha".into()];
        app.input = App::create_input_textarea("draft");
        press(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert!(app.history_search.active);
        let action = press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert_eq!(action, InlineKeyAction::Continue); // NOT Quit
        assert!(!app.history_search.active);
    }

    #[test]
    fn w2_ctrl_r_when_no_history_enters_search_empty() {
        let mut app = test_app();
        app.command_history.clear(); // ensure empty
        app.input = App::create_input_textarea("draft");
        press(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert!(app.history_search.active);
        assert!(app.history_search.matches.is_empty());
        assert_eq!(app.history_search.query, "");
    }

    #[test]
    fn w2_search_backspace_empty_query_exits() {
        let mut app = test_app();
        app.command_history = vec!["alpha".into()];
        app.input = App::create_input_textarea("draft");
        press(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        // Empty query + Backspace → exit
        press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
        assert!(!app.history_search.active);
        assert_eq!(app.input.lines().join("\n"), "draft");
    }

    #[test]
    fn w2_ctrl_r_while_picker_visible_does_not_enter_search() {
        let mut app = test_app();
        app.command_history = vec!["alpha".into()];
        app.model_picker.visible = true;
        let action = press(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(action, InlineKeyAction::Continue);
        // Search should NOT be active — picker owns the keys
        assert!(!app.history_search.active);
    }

    // === Esc-to-interrupt (CancelTurn) ===

    #[test]
    fn esc_while_streaming_returns_cancel_turn() {
        let mut app = test_app();
        app.is_streaming = true;
        let action = press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(action, InlineKeyAction::CancelTurn);
    }

    #[test]
    fn esc_while_idle_returns_continue() {
        let mut app = test_app();
        app.is_streaming = false;
        let action = press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(action, InlineKeyAction::Continue);
    }

    #[test]
    fn esc_with_slash_popup_dismisses_popup_not_cancel() {
        let mut app = test_app();
        app.is_streaming = true;
        app.slash_state.visible = true;
        let action = press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        // Esc dismisses popup first, even while streaming
        assert_eq!(action, InlineKeyAction::Continue);
        assert!(!app.slash_state.visible);
    }

    #[test]
    fn esc_with_session_picker_dismisses_picker() {
        let mut app = test_app();
        app.is_streaming = true;
        app.session_picker.visible = true;
        let action = press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(action, InlineKeyAction::Continue);
        assert!(!app.session_picker.visible);
    }

    #[test]
    fn esc_with_model_picker_dismisses_picker() {
        let mut app = test_app();
        app.is_streaming = true;
        app.model_picker.visible = true;
        let action = press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(action, InlineKeyAction::Continue);
        assert!(!app.model_picker.visible);
    }

    // === Ctrl+L ===

    #[test]
    fn ctrl_l_clears_output() {
        let mut app = test_app();
        app.push_output("some output".to_string(), false);
        assert!(!app.output.is_empty());
        let action = press(&mut app, KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert_eq!(action, InlineKeyAction::Continue);
        assert!(app.output.is_empty(), "Ctrl+L must clear output");
    }

    // === Ctrl+U ===

    #[test]
    fn ctrl_u_kills_line_to_head() {
        let mut app = test_app();
        app.input = App::create_input_textarea("hello world");
        // Move cursor to end
        app.input
            .textarea_mut()
            .move_cursor(tui_textarea::CursorMove::End);
        let action = press(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(action, InlineKeyAction::Continue);
        assert_eq!(
            app.input.lines().join(""),
            "",
            "Ctrl+U must kill line to head"
        );
    }

    #[test]
    fn ctrl_u_at_start_joins_lines() {
        let mut app = test_app();
        app.input = App::create_input_textarea("line1\nline2");
        // Cursor starts at position (0, 0)
        let action = press(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(action, InlineKeyAction::Continue);
        // At start of line, delete_line_by_head joins with previous line
        // For a 2-line input starting at line 0 col 0, it removes the newline
        let text = app.input.lines().join("\n");
        assert!(
            text.contains("line1") || text.contains("line2"),
            "Ctrl+U at start must handle newline: got {text}"
        );
    }

    // === Ctrl+D conditional ===

    #[test]
    fn ctrl_d_empty_input_quits() {
        let mut app = test_app();
        app.input = App::create_input_textarea("");
        let action = press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert_eq!(action, InlineKeyAction::Quit);
    }

    #[test]
    fn ctrl_d_nonempty_deletes_forward() {
        let mut app = test_app();
        app.input = App::create_input_textarea("hello");
        // Cursor starts at (0, 0); delete_next_char removes 'h'
        let action = press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert_eq!(action, InlineKeyAction::Continue);
        assert_eq!(
            app.input.lines().join(""),
            "ello",
            "Ctrl+D must delete char forward"
        );
    }

    #[test]
    fn ctrl_d_at_end_of_text_deletes_newline_or_noop() {
        let mut app = test_app();
        app.input = App::create_input_textarea("hi");
        app.input
            .textarea_mut()
            .move_cursor(tui_textarea::CursorMove::End);
        let action = press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert_eq!(action, InlineKeyAction::Continue);
        // At end of single line, delete_next_char is a noop
        assert_eq!(app.input.lines().join(""), "hi");
    }

    #[test]
    fn ctrl_d_multiline_at_end_of_first_line_joins() {
        let mut app = test_app();
        app.input = App::create_input_textarea("line1\nline2");
        // Move to end of first line (before newline)
        app.input
            .textarea_mut()
            .move_cursor(tui_textarea::CursorMove::End);
        let action = press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert_eq!(action, InlineKeyAction::Continue);
        // delete_next_char at end of line removes the newline, joining lines
        let text = app.input.lines().join("\n");
        assert!(
            text.contains("line1") && text.contains("line2"),
            "Ctrl+D at line end must join lines: got {text}"
        );
    }

    // === Multi-line Enter submit (FIX 1) ===

    #[test]
    fn enter_submits_multiline_buffer() {
        let mut app = test_app();
        app.input.insert_str("hello\nworld");
        let action = press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(action, InlineKeyAction::Submit);
    }

    #[test]
    fn enter_submits_multiline_with_trailing_slash_command() {
        let mut app = test_app();
        app.input.insert_str("hello\n/exit");
        let action = press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(action, InlineKeyAction::Submit);
    }

    #[test]
    fn enter_submit_multiline_does_not_insert_newline() {
        let mut app = test_app();
        app.input.insert_str("line1\nline2");
        let action = press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(action, InlineKeyAction::Submit);
        // Buffer must still be exactly the original text (no extra newline inserted)
        assert_eq!(app.input.lines().join("\n"), "line1\nline2");
    }
}
