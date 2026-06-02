use super::app::App;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Margin};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

pub fn render(frame: &mut Frame, app: &App) {
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

    let mut status_spans = vec![
        " Zen ".bold().black().on_blue(),
        " | Model: ".into(),
        Span::styled(&app.model, Style::default().fg(Color::Cyan)),
    ];
    if app.is_streaming {
        status_spans.push(" | ⏳ Processing...".bold().fg(Color::Yellow));
    }
    if app.show_thinking {
        status_spans.push(" | 🧠 Thinking: ON".fg(Color::Magenta));
    }
    if let Some(sid) = &app.session_id {
        status_spans.push(" | Session: ".into());
        status_spans.push(Span::styled(sid, Style::default().fg(Color::Green)));
    }
    status_spans.push(" | Workspace: ".into());
    status_spans.push(Span::styled(
        &app.workspace,
        Style::default().fg(Color::Yellow),
    ));
    let status = Line::from(status_spans);
    frame.render_widget(Paragraph::new(status), chunks[0]);

    let mut output_text: Vec<Line> = app
        .output
        .iter()
        .map(|line| {
            if line.is_error {
                Line::from(Span::styled(&line.text, Style::default().fg(Color::Red)))
            } else {
                Line::from(line.text.clone())
            }
        })
        .collect();

    if app.is_streaming && !app.streaming_buffer.is_empty() {
        output_text.push(Line::from(Span::styled(
            &app.streaming_buffer,
            Style::default().fg(Color::DarkGray).italic(),
        )));
        output_text.push(Line::from("▌".bold().fg(Color::Yellow)));
    }

    let scroll_offset = app
        .output
        .len()
        .saturating_sub(chunks[1].height as usize - 2) as u16;
    let output = Paragraph::new(output_text)
        .block(Block::default().borders(Borders::ALL).title(" Chat "))
        .wrap(Wrap { trim: false })
        .scroll((scroll_offset, 0));
    frame.render_widget(output, chunks[1]);

    if !app.message_queue.is_empty() {
        let mut queue_lines: Vec<Line> = Vec::new();
        for (i, msg) in app.message_queue.iter().enumerate() {
            let prefix = if i == 0 { " ▶ " } else { "   " };
            queue_lines.push(Line::from(Span::styled(
                format!("{}{}. {}", prefix, i + 1, msg),
                Style::default().fg(Color::Cyan),
            )));
        }
        let queue = Paragraph::new(queue_lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" Queue ({}) ", app.message_queue.len())),
            )
            .wrap(Wrap { trim: true });
        frame.render_widget(queue, chunks[2]);
    }

    let input_chunk = chunks[3];
    let input_area = input_chunk.inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    let visible_width = input_area.width as usize;

    let char_count = app.input.chars().count();
    let cursor_col = app.cursor_position.min(char_count);

    let scroll_start = if visible_width < 2 || cursor_col <= visible_width {
        0
    } else {
        cursor_col.saturating_sub(visible_width.saturating_sub(2))
    };

    let display_text = if scroll_start == 0 {
        app.input.clone()
    } else {
        app.input.chars().skip(scroll_start).collect()
    };

    let input_display = format!("> {}", display_text);
    let input = Paragraph::new(input_display).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Input (Enter=send, Ctrl+D=quit, Tab=complete) "),
    );

    let cursor_prefix: String = app.input.chars().take(cursor_col).collect();
    let cursor_visual_pos = UnicodeWidthStr::width(cursor_prefix.as_str()) as u16;
    let scroll_visual_offset = if scroll_start == 0 {
        0u16
    } else {
        let scrolled: String = app.input.chars().take(scroll_start).collect();
        UnicodeWidthStr::width(scrolled.as_str()) as u16
    };

    let cursor_x = input_chunk.x + 2 + cursor_visual_pos - scroll_visual_offset;
    frame.render_widget(input, input_chunk);

    frame.set_cursor_position((cursor_x, input_chunk.y + 1));

    if app.show_autocomplete && !app.autocomplete_suggestions.is_empty() {
        let max_suggestions = 5;
        let suggestion_count = app.autocomplete_suggestions.len().min(max_suggestions);
        let popup_height = (suggestion_count + 2) as u16;
        let popup_width = input_chunk.width.min(50);

        let popup_y = input_chunk.y.saturating_sub(popup_height);
        let popup_area = ratatui::layout::Rect {
            x: input_chunk.x,
            y: popup_y,
            width: popup_width,
            height: popup_height,
        };

        if popup_y < input_chunk.y {
            frame.render_widget(Clear, popup_area);

            let scroll_offset = app.autocomplete_scroll_offset;
            let suggestion_lines: Vec<Line> = app
                .autocomplete_suggestions
                .iter()
                .enumerate()
                .skip(scroll_offset)
                .take(max_suggestions)
                .map(|(i, s)| {
                    let style = if i == app.autocomplete_selected {
                        Style::default().fg(Color::Black).bg(Color::Cyan)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    };
                    let marker = if i == app.autocomplete_selected {
                        "▸ "
                    } else {
                        "  "
                    };
                    Line::from(Span::styled(format!("{}{}", marker, s), style))
                })
                .collect();

            let total = app.autocomplete_suggestions.len();
            let title = if total > max_suggestions {
                format!(
                    " Suggestions ({}/{}) (Tab=next, Enter=select, Esc=close) ",
                    app.autocomplete_selected + 1,
                    total
                )
            } else {
                " Suggestions (Tab=next, Enter=select, Esc=close) ".to_string()
            };

            let suggestions = Paragraph::new(suggestion_lines)
                .block(Block::default().borders(Borders::ALL).title(title))
                .wrap(Wrap { trim: true });
            frame.render_widget(suggestions, popup_area);
        }
    }
}
