use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use mdstream::{BlockId, BlockKind, DocumentState, MdStream, Options};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use tui_markdown::StyleSheet;

/// Zen's style sheet for tui-markdown: headings keep their marker stripped
/// (codex-convention per-level hierarchy from theme.rs), links use accent +
/// underline with the renderer's ` (url)` suffix stripped post-render, and
/// blockquotes stay deliberately plain (style freeze — line-style
/// preservation below would otherwise surface the vendored default green).
#[derive(Clone, Copy)]
struct NoMarkerStyleSheet;

impl StyleSheet for NoMarkerStyleSheet {
    fn heading(&self, level: u8) -> Style {
        crate::tui::theme::markdown_heading_style(level)
    }

    fn heading_marker(&self, _level: u8) -> &str {
        ""
    }

    fn blockquote(&self) -> Style {
        Style::new()
    }

    fn link(&self) -> Style {
        crate::tui::theme::markdown_link_style()
    }
}

fn hash_string(s: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

fn convert_text_to_owned_lines(text: ratatui::text::Text) -> Vec<Line<'static>> {
    text.lines
        .into_iter()
        .map(|line| {
            let spans: Vec<Span<'static>> = line
                .spans
                .into_iter()
                .map(|span| Span::styled(span.content.to_string(), span.style))
                .collect();
            let mut out = Line::from(spans);
            // tui-markdown styles whole elements at the line level (headings,
            // blockquotes, alerts, metadata blocks all ride on `line.style`
            // while their text spans stay plain) — dropping it silently
            // stripped every one of those styles. Preserve it verbatim so the
            // stylesheet's choices actually reach the terminal.
            out.style = line.style;
            out
        })
        .collect()
}

/// Render one top-level markdown block.
///
/// Standalone thematic breaks render as a dim em-dash rule (codex
/// convention — tui-markdown hardcodes a plain `---` line with no
/// stylesheet hook), code fences go through syntect/two-face highlighting
/// (T028 / FR-012) so the inline streaming path matches the full-screen
/// TUI; everything else stays on `tui_markdown`.
fn render_block_dispatch(raw: &str) -> Vec<Line<'static>> {
    if let Some(lines) = render_rule_block(raw) {
        return lines;
    }
    if let Some(lines) = render_code_fence_block(raw) {
        return lines;
    }
    render_block_via_tui_markdown(raw)
}

/// HR intercept: a block that is exactly one thematic-break line
/// (`---` / `***` / `___`) renders as a dim em-dash x3 rule.
/// tui-markdown's `Rule` renderer emits a hardcoded plain `---` line, so a
/// block-level intercept is the only clean hook. Multi-line `---`-led
/// blocks (YAML metadata, setext headings) fall through untouched.
fn render_rule_block(raw: &str) -> Option<Vec<Line<'static>>> {
    render_rule_block_with(raw, no_color_enabled())
}

/// `no_color` seam so tests can pin both degradation branches without
/// mutating process env (tests run concurrently).
fn render_rule_block_with(raw: &str, no_color: bool) -> Option<Vec<Line<'static>>> {
    let mut lines = raw.lines();
    let first = lines.next()?;
    if lines.next().is_some() || !is_thematic_break_line(first) {
        return None;
    }
    // FR-011: color degrades under NO_COLOR — plain Reset style, the rule
    // glyph itself still marks the break.
    let style = if no_color {
        Style::default()
    } else {
        Style::default()
            .fg(ratatui::style::Color::DarkGray)
            .add_modifier(ratatui::style::Modifier::DIM)
    };
    Some(vec![Line::styled("———".to_string(), style)])
}

/// CommonMark thematic break: 0-3 leading spaces (4+ would be an indented
/// code block), then a run of >= 3 identical `-` / `*` / `_`. Spaced forms
/// (`- - -`) intentionally don't match — they keep the tui-markdown
/// fallback rather than widening the intercept.
fn is_thematic_break_line(line: &str) -> bool {
    let stripped = line.trim_start_matches(' ');
    if line.len() - stripped.len() > 3 {
        return false;
    }
    let core = stripped.trim_end().as_bytes();
    core.len() >= 3
        && (core[0] == b'-' || core[0] == b'*' || core[0] == b'_')
        && core.iter().all(|&c| c == core[0])
}

/// Parse a fenced code block: ```` ```lang ```` opener, content, optional
/// closing fence (pending streaming blocks may still be open).
fn parse_code_fence(raw: &str) -> Option<(String, String)> {
    let trimmed = raw.trim_start();
    let rest = trimmed.strip_prefix("```")?;
    let mut lines = rest.lines();
    let lang = lines.next()?.trim().to_string();
    let mut content: Vec<&str> = Vec::new();
    for line in lines {
        if line.trim_start().starts_with("```") {
            break;
        }
        content.push(line);
    }
    Some((lang, content.join("\n")))
}

/// FR-011 NO_COLOR probe shared by the markdown render paths. Cached per
/// process (env is never mutated at runtime; tests pin degradation through
/// explicit `*_with(no_color)` seams instead of env mutation).
fn no_color_enabled() -> bool {
    static NO_COLOR: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *NO_COLOR.get_or_init(|| std::env::var("NO_COLOR").is_ok())
}

/// Highlighted, framed code fence for the inline scrollback path. Mirrors
/// `CodeCell` framing (`┌─ lang` / `└─`). Respects `NO_COLOR` (FR-011) by
/// falling back to plain text.
fn render_code_fence_block(raw: &str) -> Option<Vec<Line<'static>>> {
    let (lang, code) = parse_code_fence(raw)?;

    let no_color = no_color_enabled();

    let header_style = ratatui::style::Style::default()
        .fg(ratatui::style::Color::DarkGray)
        .add_modifier(ratatui::style::Modifier::BOLD);
    let mut lines = vec![Line::styled(format!("┌─ {}", lang), header_style)];
    let lang_lower = lang.to_lowercase();
    if no_color {
        lines.extend(code.lines().map(|l| Line::raw(l.to_string())));
    } else if matches!(lang_lower.as_str(), "diff" | "patch") {
        // T031: diff blocks render as explicit red/green deltas instead of
        // syntect's muted theme palette — the red/green IS the point.
        lines.extend(code.lines().map(render_diff_line));
    } else {
        lines.extend(super::highlight::highlight_code(&code, &lang));
    }
    lines.push(Line::styled("└─", header_style));
    Some(lines)
}

/// T031: one unified-diff line → its conventional color. `+` additions
/// green, `-` removals red, `@@` hunk headers dim cyan, `+++`/`---` file
/// headers dark gray, context unstyled. Rendered at the fence layer (not
/// syntect) so the colors are the explicit red/green the task pins rather
/// than a highlight theme's approximation.
fn render_diff_line(line: &str) -> Line<'static> {
    use ratatui::style::{Color, Modifier, Style};
    let style = if line.starts_with("+++") || line.starts_with("---") {
        Style::default().fg(Color::DarkGray)
    } else if line.starts_with('+') {
        Style::default().fg(Color::Green)
    } else if line.starts_with('-') {
        Style::default().fg(Color::Red)
    } else if line.starts_with("@@") {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM)
    } else {
        Style::default()
    };
    Line::styled(line.to_string(), style)
}

fn render_block_via_tui_markdown(raw: &str) -> Vec<Line<'static>> {
    let options = tui_markdown::Options::new(NoMarkerStyleSheet);
    let text = tui_markdown::from_str_with_options(raw, &options);
    let mut lines = convert_text_to_owned_lines(text);
    replace_unordered_list_markers(&mut lines);
    strip_link_destinations(&mut lines, crate::tui::theme::markdown_link_style());
    lines
}

/// Unordered list markers render as the `• ` bullet glyph (codex/hermes
/// convention). tui-markdown hardcodes `"- "` and emits the marker as its
/// own span shaped `"<indent>- "` (indent = nested-list padding), so an
/// exact-match span swap can never touch body text. Fenced code never
/// reaches this path (`render_code_fence_block` intercepts first). Ordered
/// `{n}. ` markers don't match the shape and stay untouched.
fn replace_unordered_list_markers(lines: &mut [Line<'static>]) {
    for line in lines {
        for span in &mut line.spans {
            let indent_len = span.content.len() - span.content.trim_start_matches(' ').len();
            if &span.content[indent_len..] == "- " {
                span.content = format!("{}\u{2022} ", " ".repeat(indent_len)).into();
            }
        }
    }
}

/// Remove the ` (destination)` suffix tui-markdown appends after every link
/// label (`label (url)` -> `label`; deliberate zen divergence from codex,
/// which keeps the destination). The renderer emits the suffix as a rigid
/// three-span group — exact `" ("`, the destination span carrying exactly
/// the link style, exact `")"` — so shape-matching is precise; the link
/// style never occurs on other span boundaries.
fn strip_link_destinations(lines: &mut [Line<'static>], link_style: Style) {
    for line in lines {
        let mut i = 0;
        while i + 2 < line.spans.len() {
            if line.spans[i].content.as_ref() == " ("
                && line.spans[i + 1].style == link_style
                && line.spans[i + 2].content.as_ref() == ")"
            {
                let _ = line.spans.drain(i..i + 3);
            } else {
                i += 1;
            }
        }
    }
}

struct RenderedBlock {
    hash: u64,
    lines: Vec<Line<'static>>,
}

pub struct IncrementalMarkdownRenderer {
    blocks: Vec<RenderedBlock>,
}

impl IncrementalMarkdownRenderer {
    pub fn new() -> Self {
        Self { blocks: Vec::new() }
    }

    pub fn update(&mut self, content: &str) -> Vec<Line<'static>> {
        let raw_blocks = split_top_level_blocks(content);

        let mut result: Vec<Line<'static>> = Vec::new();
        let mut new_blocks: Vec<RenderedBlock> = Vec::with_capacity(raw_blocks.len());

        for (i, raw) in raw_blocks.iter().enumerate() {
            let h = hash_string(raw);

            let lines: Vec<Line<'static>> = if let Some(cached) = self.blocks.get(i)
                && cached.hash == h
            {
                cached.lines.clone()
            } else {
                let is_last_block = i == raw_blocks.len() - 1;
                let render_content = if is_last_block {
                    maybe_close_unclosed_fences(raw)
                } else {
                    raw.clone()
                };
                render_block_dispatch(&render_content)
            };

            // Gap rhythm is applied at assembly (single code path for fresh
            // and cached lines) so it never desyncs from the block cache.
            let insert_gap = i > 0 && raw_block_kind(&raw_blocks[i - 1]) != raw_block_kind(raw);
            join_block_lines(&mut result, &lines, insert_gap);
            new_blocks.push(RenderedBlock { hash: h, lines });
        }

        self.blocks = new_blocks;
        result
    }

    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.blocks.clear();
    }
}

impl Default for IncrementalMarkdownRenderer {
    fn default() -> Self {
        Self::new()
    }
}

/// Block kinds for gap-rhythm classification (hermes `gap()` convention:
/// exactly one blank line between adjacent rendered blocks of DIFFERENT
/// kinds; same-kind adjacency keeps prior behavior — no inserted gap).
#[derive(Clone, Copy, PartialEq, Eq)]
enum RawBlockKind {
    Heading,
    List,
    Quote,
    Fence,
    Table,
    Rule,
    Paragraph,
}

/// Classify a raw top-level block from its first line. Only feeds gap
/// insertion — a conservative miss just changes whether one blank line is
/// inserted at a join.
fn raw_block_kind(raw: &str) -> RawBlockKind {
    let first = raw.lines().next().unwrap_or("").trim_start();
    if first.starts_with("```") {
        return RawBlockKind::Fence;
    }
    if raw.lines().count() == 1 && is_thematic_break_line(first) {
        return RawBlockKind::Rule;
    }
    if first.starts_with('#') && first.trim_start_matches('#').starts_with(' ') {
        return RawBlockKind::Heading;
    }
    if first.starts_with("- ")
        || first.starts_with("* ")
        || first.starts_with("+ ")
        || (first.as_bytes().first().is_some_and(|b| b.is_ascii_digit()) && first.contains(". "))
    {
        return RawBlockKind::List;
    }
    if first.starts_with('>') {
        return RawBlockKind::Quote;
    }
    if first.starts_with('|') {
        return RawBlockKind::Table;
    }
    RawBlockKind::Paragraph
}

fn is_blank_line(line: &Line<'static>) -> bool {
    line.spans.iter().all(|s| s.content.trim().is_empty())
}

/// Append one block's rendered lines with gap rhythm: insert exactly one
/// blank line at different-kind joins (skipped when either side already
/// ends/starts blank), squeeze incoming leading blanks when the previous
/// block already ended blank, so a join never produces two consecutive
/// blank lines. Same-kind joins keep prior adjacency.
fn join_block_lines(result: &mut Vec<Line<'static>>, lines: &[Line<'static>], insert_gap: bool) {
    if result.is_empty() {
        result.extend(lines.iter().cloned());
        return;
    }
    let prev_blank = result.last().is_some_and(is_blank_line);
    if insert_gap && !prev_blank && !lines.first().is_some_and(is_blank_line) {
        result.push(Line::raw(String::new()));
    }
    let start = if prev_blank {
        lines
            .iter()
            .position(|l| !is_blank_line(l))
            .unwrap_or(lines.len())
    } else {
        0
    };
    result.extend(lines[start..].iter().cloned());
}

fn is_continuation_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
        return true;
    }
    if trimmed.len() > 2
        && trimmed.as_bytes()[0].is_ascii_digit()
        && trimmed.as_bytes()[1] == b'.'
        && trimmed.as_bytes()[2] == b' '
    {
        return true;
    }
    if trimmed.starts_with("> ") {
        return true;
    }
    is_table_row(trimmed) || is_table_separator(trimmed)
}

fn is_table_row(trimmed: &str) -> bool {
    trimmed.starts_with('|') && trimmed.ends_with('|') && trimmed.matches('|').count() >= 2
}

fn is_table_separator(trimmed: &str) -> bool {
    trimmed.starts_with('|')
        && trimmed.ends_with('|')
        && trimmed[1..trimmed.len() - 1].split('|').all(|cell| {
            cell.trim()
                .chars()
                .all(|c| c == '-' || c == ':' || c.is_whitespace())
        })
}

fn split_top_level_blocks(content: &str) -> Vec<String> {
    let mut blocks: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut in_code_block = false;
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();

        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            current.push(line);
            i += 1;
            continue;
        }

        if !in_code_block && line.trim().is_empty() {
            let next_non_empty = lines[i + 1..].iter().find(|l| !l.trim().is_empty());
            if let Some(next) = next_non_empty {
                // Loose-list merging must never pull content into a block
                // that contains a fence: `parse_code_fence` stops at the
                // closing fence, so anything merged after it would be
                // silently dropped. A list following a fence splits instead.
                let current_has_continuation = current.iter().any(|l| is_continuation_line(l))
                    && !current.iter().any(|l| l.trim_start().starts_with("```"));
                let next_is_continuation = is_continuation_line(next);
                if current_has_continuation && next_is_continuation {
                    current.push(line);
                    i += 1;
                    continue;
                }
            }

            if !current.is_empty() {
                blocks.push(current.join("\n"));
                current.clear();
            }
            i += 1;
            continue;
        }

        current.push(line);
        i += 1;
    }

    if !current.is_empty() {
        blocks.push(current.join("\n"));
    }

    if blocks.is_empty() && !content.trim().is_empty() {
        blocks.push(content.trim().to_string());
    }

    blocks
}

fn maybe_close_unclosed_fences(block: &str) -> String {
    let fence_count = block.matches("```").count();
    if fence_count % 2 == 1 {
        return format!("{}\n```", block);
    }
    block.to_string()
}

pub fn render_markdown(content: &str) -> Vec<Line<'static>> {
    let normalized = crate::tui::render::normalize_compact_markdown(content);
    let mut renderer = IncrementalMarkdownRenderer::new();
    renderer.update(&normalized)
}

#[allow(dead_code)]
pub struct CommittedBlock {
    pub id: BlockId,
    pub kind: BlockKind,
    pub raw: String,
    pub display: String,
    pub lines: Vec<Line<'static>>,
}

pub struct PendingBlock {
    pub lines: Vec<Line<'static>>,
    /// Wave 2A additive seam: the display source this block was rendered
    /// from, so the completion flush can capture it for width-change
    /// replay (rendering semantics untouched — same string block_to_lines
    /// consumed above).
    pub raw: String,
}

pub struct StreamingMarkdown {
    stream: MdStream,
    state: DocumentState,
    committed_cache: HashMap<BlockId, Vec<Line<'static>>>,
}

impl StreamingMarkdown {
    pub fn new() -> Self {
        Self {
            stream: MdStream::new(Options::default()),
            state: DocumentState::new(),
            committed_cache: HashMap::new(),
        }
    }

    pub fn append(&mut self, chunk: &str) -> StreamingUpdate {
        let update = self.stream.append(chunk);
        let _applied = self.state.apply(update);

        let committed: Vec<CommittedBlock> = self
            .state
            .committed()
            .iter()
            .filter_map(|block| {
                let id = block.id;
                if self.committed_cache.contains_key(&id) {
                    return None;
                }
                let display = block.display_or_raw().to_string();
                let raw = block.raw.to_string();
                let lines = block_to_lines(&display);
                self.committed_cache.insert(id, lines.clone());
                Some(CommittedBlock {
                    id,
                    kind: block.kind,
                    raw,
                    display,
                    lines,
                })
            })
            .collect();

        let pending = self.state.pending().map(|block| {
            let display = block.display_or_raw().to_string();
            let lines = block_to_lines(&display);
            PendingBlock {
                lines,
                raw: display,
            }
        });

        StreamingUpdate { committed, pending }
    }

    pub fn clear(&mut self) {
        self.stream.reset();
        self.state.clear();
        self.committed_cache.clear();
    }
}

impl Default for StreamingMarkdown {
    fn default() -> Self {
        Self::new()
    }
}

pub struct StreamingUpdate {
    pub committed: Vec<CommittedBlock>,
    pub pending: Option<PendingBlock>,
}

fn block_to_lines(block_display_text: &str) -> Vec<Line<'static>> {
    if block_display_text.is_empty() {
        return Vec::new();
    }
    render_markdown(block_display_text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;

    #[test]
    fn parse_code_fence_extracts_lang_and_code() {
        let (lang, code) = parse_code_fence("```rust\nfn main() {}\n```").expect("parse");
        assert_eq!(lang, "rust");
        assert_eq!(code, "fn main() {}");
    }

    #[test]
    fn parse_code_fence_handles_open_pending_block() {
        // Streaming: fence not yet closed — content runs to the end.
        let (lang, code) = parse_code_fence("```python\nprint(1)\nprint(2)").expect("parse");
        assert_eq!(lang, "python");
        assert_eq!(code, "print(1)\nprint(2)");
    }

    #[test]
    fn parse_code_fence_rejects_non_fence_block() {
        assert!(parse_code_fence("just a paragraph").is_none());
    }

    /// T028 (FR-012): code fences in the inline markdown path must be
    /// syntax-highlighted and framed like CodeCell.
    #[test]
    fn code_fence_rendered_framed_and_highlighted() {
        let lines = render_markdown("```rust\nfn main() {}\n```");
        let joined: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref().to_string())
                    .collect::<String>()
            })
            .collect();
        let text = joined.join("\n");
        assert!(text.contains("┌─ rust"), "framing header missing: {text:?}");
        assert!(
            text.contains("fn main() {}"),
            "code content missing: {text:?}"
        );
        assert!(text.contains("└─"), "framing footer missing: {text:?}");

        // Syntect assigns explicit colors (unless NO_COLOR is set).
        if std::env::var("NO_COLOR").is_err() {
            let has_color = lines.iter().flat_map(|l| &l.spans).any(|s| {
                matches!(
                    s.style.fg,
                    Some(ratatui::style::Color::Rgb(..))
                        | Some(ratatui::style::Color::Indexed(..))
                        | Some(ratatui::style::Color::Red)
                        | Some(ratatui::style::Color::Green)
                        | Some(ratatui::style::Color::Blue)
                        | Some(ratatui::style::Color::Yellow)
                        | Some(ratatui::style::Color::Cyan)
                        | Some(ratatui::style::Color::Magenta)
                )
            });
            assert!(has_color, "expected syntect-colored spans: {joined:?}");
        }
    }

    #[test]
    fn split_paragraphs_by_blank_lines() {
        let content = "First paragraph.\n\nSecond paragraph.";
        let blocks = split_top_level_blocks(content);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], "First paragraph.");
        assert_eq!(blocks[1], "Second paragraph.");
    }

    #[test]
    fn split_keeps_code_block_intact() {
        let content = "Intro text\n\n```rust\nfn main() {}\nfn other() {}\n```\n\nAfter code.";
        let blocks = split_top_level_blocks(content);
        assert_eq!(blocks.len(), 3);
        assert!(blocks[1].contains("```"));
        assert!(blocks[1].contains("fn main()"));
        assert!(blocks[1].contains("fn other()"));
    }

    #[test]
    fn split_keeps_list_together() {
        let content = "- Item 1\n- Item 2\n- Item 3";
        let blocks = split_top_level_blocks(content);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].contains("Item 1"));
        assert!(blocks[0].contains("Item 3"));
    }

    #[test]
    fn split_preserves_list_across_blank_lines() {
        let content = "- Item 1\n\n- Item 2\n\n- Item 3";
        let blocks = split_top_level_blocks(content);
        assert_eq!(
            blocks.len(),
            1,
            "list items with blank lines should be one block"
        );
        assert!(blocks[0].contains("Item 1"));
        assert!(blocks[0].contains("Item 3"));
    }

    #[test]
    fn split_preserves_mixed_list_markers_across_blank_lines() {
        let content = "- Dash item\n\n* Star item\n\n+ Plus item";
        let blocks = split_top_level_blocks(content);
        assert_eq!(
            blocks.len(),
            1,
            "mixed list markers with blank lines should be one block"
        );
    }

    #[test]
    fn split_preserves_numbered_list_across_blank_lines() {
        let content = "1. First\n\n2. Second\n\n3. Third";
        let blocks = split_top_level_blocks(content);
        assert_eq!(
            blocks.len(),
            1,
            "numbered list items with blank lines should be one block"
        );
    }

    #[test]
    fn split_preserves_blockquote_across_blank_lines() {
        let content = "> Quote one\n\n> Quote two";
        let blocks = split_top_level_blocks(content);
        assert_eq!(blocks.len(), 1, "blockquote paragraphs should be one block");
    }

    #[test]
    fn split_preserves_table_across_blank_lines() {
        let content = "| Header |\n\n| Cell |";
        let blocks = split_top_level_blocks(content);
        assert_eq!(
            blocks.len(),
            1,
            "table rows with blank lines should be one block"
        );
        assert!(blocks[0].contains("Header"));
        assert!(blocks[0].contains("Cell"));
    }

    #[test]
    fn split_preserves_table_with_separator_across_blank_lines() {
        let content = "| Header |\n\n|--------|\n\n| Cell |";
        let blocks = split_top_level_blocks(content);
        assert_eq!(
            blocks.len(),
            1,
            "table with separator rows should be one block"
        );
    }

    #[test]
    fn split_separates_paragraph_from_list() {
        let content = "Paragraph text.\n\n- Item 1\n- Item 2";
        let blocks = split_top_level_blocks(content);
        assert_eq!(
            blocks.len(),
            2,
            "paragraph and list should be separate blocks"
        );
        assert!(blocks[0].contains("Paragraph"));
        assert!(blocks[1].contains("Item 1"));
    }

    #[test]
    fn split_separates_list_from_paragraph() {
        let content = "- Item 1\n- Item 2\n\nParagraph text.";
        let blocks = split_top_level_blocks(content);
        assert_eq!(
            blocks.len(),
            2,
            "list and paragraph should be separate blocks"
        );
        assert!(blocks[0].contains("Item 1"));
        assert!(blocks[1].contains("Paragraph"));
    }

    #[test]
    fn split_heading_paragraph_list() {
        let content = "# Title\n\nBody paragraph.\n\n- Item 1\n- Item 2\n\nFinal paragraph.";
        let blocks = split_top_level_blocks(content);
        assert_eq!(
            blocks.len(),
            4,
            "heading, paragraph, list, paragraph should be 4 blocks"
        );
        assert!(blocks[0].contains("# Title"));
        assert!(blocks[1].contains("Body paragraph."));
        assert!(blocks[2].contains("Item 1"));
        assert!(blocks[3].contains("Final paragraph."));
    }

    #[test]
    fn renderer_caches_unchanged_blocks() {
        let mut renderer = IncrementalMarkdownRenderer::new();

        let v1 = renderer.update("Hello world");
        let blocks_after_1 = renderer.blocks.len();
        assert!(!v1.is_empty());

        let v2 = renderer.update("Hello world\n\nNew block");
        assert!(renderer.blocks.len() > blocks_after_1);
        assert!(v2.len() > v1.len());
    }

    #[test]
    fn renderer_renders_markdown() {
        let mut renderer = IncrementalMarkdownRenderer::new();
        let lines = renderer.update("# Heading\n\nSome **bold** text.");
        assert!(!lines.is_empty());
    }

    #[test]
    fn renderer_handles_streaming_growth() {
        let mut renderer = IncrementalMarkdownRenderer::new();

        renderer.update("Hello");
        let cache_1 = renderer.blocks.len();

        renderer.update("Hello\n\nWorld");
        let cache_2 = renderer.blocks.len();
        assert!(cache_2 >= cache_1);

        let lines = renderer.update("Hello\n\nWorld\n\nMore");
        assert!(!lines.is_empty());
        assert!(renderer.blocks.len() >= 3);
    }

    #[test]
    fn heading_and_paragraph_produce_separate_lines() {
        let lines = render_markdown("# Title\n\nBody paragraph.");
        assert!(
            lines.len() >= 2,
            "heading + paragraph should be >= 2 lines, got {}",
            lines.len()
        );
    }

    #[test]
    fn list_items_render_as_lines() {
        let lines = render_markdown("- One\n- Two\n- Three");
        assert!(
            lines.len() >= 3,
            "3 list items should be >= 3 lines, got {}",
            lines.len()
        );
    }

    #[test]
    fn no_backslash_hack_in_output() {
        let lines = render_markdown("Line one\nLine two");
        for line in &lines {
            for span in &line.spans {
                assert!(
                    !span.content.ends_with('\\'),
                    "span should not end with backslash hack: {:?}",
                    span.content
                );
            }
        }
    }

    fn extract_text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref().to_string()))
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn heading_marker_stripped() {
        let lines = render_markdown("# Heading\n\nBody.");
        let text = extract_text(&lines);
        assert!(
            !text.contains('#'),
            "heading marker # should be stripped, got: {text:?}"
        );
    }

    #[test]
    fn compact_heading_normalized() {
        let lines = render_markdown("# Heading\nSome text.");
        let text = extract_text(&lines);
        assert!(
            !text.contains('#'),
            "compact heading should be normalized, got: {text:?}"
        );
    }

    #[test]
    fn bold_marker_stripped() {
        let lines = render_markdown("**bold**");
        let text = extract_text(&lines);
        assert!(
            !text.contains('*'),
            "bold markers should be stripped, got: {text:?}"
        );
    }

    #[test]
    fn inline_code_marker_stripped() {
        let lines = render_markdown("Use `cargo` to build.");
        let text = extract_text(&lines);
        assert!(
            !text.contains('`'),
            "backticks should be stripped, got: {text:?}"
        );
    }

    #[test]
    fn streaming_markdown_empty_initial() {
        let sm = StreamingMarkdown::new();
        assert!(sm.committed_cache.is_empty());
    }

    #[test]
    fn streaming_markdown_append_single_paragraph() {
        let mut sm = StreamingMarkdown::new();
        let update = sm.append("Hello world\n\n");
        let total_lines: usize = update
            .committed
            .iter()
            .map(|b| b.lines.len())
            .sum::<usize>()
            + update.pending.as_ref().map(|p| p.lines.len()).unwrap_or(0);
        assert!(total_lines > 0, "should have rendered lines");
    }

    #[test]
    fn streaming_markdown_clear_resets_state() {
        let mut sm = StreamingMarkdown::new();
        sm.append("Some text.\n\n");
        sm.clear();
        assert!(sm.committed_cache.is_empty());
    }

    #[test]
    fn streaming_markdown_code_fence_pending() {
        let mut sm = StreamingMarkdown::new();
        let update = sm.append("```rust\nfn main() {\n");
        assert!(update.pending.is_some());
        let pending = update.pending.unwrap();
        let text: String = pending
            .lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(text.contains("fn main"));
    }

    // === T031: diff fence red/green rendering ===

    #[test]
    fn t031_diff_fence_renders_red_green_lines() {
        let md = concat!(
            "```diff\n",
            "+++ b/src/main.rs\n",
            "--- a/src/main.rs\n",
            "@@ -1,2 +1,2 @@\n",
            "-old line\n",
            "+new line\n",
            " context\n",
            "```"
        );
        let lines = render_markdown(md);
        let line_starting = |prefix: &str| {
            lines
                .iter()
                .find(|l| {
                    l.spans
                        .first()
                        .is_some_and(|s| s.content.starts_with(prefix))
                })
                .unwrap_or_else(|| panic!("line starting with {prefix:?} must render"))
        };
        let fg = |line: &ratatui::text::Line| {
            line.style
                .fg
                .or_else(|| line.spans.first().and_then(|s| s.style.fg))
        };
        assert_eq!(
            fg(line_starting("+new")),
            Some(ratatui::style::Color::Green)
        );
        assert_eq!(fg(line_starting("-old")), Some(ratatui::style::Color::Red));
        assert_eq!(fg(line_starting("@@")), Some(ratatui::style::Color::Cyan));
        assert_eq!(
            fg(line_starting("+++")),
            Some(ratatui::style::Color::DarkGray)
        );
        assert_eq!(
            fg(line_starting("---")),
            Some(ratatui::style::Color::DarkGray)
        );
        assert_eq!(fg(line_starting(" context")), None);
    }

    #[test]
    fn t031_non_diff_fences_still_render_framed() {
        let lines = render_markdown("```rust\nlet x = 1;\n```");
        assert!(
            lines.iter().any(|l| l
                .spans
                .first()
                .is_some_and(|s| s.content.contains("┌─ rust"))),
            "rust fence must keep the framed syntect path"
        );
        // syntect splits the body into many highlighted spans — flatten the
        // line text before matching.
        let line_text = |l: &ratatui::text::Line| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        assert!(
            lines.iter().any(|l| line_text(l).contains("let x = 1;")),
            "fence body must render"
        );
    }
    // === codex/hermes convergence pins (002-agentic-tui) ===

    fn line_text(l: &Line<'static>) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn find_line<'a>(lines: &'a [Line<'static>], text: &str) -> &'a Line<'static> {
        lines
            .iter()
            .find(|l| line_text(l) == text)
            .unwrap_or_else(|| panic!("line {text:?} must render, got {lines:?}"))
    }

    /// (a) Heading hierarchy: H1 bold+underlined, H2 bold, H3 bold+italic,
    /// H4-H6 italic — modifier-only (no fg), markers stay stripped.
    #[test]
    fn heading_hierarchy_h1_to_h4() {
        let lines = render_markdown("# H1\n\n## H2\n\n### H3\n\n#### H4");
        let cases = [
            ("H1", Modifier::BOLD | Modifier::UNDERLINED),
            ("H2", Modifier::BOLD),
            ("H3", Modifier::BOLD | Modifier::ITALIC),
            ("H4", Modifier::ITALIC),
        ];
        for (text, mods) in cases {
            let line = find_line(&lines, text);
            assert!(
                line.style.add_modifier.contains(mods),
                "{text} must carry {mods:?}, got {:?}",
                line.style
            );
            // Modifier-only styling: no fg color on any level, so NO_COLOR
            // degradation is structural (nothing to strip).
            assert_eq!(line.style.fg, None, "{text} must carry no fg color");
        }
    }

    /// (b) Unordered bullets render as `• ` (codex/hermes glyph).
    #[test]
    fn unordered_bullets_use_bullet_glyph() {
        let lines = render_markdown("- One\n- Two");
        assert_eq!(line_text(&lines[0]), "\u{2022} One");
        assert_eq!(line_text(&lines[1]), "\u{2022} Two");
        for l in &lines {
            for s in &l.spans {
                assert!(
                    !s.content.contains("- "),
                    "no dash marker may remain: {s:?}"
                );
            }
        }
    }

    /// (b) Ordered `{n}. ` markers are untouched by the bullet swap.
    #[test]
    fn ordered_list_markers_unchanged() {
        let lines = render_markdown("1. First");
        assert_eq!(line_text(&lines[0]), "1. First");
        let marker = &lines[0].spans[0];
        assert_eq!(marker.content.as_ref(), "1. ");
        assert_eq!(
            marker.style.fg,
            Some(ratatui::style::Color::LightBlue),
            "ordered marker keeps the vendored light-blue"
        );
    }

    /// (b) Nested-list indentation is preserved by the glyph swap (the
    /// vendored renderer flattens 2-space nesting to siblings — pinned).
    #[test]
    fn nested_bullet_indent_preserved() {
        let lines = render_markdown("- Top\n  - Nested");
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert_eq!(
            texts,
            vec!["\u{2022} Top".to_string(), "\u{2022} Nested".to_string()]
        );
    }

    /// (d) Standalone rule renders as dim em-dash x3 with exactly one blank
    /// line before and after (join-supplied), never `---`.
    #[test]
    fn hr_renders_em_dash_rule_dim() {
        let lines = render_markdown("Above\n\n---\n\nBelow");
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert_eq!(
            texts,
            vec![
                "Above".to_string(),
                String::new(),
                "\u{2014}\u{2014}\u{2014}".to_string(),
                String::new(),
                "Below".to_string()
            ]
        );
        let rule = find_line(&lines, "\u{2014}\u{2014}\u{2014}");
        if std::env::var("NO_COLOR").is_err() {
            assert_eq!(rule.style.fg, Some(ratatui::style::Color::DarkGray));
            assert!(rule.style.add_modifier.contains(Modifier::DIM));
        }
    }

    /// (d)/(h) HR intercept degradation: plain Reset style under NO_COLOR;
    /// only exact single-line breaks intercept (`***`/`___` yes; 4-space
    /// indented code and setext `Title\n---` no).
    #[test]
    fn hr_no_color_renders_plain_reset() {
        let lines = render_rule_block_with("---", true).expect("rule");
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "\u{2014}\u{2014}\u{2014}");
        assert_eq!(lines[0].style, ratatui::style::Style::default());
        for form in ["***", "___"] {
            assert!(
                render_rule_block_with(form, true).is_some(),
                "{form} is a break"
            );
        }
        assert!(
            render_rule_block_with("    ---", true).is_none(),
            "indented code"
        );
        assert!(
            render_rule_block_with("Title\n---", true).is_none(),
            "setext H2"
        );
    }

    /// (f) Strikethrough parses (vendored pulldown-cmark
    /// ENABLE_STRIKETHROUGH) and renders with the CROSSED_OUT modifier,
    /// markers stripped.
    #[test]
    fn strikethrough_enabled_pin() {
        let lines = render_markdown("Hello ~~gone~~ world");
        let text = extract_text(&lines);
        assert_eq!(text, "Hello gone world");
        assert!(!text.contains('~'), "tildes must be stripped: {text:?}");
        let struck = lines
            .iter()
            .flat_map(|l| &l.spans)
            .find(|s| s.content.as_ref() == "gone")
            .expect("struck span");
        assert!(
            struck.style.add_modifier.contains(Modifier::CROSSED_OUT),
            "gone must be crossed out, got {:?}",
            struck.style
        );
    }

    /// (g) Blockquote freeze: `>` prefix kept, body preserved, deliberately
    /// plain styling (no fg anywhere — pins NoMarkerStyleSheet::blockquote).
    #[test]
    fn blockquote_rendering_pin() {
        let lines = render_markdown("> quoted text");
        assert_eq!(lines.len(), 1, "single quote line, got {lines:?}");
        let line = &lines[0];
        assert_eq!(line_text(line), "> quoted text");
        assert_eq!(line.spans[0].content.as_ref(), ">");
        assert_eq!(line.spans.last().unwrap().content.as_ref(), "quoted text");
        assert_eq!(line.style.fg, None, "blockquote stays plain (style freeze)");
        for s in &line.spans {
            assert_eq!(s.style.fg, None, "no fg on quote spans");
        }
    }

    /// (c) Link label keeps the accent+underline style; the renderer's
    /// appended ` (url)` destination is stripped (deliberate divergence).
    #[test]
    fn link_label_underlined_no_url() {
        let lines = render_markdown("See [docs](https://example.com) now");
        let text = extract_text(&lines);
        assert_eq!(text, "See docs now", "destination must be stripped");
        let label = lines
            .iter()
            .flat_map(|l| &l.spans)
            .find(|s| s.content.as_ref() == "docs")
            .expect("label span");
        assert!(
            label.style.add_modifier.contains(Modifier::UNDERLINED),
            "label must be underlined, got {:?}",
            label.style
        );
        // Env-independent equality: the render path computes the style with
        // the same function this assertion uses.
        assert_eq!(label.style, crate::tui::theme::markdown_link_style());
    }

    /// (h) Link style degradation seams: underline-only (no fg) under
    /// NO_COLOR, accent+underline otherwise.
    #[test]
    fn link_no_color_degrades_to_underline_only() {
        let degraded = crate::tui::theme::markdown_link_style_with(true);
        assert_eq!(degraded.fg, None, "NO_COLOR must drop the accent color");
        assert!(degraded.add_modifier.contains(Modifier::UNDERLINED));

        let colored = crate::tui::theme::markdown_link_style_with(false);
        assert_eq!(colored.fg, Some(ratatui::style::Color::Rgb(90, 143, 110)));
        assert!(colored.add_modifier.contains(Modifier::UNDERLINED));
    }

    /// (e) Gap rhythm: exactly one blank line between different-kind blocks.
    #[test]
    fn gap_rhythm_exactly_one_blank_between_kinds() {
        let lines = render_markdown("Para.\n\n- Item\n\n> Quote");
        let blankness: Vec<bool> = lines.iter().map(is_blank_line).collect();
        assert_eq!(
            blankness,
            vec![false, true, false, true, false],
            "para/blank/list/blank/quote, got {blankness:?}"
        );
    }

    /// (e) No two consecutive blank lines across a rule join chain.
    #[test]
    fn no_double_blank_at_joins_with_rule() {
        let lines = render_markdown("Above\n\n---\n\nBelow\n\n- x");
        let blankness: Vec<bool> = lines.iter().map(is_blank_line).collect();
        // Above/blank/rule/blank/Below — then Below→list is itself a
        // different-kind join and gets its own single blank.
        assert_eq!(
            blankness,
            vec![false, true, false, true, false, true, false]
        );
        for w in blankness.windows(2) {
            assert!(!(w[0] && w[1]), "double blank at join: {blankness:?}");
        }
    }

    /// (e) Same-kind adjacency keeps current behavior (no inserted gap).
    #[test]
    fn same_kind_adjacency_keeps_current_behavior() {
        let lines = render_markdown("First.\n\nSecond.");
        assert_eq!(lines.len(), 2, "no gap between same-kind blocks");
        assert!(!is_blank_line(&lines[0]) && !is_blank_line(&lines[1]));
    }

    /// (b) Fenced code content is byte-identical — the bullet swap never
    /// touches fence bodies (they intercept before the tui-markdown path).
    #[test]
    fn fence_content_untouched_by_bullet_rewrite() {
        let lines = render_markdown("```\n- item one\n- item two\n```\n\n- Real");
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(
            texts.contains(&"- item one".to_string()) && texts.contains(&"- item two".to_string()),
            "fence body must stay byte-identical, got {texts:?}"
        );
        assert!(
            !texts.iter().any(|t| t.contains("\u{2022} item")),
            "bullet swap must not reach fence body"
        );
        assert!(
            texts.iter().any(|t| t == "\u{2022} Real"),
            "real list outside the fence still gets the glyph, got {texts:?}"
        );
    }
}
