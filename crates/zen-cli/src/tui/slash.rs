use crate::tui::theme::OutputTheme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

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
        if let Some(stripped) = input.strip_prefix('/') {
            let has_space = stripped.contains(' ');
            if has_space {
                self.visible = false;
                return;
            }

            let token = stripped.split_whitespace().next().unwrap_or("");
            self.filter = token.to_lowercase();
            self.recompute_filtered(registry);
            self.selected = 0;
            self.visible = !self.filtered_indices.is_empty();
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

    pub fn at_first(&self) -> bool {
        self.selected == 0
    }

    pub fn at_last(&self) -> bool {
        self.selected + 1 >= self.filtered_indices.len()
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
    if !state.visible || state.filtered_indices.is_empty() {
        return;
    }

    let visible_count = state.visible_count();
    let popup_height = (visible_count as u16) + 2;
    let popup_y = input_area.y.saturating_sub(popup_height);
    let popup_area =
        ratatui::layout::Rect::new(input_area.x, popup_y, input_area.width, popup_height);

    render_slash_popup_inner(frame, state, popup_area, theme, registry, MAX_POPUP_ROWS);
}

const INLINE_POPUP_ROWS: usize = 4;

pub fn render_slash_popup_inline(
    frame: &mut ratatui::Frame,
    state: &SlashState,
    popup_area: ratatui::layout::Rect,
    theme: &dyn OutputTheme,
    registry: &SlashCommandRegistry,
) {
    if !state.visible || state.filtered_indices.is_empty() {
        return;
    }
    render_slash_popup_inner(frame, state, popup_area, theme, registry, INLINE_POPUP_ROWS);
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
    let block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_style(theme.text_muted())
        .title(" Commands ")
        .style(Style::default().bg(bg_color));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let visible_count = state.filtered_indices.len().min(max_rows);
    let start = if state.selected >= max_rows {
        state.selected - max_rows + 1
    } else {
        0
    };

    let selected_style = Style::default()
        .fg(theme.info_accent())
        .add_modifier(Modifier::BOLD);
    let unselected_style = Style::default();
    let desc_style = theme.text_muted();
    let row_bg = Style::default().bg(bg_color);

    for (row, &cmd_idx) in state.filtered_indices[start..start + visible_count]
        .iter()
        .enumerate()
    {
        let cmd = &registry.all_commands()[cmd_idx];
        let is_selected = row + start == state.selected;

        let name_style = if is_selected {
            selected_style
        } else {
            unselected_style
        };

        let mut spans = vec![Span::styled(
            format!("  /{}", cmd.name),
            name_style.patch(row_bg),
        )];
        if !cmd.aliases.is_empty() {
            let alias_str = cmd
                .aliases
                .iter()
                .map(|a| format!("/{}", a))
                .collect::<Vec<_>>()
                .join(" ");
            spans.push(Span::styled(
                format!(" ({})", alias_str),
                desc_style.patch(row_bg),
            ));
        }
        spans.push(Span::styled(
            format!("  {}", cmd.description),
            desc_style.patch(row_bg),
        ));

        let line = Line::from(spans);

        if row as u16 >= inner.height {
            break;
        }
        let row_area = ratatui::layout::Rect::new(inner.x, inner.y + row as u16, inner.width, 1);
        frame.render_widget(ratatui::widgets::Paragraph::new(line), row_area);
    }
}
