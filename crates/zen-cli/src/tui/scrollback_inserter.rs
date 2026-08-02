use std::io;

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Rect, Size};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Widget, Wrap};
use unicode_width::UnicodeWidthChar;

use super::app::ScrollbackEntry;

/// Insert all finalized scrollback entries above the inline viewport.
///
/// Uses the terminal's `insert_before` primitive so the emitted lines become
/// native terminal scrollback text (selectable, copyable, searchable). The
/// bottom viewport is not redrawn here; the caller should call
/// `terminal.draw()` afterwards.
pub fn insert_scrollback_queue(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    queue: &mut std::collections::VecDeque<ScrollbackEntry>,
) -> io::Result<()> {
    while let Some(entry) = queue.pop_front() {
        if entry.lines.is_empty() {
            continue;
        }
        insert_lines(terminal, &entry.lines, entry.wrap)?;
    }
    Ok(())
}

/// Insert a single block of `Line`s above the inline viewport.
///
/// When `wrap` is true, lines are soft-wrapped to the terminal width. When
/// false, lines are truncated to the terminal width (suitable for ASCII art
/// banners where overflow is intentional and wrapping would break alignment).
pub fn insert_lines(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    lines: &[Line],
    wrap: bool,
) -> io::Result<()> {
    if lines.is_empty() {
        return Ok(());
    }

    let size = terminal.size().unwrap_or(Size::new(20, 24));
    let width = size.width;
    if width == 0 {
        return Ok(());
    }

    let text = Text::from(lines.to_vec());
    if wrap {
        let height = measure_wrapped_height(&text, width);
        let area = Rect::new(0, 0, width, height);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .render(area, &mut buf);
        terminal.insert_before(height, |b| {
            for (dst_y, src_y) in (0..height).enumerate() {
                copy_row(b, dst_y as u16, &buf, src_y, width);
            }
        })?;
    } else {
        let height = lines.len() as u16;
        let area = Rect::new(0, 0, width, height);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        for (i, line) in lines.iter().enumerate() {
            let truncated = truncate_line(line, width);
            let line_area = Rect::new(0, i as u16, width, 1);
            truncated.render(line_area, &mut buf);
        }
        terminal.insert_before(height, |b| {
            for (dst_y, src_y) in (0..height).enumerate() {
                copy_row(b, dst_y as u16, &buf, src_y, width);
            }
        })?;
    }

    Ok(())
}

fn measure_wrapped_height(text: &Text, width: u16) -> u16 {
    let max_rows = 10_000u16;
    let area = Rect::new(0, 0, width, max_rows);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    Paragraph::new(text.clone())
        .wrap(Wrap { trim: false })
        .render(area, &mut buf);

    let mut first: Option<u16> = None;
    let mut last = 0u16;
    for y in 0..area.height {
        let row_has_content =
            (0..area.width).any(|x| !buf.cell((x, y)).is_none_or(|cell| cell.symbol().is_empty()));
        if row_has_content {
            if first.is_none() {
                first = Some(y);
            }
            last = y + 1;
        }
    }
    match first {
        Some(f) => (last - f).max(1),
        None => 1,
    }
}

fn copy_row(
    dst: &mut ratatui::buffer::Buffer,
    dst_y: u16,
    src: &ratatui::buffer::Buffer,
    src_y: u16,
    width: u16,
) {
    for x in 0..width {
        let cell = src.cell((x, src_y)).cloned().unwrap_or_default();
        if let Some(dst_cell) = dst.cell_mut((x, dst_y)) {
            dst_cell.set_symbol(cell.symbol());
            dst_cell.set_style(cell.style());
        }
    }
}

fn truncate_line<'a>(line: &'a Line<'a>, width: u16) -> Line<'a> {
    if width == 0 {
        return Line::from("");
    }
    let line_width = line.width();
    if line_width <= width as usize {
        return line.clone();
    }

    let mut spans: Vec<Span<'a>> = Vec::new();
    let mut current_width = 0usize;
    for span in &line.spans {
        let span_width = span.width();
        if current_width + span_width <= width as usize {
            spans.push(span.clone());
            current_width += span_width;
        } else {
            let remaining = width as usize - current_width;
            if remaining > 0 {
                spans.push(truncate_span(span, remaining));
            }
            break;
        }
    }
    Line::from(spans)
}

fn truncate_span<'a>(span: &'a Span<'a>, max_width: usize) -> Span<'a> {
    let mut chars = Vec::new();
    let mut current_width = 0usize;
    for ch in span.content.chars() {
        let w = ch.width().unwrap_or(0);
        if current_width + w > max_width {
            break;
        }
        chars.push(ch);
        current_width += w;
    }
    Span::styled(chars.into_iter().collect::<String>(), span.style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Span;

    #[test]
    fn truncate_line_respects_width() {
        let line = Line::from("hello world this is long");
        let truncated = truncate_line(&line, 10);
        assert_eq!(truncated.width(), 10);
    }

    #[test]
    fn truncate_span_respects_width() {
        let span = Span::raw("hello world");
        let truncated = truncate_span(&span, 5);
        assert_eq!(truncated.width(), 5);
    }
}
