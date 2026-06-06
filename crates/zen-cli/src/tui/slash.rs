use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

pub struct SlashCommand {
    pub name: &'static str,
    pub description: &'static str,
}

pub const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "help",
        description: "Show available commands",
    },
    SlashCommand {
        name: "quit",
        description: "Exit TUI",
    },
    SlashCommand {
        name: "clear",
        description: "Clear output",
    },
    SlashCommand {
        name: "thinking",
        description: "Toggle thinking display",
    },
    SlashCommand {
        name: "model",
        description: "Switch provider/model",
    },
    SlashCommand {
        name: "export",
        description: "Export chat to Markdown",
    },
    SlashCommand {
        name: "note",
        description: "Create a note",
    },
    SlashCommand {
        name: "search",
        description: "Search knowledge base",
    },
    SlashCommand {
        name: "session",
        description: "List and select sessions",
    },
    SlashCommand {
        name: "new",
        description: "Create new session",
    },
    SlashCommand {
        name: "fork",
        description: "Fork current session",
    },
    SlashCommand {
        name: "rename",
        description: "Rename current session",
    },
    SlashCommand {
        name: "archive",
        description: "Archive current session",
    },
    SlashCommand {
        name: "serve",
        description: "Start gateway daemon",
    },
    SlashCommand {
        name: "config",
        description: "Show configuration",
    },
    SlashCommand {
        name: "consolidate",
        description: "Run consolidation pipeline",
    },
    SlashCommand {
        name: "lint",
        description: "Run knowledge lint",
    },
];

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

    pub fn on_input_change(&mut self, input: &str) {
        if let Some(stripped) = input.strip_prefix('/') {
            let has_space = stripped.contains(' ');
            if has_space {
                self.visible = false;
                return;
            }

            let token = stripped.split_whitespace().next().unwrap_or("");
            self.filter = token.to_lowercase();
            self.recompute_filtered();
            self.selected = 0;
            self.visible = !self.filtered_indices.is_empty();
        } else {
            self.visible = false;
        }
    }

    fn recompute_filtered(&mut self) {
        self.filtered_indices = SLASH_COMMANDS
            .iter()
            .enumerate()
            .filter(|(_, cmd)| cmd.name.starts_with(&self.filter))
            .map(|(i, _)| i)
            .collect();
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

    pub fn selected_command(&self) -> Option<&'static str> {
        self.filtered_indices
            .get(self.selected)
            .map(|&idx| SLASH_COMMANDS[idx].name)
    }

    pub fn dismiss(&mut self) {
        self.visible = false;
    }

    #[allow(dead_code)]
    pub fn visible_rows(&self) -> impl Iterator<Item = &SlashCommand> {
        let start = if self.selected >= MAX_POPUP_ROWS {
            self.selected - MAX_POPUP_ROWS + 1
        } else {
            0
        };
        let end = (start + MAX_POPUP_ROWS).min(self.filtered_indices.len());
        self.filtered_indices[start..end]
            .iter()
            .map(|&idx| &SLASH_COMMANDS[idx])
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
) {
    if !state.visible || state.filtered_indices.is_empty() {
        return;
    }

    let visible_count = state.visible_count();
    let popup_height = (visible_count as u16) + 2;
    let popup_y = input_area.y.saturating_sub(popup_height);
    let popup_area =
        ratatui::layout::Rect::new(input_area.x, popup_y, input_area.width, popup_height);

    frame.render_widget(ratatui::widgets::Clear, popup_area);

    let block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(" Commands ");

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let start = if state.selected >= MAX_POPUP_ROWS {
        state.selected - MAX_POPUP_ROWS + 1
    } else {
        0
    };

    for (row, &cmd_idx) in state.filtered_indices[start..start + visible_count]
        .iter()
        .enumerate()
    {
        let cmd = &SLASH_COMMANDS[cmd_idx];
        let is_selected = row + start == state.selected;

        let style = if is_selected {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        let desc_style = Style::default().fg(Color::DarkGray);

        let line = Line::from(vec![
            Span::styled(format!("  /{}", cmd.name), style),
            Span::styled(format!("  {}", cmd.description), desc_style),
        ]);

        let row_area = ratatui::layout::Rect::new(inner.x, inner.y + row as u16, inner.width, 1);
        frame.render_widget(ratatui::widgets::Paragraph::new(line), row_area);
    }
}
