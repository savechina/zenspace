use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use ratatui::text::{Line, Span};

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

fn render_block_via_tui_markdown(raw: &str) -> Vec<Line<'static>> {
    let text = tui_markdown::from_str(raw);
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
        Self {
            blocks: Vec::new(),
        }
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

            let lines = render_block_via_tui_markdown(raw);
            result.extend(lines.iter().cloned());
            new_blocks.push(RenderedBlock { hash: h, lines });
        }

        self.blocks = new_blocks;
        result
    }

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
    trimmed.starts_with('|')
        && trimmed.ends_with('|')
        && trimmed.matches('|').count() >= 2
}

fn is_table_separator(trimmed: &str) -> bool {
    trimmed.starts_with('|')
        && trimmed.ends_with('|')
        && trimmed[1..trimmed.len() - 1]
            .split('|')
            .all(|cell| cell.trim().chars().all(|c| c == '-' || c == ':' || c.is_whitespace()))
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

pub fn render_markdown(content: &str) -> Vec<Line<'static>> {
    let mut renderer = IncrementalMarkdownRenderer::new();
    renderer.update(content)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(blocks.len(), 1, "list items with blank lines should be one block");
        assert!(blocks[0].contains("Item 1"));
        assert!(blocks[0].contains("Item 3"));
    }

    #[test]
    fn split_preserves_mixed_list_markers_across_blank_lines() {
        let content = "- Dash item\n\n* Star item\n\n+ Plus item";
        let blocks = split_top_level_blocks(content);
        assert_eq!(blocks.len(), 1, "mixed list markers with blank lines should be one block");
    }

    #[test]
    fn split_preserves_numbered_list_across_blank_lines() {
        let content = "1. First\n\n2. Second\n\n3. Third";
        let blocks = split_top_level_blocks(content);
        assert_eq!(blocks.len(), 1, "numbered list items with blank lines should be one block");
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
        assert_eq!(blocks.len(), 1, "table rows with blank lines should be one block");
        assert!(blocks[0].contains("Header"));
        assert!(blocks[0].contains("Cell"));
    }

    #[test]
    fn split_preserves_table_with_separator_across_blank_lines() {
        let content = "| Header |\n\n|--------|\n\n| Cell |";
        let blocks = split_top_level_blocks(content);
        assert_eq!(blocks.len(), 1, "table with separator rows should be one block");
    }

    #[test]
    fn split_separates_paragraph_from_list() {
        let content = "Paragraph text.\n\n- Item 1\n- Item 2";
        let blocks = split_top_level_blocks(content);
        assert_eq!(blocks.len(), 2, "paragraph and list should be separate blocks");
        assert!(blocks[0].contains("Paragraph"));
        assert!(blocks[1].contains("Item 1"));
    }

    #[test]
    fn split_separates_list_from_paragraph() {
        let content = "- Item 1\n- Item 2\n\nParagraph text.";
        let blocks = split_top_level_blocks(content);
        assert_eq!(blocks.len(), 2, "list and paragraph should be separate blocks");
        assert!(blocks[0].contains("Item 1"));
        assert!(blocks[1].contains("Paragraph"));
    }

    #[test]
    fn split_heading_paragraph_list() {
        let content = "# Title\n\nBody paragraph.\n\n- Item 1\n- Item 2\n\nFinal paragraph.";
        let blocks = split_top_level_blocks(content);
        assert_eq!(blocks.len(), 4, "heading, paragraph, list, paragraph should be 4 blocks");
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
        assert!(lines.len() >= 2, "heading + paragraph should be >= 2 lines, got {}", lines.len());
    }

    #[test]
    fn list_items_render_as_lines() {
        let lines = render_markdown("- One\n- Two\n- Three");
        assert!(lines.len() >= 3, "3 list items should be >= 3 lines, got {}", lines.len());
    }

    #[test]
    fn no_backslash_hack_in_output() {
        // Verify preserve_line_breaks is gone — no `\` at end of lines
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
}
