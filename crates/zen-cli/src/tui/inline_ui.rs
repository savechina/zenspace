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

    let popup_height = if any_picker { POPUP_HEIGHT } else { 0 };
    let tail_height = if !any_picker && app.is_streaming && !app.viewport_tail.is_empty() {
        dynamic_tail_height(frame.area().height, popup_height)
    } else {
        0
    };

    let constraints = [
        Constraint::Min(3),
        Constraint::Length(tail_height),
        Constraint::Length(popup_height),
        Constraint::Length(INPUT_HEIGHT),
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
    chunk_idx += 1;
    frame.render_widget(app.input.textarea(), input_area);

    let footer_area = chunks[chunk_idx];

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
    if app.is_streaming {
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

fn dynamic_tail_height(viewport_height: u16, popup_height: u16) -> u16 {
    let available = viewport_height.saturating_sub(INPUT_HEIGHT + FOOTER_HEIGHT + popup_height);
    let preferred = available / 3;
    preferred.clamp(MIN_TAIL_HEIGHT, MAX_TAIL_HEIGHT)
}
