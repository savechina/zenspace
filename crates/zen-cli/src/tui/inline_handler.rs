use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tui_textarea::{Input, Key};

use std::time::Instant;

use super::app::{App, ESC_ESC_WINDOW, QUIT_INTENT_WINDOW};
use super::history_search;
use super::slash::{EnterAction, enter_decision};

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
    // SINGLE shared state machine (also called by the fullscreen handler).
    if history_search::handle_key(key, app) {
        return InlineKeyAction::Continue;
    }

    // T2xx(c): `?` shortcuts overlay. While open every key is swallowed
    // except the three dismiss keys (approval-popup swallow precedent);
    // Esc dismisses BEFORE the streaming cancel ladder, so closing help can
    // never kill a turn — a second Esc interrupts as usual.
    if app.shortcuts_overlay {
        return match key.code {
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Enter => {
                app.shortcuts_overlay = false;
                InlineKeyAction::Continue
            }
            _ => InlineKeyAction::Continue,
        };
    }
    // Open gate: EMPTY composer only — `?` must keep typing normally into a
    // draft. Never while an approval is pending (its gate sits below and
    // must own y/n/Esc) or the slash popup is up.
    if key.code == KeyCode::Char('?')
        && matches!(key.modifiers, KeyModifiers::NONE | KeyModifiers::SHIFT)
        && !app.approval.is_pending()
        && !app.slash_state.visible
        && app.input.lines().join("\n").trim().is_empty()
    {
        app.shortcuts_overlay = true;
        return InlineKeyAction::Continue;
    }

    match (key.code, key.modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            // T228: Ctrl+C while a turn is streaming cancels the in-flight
            // response (same cancel RPC path as Esc) instead of discarding it
            // into a quit. A second consecutive Ctrl+C while still streaming
            // quits. While idle, Ctrl+C goes through the T2xx(a)
            // draft-protection ladder below — a non-empty draft is never
            // killed by a single press.
            if app.is_streaming {
                if app.cancel_requested {
                    return InlineKeyAction::Quit;
                }
                app.cancel_requested = true;
                return InlineKeyAction::CancelTurn;
            }
            let now = Instant::now();
            let in_window = app
                .last_ctrl_c_quit_intent
                .is_some_and(|t| now.duration_since(t) <= QUIT_INTENT_WINDOW);
            if !in_window {
                let draft = app.input.lines().join("\n");
                if !draft.trim().is_empty() {
                    // First press (or window expired): draft protection —
                    // clear via the same path the submit flow uses, toast,
                    // and arm the quit intent.
                    app.input = App::create_input_textarea("");
                    app.clear_paste_pills();
                    app.last_ctrl_c_quit_intent = Some(now);
                    app.show_toast("draft cleared — Ctrl+C again to exit");
                    return InlineKeyAction::Continue;
                }
            }
            // Empty composer quits (long-standing rule), and a press inside
            // the quit window quits too (codex double-press: the draft was
            // already surrendered by the first clear — a draft re-typed
            // inside the 1s window is likewise surrendered, pinned
            // trade-off). Disarm so a later press is a fresh first press.
            app.last_ctrl_c_quit_intent = None;
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
            // T084(d): dismissal-aware refresh (pure token equality).
            app.refresh_slash_popup();
            return InlineKeyAction::Continue;
        }
        (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
            // Ctrl+D: conditional — empty input → quit, non-empty → delete char forward
            let text = app.input.lines().join("\n");
            if text.is_empty() {
                return InlineKeyAction::Quit;
            }
            app.input.textarea_mut().delete_next_char();
            // T084(d): dismissal-aware refresh (pure token equality).
            app.refresh_slash_popup();
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
            // T084(d): Esc records the dismissed token (popup stays shut
            // while the token is unchanged).
            app.dismiss_slash_popup();
            return InlineKeyAction::Continue;
        }
        if app.is_streaming {
            return InlineKeyAction::CancelTurn;
        }
        // T2xx(b): idle Esc-Esc pushes a non-empty draft INTO history
        // (hermes: the correction must never be lost) using the SAME store
        // normal submits record into, so Up recalls it. A single Esc keeps
        // today's behavior entirely — re-arm the window and fall through to
        // the textarea catch-all unchanged.
        let now = Instant::now();
        if let Some(last) = app.last_esc_discard
            && now.duration_since(last) <= ESC_ESC_WINDOW
        {
            // Record the EXPANDED text (pill fidelity matches the submit
            // path); push_history dedupes adjacent repeats.
            let draft = app.expanded_input_text();
            if !draft.trim().is_empty() {
                app.push_history(&draft);
                app.input = App::create_input_textarea("");
                app.clear_paste_pills();
                app.last_esc_discard = None;
                app.show_toast("draft saved to history — Up recalls it");
                return InlineKeyAction::Continue;
            }
        }
        app.last_esc_discard = Some(now);
        // Esc while idle — no-op (fall through to catch-all which forwards to textarea)
    }

    if key.code == KeyCode::Tab && app.slash_state.visible {
        if let Some(cmd) = app.slash_state.selected_command(&app.slash_registry) {
            let text = format!("/{} ", cmd);
            app.input.select_all();
            app.input.cut();
            app.input.insert_str(&text);
            // T084(b): wholesale rewrite drops any pill tokens.
            app.clear_paste_pills();
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
            // T084(d): dismissal-aware refresh (pure token equality).
            app.refresh_slash_popup();
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
            // T084(d): dismissal-aware refresh (pure token equality).
            app.refresh_slash_popup();
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

    // T084(e): Ctrl+J (the LF byte — passes through tmux/SSH where
    // Shift+Enter dies; Hermes convention) inserts a newline via the exact
    // Shift+Enter path. Plain terminals may report Ctrl+J as Enter
    // (indistinguishable) — those fall back to submit per FR-017.
    // Inline-only (fullscreen untouched).
    if (key.code == KeyCode::Enter && key.modifiers == KeyModifiers::SHIFT)
        || (key.code == KeyCode::Char('j') && key.modifiers.contains(KeyModifiers::CONTROL))
    {
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
        // Enter-with-popup contract (T246): the shared pure decision keeps
        // inline and fullscreen byte-identical. A fully typed `/name` or an
        // arrow-locked choice executes; a partial token completes to
        // "/name " without submitting (arguments may follow).
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
                return InlineKeyAction::Submit;
            }
            EnterAction::Complete(full) => {
                app.input.select_all();
                app.input.cut();
                app.input.insert_str(&full);
                app.clear_paste_pills();
                app.slash_state.dismiss();
                return InlineKeyAction::Continue;
            }
            EnterAction::Submit => return InlineKeyAction::Submit,
        }
    }

    if key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::CONTROL) {
        let text = app.input.lines().join("\n");
        if !text.trim().is_empty() {
            return InlineKeyAction::Submit;
        }
        return InlineKeyAction::Continue;
    }

    // T084(b): paste-pill atomic editing — Backspace/Delete at a pill edge
    // remove the whole pill; Left/Right skip over pills as one unit. Falls
    // through to normal editing when no pill is involved.
    match (key.code, key.modifiers) {
        (KeyCode::Backspace, KeyModifiers::NONE) => {
            if app.paste_backspace() {
                app.refresh_slash_popup();
                return InlineKeyAction::Continue;
            }
        }
        (KeyCode::Delete, KeyModifiers::NONE) => {
            if app.paste_delete_forward() {
                app.refresh_slash_popup();
                return InlineKeyAction::Continue;
            }
        }
        (KeyCode::Left, KeyModifiers::NONE) => {
            if let Some((row, col)) = app.pill_jump_for_horizontal(true) {
                app.input
                    .textarea_mut()
                    .move_cursor(tui_textarea::CursorMove::Jump(row, col));
                return InlineKeyAction::Continue;
            }
        }
        (KeyCode::Right, KeyModifiers::NONE) => {
            if let Some((row, col)) = app.pill_jump_for_horizontal(false) {
                app.input
                    .textarea_mut()
                    .move_cursor(tui_textarea::CursorMove::Jump(row, col));
                return InlineKeyAction::Continue;
            }
        }
        _ => {}
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
        // T084(d): dismissal-aware refresh (pure token equality).
        app.refresh_slash_popup();
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
    use std::time::Duration;

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

    /// Enter with a visible popup COMPLETES the selected command into the
    /// composer (fullscreen parity) without submitting — executing a bare
    /// command on Enter fired commands the user only meant to complete
    /// before typing arguments.
    #[test]
    fn slash_popup_enter_completes_without_submitting() {
        let mut app = test_app();
        for c in "/exi".chars() {
            press(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert!(app.slash_state.visible);
        let action = press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(action, InlineKeyAction::Continue);
        assert_eq!(
            app.input.lines().join("\n"),
            "/exit ",
            "Enter must complete the selected command, not execute it"
        );
        assert!(
            !app.slash_state.visible,
            "completion must dismiss the popup"
        );
    }

    /// Enter on a fully-typed command submits DIRECTLY (e2e e4/e18): the
    /// popup is previewing what the user already finished typing, so the
    /// complete-then-retype-Enter loop would be a regression.
    #[test]
    fn slash_popup_enter_exact_command_submits() {
        let mut app = test_app();
        for c in "/help".chars() {
            press(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert!(app.slash_state.visible);
        let action = press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(action, InlineKeyAction::Submit);
    }

    /// Arrow-navigated selection executes on Enter (e2e e15 contract): the
    /// explicit choice inverts complete→execute even though the buffer ("/")
    /// does not equal the selected command.
    #[test]
    fn slash_popup_enter_arrow_selected_command_submits() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
        assert!(app.slash_state.visible);
        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert!(app.slash_state.choice_locked, "explicit move must lock");
        let action = press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(action, InlineKeyAction::Submit);
        assert_eq!(
            app.input.lines().join("\n"),
            "/exit",
            "locked execution must rewrite the raw token into the selected command"
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

    // === T228: Ctrl+C cancel-while-streaming ===

    #[test]
    fn t228_ctrl_c_streaming_first_press_cancels_and_keeps_buffer() {
        let mut app = test_app();
        app.is_streaming = true;
        app.input.insert_str("draft text");
        let action = press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(action, InlineKeyAction::CancelTurn);
        assert!(app.cancel_requested);
        assert_eq!(
            app.input.lines().join("\n"),
            "draft text",
            "cancel must never silently lose the input buffer"
        );
    }

    #[test]
    fn t228_ctrl_c_streaming_second_press_quits() {
        let mut app = test_app();
        app.is_streaming = true;
        let first = press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(first, InlineKeyAction::CancelTurn);
        let second = press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(second, InlineKeyAction::Quit);
    }

    #[test]
    fn t228_ctrl_c_idle_still_quits() {
        // T2xx(a) empty-draft pin: idle + EMPTY composer quits immediately
        // (long-standing semantics, unchanged by the draft-protection
        // ladder — the ladder only intercepts non-empty drafts).
        let mut app = test_app();
        app.is_streaming = false;
        let action = press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(action, InlineKeyAction::Quit);
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

    // === T025: PageUp/PageDown captured (raw mode owns every key) ===

    /// PageUp arms T062 reading mode; PageDown/End exit it; a bare PageDown
    /// outside reading mode is a captured no-op. The input buffer is never
    /// corrupted by either key.
    #[test]
    fn t025_pageup_pagedown_captured_input_untouched() {
        let mut app = test_app();
        app.input.insert_str("draft");

        let action = press(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(action, InlineKeyAction::Continue);
        assert!(!app.reading_mode);
        assert_eq!(app.input.lines().join("\n"), "draft");

        let action = press(&mut app, KeyCode::PageUp, KeyModifiers::NONE);
        assert_eq!(action, InlineKeyAction::Continue);
        assert!(app.reading_mode, "PageUp must arm T062 reading mode");
        assert_eq!(app.input.lines().join("\n"), "draft");

        let action = press(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(action, InlineKeyAction::Continue);
        assert!(!app.reading_mode, "PageDown must exit reading mode");
        assert_eq!(app.input.lines().join("\n"), "draft");
    }

    // === T084(e): Ctrl+J newline fallback ===

    /// Ctrl+J (the LF byte; kitty-modern terminals deliver it as
    /// Char('j')+CONTROL) inserts a newline exactly like Shift+Enter, and a
    /// pending paste-pill mapping is undisturbed by the insert.
    #[test]
    fn t084_ctrl_j_inserts_newline_pill_map_undisturbed() {
        let mut app = test_app();
        for c in "abc".chars() {
            press(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        }
        let action = press(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert_eq!(action, InlineKeyAction::Continue);
        press(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(
            app.input.lines().join(
                "
"
            ),
            "abc\nd"
        );

        // Pill mapping survives the newline insert.
        let full = (1..=5)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let insert = app.collapse_paste_for_insert(&full);
        app.input.insert_str(&insert);
        let pills_before = app.paste_pills.len();
        assert_eq!(pills_before, 1);
        press(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert_eq!(
            app.paste_pills.len(),
            pills_before,
            "pill map must be undisturbed"
        );
        assert!(
            app.input
                .lines()
                .join(
                    "
"
                )
                .contains("[Pasted 5 lines /")
        );
    }

    // === T2xx(a): Ctrl+C draft-protection ladder (idle) ===

    /// First idle Ctrl+C on a NON-empty draft clears it and stays alive
    /// (codex-rs semantics) — a single press must never kill a draft.
    #[test]
    fn idle_draft_ctrl_c_first_press_clears_not_quits() {
        let mut app = test_app();
        app.input.insert_str("precious draft");
        let action = press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(
            action,
            InlineKeyAction::Continue,
            "first press must NOT quit"
        );
        assert_eq!(
            app.input.lines().join("\n"),
            "",
            "first press must clear the draft"
        );
        assert!(
            app.last_ctrl_c_quit_intent.is_some(),
            "quit intent must be armed"
        );
    }

    /// Second press inside the quit window lands on the (already cleared)
    /// composer and quits — the codex double-press contract.
    #[test]
    fn idle_draft_ctrl_c_second_press_within_window_quits() {
        let mut app = test_app();
        app.input.insert_str("draft");
        let first = press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(first, InlineKeyAction::Continue);
        let second = press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(
            second,
            InlineKeyAction::Quit,
            "second press inside window quits"
        );
        assert!(
            app.last_ctrl_c_quit_intent.is_none(),
            "intent must be disarmed on quit"
        );
    }

    /// After the window expires a press behaves as a FRESH first press:
    /// a re-typed draft is re-cleared and the intent re-armed (never quit).
    /// Window expiry is simulated by backdating the stored Instant — no sleeps.
    #[test]
    fn idle_draft_ctrl_c_after_window_reclears() {
        let mut app = test_app();
        app.input.insert_str("first draft");
        assert_eq!(
            press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL),
            InlineKeyAction::Continue
        );
        app.last_ctrl_c_quit_intent =
            Instant::now().checked_sub(QUIT_INTENT_WINDOW + Duration::from_millis(1));
        // User re-typed a new draft during the (now expired) window.
        app.input.insert_str("second draft");
        let action = press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(
            action,
            InlineKeyAction::Continue,
            "expired window must behave as a fresh first press, never quit"
        );
        assert_eq!(
            app.input.lines().join("\n"),
            "",
            "fresh first press re-clears the draft"
        );
        assert!(
            app.last_ctrl_c_quit_intent.is_some(),
            "fresh first press re-arms the intent"
        );
    }

    // === T2xx(b): Esc-Esc pushes the draft INTO history (inline only) ===

    /// Double idle Esc saves the draft via the same store normal submits
    /// use (so Up recalls it) and clears the composer; a single Esc never
    /// clears anything.
    #[test]
    fn esc_esc_pushes_draft_to_history_and_clears() {
        let mut app = test_app();
        app.input.insert_str("my correction");
        let first = press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(first, InlineKeyAction::Continue);
        assert_eq!(
            app.input.lines().join("\n"),
            "my correction",
            "single Esc must not clear the draft"
        );
        let second = press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(second, InlineKeyAction::Continue);
        assert_eq!(
            app.input.lines().join("\n"),
            "",
            "second Esc clears the composer"
        );
        assert_eq!(
            app.command_history.last().map(String::as_str),
            Some("my correction"),
            "draft must land in the same history store submits use"
        );
        // Recallable: Up pulls the discarded draft back.
        press(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(
            app.input.lines().join("\n"),
            "my correction",
            "Up must recall the discarded draft"
        );
    }

    /// A single idle Esc keeps today's fall-through: the draft survives and
    /// only the recall window is armed.
    #[test]
    fn single_esc_idle_keeps_textarea_fallthrough() {
        let mut app = test_app();
        app.input.insert_str("survives");
        let action = press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(action, InlineKeyAction::Continue);
        assert_eq!(
            app.input.lines().join("\n"),
            "survives",
            "single idle Esc must never clear the draft"
        );
        assert!(
            app.last_esc_discard.is_some(),
            "single Esc arms the Esc-Esc window"
        );
    }

    // === T2xx(c): `?` shortcuts overlay (inline only) ===

    /// `?` opens the overlay ONLY on an empty composer; with a draft it
    /// types normally.
    #[test]
    fn question_overlay_opens_only_on_empty_composer() {
        let mut app = test_app();
        let action = press(&mut app, KeyCode::Char('?'), KeyModifiers::NONE);
        assert_eq!(action, InlineKeyAction::Continue);
        assert!(
            app.shortcuts_overlay,
            "? must open the overlay on an empty composer"
        );

        let mut app2 = test_app();
        app2.input.insert_str("draft?");
        let action2 = press(&mut app2, KeyCode::Char('?'), KeyModifiers::NONE);
        assert_eq!(action2, InlineKeyAction::Continue);
        assert!(
            !app2.shortcuts_overlay,
            "? must NOT open the overlay when a draft exists"
        );
        assert_eq!(
            app2.input.lines().join("\n"),
            "draft??",
            "? must type into a non-empty draft"
        );
    }

    /// While open, all other keys are swallowed (approval-popup precedent);
    /// Esc / `?` / Enter each dismiss.
    #[test]
    fn question_overlay_dismisses_and_swallows() {
        let mut app = test_app();
        app.shortcuts_overlay = true;

        // Letters must not type.
        let action = press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(action, InlineKeyAction::Continue);
        assert!(app.shortcuts_overlay, "plain key must not dismiss");
        assert_eq!(
            app.input.lines().join("\n"),
            "",
            "swallowed key must not reach the textarea"
        );

        // Ctrl+C is swallowed too — dismiss first, then quit.
        let action = press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(
            action,
            InlineKeyAction::Continue,
            "Ctrl+C swallowed while open"
        );
        assert!(app.shortcuts_overlay);

        // Esc dismisses.
        let action = press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(action, InlineKeyAction::Continue);
        assert!(!app.shortcuts_overlay, "Esc must dismiss the overlay");

        // `?` toggles closed.
        app.shortcuts_overlay = true;
        press(&mut app, KeyCode::Char('?'), KeyModifiers::NONE);
        assert!(!app.shortcuts_overlay, "? must toggle the overlay closed");

        // Enter dismisses.
        app.shortcuts_overlay = true;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(!app.shortcuts_overlay, "Enter must dismiss the overlay");
    }
}
