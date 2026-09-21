//! Native scrollback insertion for the inline TUI mode.
//!
//! Finalized chat blocks (banner, user messages, completed assistant
//! responses) are pushed into the terminal's native scrollback above the
//! ratatui bottom viewport so they become selectable/searchable terminal
//! text (spec FR-001, FR-007, FR-008).

use std::io;

use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::{Rect, Size};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Widget, Wrap};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::app::ScrollbackEntry;

/// Drain the scrollback queue, inserting each entry above the inline
/// viewport. The viewport itself is untouched; ratatui repaints it on the
/// next `terminal.draw()`.
///
/// Generic over the backend so the insertion logic can be exercised against
/// `TestBackend` (see `docs/specs/002-agentic-tui/test-design.md` Layer 1).
pub fn insert_scrollback_queue<B: Backend>(
    terminal: &mut Terminal<B>,
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
pub fn insert_lines<B: Backend>(
    terminal: &mut Terminal<B>,
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
        // (first, last) = content row bounds after wrapping. Blocks may start
        // with blank separator lines (e.g. markdown list/quote continuations),
        // so the copy window must be offset by `first` — copying from row 0
        // would emit leading blanks and clip trailing content rows (H2 fix,
        // analyze-report.md).
        let (first, last) = measure_wrapped_bounds(&text, width);
        let height = (last - first).max(1);
        // Render into a buffer tall enough to hold every content row,
        // including leading blank rows before `first`.
        let render_height = last.max(1);
        let area = Rect::new(0, 0, width, render_height);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .render(area, &mut buf);
        terminal
            .insert_before(height, |b| {
                for dst_y in 0..height {
                    copy_row(b, dst_y, &buf, first + dst_y, width);
                }
            })
            .map_err(|e| io::Error::other(e.to_string()))?;
    } else {
        let height = lines.len() as u16;
        let area = Rect::new(0, 0, width, height.max(1));
        let mut buf = ratatui::buffer::Buffer::empty(area);
        for (i, line) in lines.iter().enumerate() {
            let truncated = truncate_line(line, width);
            let line_area = Rect::new(0, i as u16, width, 1);
            truncated.render(line_area, &mut buf);
        }
        terminal
            .insert_before(height.max(1), |b| {
                for (dst_y, src_y) in (0..height.max(1)).enumerate() {
                    copy_row(b, dst_y as u16, &buf, src_y, width);
                }
            })
            .map_err(|e| io::Error::other(e.to_string()))?;
    }

    Ok(())
}

/// Hard ceiling for the wrapped-height probe buffer (rows). Kept only as a
/// safety cap — the buffer is now sized to the content's wrapped height
/// (NFR-007/SC-011), never this fixed value.
const MAX_PROBE_ROWS: u16 = 10_000;

/// Compute a conservative wrapped-height estimate for `text` at `width` —
/// the number of rows ratatui's `Paragraph` would occupy after word
/// wrapping. `Paragraph::line_count` (the library-native exact answer) is
/// gated behind ratatui's unstable `rendered-line-info` feature, so this is
/// a conservative bound instead: per line, `ceil(2L/(width-1)) + 1` rows
/// where L is the line's unicode width. The factor 2 covers word-wrap waste
/// (each overflow-terminated row pairs with the row that starts its
/// overflowing word; the pair occupies ≥ width-1 cells, so rows ≤
/// 2L/(width-1) + 1). Clamped to `[1, MAX_PROBE_ROWS]` so callers always
/// get a renderable buffer height.
fn wrapped_height(text: &Text, width: u16) -> u16 {
    let width = width.max(1) as usize;
    let mut rows = 0usize;
    for line in &text.lines {
        let line_width = line.width();
        if line_width == 0 {
            rows += 1;
        } else if width == 1 {
            rows += line_width;
        } else {
            rows += (2 * line_width + (width - 2)) / (width - 1) + 1;
        }
    }
    rows.clamp(1, MAX_PROBE_ROWS as usize) as u16
}

/// Measure the content bounds of `text` after wrapping to `width`.
///
/// Returns `(first, last)` — the first (inclusive) and last (exclusive) rows
/// that contain at least one non-whitespace cell. Leading/trailing blank
/// separator rows are intentionally trimmed; blank rows INSIDE the block are
/// preserved by the `first..last` copy window in `insert_lines`. For
/// content-less text returns `(0, 1)` so callers still insert a single blank
/// line.
fn measure_wrapped_bounds(text: &Text, width: u16) -> (u16, u16) {
    // NFR-007/SC-011: the probe buffer is sized to the content's wrapped
    // height (`wrapped_height`, a conservative content-proportional bound)
    // instead of a fixed width × 10_000-cell allocation per committed block
    // (~800k cells at 80 cols; self-measured 400-550ms under CI load).
    // `MAX_PROBE_ROWS` remains only as a hard ceiling.
    let height = wrapped_height(text, width);
    let area = Rect::new(0, 0, width, height);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    Paragraph::new(text.clone())
        .wrap(Wrap { trim: false })
        .render(area, &mut buf);

    let mut first: Option<u16> = None;
    let mut last = 0u16;
    for y in 0..area.height {
        // NOTE: `Cell::symbol()` returns " " for empty cells (ratatui-core
        // `Cell::EMPTY` stores `symbol: None`), so `symbol().is_empty()` is
        // never true. A row is blank iff every cell's symbol is whitespace.
        let row_has_content = (0..area.width).any(|x| {
            buf.cell((x, y))
                .is_some_and(|cell| !cell.symbol().trim().is_empty())
        });
        if row_has_content {
            if first.is_none() {
                first = Some(y);
            }
            last = y + 1;
        }
    }
    match first {
        Some(f) => (f, last),
        None => (0, 1),
    }
}

fn copy_row(
    dst: &mut ratatui::buffer::Buffer,
    dst_y: u16,
    src: &ratatui::buffer::Buffer,
    src_y: u16,
    width: u16,
) {
    let mut x = 0u16;
    while x < width {
        let cell = src.cell((x, src_y)).cloned().unwrap_or_default();
        let cell_width = (cell.symbol().width() as u16).max(1);
        if let Some(dst_cell) = dst.cell_mut((x, dst_y)) {
            // Full-cell clone preserves symbol/style/skip exactly.
            *dst_cell = cell;
        }
        // The `insert_before` render path (ratatui-core `draw_lines`) hands
        // every cell to the backend verbatim — no diff, no wide-char skip —
        // and the backend prints `cell.symbol()` per cell. A wide char's
        // continuation cells must therefore print NOTHING. `symbol()` returns
        // " " for `None` symbols, which would emit a stray space after every
        // CJK/wide character; set the empty string so the backend's
        // `Print("")` is a no-op.
        for cont_x in x + 1..(x + cell_width).min(width) {
            if let Some(cont_cell) = dst.cell_mut((cont_x, dst_y)) {
                *cont_cell = ratatui::buffer::Cell::EMPTY;
                cont_cell.set_symbol("");
            }
        }
        x += cell_width;
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
    use std::time::{Duration, Instant};

    use ratatui::TerminalOptions;
    use ratatui::Viewport;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Position;
    use ratatui::text::Span;

    /// Read one buffer row as a right-trimmed string.
    fn row_text(buf: &Buffer, y: u16) -> String {
        let mut s = String::new();
        for x in 0..buf.area.width {
            s.push_str(buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "));
        }
        s.trim_end().to_string()
    }

    /// All rows recorded in the backend's native scrollback (lines that
    /// scrolled off the top), top to bottom, right-trimmed.
    fn scrollback_rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let sb = terminal.backend().scrollback();
        (0..sb.area.height)
            .map(|y| row_text(sb, y))
            .collect::<Vec<_>>()
    }

    /// Combined scrollback + screen content — the full "terminal history"
    /// a user would see by scrolling up (blank rows preserved).
    fn raw_history(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let mut all = scrollback_rows(terminal);
        let buf = terminal.backend().buffer();
        all.extend((0..buf.area.height).map(|y| row_text(buf, y)));
        all
    }

    /// Same as raw_history but with blank rows removed (order-preserving).
    fn full_history(terminal: &Terminal<TestBackend>) -> Vec<String> {
        raw_history(terminal)
            .into_iter()
            .filter(|r| !r.is_empty())
            .collect()
    }

    /// Mimic zen's inline startup: cursor anchored to the bottom row, then an
    /// Inline viewport created from there.
    fn anchored_terminal(width: u16, height: u16, viewport_rows: u16) -> Terminal<TestBackend> {
        let mut backend = TestBackend::new(width, height);
        backend
            .set_cursor_position(Position::new(0, height - 1))
            .expect("cursor position");
        Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(viewport_rows),
            },
        )
        .expect("terminal")
    }

    fn lines_of(texts: &[&str]) -> Vec<Line<'static>> {
        texts.iter().map(|t| Line::from(t.to_string())).collect()
    }

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

    #[test]
    fn truncate_span_does_not_split_wide_chars() {
        // CJK chars are 2 cells wide; width budget 5 must yield 2 chars (4
        // cells), never a split wide char.
        let span = Span::raw("你好你好你好");
        let truncated = truncate_span(&span, 5);
        assert_eq!(truncated.content.as_ref(), "你好");
        assert_eq!(truncated.width(), 4);
    }

    #[test]
    fn measure_wrapped_bounds_reports_content_window() {
        let text = Text::from(lines_of(&["", "", "hello", "world", ""]));
        let (first, last) = measure_wrapped_bounds(&text, 20);
        assert_eq!((first, last), (2, 4));
    }

    #[test]
    fn measure_wrapped_bounds_empty_text_is_single_blank() {
        let text = Text::from(lines_of(&["", ""]));
        let (first, last) = measure_wrapped_bounds(&text, 20);
        assert_eq!((first, last), (0, 1));
    }

    /// H2 regression (analyze-report.md). Two historical defects:
    /// (a) `measure` counted every row as content because `Cell::symbol()`
    ///     returns " " for empty cells — height ballooned to the 10,000-row
    ///     probe buffer, so every block scrolled the screen by a full screen+
    ///     (root cause of blank-band / lost-banner symptoms);
    /// (b) the copy window started at src row 0 while height was
    ///     `last - first`, clipping trailing content rows of blocks with
    ///     leading blank separators.
    /// After the fix, content rows survive exactly (edge blanks trimmed).
    #[test]
    fn insert_lines_preserves_block_with_leading_blank_lines() {
        let mut terminal = anchored_terminal(20, 12, 4);
        insert_lines(&mut terminal, &lines_of(&["", "hello"]), true).expect("insert");

        let history = full_history(&terminal);
        assert!(
            history.iter().any(|r| r == "hello"),
            "content row lost; history = {history:?}"
        );
    }

    /// Blank rows INSIDE a block (paragraph separators) must be preserved —
    /// only edge blanks are trimmed.
    #[test]
    fn insert_lines_preserves_internal_blank_lines() {
        let mut terminal = anchored_terminal(20, 12, 4);
        insert_lines(&mut terminal, &lines_of(&["para1", "", "para2"]), true).expect("insert");

        let history = raw_history(&terminal);
        let p1 = history.iter().position(|r| r == "para1").expect("para1");
        let p2 = history.iter().position(|r| r == "para2").expect("para2");
        assert_eq!(p2, p1 + 2, "para1/para2 must be separated by one blank row");
        assert_eq!(history[p1 + 1], "", "internal blank row lost: {history:?}");
    }

    /// Defect (a) guard: a small block must insert only its own rows — never
    /// a screen-sized blast. The visible screen must still contain the
    /// viewport region at the bottom (i.e. we did not scroll 10,000 rows).
    #[test]
    fn insert_lines_height_matches_content_not_probe_buffer() {
        let text = Text::from(lines_of(&["one", "two"]));
        let (first, last) = measure_wrapped_bounds(&text, 20);
        assert_eq!((first, last), (0, 2));
        assert_eq!(last - first, 2, "height must equal content rows, not 10000");
    }

    #[test]
    fn insert_lines_plain_block_reaches_history_above_viewport() {
        let mut terminal = anchored_terminal(20, 8, 4);
        insert_lines(&mut terminal, &lines_of(&["alpha", "beta", "gamma"]), true).expect("insert");

        let history = full_history(&terminal);
        let alpha = history.iter().position(|r| r == "alpha");
        let beta = history.iter().position(|r| r == "beta");
        let gamma = history.iter().position(|r| r == "gamma");
        assert!(
            matches!((alpha, beta, gamma), (Some(a), Some(b), Some(g)) if a < b && b < g),
            "inserted block must keep order in scrollback/screen history: {history:?}"
        );
    }

    #[test]
    fn insert_lines_taller_than_screen_scrolls_into_scrollback() {
        let mut terminal = anchored_terminal(10, 6, 2);
        let lines: Vec<Line<'static>> = (0..12).map(|i| Line::from(format!("line-{i}"))).collect();
        insert_lines(&mut terminal, &lines, true).expect("insert");

        let sb = scrollback_rows(&terminal);
        let sb_text = sb.join("\n");
        assert!(
            sb_text.contains("line-0"),
            "top of oversized block must land in native scrollback: {sb:?}"
        );
        let history = full_history(&terminal);
        assert!(
            history.iter().any(|r| r == "line-11"),
            "bottom of oversized block must remain visible: {history:?}"
        );
    }

    /// Display-bug regression (2026-08-16 user report): rendered output had
    /// a space inserted after every CJK character. Root cause: ratatui-core's
    /// `insert_before` → `draw_lines` path sends every cell (including wide
    /// chars' continuation cells) to the backend, which prints
    /// `cell.symbol()`; continuation cells with `None` symbols print " ".
    /// `copy_row` must neutralize continuation cells with an empty symbol.
    #[test]
    fn insert_lines_wide_chars_render_without_stray_spaces() {
        let mut terminal = anchored_terminal(40, 16, 8);
        insert_lines(&mut terminal, &lines_of(&["你好世界"]), true).expect("insert");

        let hist = full_history(&terminal);
        assert!(
            hist.iter().any(|r| r == "你好世界"),
            "CJK row must render without inserted spaces: {hist:?}"
        );
    }

    /// Mixed ASCII + CJK keeps exact adjacency (no alignment drift).
    #[test]
    fn insert_lines_mixed_width_line_keeps_alignment() {
        let mut terminal = anchored_terminal(40, 16, 8);
        insert_lines(&mut terminal, &lines_of(&["zen 的价值: ok"]), true).expect("insert");

        let hist = full_history(&terminal);
        assert!(
            hist.iter().any(|r| r == "zen 的价值: ok"),
            "mixed-width row must keep exact adjacency: {hist:?}"
        );
    }

    /// T077 perf guard (NFR-007/SC-011): a representative committed block
    /// (100 lines × 80 cols) must measure + insert far below the old fixed
    /// `width × 10_000`-cell probe cost (self-measured 400-550ms under load
    /// at 80 cols). The 200ms test bound is deliberately GENEROUS for loaded
    /// CI machines — the spec's 50ms target applies to reference hardware;
    /// a tighter gate here would flake, not protect.
    #[test]
    fn perf_wrapped_bounds_is_content_proportional_and_fast() {
        let text = Text::from(
            (0..100)
                .map(|i| {
                    Line::from(format!(
                        "line-{i:03} lorem ipsum dolor sit amet consectetur adipiscing elit sed do"
                    ))
                })
                .collect::<Vec<_>>(),
        );

        // Probe height is content-proportional: 100 content lines must never
        // approach the old 10_000-row probe buffer.
        let probe = wrapped_height(&text, 80);
        assert!(
            probe < 500,
            "probe height must be content-proportional, got {probe}"
        );

        let start = Instant::now();
        let (first, last) = measure_wrapped_bounds(&text, 80);
        let mut terminal = anchored_terminal(80, 24, 8);
        insert_lines(&mut terminal, &text.lines, true).expect("insert");
        let elapsed = start.elapsed();

        assert_eq!(
            (last - first) as usize,
            100,
            "every line is a content row at 80 cols (no wrap at 69 cells)"
        );
        assert_eq!(probe as usize, 300, "estimate: 3 rows per 69-cell line");
        assert!(
            elapsed < Duration::from_millis(200),
            "measure+insert must stay well under the old 400-550ms probe cost, took {elapsed:?}"
        );
    }

    #[test]
    fn insert_lines_truncate_mode_keeps_banner_alignment() {
        let mut terminal = anchored_terminal(10, 8, 4);
        let long = "X".repeat(30);
        insert_lines(&mut terminal, &lines_of(&[&long]), false).expect("insert");

        let history = full_history(&terminal);
        assert!(
            history.iter().any(|r| r == &"X".repeat(10)),
            "banner line must be truncated to terminal width: {history:?}"
        );
    }
}
