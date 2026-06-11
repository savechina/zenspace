use super::app::{App, InputMode};
use super::cell::streaming::StreamingCell;
use super::session_picker::render_session_picker;
use super::slash::render_slash_popup;
use super::theme::OutputTheme;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

pub fn render(frame: &mut Frame, app: &App, active_toast: Option<&str>) {
    let theme = app.theme.as_ref();
    let muted = theme.text_muted();
    let accent_fg = Style::default().fg(theme.info_accent());
    let bg_color = theme.bg();
    let chat_block_bg = Style::default().bg(bg_color);

    let queue_height = if app.message_queue.is_empty() {
        0
    } else {
        2 + app.message_queue.len() as u16
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(queue_height),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let brand_badge = Span::styled(
        " Zen ",
        Style::default()
            .fg(Color::Black)
            .bg(theme.info_accent())
            .add_modifier(Modifier::BOLD),
    );
    let mut status_spans: Vec<Span<'static>> = vec![brand_badge];
    status_spans.push(Span::styled(" | Model: ", muted));
    status_spans.push(Span::styled(app.model.clone(), accent_fg));
    if app.is_streaming {
        status_spans.push(Span::styled(
            " | ⏳ Processing...",
            theme.text_muted().add_modifier(Modifier::BOLD),
        ));
    }
    if app.show_thinking {
        status_spans.push(Span::styled(" | 🧠 Thinking: ON", accent_fg));
    }
    if let Some(sid) = app.session_id.clone() {
        status_spans.push(Span::styled(" | Session: ", muted));
        status_spans.push(Span::styled(sid, accent_fg));
    }
    status_spans.push(Span::styled(" | Workspace: ", muted));
    status_spans.push(Span::styled(app.workspace.clone(), accent_fg));
    if app.input.effective_mode() == InputMode::Selection && !app.output.is_empty() {
        let sel_text = format!(
            " | 🖱 SEL {}/{} ",
            app.input.selected_cell_idx() + 1,
            app.output.len()
        );
        status_spans.push(Span::styled(
            sel_text,
            Style::default()
                .fg(Color::Black)
                .bg(theme.info_accent())
                .add_modifier(Modifier::BOLD),
        ));
    }
    let status = Line::from(status_spans);
    let status_bar = Paragraph::new(status).block(Block::default().bg(bg_color));
    frame.render_widget(status_bar, chunks[0]);

    let mut all_lines: Vec<Line<'static>> = Vec::new();
    let blank_line = Line::styled("", Style::default().bg(bg_color));
    let mut selected_cell_line: Option<usize> = None;
    for (cell_idx, cell) in app.output.iter().enumerate() {
        let cell_lines = cell.display_lines();
        if !cell_lines.is_empty() {
            if cell_idx == app.input.selected_cell_idx()
                && app.input.effective_mode() == InputMode::Selection
            {
                selected_cell_line = Some(all_lines.len());
            }
            all_lines.extend(cell_lines);
            all_lines.push(blank_line.clone());
        }
    }

    if app.is_streaming && !app.stream_collector.is_empty() {
        let streaming_cell = StreamingCell::new(app.stream_collector.buffer(), theme);
        all_lines.extend(streaming_cell.display_lines());
    }

    let inner_width = chunks[1].width.saturating_sub(2) as usize;
    let visual_line_count: usize = all_lines
        .iter()
        .map(|line| {
            let width = line.width();
            if width == 0 || inner_width == 0 {
                1
            } else {
                width.div_ceil(inner_width)
            }
        })
        .sum();
    let visible_height = chunks[1].height.saturating_sub(2) as usize;
    let max_scroll = visual_line_count.saturating_sub(visible_height);

    let scroll = if app.auto_scroll {
        max_scroll
    } else {
        app.scroll_offset.min(max_scroll)
    };

    let paragraph = Paragraph::new(all_lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Chat ")
                .border_style(chat_block_bg)
                .style(chat_block_bg),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph.scroll((scroll as u16, 0)), chunks[1]);

    if let Some(line_idx) = selected_cell_line {
        let border_width = 1;
        let inner_area_y = chunks[1].y + border_width;
        let visible_y = inner_area_y as i32 + (line_idx as i32 - scroll as i32);
        if visible_y >= inner_area_y as i32
            && visible_y < (inner_area_y as i32 + visible_height as i32)
        {
            let marker_area = Rect::new(chunks[1].x + border_width, visible_y as u16, 1, 1);
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "▶",
                    Style::default().fg(Color::Black).bg(theme.info_accent()),
                )),
                marker_area,
            );
        }
    }

    if !app.message_queue.is_empty() {
        let mut queue_lines: Vec<Line> = Vec::new();
        for (i, msg) in app.message_queue.iter().enumerate() {
            let prefix = if i == 0 { " ▶ " } else { "   " };
            queue_lines.push(Line::from(Span::styled(
                format!("{}{}. {}", prefix, i + 1, msg),
                muted,
            )));
        }
        let queue = Paragraph::new(queue_lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" Queue ({}) ", app.message_queue.len()))
                    .style(Style::default().bg(bg_color)),
            )
            .wrap(Wrap { trim: true });
        frame.render_widget(queue, chunks[2]);
    }

    let input_chunk = chunks[3];
    frame.render_widget(app.input.textarea(), input_chunk);

    render_slash_popup(
        frame,
        &app.slash_state,
        input_chunk,
        theme,
        &app.slash_registry,
    );
    render_session_picker(frame, &app.session_picker, app.session_id.as_deref(), theme);
    render_toast_banner(frame, active_toast, theme);
}

fn render_toast_banner(frame: &mut Frame, active_toast: Option<&str>, theme: &dyn OutputTheme) {
    if let Some(msg) = active_toast {
        let area = frame.area();
        let msg_width = msg.chars().count() as u16 + 4;
        let x = (area.width.saturating_sub(msg_width)) / 2;
        let y = 2;
        let toast_area = Rect::new(x, y, msg_width.min(area.width), 3);

        frame.render_widget(Clear, toast_area);
        let toast_bg_color = if msg.starts_with('✓') {
            Color::Rgb(0, 162, 97)
        } else if msg.starts_with('✗') {
            Color::Rgb(248, 113, 113)
        } else {
            theme.info_accent()
        };
        let toast_text = Paragraph::new(Line::from(Span::styled(
            format!(" {} ", msg),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .style(Style::default().bg(toast_bg_color)),
        );
        frame.render_widget(toast_text, toast_area);
    }
}
