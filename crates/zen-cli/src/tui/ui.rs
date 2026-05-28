use super::app::App;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Margin};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

pub fn render(frame: &mut Frame, app: &App) {
    let queue_height = if app.message_queue.is_empty() { 0 } else { 2 + app.message_queue.len() as u16 };

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
            let prefix = if i == 0 {
                " ▶ "
            } else {
                "   "
            };
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

    let input_area = chunks[3].inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    let visible_width = input_area.width as usize;

    let cursor_col = app.cursor_position.min(app.input.len());
    let (scroll_start, display_text) = if cursor_col <= visible_width {
        (0, app.input.as_str())
    } else {
        let offset = cursor_col - visible_width + 1;
        (offset, &app.input[offset..])
    };

    let input_display = format!("> {}", display_text);
    let input = Paragraph::new(input_display).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Input (Enter=send, Ctrl+D=quit) "),
    );
    frame.render_widget(input, chunks[3]);

    frame.set_cursor_position((
        input_area.x + (cursor_col.saturating_sub(scroll_start)) as u16 + 2,
        input_area.y,
    ));
}
