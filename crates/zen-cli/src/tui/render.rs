/// Normalize compact markdown by inserting blank lines before block elements.
///
/// Handles headings (`#`), code fences (` ``` `), list items (`- `, `* `, `+ `, `N. `),
/// blockquotes (`>`), and table rows (`|`). Respects inline code boundaries and avoids
/// false positives like `this - that` being turned into list items.
pub fn normalize_compact_markdown(content: &str) -> String {
    if !content.contains(' ') {
        return content.to_string();
    }

    let chars: Vec<char> = content.chars().collect();
    let len = chars.len();
    let mut result = String::with_capacity(len + 64);
    let mut i = 0;
    let mut in_inline_code = false;
    let mut in_code_fence = false;

    while i < len {
        let ch = chars[i];

        // --- Backtick handling (inline code + code fences) ---
        if ch == '`' {
            if i + 2 < len && chars[i + 1] == '`' && chars[i + 2] == '`' {
                if !in_code_fence {
                    if !result.is_empty() && !result.ends_with('\n') {
                        result.push_str("\n\n");
                    }
                    result.push_str("```");
                    in_code_fence = true;
                    i += 3;
                    continue;
                } else {
                    result.push_str("```");
                    in_code_fence = false;
                    i += 3;
                    if i < len && !chars[i].is_whitespace() {
                        result.push_str("\n\n");
                    }
                    continue;
                }
            }
            // Single backtick: toggle inline code
            in_inline_code = !in_inline_code;
            result.push(ch);
            i += 1;
            continue;
        }

        // Skip content inside inline code or code fences
        if in_inline_code || in_code_fence {
            result.push(ch);
            i += 1;
            continue;
        }

        // --- Heading: '#' at word boundary (start, after space, or after newline), followed by space ---
        if ch == '#' {
            let at_boundary = i == 0 || chars[i - 1] == ' ' || chars[i - 1] == '\n';
            let followed_by_space = i + 1 < len && chars[i + 1] == ' ';
            if at_boundary && followed_by_space && !result.is_empty() && !result.ends_with('\n') {
                result.push_str("\n\n");
            }
            result.push(ch);
            i += 1;
            continue;
        }

        // --- Unordered list: '-', '*', '+' followed by space ---
        if (ch == '-' || ch == '*' || ch == '+') && i + 1 < len && chars[i + 1] == ' ' {
            let mut j = i;
            while j > 0 && chars[j - 1] == ' ' {
                j -= 1;
            }
            let is_list = j == 0 || !chars[j - 1].is_alphanumeric();
            let on_table_row = result
                .rsplit('\n')
                .next()
                .map(|line| line.trim().starts_with('|'))
                .unwrap_or(false);
            if is_list && !on_table_row && !result.is_empty() && !result.ends_with('\n') {
                result.push('\n');
            }
            result.push(ch);
            i += 1;
            continue;
        }

        // --- Ordered list: digit + '.' + ' ' ---
        if ch.is_ascii_digit() && i + 2 < len && chars[i + 1] == '.' && chars[i + 2] == ' ' {
            let is_list = i == 0 || !chars[i - 1].is_alphanumeric();
            if is_list && !result.is_empty() && !result.ends_with('\n') {
                result.push('\n');
            }
            result.push(ch);
            i += 1;
            continue;
        }

        // --- Blockquote: '>' at start or after space ---
        if ch == '>' && (i == 0 || chars[i - 1] == ' ') {
            if !result.is_empty() && !result.ends_with('\n') {
                result.push('\n');
            }
            result.push(ch);
            i += 1;
            continue;
        }

        result.push(ch);
        i += 1;
    }

    insert_table_separators(&result)
}

fn is_table_row(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('|') && trimmed.ends_with('|') && trimmed.matches('|').count() >= 2
}

fn is_table_separator(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.starts_with('|') || !trimmed.ends_with('|') {
        return false;
    }
    trimmed[1..trimmed.len() - 1].split('|').all(|cell| {
        cell.trim()
            .chars()
            .all(|c| c == '-' || c == ':' || c.is_whitespace())
    })
}

fn make_separator_row(header_row: &str) -> String {
    let trimmed = header_row.trim();
    let cells: Vec<&str> = trimmed.split('|').collect();
    let parts: Vec<String> = cells
        .iter()
        .skip(1)
        .take(cells.len().saturating_sub(2))
        .map(|cell| {
            let width = cell.trim().chars().count().max(3);
            "-".repeat(width)
        })
        .collect();
    format!("|{}|", parts.join("|"))
}

/// Insert a markdown table separator row after the first row of any group of
/// consecutive table rows that lacks one. pulldown-cmark (and therefore
/// tui-markdown) requires a separator to recognize a block as a table.
fn insert_table_separators(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() < 2 {
        return content.to_string();
    }

    let mut result: Vec<String> = Vec::with_capacity(lines.len() + 4);
    let mut i = 0;

    while i < lines.len() {
        if is_table_row(lines[i]) {
            let start = i;
            while i < lines.len() && is_table_row(lines[i]) {
                i += 1;
            }
            let end = i;
            let row_count = end - start;
            let has_separator = lines[start..end].iter().any(|l| is_table_separator(l));

            if row_count >= 2 && !has_separator {
                result.push(lines[start].to_string());
                result.push(make_separator_row(lines[start]));
                for line in &lines[start + 1..end] {
                    result.push(line.to_string());
                }
            } else {
                for line in &lines[start..end] {
                    result.push(line.to_string());
                }
            }
        } else {
            result.push(lines[i].to_string());
            i += 1;
        }
    }

    result.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_compact_markdown_inserts_blank_lines() {
        let compact = "Some prose. # Heading more text. ```rust fn main() {} ``` - item";
        let normalized = normalize_compact_markdown(compact);
        assert!(
            normalized.contains("\n\n#"),
            "should insert blank line before heading"
        );
        assert!(
            normalized.contains("\n\n```"),
            "should insert blank line before code fence"
        );
        assert!(
            normalized.contains("\n- "),
            "should insert newline before list item"
        );
    }

    #[test]
    fn normalize_compact_preserves_already_spaced() {
        let good = "# Title\n\nBody text\n\n## Heading\n\n- item";
        let normalized = normalize_compact_markdown(good);
        assert_eq!(
            normalized, good,
            "already-spaced markdown should be unchanged"
        );
    }

    #[test]
    fn normalize_compact_skips_no_spaces() {
        let bare = "singleword";
        let normalized = normalize_compact_markdown(bare);
        assert_eq!(normalized, bare, "no-space content should be unchanged");
    }

    #[test]
    fn normalize_heading_inside_inline_code_ignored() {
        let input = "text `# not a heading` more";
        let normalized = normalize_compact_markdown(input);
        assert!(
            !normalized.contains("\n\n#"),
            "should not insert blank line before # inside inline code"
        );
    }

    #[test]
    fn normalize_code_fence_inside_inline_code_ignored() {
        let input = "text `not a fence` more";
        let normalized = normalize_compact_markdown(input);
        assert!(
            !normalized.contains("\n\n`"),
            "should not insert blank line before content inside inline code"
        );
    }

    #[test]
    fn normalize_prose_dash_not_list() {
        let input = "this - that";
        let normalized = normalize_compact_markdown(input);
        assert_eq!(
            normalized, "this - that",
            "should not turn prose dash into list item"
        );
    }

    #[test]
    fn normalize_ordered_list() {
        let input = "text 1. first 2. second";
        let normalized = normalize_compact_markdown(input);
        assert!(
            normalized.contains("\n1."),
            "should insert newline before ordered list item"
        );
    }

    #[test]
    fn normalize_blockquote() {
        let input = "text > quote";
        let normalized = normalize_compact_markdown(input);
        assert!(
            normalized.contains("\n>"),
            "should insert newline before blockquote"
        );
    }

    #[test]
    fn normalize_table_row_not_broken() {
        let input = "| a | b | c |";
        let normalized = normalize_compact_markdown(input);
        assert_eq!(
            normalized, "| a | b | c |",
            "single table row should stay intact"
        );
    }

    #[test]
    fn normalize_inserts_table_separator() {
        let input = "| a | b |\n| c | d |";
        let normalized = normalize_compact_markdown(input);
        assert!(
            normalized.contains("|---|"),
            "should insert separator row for multi-line table: got {}",
            normalized
        );
        assert!(
            normalized.contains("| a | b |"),
            "should preserve header row"
        );
        assert!(normalized.contains("| c | d |"), "should preserve data row");
    }

    #[test]
    fn normalize_preserves_existing_table_separator() {
        let input = "| a | b |\n|---|---|\n| c | d |";
        let normalized = normalize_compact_markdown(input);
        assert_eq!(
            normalized, input,
            "already-valid table should not be modified"
        );
    }

    #[test]
    fn normalize_does_not_corrupt_table_separator_with_spaces() {
        let input = "| a | b |\n| --- | --- |\n| c | d |";
        let normalized = normalize_compact_markdown(input);
        assert!(
            normalized.contains("| --- | --- |"),
            "separator row with spaces should stay intact: got {}",
            normalized
        );
    }

    #[test]
    fn normalize_heading_at_start() {
        let input = "# Heading text";
        let normalized = normalize_compact_markdown(input);
        assert_eq!(normalized, "# Heading text");
    }
}
