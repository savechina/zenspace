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
const POPUP_HEIGHT: u16 = 4;
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
        POPUP_HEIGHT.min(area_height.saturating_sub(fixed_rows))
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

    let mut footer_spans: Vec<Span<'static>> = Vec::with_capacity(12);
    footer_spans.push(Span::styled(
        " Zen ",
        Style::default()
            .fg(Color::Black)
            .bg(info_accent)
            .add_modifier(Modifier::BOLD),
    ));
    footer_spans.push(Span::styled(" | ", muted));
    footer_spans.push(Span::styled(format!(" {} ", app.model), accent_fg));
    if app.reading_mode {
        footer_spans.push(Span::styled(
            " | \u{23f8} reading — PageDown resumes",
            Style::default().fg(info_accent),
        ));
    }
    if let Some(hint) = app.status_hint.as_deref() {
        // T056: visible pre-LLM progress instead of a silent freeze.
        footer_spans.push(Span::styled(
            format!(" | \u{23f3} {hint}"),
            Style::default().fg(info_accent),
        ));
    } else if app.is_streaming {
        footer_spans.push(Span::styled(
            " | \u{23f3}",
            Style::default()
                .fg(info_accent)
                .add_modifier(Modifier::BOLD),
        ));
    }
    footer_spans.push(Span::styled(" | ", muted));
    footer_spans.push(Span::styled(
        format!("{} tok", app.current_response_tokens),
        accent_fg,
    ));
    if app.session_id.is_some() {
        footer_spans.push(Span::styled(" | ", muted));
        footer_spans.push(Span::styled(
            app.session_id.clone().unwrap_or_default(),
            accent_fg,
        ));
    }
    footer_spans.push(Span::styled(" | ", muted));
    footer_spans.push(Span::styled(app.workspace.clone(), accent_fg));

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

        // Popup (4 rows) hugs the top; its border title is visible.
        assert!(
            row_text(&buf, 0).contains("Commands"),
            "popup top border/title must render on row 0: {:?}",
            row_text(&buf, 0)
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

    /// The popup shrinks on small viewports instead of squeezing the input.
    #[test]
    fn popup_shrinks_in_small_viewport() {
        let mut app = test_app();
        app.slash_state.visible = true;
        app.slash_state.filtered_indices = vec![0, 1, 2];
        app.slash_state.selected = 0;
        let buf = draw_ui(60, 6, &mut app);

        // popup = min(4, 6 - 3 - 1) = 2 rows; input + footer intact below.
        assert!(row_text(&buf, 0).contains("Commands"));
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
            row_text(&buf, 4).contains("Commands"),
            "popup must start at row 4 (filler 0..4): {:?}",
            row_text(&buf, 4)
        );
        assert!(
            row_text(&buf, 8).contains("Input"),
            "input must directly follow the popup: {:?}",
            row_text(&buf, 8)
        );
        assert!(row_text(&buf, 11).contains("Zen"));
        for y in 0..4u16 {
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
}
