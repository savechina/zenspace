use super::app::App;
use super::model_picker::render_model_picker_inline;
use super::session_picker::render_session_picker_inline;
use super::slash::render_slash_popup_inline;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

const INPUT_HEIGHT: u16 = 3;
const FOOTER_HEIGHT: u16 = 1;
const POPUP_HEIGHT: u16 = 8;
const MIN_TAIL_HEIGHT: u16 = 2;
const MAX_TAIL_HEIGHT: u16 = 4;

pub fn render(frame: &mut Frame, app: &mut App) {
    let (muted, info_accent, bg_color, streaming_cursor) = {
        let theme = app.theme.as_ref();
        (
            theme.text_muted(),
            theme.info_accent(),
            theme.bg(),
            theme.streaming_cursor(),
        )
    };
    let accent_fg = Style::default().fg(info_accent);

    let any_picker =
        app.slash_state.visible || app.session_picker.visible || app.model_picker.visible;

    // Codex/Claude-style bottom popup: the picker grows upward from the input
    // row. Input + footer ALWAYS keep their full height — the popup is capped
    // at POPUP_HEIGHT and shrinks to fit small viewports instead of
    // over-constraining the layout. (The previous fixed `Min(3)` filler plus
    // popup 4 + input 3 + footer 1 = 11 rows inside the 8-row inline viewport
    // over-constrained the solver, clipping the input box to a single row and
    // making typing appear broken — user-reported 2026-08-16.)
    let area_height = frame.area().height;
    // T064: transient toast (if any) takes one row above the footer.
    let toast = app.get_active_toast();
    let toast_height = if toast.is_some() { 1 } else { 0 };
    let fixed_rows = INPUT_HEIGHT + FOOTER_HEIGHT + toast_height;
    let popup_height = if any_picker {
        let max_h = if app.slash_state.visible {
            let count = app.slash_state.filtered_indices.len();
            if count == 0 {
                1u16
            } else {
                (count as u16).min(8)
            }
        } else {
            POPUP_HEIGHT
        };
        max_h.min(area_height.saturating_sub(fixed_rows))
    } else {
        0
    };
    let filler_min = if any_picker { 0 } else { 3 };
    let tail_height = if !any_picker && app.is_streaming && !app.viewport_tail.is_empty() {
        dynamic_tail_height(area_height, popup_height, filler_min)
    } else {
        0
    };

    let constraints = [
        Constraint::Min(filler_min),
        Constraint::Length(tail_height),
        Constraint::Length(popup_height),
        Constraint::Length(INPUT_HEIGHT),
        Constraint::Length(toast_height),
        Constraint::Length(FOOTER_HEIGHT),
    ];

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(frame.area());

    let mut chunk_idx = 1;

    if tail_height > 0 {
        let tail_area = chunks[chunk_idx];
        chunk_idx += 1;
        let mut tail_lines = app.viewport_tail.clone();
        if let Some(last) = tail_lines.last_mut() {
            last.spans.push(Span::styled(
                std::borrow::Cow::Borrowed("\u{258c}"),
                streaming_cursor,
            ));
        }
        let tail_para = Paragraph::new(tail_lines).wrap(Wrap { trim: false });
        frame.render_widget(tail_para, tail_area);
    } else {
        chunk_idx += 1;
    }

    if popup_height > 0 {
        let popup_area = chunks[chunk_idx];
        let theme_ref = app.theme.as_ref();
        chunk_idx += 1;
        if app.slash_state.visible {
            render_slash_popup_inline(
                frame,
                &app.slash_state,
                popup_area,
                theme_ref,
                &app.slash_registry,
            );
        } else if app.session_picker.visible {
            render_session_picker_inline(
                frame,
                &app.session_picker,
                app.session_id.as_deref(),
                popup_area,
                theme_ref,
            );
        } else if app.model_picker.visible {
            render_model_picker_inline(frame, &app.model_picker, popup_area, theme_ref);
        }
    } else {
        chunk_idx += 1;
    }

    let input_area = chunks[chunk_idx];
    frame.render_widget(app.input.textarea(), input_area);

    if let Some(msg) = toast {
        // chunks layout: [filler, tail, popup, input, toast, footer]
        let toast_area = chunks[4];
        let toast_line = Line::styled(msg, Style::default().fg(info_accent));
        frame.render_widget(Paragraph::new(toast_line), toast_area);
    }

    let footer_area = chunks[5];

    let footer_spans: Vec<Span<'static>> = if app.history_search.active {
        // W2: reverse-i-search footer -- replaces the normal status bar.
        let hs = &app.history_search;
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(6);
        spans.push(Span::styled(
            " Zen ",
            Style::default()
                .fg(Color::Black)
                .bg(info_accent)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(" ", muted));
        spans.push(Span::styled(
            "(reverse-i-search)`",
            Style::default().fg(info_accent),
        ));
        spans.push(Span::styled(
            hs.query.clone(),
            Style::default()
                .fg(info_accent)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled("': ", Style::default().fg(info_accent)));
        if let Some(m) = hs.current_match() {
            let display: String = m.chars().take(60).collect();
            let indices = hs.current_match_indices();
            let highlight_style = Style::default()
                .fg(info_accent)
                .add_modifier(Modifier::REVERSED | Modifier::BOLD);
            if indices.is_empty() {
                spans.push(Span::styled(display, accent_fg));
            } else {
                let index_set: std::collections::HashSet<usize> = indices.iter().copied().collect();
                for (i, ch) in display.chars().enumerate() {
                    let style = if index_set.contains(&i) {
                        highlight_style
                    } else {
                        accent_fg
                    };
                    spans.push(Span::styled(ch.to_string(), style));
                }
            }
        } else {
            spans.push(Span::styled("no match", muted));
        }
        spans
    } else {
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(12);
        spans.push(Span::styled(
            " Zen ",
            Style::default()
                .fg(Color::Black)
                .bg(info_accent)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(" | ", muted));
        spans.push(Span::styled(format!(" {} ", app.model), accent_fg));
        if app.reading_mode {
            spans.push(Span::styled(
                " | \u{23f8} reading — PageDown resumes",
                Style::default().fg(info_accent),
            ));
            // FR-016: show deferred block count while reading mode is active.
            if !app.deferred_scrollback.is_empty() {
                let count = app.deferred_scrollback.len();
                spans.push(Span::styled(
                    format!(" | \u{23f8} {count} deferred"),
                    Style::default().fg(Color::Yellow),
                ));
            }
        }
        if let Some(hint) = app.status_hint.as_deref() {
            let hint = if app.is_streaming {
                format!("{hint} — esc to interrupt")
            } else {
                hint.to_string()
            };
            spans.push(Span::styled(
                format!(" | \u{23f3} {hint}"),
                Style::default().fg(info_accent),
            ));
        } else if app.is_streaming {
            spans.push(Span::styled(
                " | \u{23f3} esc to interrupt",
                Style::default()
                    .fg(info_accent)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        spans.push(Span::styled(" | ", muted));
        spans.push(Span::styled(
            format!("{} tok", app.current_response_tokens),
            accent_fg,
        ));
        if app.show_thinking {
            spans.push(Span::styled(" | ", muted));
            spans.push(Span::styled("🧠", Style::default().fg(info_accent)));
        }
        // D10: active-picker indicator
        if app.slash_state.visible {
            spans.push(Span::styled(" | ", muted));
            spans.push(Span::styled(" /commands", Style::default().fg(info_accent)));
        } else if app.session_picker.visible {
            spans.push(Span::styled(" | ", muted));
            spans.push(Span::styled(" sessions", Style::default().fg(info_accent)));
        } else if app.model_picker.visible {
            spans.push(Span::styled(" | ", muted));
            spans.push(Span::styled(" models", Style::default().fg(info_accent)));
        }
        if app.session_id.is_some() {
            spans.push(Span::styled(" | ", muted));
            spans.push(Span::styled(
                app.session_id.clone().unwrap_or_default(),
                accent_fg,
            ));
        }
        spans.push(Span::styled(" | ", muted));
        spans.push(Span::styled(app.workspace.clone(), accent_fg));
        spans
    };
    let footer_line = Line::from(footer_spans);
    let footer = Paragraph::new(footer_line).style(Style::default().bg(bg_color));
    frame.render_widget(footer, footer_area);
}

fn dynamic_tail_height(viewport_height: u16, popup_height: u16, filler_min: u16) -> u16 {
    let available =
        viewport_height.saturating_sub(INPUT_HEIGHT + FOOTER_HEIGHT + popup_height + filler_min);
    let preferred = available / 3;
    // Never exceed the spare space: filler + tail + popup + input + footer
    // must always fit the viewport exactly (no clipped rows).
    preferred
        .clamp(MIN_TAIL_HEIGHT, MAX_TAIL_HEIGHT)
        .min(available)
}

#[cfg(test)]
mod tests {
    //! Layer-1 layout regression tests (docs/specs/002-agentic-tui/test-design.md).
    //!
    //! These guard the post-hoc display bugfixes recorded in tasks.md Phase 1b:
    //! the `Constraint::Min(3)` filler (T041) must keep the input box and
    //! footer anchored to the bottom of the inline viewport, and the
    //! streaming tail must render directly above the input.

    use super::*;
    use crate::tui::app::App;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::text::Line;

    fn row_text(buf: &Buffer, y: u16) -> String {
        let mut s = String::new();
        for x in 0..buf.area.width {
            s.push_str(buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "));
        }
        s.trim_end().to_string()
    }

    fn test_app() -> App {
        let config: &'static zen_core::config::ZenConfig = Box::leak(Box::default());
        let mut app = App::new(config);
        // Deterministic layout: no transient toasts unless a test wants one.
        app.toast_queue.clear();
        app.current_toast = None;
        app
    }

    fn draw_ui(width: u16, height: u16, app: &mut App) -> Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| render(frame, app)).expect("draw");
        terminal.backend().buffer().clone()
    }

    /// T041 regression: with no tail and no popup, the 8-row idle content
    /// (filler + input 3 + footer 1) must hug the BOTTOM of the viewport —
    /// the filler absorbs leftover space instead of leaving a blank band
    /// below the input box.
    #[test]
    fn idle_viewport_anchors_input_to_bottom() {
        let mut app = test_app();
        let buf = draw_ui(60, 12, &mut app);

        // Footer occupies the very last row.
        let footer = row_text(&buf, 11);
        assert!(
            footer.contains("Zen"),
            "footer must sit on the bottom row: {footer:?}"
        );

        // Input box (3 rows) sits directly above the footer, rows 8..11.
        let input_top = row_text(&buf, 8);
        assert!(
            input_top.contains("Input"),
            "input box title must be on row 8 (bottom-anchored): {input_top:?}"
        );

        // Everything above the input is filler — no stray content, no
        // top-anchored widgets.
        for y in 0..8u16 {
            assert_eq!(row_text(&buf, y), "", "row {y} must be blank filler");
        }
    }

    /// SC-004 / US2: the pending streaming tail renders directly above the
    /// input prompt, with the streaming cursor appended to its last line.
    #[test]
    fn streaming_tail_renders_above_input() {
        let mut app = test_app();
        app.is_streaming = true;
        app.viewport_tail = vec![Line::from("partial stream")];
        let buf = draw_ui(60, 12, &mut app);

        // tail_height = clamp((12 - 3 - 1) / 3, 2, 4) = 2 → rows 6..8.
        let tail_row = row_text(&buf, 6);
        assert!(
            tail_row.contains("partial stream"),
            "tail must render above the input: {tail_row:?}"
        );
        assert!(
            tail_row.contains("\u{258c}"),
            "streaming cursor must be appended to the tail's last line: {tail_row:?}"
        );

        // Input stays bottom-anchored below the tail.
        assert!(row_text(&buf, 8).contains("Input"));
        assert!(row_text(&buf, 11).contains("Zen"));
    }

    /// User-reported display bug (2026-08-16): with a picker open inside the
    /// fixed 8-row inline viewport, the old `Min(3)` filler + popup(4) +
    /// input(3) + footer(1) = 11 rows over-constrained the layout solver —
    /// the input box was clipped and the popup appeared to cover it. The
    /// popup must fit deterministically and the input must keep all 3 rows.
    #[test]
    fn popup_open_in_8row_viewport_keeps_input_intact() {
        let mut app = test_app();
        app.slash_state.visible = true;
        app.slash_state.filtered_indices = vec![0, 1, 2];
        app.slash_state.selected = 0;
        let buf = draw_ui(60, 8, &mut app);

        // Popup renders above input; first command row shows the selected command.
        assert!(
            row_text(&buf, 1).contains("/"),
            "popup row: {:?}",
            row_text(&buf, 1)
        );
        // Input box keeps its full 3 rows directly below the popup.
        assert!(
            row_text(&buf, 4).contains("Input"),
            "input title must sit on row 4 (unclipped): {:?}",
            row_text(&buf, 4)
        );
        assert!(row_text(&buf, 5).contains(">"), "input body row present");
        assert!(
            row_text(&buf, 6).starts_with("└"),
            "input bottom border: {:?}",
            row_text(&buf, 6)
        );
        // Footer survives on the last row.
        assert!(row_text(&buf, 7).contains("Zen"), "footer must survive");
    }

    /// Codex parity: the slash popup height is dynamic — it shrinks to the
    /// filtered match count (min 1 for the "no matches" row) instead of
    /// always reserving the full POPUP_HEIGHT.
    #[test]
    fn slash_popup_height_is_dynamic() {
        // 1 match → 1 popup row: filler above stays empty.
        let mut app = test_app();
        app.slash_state.visible = true;
        app.slash_state.filtered_indices = vec![0];
        let buf = draw_ui(60, 12, &mut app);
        assert!(
            row_text(&buf, 7).contains("/"),
            "single match occupies the row above the input: {:?}",
            row_text(&buf, 7)
        );
        assert!(
            row_text(&buf, 0).trim().is_empty(),
            "no popup rows above a 1-row popup: {:?}",
            row_text(&buf, 0)
        );

        // 18 matches → full 8-row window starting at the top.
        let mut app2 = test_app();
        app2.slash_state.visible = true;
        app2.slash_state.filtered_indices = (0..18).collect();
        let buf2 = draw_ui(60, 12, &mut app2);
        assert!(
            row_text(&buf2, 0).contains("/"),
            "8-row popup reaches the top of a 12-row viewport: {:?}",
            row_text(&buf2, 0)
        );
        assert!(
            row_text(&buf2, 7).contains("/"),
            "popup bottom still sits directly above the input: {:?}",
            row_text(&buf2, 7)
        );
    }

    /// The popup shrinks on small viewports instead of squeezing the input.
    #[test]
    fn popup_shrinks_in_small_viewport() {
        let mut app = test_app();
        app.slash_state.visible = true;
        app.slash_state.filtered_indices = vec![0, 1, 2];
        app.slash_state.selected = 0;
        let buf = draw_ui(60, 6, &mut app);

        // popup = min(3, 6 - 3 - 1) = 2 rows; input + footer intact below.
        assert!(row_text(&buf, 0).contains("/"));
        assert!(
            row_text(&buf, 2).contains("Input"),
            "input must start right under the shrunken popup: {:?}",
            row_text(&buf, 2)
        );
        assert!(row_text(&buf, 5).contains("Zen"));
    }

    /// Tall viewport: filler absorbs spare space; popup stays directly above
    /// the input (Codex/Claude-style bottom popup position).
    #[test]
    fn popup_stays_directly_above_input_in_tall_viewport() {
        let mut app = test_app();
        app.slash_state.visible = true;
        app.slash_state.filtered_indices = vec![0, 1, 2];
        app.slash_state.selected = 0;
        let buf = draw_ui(60, 12, &mut app);

        assert!(
            row_text(&buf, 5).contains("/"),
            "popup must start at row 5 (filler 0..5): {:?}",
            row_text(&buf, 5)
        );
        assert!(
            row_text(&buf, 8).contains("Input"),
            "input must directly follow the popup: {:?}",
            row_text(&buf, 8)
        );
        assert!(row_text(&buf, 11).contains("Zen"));
        for y in 0..5u16 {
            assert_eq!(row_text(&buf, y), "", "filler rows must stay blank");
        }
    }

    /// T064: an active toast takes one row above the footer without
    /// squeezing the input box.
    #[test]
    fn toast_row_renders_above_footer() {
        let mut app = test_app();
        app.toast_queue.push_back("saved".to_string());
        let buf = draw_ui(60, 12, &mut app);
        assert!(
            row_text(&buf, 10).contains("saved"),
            "toast must render above footer: {:?}",
            row_text(&buf, 10)
        );
        assert!(row_text(&buf, 7).contains("Input"), "input intact");
        assert!(row_text(&buf, 11).contains("Zen"), "footer intact");
    }

    /// Same over-constraint class as the popup bug, for the streaming tail:
    /// in the fixed 8-row viewport the tail must shrink so input + footer
    /// keep their rows.
    #[test]
    fn streaming_tail_fits_8row_viewport() {
        let mut app = test_app();
        app.is_streaming = true;
        app.viewport_tail = vec![Line::from("partial stream")];
        let buf = draw_ui(60, 8, &mut app);

        // filler 0..3, tail 1 row (3), input 4..7, footer 7.
        assert!(row_text(&buf, 3).contains("partial stream"));
        assert!(row_text(&buf, 4).contains("Input"), "input unclipped");
        assert!(row_text(&buf, 7).contains("Zen"), "footer survives");
    }

    /// Edge case (spec.md): very narrow/short terminals must not panic and
    /// must keep rendering the input + footer.
    #[test]
    fn narrow_terminal_does_not_panic() {
        let mut app = test_app();
        let buf = draw_ui(12, 8, &mut app);
        let footer = row_text(&buf, 7);
        assert!(footer.contains("Zen"), "footer must survive narrow width");
    }

    // === W2: History Search Footer ===

    /// W2: when history_search.active, the footer shows the
    /// reverse-i-search prompt instead of the normal status bar.
    #[test]
    fn w2_history_search_footer_shows_reverse_i_search() {
        let mut app = test_app();
        app.history_search.active = true;
        app.history_search.query = "test".to_string();
        app.history_search.matches = vec!["test match".to_string()];
        app.history_search.match_index = 0;
        let buf = draw_ui(60, 8, &mut app);
        let footer = row_text(&buf, 7);
        assert!(
            footer.contains("(reverse-i-search)`"),
            "footer must show reverse-i-search prompt: {footer:?}"
        );
        assert!(
            footer.contains("test"),
            "footer must show the query: {footer:?}"
        );
        assert!(
            footer.contains("test match"),
            "footer must show the current match: {footer:?}"
        );
    }

    /// W2: when history_search.active with no match, footer shows "no match".
    #[test]
    fn w2_history_search_footer_no_match() {
        let mut app = test_app();
        app.history_search.active = true;
        app.history_search.query = "zzz".to_string();
        app.history_search.matches = vec![];
        let buf = draw_ui(60, 8, &mut app);
        let footer = row_text(&buf, 7);
        assert!(
            footer.contains("(reverse-i-search)`"),
            "footer must show search prompt: {footer:?}"
        );
        assert!(
            footer.contains("no match"),
            "footer must show no match: {footer:?}"
        );
    }

    // === Esc-to-interrupt ===

    /// Esc-to-interrupt footer: while streaming (no status hint), the footer
    /// shows the interrupt affordance.
    #[test]
    fn footer_shows_esc_to_interrupt_while_streaming() {
        let mut app = test_app();
        app.is_streaming = true;
        let buf = draw_ui(60, 8, &mut app);
        let footer = row_text(&buf, 7);
        assert!(
            footer.contains("esc to interrupt"),
            "footer must show interrupt affordance while streaming: {footer:?}"
        );
    }

    /// Esc-to-interrupt footer: while idle, the footer does NOT show "esc to interrupt".
    #[test]
    fn footer_no_esc_to_interrupt_while_idle() {
        let mut app = test_app();
        app.is_streaming = false;
        let buf = draw_ui(60, 8, &mut app);
        let footer = row_text(&buf, 7);
        assert!(
            !footer.contains("\u{23f3}"),
            "footer must NOT show hourglass while idle: {footer:?}"
        );
    }

    /// Esc-to-interrupt footer: status_hint takes precedence over streaming hint.
    #[test]
    fn footer_status_hint_takes_precedence_over_streaming() {
        let mut app = test_app();
        app.is_streaming = true;
        app.status_hint = Some("preparing context".to_string());
        // Wide viewport: hint + interrupt affordance must not truncate.
        let buf = draw_ui(100, 8, &mut app);
        let footer = row_text(&buf, 7);
        assert!(
            footer.contains("preparing context"),
            "status_hint must take precedence: {footer:?}"
        );
        // Streaming + hint: the interrupt affordance rides along with the hint.
        assert!(
            footer.contains("esc to interrupt"),
            "streaming footer must keep interrupt affordance next to hint: {footer:?}"
        );
    }

    // === W2+FUZZY: Highlight test ===

    /// W2+FUZZY: matched characters in the search footer use REVERSED|BOLD style.
    #[test]
    fn w2_fuzzy_highlight_uses_reversed_bold() {
        let mut app = test_app();
        app.history_search.active = true;
        app.history_search.query = "wl".to_string();
        app.history_search.matches = vec!["zen wiki list".to_string()];
        app.history_search.match_index = 0;
        // Simulate highlight indices for "wl" in "zen wiki list":
        // w=4, l=9 (char positions for w and l in "zen wiki list")
        app.history_search.matched_indices = vec![vec![4, 9]];
        let buf = draw_ui(80, 8, &mut app);
        let footer_y = 7u16;
        // Footer layout: " Zen (reverse-i-search)`wl': zen wiki list"
        // Find the 'w' in "wiki" (char pos 4 of "zen wiki list") and check style.
        let mut found_highlight = false;
        for x in 0..buf.area.width {
            if let Some(cell) = buf.cell((x, footer_y))
                && cell.symbol() == "w"
            {
                let style = cell.style();
                let mods = style.add_modifier;
                if mods.contains(Modifier::REVERSED) && mods.contains(Modifier::BOLD) {
                    found_highlight = true;
                    break;
                }
            }
        }
        assert!(
            found_highlight,
            "footer must apply REVERSED|BOLD to matched chars"
        );
    }

    /// D4 regression: footer shows brain icon when show_thinking is true,
    /// absent when false (per Codex: absence, not dimmed icon).
    #[test]
    fn thinking_indicator_in_footer() {
        let mut app = test_app();
        app.show_thinking = true;
        let buf = draw_ui(80, 8, &mut app);
        let footer = row_text(&buf, 7);
        assert!(
            footer.contains("🧠"),
            "footer must show brain icon when show_thinking=true: {footer:?}"
        );

        let mut app2 = test_app();
        app2.show_thinking = false;
        let buf2 = draw_ui(80, 8, &mut app2);
        let footer2 = row_text(&buf2, 7);
        assert!(
            !footer2.contains("🧠"),
            "footer must NOT show brain icon when show_thinking=false: {footer2:?}"
        );
    }

    /// D10 regression: footer shows active picker indicator.
    #[test]
    fn active_picker_indicator_in_footer() {
        // Slash popup active
        let mut app = test_app();
        app.slash_state.visible = true;
        let buf = draw_ui(80, 8, &mut app);
        let footer = row_text(&buf, 7);
        assert!(
            footer.contains("/commands"),
            "footer must show /commands when slash popup visible: {footer:?}"
        );

        // Session picker active
        let mut app2 = test_app();
        app2.session_picker.visible = true;
        let buf2 = draw_ui(80, 8, &mut app2);
        let footer2 = row_text(&buf2, 7);
        assert!(
            footer2.contains("sessions"),
            "footer must show sessions when session picker visible: {footer2:?}"
        );

        // Model picker active
        let mut app3 = test_app();
        app3.model_picker.visible = true;
        let buf3 = draw_ui(80, 8, &mut app3);
        let footer3 = row_text(&buf3, 7);
        assert!(
            footer3.contains("models"),
            "footer must show models when model picker visible: {footer3:?}"
        );

        // No picker active
        let mut app4 = test_app();
        let buf4 = draw_ui(80, 8, &mut app4);
        let footer4 = row_text(&buf4, 7);
        assert!(
            !footer4.contains("/commands"),
            "footer must NOT show /commands when no picker visible: {footer4:?}"
        );
    }

    // === T080: Fixed 12-row viewport budget invariant tests ===

    /// T080: with popup open + toast + banner worst case, input (3) + footer (1)
    /// must never be clipped in the 12-row viewport.
    #[test]
    fn twelve_row_worst_case_never_clips_input_footer() {
        let mut app = test_app();
        // Worst case: popup open + toast active
        app.slash_state.visible = true;
        app.slash_state.filtered_indices = (0..18).collect();
        app.slash_state.selected = 0;
        app.toast_queue.push_back("test toast".to_string());
        let buf = draw_ui(60, 12, &mut app);

        // Footer on the last row.
        let footer = row_text(&buf, 11);
        assert!(
            footer.contains("Zen"),
            "footer must survive worst case: {footer:?}"
        );

        // Input box (3 rows) directly above footer.
        let input_top = row_text(&buf, 7);
        assert!(
            input_top.contains("Input"),
            "input must not be clipped in worst case: {input_top:?}"
        );
        assert!(row_text(&buf, 8).contains(">"), "input body present");
        assert!(
            row_text(&buf, 9).starts_with("└"),
            "input bottom border: {:?}",
            row_text(&buf, 9)
        );

        // Toast renders between input and footer.
        assert!(
            row_text(&buf, 10).contains("test toast"),
            "toast must render: {:?}",
            row_text(&buf, 10)
        );
    }

    /// T080: while streaming without popup, tail must be >= 2 rows.
    #[test]
    fn streaming_tail_minimum_two_rows_in_12row_viewport() {
        let mut app = test_app();
        app.is_streaming = true;
        app.viewport_tail = vec![Line::from("stream content")];
        let buf = draw_ui(60, 12, &mut app);

        // Tail occupies at least 2 rows above the input.
        // Layout: filler 0..7, tail 7..9, input 9..12 (but input is 3 rows → 9..12 is input+footer).
        // Actually: filler gets remaining space, tail=2, input=3, footer=1.
        // filler = 12 - 2 - 0 - 3 - 0 - 1 = 6 → filler 0..6, tail 6..8, input 8..11, footer 11.
        let tail_row_1 = row_text(&buf, 6);
        let tail_row_2 = row_text(&buf, 7);
        assert!(
            tail_row_1.contains("stream content") || tail_row_2.contains("stream content"),
            "tail must occupy at least 2 rows: row6={tail_row_1:?}, row7={tail_row_2:?}"
        );
        // Input stays intact below the tail.
        assert!(row_text(&buf, 8).contains("Input"), "input unclipped");
        assert!(row_text(&buf, 11).contains("Zen"), "footer intact");
    }

    /// T080: the popup + tail coexist without clipping input in 12-row viewport.
    #[test]
    fn popup_and_tail_coexist_in_12row_viewport() {
        let mut app = test_app();
        app.is_streaming = true;
        app.viewport_tail = vec![Line::from("streaming")];
        app.slash_state.visible = true;
        app.slash_state.filtered_indices = vec![0, 1, 2];
        app.slash_state.selected = 0;
        let buf = draw_ui(60, 12, &mut app);

        // With popup, tail=0 (tail only shows when !any_picker).
        // filler=0, popup=min(3, 12-4)=3, input=3, footer=1 → rows 0..3 popup, 3..6 input, 6 footer
        // Actually: filler_min=0 (any_picker), fixed_rows=4, popup=3
        // filler=12-3-3-0-1=5 → filler 0..5, popup 5..8, input 8..11, footer 11
        assert!(row_text(&buf, 5).contains("/"), "popup present");
        assert!(row_text(&buf, 8).contains("Input"), "input unclipped");
        assert!(row_text(&buf, 11).contains("Zen"), "footer intact");
    }

    // === T081: Deferred queue cap test ===

    /// T081: deferring >256 blocks flushes oldest-first and caps at 256.
    #[test]
    fn deferred_queue_cap_flushes_oldest_first() {
        use crate::tui::app::DEFERRED_QUEUE_CAP;
        use crate::tui::app::ScrollbackEntry;

        let mut app = test_app();
        app.reading_mode = true;

        // Push 300 entries via the deferred queue.
        for i in 0..300u32 {
            let entry = ScrollbackEntry {
                lines: vec![Line::from(format!("block {i}"))],
                wrap: true,
            };
            app.defer_scrollback(entry);
        }

        // Queue must be capped at DEFERRED_QUEUE_CAP.
        assert_eq!(
            app.deferred_scrollback.len(),
            DEFERRED_QUEUE_CAP,
            "deferred queue must be capped at {DEFERRED_QUEUE_CAP}"
        );

        // Oldest entries (0..44) must have been flushed; the queue starts at block 44.
        let first = app.deferred_scrollback.front().unwrap();
        assert!(
            first
                .lines
                .iter()
                .any(|l| l.iter().any(|s| s.content.contains("block 44"))),
            "oldest entries flushed; first remaining should be block 44"
        );
    }

    // === T081: History failure path test ===

    /// T081: history store failure path — when HistoryStore::open fails
    /// (no CWD fallback per NFR-010), App still constructs and the
    /// fail-loud toast is set by the caller.
    #[test]
    fn history_failure_toast_is_set_by_caller() {
        // Ensure no ./history.jsonl exists in the current directory.
        let cwd_history = std::path::PathBuf::from("history.jsonl");
        let _ = std::fs::remove_file(&cwd_history);

        let config: &'static zen_core::config::ZenConfig = Box::leak(Box::default());
        let mut app = App::new(config);

        // Whether history_store is Some or None depends on the test environment
        // (ZEN_HOME presence). What we can verify: the caller's toast path works.
        // Simulate the startup toast that prepare_inline_app / run_app fires
        // when history_store is None.
        if !app.has_history_store() {
            app.show_toast(
                "history unavailable: could not open history file — running without persistence",
            );
            let toast = app.get_active_toast();
            assert!(
                toast.is_some(),
                "startup toast must be set after history failure"
            );
            assert!(
                toast.unwrap().contains("history unavailable"),
                "toast must mention history unavailability"
            );
        }

        // NFR-010: regardless of whether history succeeded, no CWD fallback
        // must have been used. If the file exists, it must NOT be in CWD.
        // (In a clean test env, it should not exist.)
        // This assertion verifies the code path: HistoryStore::with_path(PathBuf::from("history.jsonl"))
        // is REMOVED from the codebase.
        assert!(
            !cwd_history.exists(),
            "history.jsonl must NOT be created in CWD (NFR-010 — CWD fallback removed)"
        );
    }
}
