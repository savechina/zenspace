use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use mdstream::{BlockId, BlockKind, DocumentState, MdStream, Options};
use ratatui::text::{Line, Span};
use tui_markdown::StyleSheet;

#[derive(Clone, Copy)]
struct NoMarkerStyleSheet;

impl StyleSheet for NoMarkerStyleSheet {
    fn heading_marker(&self, _level: u8) -> &str {
        ""
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
            Line::from(spans)
        })
        .collect()
}

/// Render one top-level markdown block.
///
/// Code fences go through syntect/two-face highlighting (T028 / FR-012) so
/// the inline streaming path matches the full-screen TUI; everything else
/// stays on `tui_markdown`.
fn render_block_dispatch(raw: &str) -> Vec<Line<'static>> {
    if let Some(lines) = render_code_fence_block(raw) {
        return lines;
    }
    render_block_via_tui_markdown(raw)
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

/// Highlighted, framed code fence for the inline scrollback path. Mirrors
/// `CodeCell` framing (`┌─ lang` / `└─`). Respects `NO_COLOR` (FR-011) by
/// falling back to plain text.
fn render_code_fence_block(raw: &str) -> Option<Vec<Line<'static>>> {
    let (lang, code) = parse_code_fence(raw)?;

    static NO_COLOR: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let no_color = *NO_COLOR.get_or_init(|| std::env::var("NO_COLOR").is_ok());

    let header_style = ratatui::style::Style::default()
        .fg(ratatui::style::Color::DarkGray)
        .add_modifier(ratatui::style::Modifier::BOLD);
    let mut lines = vec![Line::styled(format!("┌─ {}", lang), header_style)];
    if no_color {
        lines.extend(code.lines().map(|l| Line::raw(l.to_string())));
    } else {
        lines.extend(super::highlight::highlight_code(&code, &lang));
    }
    lines.push(Line::styled("└─", header_style));
    Some(lines)
}

fn render_block_via_tui_markdown(raw: &str) -> Vec<Line<'static>> {
    let options = tui_markdown::Options::new(NoMarkerStyleSheet);
    let text = tui_markdown::from_str_with_options(raw, &options);
    convert_text_to_owned_lines(text)
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

            if let Some(cached) = self.blocks.get(i)
                && cached.hash == h
            {
                result.extend(cached.lines.iter().cloned());
                new_blocks.push(RenderedBlock {
                    hash: h,
                    lines: cached.lines.clone(),
                });
                continue;
            }

            let is_last_block = i == raw_blocks.len() - 1;
            let render_content = if is_last_block {
                maybe_close_unclosed_fences(raw)
            } else {
                raw.clone()
            };

            let lines = render_block_dispatch(&render_content);
            result.extend(lines.iter().cloned());
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
                let current_has_continuation = current.iter().any(|l| is_continuation_line(l));
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
            PendingBlock { lines }
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
}
