use super::app::App;
use super::cell::streaming::StreamingCell;
use super::session_picker::render_session_picker;
use super::slash::render_slash_popup;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

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

    let mut all_lines: Vec<Line<'static>> = Vec::new();
    for cell in &app.output {
        all_lines.extend(cell.display_lines());
        all_lines.push(Line::default());
    }

    if app.is_streaming && !app.stream_collector.is_empty() {
        let streaming_cell = StreamingCell::new(app.stream_collector.buffer(), app.theme.as_ref());
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
        .block(Block::default().borders(Borders::ALL).title(" Chat "))
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph.scroll((scroll as u16, 0)), chunks[1]);

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
    frame.render_widget(&app.input, input_chunk);

    render_slash_popup(frame, &app.slash_state, input_chunk);
    render_session_picker(frame, &app.session_picker, app.session_id.as_deref());
}
