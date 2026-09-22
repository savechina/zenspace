use crate::tui::theme::OutputTheme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use std::collections::HashMap;

pub struct SlashCommand {
    pub name: String,
    pub aliases: Vec<String>,
    pub description: String,
}

pub struct SlashCommandRegistry {
    commands: Vec<SlashCommand>,
    alias_index: HashMap<String, usize>,
}

impl SlashCommandRegistry {
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
            alias_index: HashMap::new(),
        }
    }

    pub fn register(&mut self, name: String, aliases: Vec<String>, description: String) {
        let idx = self.commands.len();
        self.commands.push(SlashCommand {
            name: name.clone(),
            aliases: aliases.clone(),
            description,
        });

        self.alias_index.insert(name, idx);
        for alias in aliases {
            self.alias_index.insert(alias, idx);
        }
    }

    pub fn get_by_name_or_alias(&self, input: &str) -> Option<&SlashCommand> {
        self.alias_index
            .get(input)
            .and_then(|&idx| self.commands.get(idx))
    }

    pub fn filter_indices(&self, prefix: &str) -> Vec<usize> {
        self.commands
            .iter()
            .enumerate()
            .filter(|(_, cmd)| cmd.name.starts_with(prefix))
            .map(|(idx, _)| idx)
            .collect()
    }

    #[allow(dead_code)]
    pub fn filter(&self, prefix: &str) -> Vec<&SlashCommand> {
        self.commands
            .iter()
            .filter(|cmd| cmd.name.starts_with(prefix))
            .collect()
    }

    pub fn all_commands(&self) -> &[SlashCommand] {
        &self.commands
    }
}

impl Default for SlashCommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub fn create_default_registry() -> SlashCommandRegistry {
    let mut registry = SlashCommandRegistry::new();

    registry.register(
        "help".to_string(),
        vec!["h".to_string()],
        "Show available commands".to_string(),
    );
    registry.register(
        "exit".to_string(),
        vec!["q".to_string(), "quit".to_string()],
        "Exit TUI".to_string(),
    );
    registry.register(
        "clear".to_string(),
        vec!["cls".to_string()],
        "Clear output".to_string(),
    );
    registry.register(
        "thinking".to_string(),
        vec![],
        "Toggle thinking display".to_string(),
    );
    registry.register(
        "tools".to_string(),
        vec![],
        "Toggle tool intermediate expansion".to_string(),
    );
    registry.register(
        "model".to_string(),
        vec![],
        "Switch provider/model".to_string(),
    );
    registry.register(
        "variant".to_string(),
        vec!["vc".to_string(), "variant_cycle".to_string()],
        "Cycle through model variants".to_string(),
    );
    registry.register(
        "export".to_string(),
        vec!["e".to_string()],
        "Export chat to Markdown".to_string(),
    );
    registry.register(
        "note".to_string(),
        vec!["n".to_string()],
        "Create a note".to_string(),
    );
    registry.register(
        "search".to_string(),
        vec!["s".to_string()],
        "Search knowledge base".to_string(),
    );
    registry.register(
        "session".to_string(),
        vec!["ss".to_string()],
        "List and select sessions".to_string(),
    );
    registry.register("new".to_string(), vec![], "Create new session".to_string());
    registry.register(
        "fork".to_string(),
        vec![],
        "Fork current session".to_string(),
    );
    registry.register(
        "rename".to_string(),
        vec!["r".to_string()],
        "Rename current session".to_string(),
    );
    registry.register(
        "archive".to_string(),
        vec!["a".to_string()],
        "Archive current session".to_string(),
    );
    registry.register(
        "serve".to_string(),
        vec![],
        "Start gateway daemon".to_string(),
    );
    registry.register(
        "config".to_string(),
        vec![],
        "Show configuration".to_string(),
    );
    registry.register(
        "distill".to_string(),
        vec![],
        "Run distillation pipeline".to_string(),
    );
    registry.register("lint".to_string(), vec![], "Run knowledge lint".to_string());

    registry
}

pub const MAX_POPUP_ROWS: usize = 8;

pub struct SlashState {
    pub visible: bool,
    pub filter: String,
    pub filtered_indices: Vec<usize>,
    pub selected: usize,
}

impl SlashState {
    pub fn new() -> Self {
        Self {
            visible: false,
            filter: String::new(),
            filtered_indices: Vec::new(),
            selected: 0,
        }
    }

    pub fn on_input_change(&mut self, input: &str, registry: &SlashCommandRegistry) {
        let trimmed = input.trim_start();
        if let Some(stripped) = trimmed.strip_prefix('/') {
            let has_space = stripped.contains(' ');
            if has_space {
                self.visible = false;
                return;
            }

            let token = stripped.split_whitespace().next().unwrap_or("");
            let new_filter = token.to_lowercase();
            // Recompute every call (cheap: ~18 commands) so a first-time or
            // re-shown filter always has a populated match list; only the
            // selection resets on an actual filter change (Codex semantics).
            let changed = new_filter != self.filter;
            self.filter = new_filter;
            self.recompute_filtered(registry);
            if changed {
                self.selected = 0;
            } else {
                self.selected = self
                    .selected
                    .min(self.filtered_indices.len().saturating_sub(1));
            }
            // UX4: keep popup visible when filter is non-empty (show "no matches")
            self.visible = !self.filter.is_empty() || !self.filtered_indices.is_empty();
        } else {
            self.visible = false;
        }
    }

    fn recompute_filtered(&mut self, registry: &SlashCommandRegistry) {
        self.filtered_indices = registry.filter_indices(&self.filter);
    }

    pub fn move_up(&mut self) {
        if self.filtered_indices.is_empty() {
            return;
        }
        self.selected = if self.selected == 0 {
            self.filtered_indices.len() - 1
        } else {
            self.selected - 1
        };
    }

    pub fn move_down(&mut self) {
        if self.filtered_indices.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.filtered_indices.len();
    }

    pub fn selected_command<'a>(&self, registry: &'a SlashCommandRegistry) -> Option<&'a str> {
        self.filtered_indices
            .get(self.selected)
            .and_then(|&idx| registry.all_commands().get(idx))
            .map(|cmd| cmd.name.as_str())
    }

    pub fn dismiss(&mut self) {
        self.visible = false;
    }

    #[allow(dead_code)]
    pub fn visible_rows<'a>(&self, registry: &'a SlashCommandRegistry) -> Vec<&'a SlashCommand> {
        let start = if self.selected >= MAX_POPUP_ROWS {
            self.selected - MAX_POPUP_ROWS + 1
        } else {
            0
        };
        let end = (start + MAX_POPUP_ROWS).min(self.filtered_indices.len());
        self.filtered_indices[start..end]
            .iter()
            .filter_map(|&idx| registry.all_commands().get(idx))
            .collect()
    }

    pub fn visible_count(&self) -> usize {
        self.filtered_indices.len().min(MAX_POPUP_ROWS)
    }
}

impl Default for SlashState {
    fn default() -> Self {
        Self::new()
    }
}

pub fn render_slash_popup(
    frame: &mut ratatui::Frame,
    state: &SlashState,
    input_area: ratatui::layout::Rect,
    theme: &dyn OutputTheme,
    registry: &SlashCommandRegistry,
) {
    if !state.visible {
        return;
    }

    let visible_count = state.visible_count();
    let popup_y = input_area.y.saturating_sub(visible_count as u16);
    let popup_area = ratatui::layout::Rect::new(
        input_area.x,
        popup_y,
        input_area.width,
        visible_count as u16,
    );

    render_slash_popup_inner(frame, state, popup_area, theme, registry, MAX_POPUP_ROWS);
}

const INLINE_POPUP_ROWS: usize = 8;

pub fn render_slash_popup_inline(
    frame: &mut ratatui::Frame,
    state: &SlashState,
    popup_area: ratatui::layout::Rect,
    theme: &dyn OutputTheme,
    registry: &SlashCommandRegistry,
) {
    if !state.visible {
        return;
    }
    let max_rows = (popup_area.height as usize).clamp(1, INLINE_POPUP_ROWS);
    render_slash_popup_inner(frame, state, popup_area, theme, registry, max_rows);
}

/// T084(a): slash-menu grouping. The registry stores no category field, so the
/// group derives from the command name in registration order (General → Model
/// → Knowledge → Session → System); unknown names fall into "Other".
pub fn slash_group(name: &str) -> &'static str {
    match name {
        "help" | "exit" | "clear" | "thinking" | "tools" => "General",
        "model" | "variant" => "Model",
        "export" | "note" | "search" => "Knowledge",
        "session" | "new" | "fork" | "rename" | "archive" => "Session",
        "serve" | "config" | "distill" | "lint" => "System",
        _ => "Other",
    }
}

/// T084(d): dismissed-token memory (codex-rs `DismissedToken` mechanics at our
/// scale: one Option on App, replaced on each Esc — occurrence-count fidelity
/// is not required).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DismissedToken {
    pub token: String,
}

/// T084(d): the current command token = input text from the leading `/` up to
/// the first whitespace (`Some("")` for a bare `/`; None when the input does
/// not start with `/`).
pub fn slash_command_token(input: &str) -> Option<String> {
    let stripped = input.trim_start().strip_prefix('/')?;
    Some(stripped.split_whitespace().next().unwrap_or("").to_string())
}

/// T084(a): a popup row is either a group header or a command item. Headers
/// scroll with the list (never sticky) and count toward the max-rows budget.
enum PopupRow {
    Header(&'static str),
    Item { cmd_idx: usize, filtered_pos: usize },
}

/// T084(a): flat row list in filtered (registry) order, with a group header
/// inserted on each group change — but only when the filtered list spans ≥2
/// groups (a lone header is noise and would steal a row of ADR-006 budget).
fn popup_rows(state: &SlashState, registry: &SlashCommandRegistry) -> Vec<PopupRow> {
    let mut multi_group = false;
    {
        let mut seen: Option<&'static str> = None;
        for &idx in &state.filtered_indices {
            if let Some(cmd) = registry.all_commands().get(idx) {
                let group = slash_group(&cmd.name);
                match seen {
                    None => seen = Some(group),
                    Some(prev) if prev != group => {
                        multi_group = true;
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
    let mut rows = Vec::new();
    let mut last_group = "";
    for (pos, &idx) in state.filtered_indices.iter().enumerate() {
        let Some(cmd) = registry.all_commands().get(idx) else {
            continue;
        };
        let group = slash_group(&cmd.name);
        if multi_group && group != last_group {
            rows.push(PopupRow::Header(group));
            last_group = group;
        }
        rows.push(PopupRow::Item {
            cmd_idx: idx,
            filtered_pos: pos,
        });
    }
    rows
}

/// T084(a): flat popup row count (items + group headers) for the inline slot
/// budget in `inline_ui.rs` — headers consume popup rows exactly like items
/// (the list clamps to the slot; headers never scroll off independently).
/// Minimum 1: the "no matches" empty-state row keeps its row.
pub(crate) fn popup_row_count(state: &SlashState, registry: &SlashCommandRegistry) -> usize {
    popup_rows(state, registry).len().max(1)
}

/// T084(a): `/name` (+ dim aliases) display field shared by width measurement
/// and rendering so the description column aligns.
fn slash_name_text(cmd: &SlashCommand, selected: bool) -> String {
    // T084(f): the selected row carries the codex-rs `› ` glyph, replacing the
    // previous 2-space inset (unselected rows keep the inset; `›` is U+203A,
    // distinct from the U+25B8 glyph the old no-glyph test pins against).
    let mut text = if selected {
        format!("› /{}", cmd.name)
    } else {
        format!("  /{}", cmd.name)
    };
    if !cmd.aliases.is_empty() {
        let alias_str = cmd
            .aliases
            .iter()
            .map(|a| format!("/{a}"))
            .collect::<Vec<_>>()
            .join(" ");
        text.push_str(&format!(" ({alias_str})"));
    }
    text
}

fn render_slash_popup_inner(
    frame: &mut ratatui::Frame,
    state: &SlashState,
    popup_area: ratatui::layout::Rect,
    theme: &dyn OutputTheme,
    registry: &SlashCommandRegistry,
    max_rows: usize,
) {
    frame.render_widget(ratatui::widgets::Clear, popup_area);

    let bg_color = theme.bg();
    let row_bg = Style::default().bg(bg_color);

    // UX4: when filter is non-empty but no matches, show "no matches" row
    if state.filtered_indices.is_empty() && !state.filter.is_empty() {
        let no_match_line = Line::from(vec![Span::styled(
            "  no matches",
            theme
                .text_muted()
                .add_modifier(Modifier::ITALIC)
                .patch(row_bg),
        )]);
        let row_area = ratatui::layout::Rect::new(popup_area.x, popup_area.y, popup_area.width, 1);
        frame.render_widget(ratatui::widgets::Paragraph::new(no_match_line), row_area);
        return;
    }

    let total = state.filtered_indices.len();
    let rows = popup_rows(state, registry);
    if rows.is_empty() {
        return;
    }
    // Selection indexes filtered ITEMS, not rows: map to the row position so
    // the sliding window keeps the selected item visible.
    let sel_row = rows
        .iter()
        .position(
            |row| matches!(row, PopupRow::Item { filtered_pos, .. } if *filtered_pos == state.selected),
        )
        .unwrap_or(0);
    let mut start = if sel_row >= max_rows {
        sel_row - max_rows + 1
    } else {
        0
    };
    // T084(a): never lead the window with a group header — a header on the
    // top row shows no command and would push an item out of the slot. The
    // header re-enters the window once its group's first item is selected.
    while start < rows.len() && matches!(rows[start], PopupRow::Header(_)) {
        start += 1;
    }
    let end = (start + max_rows).min(rows.len());
    let window = &rows[start..end];
    // UX5: scroll indicators
    let has_items_above = start > 0;
    let has_items_below = end < rows.len();

    let selected_style = Style::default()
        .fg(ratatui::style::Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let unselected_name_style = Style::default();
    let unselected_desc_style = theme.text_muted();
    let header_style = theme
        .text_muted()
        .add_modifier(Modifier::ITALIC)
        .patch(row_bg);

    // T084(a): aligned description column — measure the name field over the
    // visible items, pad shorter names, truncate descriptions to fit.
    let name_width = window
        .iter()
        .filter_map(|row| match row {
            PopupRow::Item { cmd_idx, .. } => registry
                .all_commands()
                .get(*cmd_idx)
                .map(|cmd| slash_name_text(cmd, true).width()),
            PopupRow::Header(_) => None,
        })
        .max()
        .unwrap_or(0);
    let desc_avail = (popup_area.width as usize).saturating_sub(name_width + 2);

    // T084(f): the match index rides the last ITEM row (footer-within-budget —
    // ADR-006 forbids spending an extra row on it).
    let last_item = window
        .iter()
        .rposition(|row| matches!(row, PopupRow::Item { .. }));
    for (row, popup_row) in window.iter().enumerate() {
        let mut spans = Vec::new();
        match popup_row {
            PopupRow::Header(group) => {
                spans.push(Span::styled(format!("  {group}"), header_style));
            }
            PopupRow::Item {
                cmd_idx,
                filtered_pos,
            } => {
                let cmd = &registry.all_commands()[*cmd_idx];
                let is_selected = *filtered_pos == state.selected;

                let name_style = if is_selected {
                    selected_style
                } else {
                    unselected_name_style
                }
                .patch(row_bg);
                let alias_style = if is_selected {
                    selected_style
                } else {
                    unselected_desc_style
                }
                .patch(row_bg);
                let mut name_field = slash_name_text(cmd, is_selected);
                // Split the alias suffix back off so unselected rows keep the
                // old name-default / aliases-dim coloring.
                let alias_at = name_field.find(" (").unwrap_or(name_field.len());
                let alias_suffix = name_field.split_off(alias_at);
                spans.push(Span::styled(name_field.clone(), name_style));
                if !alias_suffix.is_empty() {
                    spans.push(Span::styled(alias_suffix.clone(), alias_style));
                }
                let pad = name_width.saturating_sub(name_field.width() + alias_suffix.width());
                if pad > 0 {
                    spans.push(Span::styled(
                        " ".repeat(pad),
                        Style::default().patch(row_bg),
                    ));
                }
                let desc: String = cmd.description.chars().take(desc_avail).collect();
                let desc_style = if is_selected {
                    selected_style
                } else {
                    unselected_desc_style
                }
                .patch(row_bg);
                spans.push(Span::styled(format!("  {desc}"), desc_style));

                // T084(f): match index on the last item row.
                if Some(row) == last_item {
                    spans.push(Span::styled(
                        format!("  {}/{}", state.selected + 1, total),
                        unselected_desc_style.patch(row_bg),
                    ));
                }
            }
        }

        // UX5: append scroll indicator to first/last rendered row
        if row == 0 && has_items_above {
            spans.push(Span::styled("  ▲", unselected_desc_style.patch(row_bg)));
        }
        let is_last_rendered = row + 1 == window.len();
        if is_last_rendered && has_items_below {
            spans.push(Span::styled("  ▼", unselected_desc_style.patch(row_bg)));
        }

        let line = Line::from(spans);

        if row as u16 >= popup_area.height {
            break;
        }
        let row_area = ratatui::layout::Rect::new(
            popup_area.x,
            popup_area.y + row as u16,
            popup_area.width,
            1,
        );
        frame.render_widget(ratatui::widgets::Paragraph::new(line), row_area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // === UX4: No-matches empty state ===

    #[test]
    fn filter_no_matches_keeps_popup_visible_with_message() {
        let mut registry = SlashCommandRegistry::new();
        registry.register("help".to_string(), vec![], "Show help".to_string());
        let mut state = SlashState::new();
        state.on_input_change("/zzz", &registry);
        assert!(
            state.visible,
            "popup must stay visible for non-empty filter"
        );
        assert!(state.filtered_indices.is_empty(), "no matches expected");
        assert_eq!(state.filter, "zzz");
    }

    #[test]
    fn filter_empty_matches_all() {
        let mut registry = SlashCommandRegistry::new();
        registry.register("help".to_string(), vec![], "Show help".to_string());
        let mut state = SlashState::new();
        state.on_input_change("/", &registry);
        assert!(state.visible, "popup visible for empty prefix");
        assert!(
            !state.filtered_indices.is_empty(),
            "all commands match empty prefix"
        );
    }

    #[test]
    fn move_up_down_on_empty_filtered_no_panic() {
        let registry = SlashCommandRegistry::new();
        let mut state = SlashState::new();
        state.on_input_change("/zzz", &registry);
        assert!(state.filtered_indices.is_empty());
        // Must not panic
        state.move_up();
        state.move_down();
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn selected_command_on_empty_filtered_returns_none() {
        let registry = SlashCommandRegistry::new();
        let mut state = SlashState::new();
        state.on_input_change("/zzz", &registry);
        assert!(state.selected_command(&registry).is_none());
    }

    #[test]
    fn render_no_matches_shows_popup_with_message() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut registry = SlashCommandRegistry::new();
        registry.register("help".to_string(), vec![], "Show help".to_string());
        let mut state = SlashState::new();
        state.on_input_change("/zzz", &registry);
        assert!(state.visible);

        let backend = TestBackend::new(60, 4);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let popup_area = ratatui::layout::Rect::new(0, 0, 60, 4);
        let theme = crate::tui::theme::ZenTheme;
        terminal
            .draw(|frame| {
                render_slash_popup_inline(frame, &state, popup_area, &theme, &registry);
            })
            .expect("draw");

        let buf = terminal.backend().buffer().clone();
        let row_text = |y: u16| -> String {
            let mut s = String::new();
            for x in 0..buf.area.width {
                s.push_str(buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "));
            }
            s.trim_end().to_string()
        };
        let first_row = row_text(0);
        assert!(
            first_row.contains("no matches"),
            "popup must show 'no matches' row: {first_row:?}"
        );
    }

    /// Codex parity: the selected row is highlighted by color/weight only —
    /// no "▸" glyph prefix.
    #[test]
    fn render_selected_row_has_no_glyph_prefix() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let registry = create_default_registry();
        let mut state = SlashState::new();
        state.on_input_change("/", &registry);

        let backend = TestBackend::new(60, 8);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let popup_area = ratatui::layout::Rect::new(0, 0, 60, 8);
        let theme = crate::tui::theme::ZenTheme;
        terminal
            .draw(|frame| {
                render_slash_popup_inline(frame, &state, popup_area, &theme, &registry);
            })
            .expect("draw");

        let buf = terminal.backend().buffer().clone();
        let row_text = |y: u16| -> String {
            let mut s = String::new();
            for x in 0..buf.area.width {
                s.push_str(buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "));
            }
            s.trim_end().to_string()
        };
        for y in 0..8 {
            assert!(
                !row_text(y).contains('\u{25B8}'),
                "no glyph prefix expected (Codex style): {:?}",
                row_text(y)
            );
        }
    }

    // === UX5: Scroll indicators ===

    #[test]
    fn scroll_indicators_mid_list_shows_both() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut registry = SlashCommandRegistry::new();
        // Register 12 commands so we can scroll within the 8-row window
        for i in 0..12 {
            registry.register(format!("cmd{}", i), vec![], format!("Command {}", i));
        }
        let mut state = SlashState::new();
        state.on_input_change("/", &registry);
        // Select item 8 of 12 — window start = 1 (items above AND below)
        state.selected = 8;

        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let popup_area = ratatui::layout::Rect::new(0, 0, 60, 10);
        let theme = crate::tui::theme::ZenTheme;
        terminal
            .draw(|frame| {
                render_slash_popup_inline(frame, &state, popup_area, &theme, &registry);
            })
            .expect("draw");

        let buf = terminal.backend().buffer().clone();
        let row_text = |y: u16| -> String {
            let mut s = String::new();
            for x in 0..buf.area.width {
                s.push_str(buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "));
            }
            s.trim_end().to_string()
        };

        // First row should have ▲ (scrolled down from top)
        let first = row_text(0);
        assert!(
            first.contains("▲"),
            "first row must show ▲ when scrolled: {first:?}"
        );
        // Last row should have ▼ (not at bottom)
        let last = row_text(7);
        assert!(
            last.contains("▼"),
            "last row must show ▼ when not at bottom: {last:?}"
        );
    }

    #[test]
    fn scroll_indicators_at_top_no_up_arrow() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut registry = SlashCommandRegistry::new();
        for i in 0..12 {
            registry.register(format!("cmd{}", i), vec![], format!("Command {}", i));
        }
        let mut state = SlashState::new();
        state.on_input_change("/", &registry);
        state.selected = 0; // at top

        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let popup_area = ratatui::layout::Rect::new(0, 0, 60, 10);
        let theme = crate::tui::theme::ZenTheme;
        terminal
            .draw(|frame| {
                render_slash_popup_inline(frame, &state, popup_area, &theme, &registry);
            })
            .expect("draw");

        let buf = terminal.backend().buffer().clone();
        let row_text = |y: u16| -> String {
            let mut s = String::new();
            for x in 0..buf.area.width {
                s.push_str(buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "));
            }
            s.trim_end().to_string()
        };

        let first = row_text(0);
        assert!(
            !first.contains("▲"),
            "first row must NOT show ▲ at top: {first:?}"
        );
        let last = row_text(7);
        assert!(
            last.contains("▼"),
            "last row must show ▼ when not at bottom: {last:?}"
        );
    }

    #[test]
    fn scroll_indicators_at_bottom_no_down_arrow() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut registry = SlashCommandRegistry::new();
        for i in 0..12 {
            registry.register(format!("cmd{}", i), vec![], format!("Command {}", i));
        }
        let mut state = SlashState::new();
        state.on_input_change("/", &registry);
        state.selected = 11; // last item

        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let popup_area = ratatui::layout::Rect::new(0, 0, 60, 10);
        let theme = crate::tui::theme::ZenTheme;
        terminal
            .draw(|frame| {
                render_slash_popup_inline(frame, &state, popup_area, &theme, &registry);
            })
            .expect("draw");

        let buf = terminal.backend().buffer().clone();
        let row_text = |y: u16| -> String {
            let mut s = String::new();
            for x in 0..buf.area.width {
                s.push_str(buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "));
            }
            s.trim_end().to_string()
        };

        let first = row_text(0);
        assert!(
            first.contains("▲"),
            "first row must show ▲ when scrolled: {first:?}"
        );
        let last = row_text(3);
        assert!(
            !last.contains("▼"),
            "last row must NOT show ▼ at bottom: {last:?}"
        );
    }

    // === Multi-line slash command popup (FIX 2) ===

    #[test]
    fn on_input_change_leading_newline_before_slash_shows_popup() {
        let mut registry = SlashCommandRegistry::new();
        registry.register("exit".to_string(), vec![], "Exit TUI".to_string());
        let mut state = SlashState::new();
        // Buffer with leading blank line then /ex — trim_start strips the newline
        state.on_input_change("\n/ex", &registry);
        assert!(
            state.visible,
            "popup must show for buffer whose trim_start begins with /"
        );
        assert_eq!(state.filter, "ex");
    }

    #[test]
    fn on_input_change_leading_newline_slash_empty_filter_shows_all() {
        let mut registry = SlashCommandRegistry::new();
        registry.register("exit".to_string(), vec![], "Exit TUI".to_string());
        let mut state = SlashState::new();
        state.on_input_change("\n/", &registry);
        assert!(
            state.visible,
            "popup must show for \n/ (bare slash after newline)"
        );
        assert!(
            !state.filtered_indices.is_empty(),
            "all commands match empty prefix"
        );
    }

    #[test]
    fn on_input_change_text_before_slash_hides_popup() {
        let mut registry = SlashCommandRegistry::new();
        registry.register("exit".to_string(), vec![], "Exit TUI".to_string());
        let mut state = SlashState::new();
        // "hello" before the / means trim_start still starts with 'h', not '/'
        state.on_input_change("hello\n/exit", &registry);
        assert!(
            !state.visible,
            "popup must hide when text precedes the slash"
        );
    }

    // === T084(a): grouping + description column ===

    #[test]
    fn t084_slash_groups_follow_registry_order_with_descriptions() {
        let registry = create_default_registry();
        let mut state = SlashState::new();
        state.on_input_change("/", &registry);
        let rows = popup_rows(&state, &registry);
        let groups: Vec<&str> = rows
            .iter()
            .filter_map(|row| match row {
                PopupRow::Header(group) => Some(*group),
                _ => None,
            })
            .collect();
        assert_eq!(
            groups,
            vec!["General", "Model", "Knowledge", "Session", "System"],
            "headers must follow registry first-appearance order"
        );
        let names: Vec<&str> = rows
            .iter()
            .filter_map(|row| match row {
                PopupRow::Item { cmd_idx, .. } => registry
                    .all_commands()
                    .get(*cmd_idx)
                    .map(|c| c.name.as_str()),
                _ => None,
            })
            .collect();
        let expected: Vec<&str> = registry
            .all_commands()
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, expected, "items must keep registry order");
    }

    #[test]
    fn t084_single_group_omits_header_row() {
        let mut registry = SlashCommandRegistry::new();
        for i in 0..12 {
            registry.register(format!("cmd{i}"), vec![], format!("Command {i}"));
        }
        let mut state = SlashState::new();
        state.on_input_change("/", &registry);
        let rows = popup_rows(&state, &registry);
        assert_eq!(rows.len(), 12, "no header rows for a single group");
        assert!(
            rows.iter().all(|row| matches!(row, PopupRow::Item { .. })),
            "every row must be an item"
        );
    }

    // === T084(f): `›` glyph + match index ===

    #[test]
    fn t084_selected_row_has_glyph_prefix_and_match_index() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let registry = create_default_registry();
        let total = registry.all_commands().len();
        let mut state = SlashState::new();
        state.on_input_change("/", &registry);

        // Wide viewport (100 cols): the last window item row is already
        // full-width at 60 cols, which would clip the match index — the
        // index asserts need a viewport where the description column fits.
        let backend = TestBackend::new(100, 8);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let popup_area = ratatui::layout::Rect::new(0, 0, 100, 8);
        let theme = crate::tui::theme::ZenTheme;
        terminal
            .draw(|frame| {
                render_slash_popup_inline(frame, &state, popup_area, &theme, &registry);
            })
            .expect("draw");

        let buf = terminal.backend().buffer().clone();
        let row_text = |y: u16| -> String {
            let mut s = String::new();
            for x in 0..buf.area.width {
                s.push_str(buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "));
            }
            s.trim_end().to_string()
        };
        let help_row = (0..8)
            .map(row_text)
            .find(|row| row.contains("/help"))
            .expect("help row must render");
        assert!(
            help_row.starts_with('\u{203a}'),
            "selected row must carry the › glyph: {help_row:?}"
        );
        assert!(
            help_row.contains("Show available commands"),
            "description column must render: {help_row:?}"
        );
        let index = format!("1/{total}");
        assert!(
            (0..8).map(row_text).any(|row| row.contains(&index)),
            "match index 1/{total} must render on the last item row"
        );
    }

    // === T084(d): command-token extraction ===

    #[test]
    fn t084_slash_command_token_extraction() {
        assert_eq!(slash_command_token("/exit"), Some("exit".to_string()));
        assert_eq!(slash_command_token("/"), Some(String::new()));
        assert_eq!(slash_command_token("/ex args here"), Some("ex".to_string()));
        assert_eq!(slash_command_token("  /model x"), Some("model".to_string()));
        assert_eq!(slash_command_token("hello"), None);
        assert_eq!(slash_command_token(""), None);
    }

    // === T084(a): slot budget counts headers ===

    #[test]
    fn t084_popup_row_count_includes_headers_and_floors_at_one() {
        // Registry order: Model → General(help) → System(serve): 3 items,
        // 3 groups → 3 header rows (every group change opens one).
        let mut registry = SlashCommandRegistry::new();
        registry.register("model".to_string(), vec![], "Model".to_string());
        registry.register("help".to_string(), vec![], "Help".to_string());
        registry.register("serve".to_string(), vec![], "Serve".to_string());
        let mut state = SlashState::new();
        state.on_input_change("/", &registry);
        assert_eq!(
            popup_row_count(&state, &registry),
            6,
            "headers must count toward the popup slot budget"
        );
        // No matches: the empty-state row still needs one row.
        state.on_input_change("/zz", &registry);
        assert_eq!(popup_row_count(&state, &registry), 1);
    }
}
