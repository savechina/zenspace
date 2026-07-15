use crate::tui::theme::OutputTheme;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use zen_core::types::SessionRecord;

pub const MAX_SESSION_ROWS: usize = 10;

pub struct SessionPickerState {
    pub visible: bool,
    pub sessions: Vec<SessionRecord>,
    pub selected: usize,
    pub scroll_offset: usize,
    pub archive_pending: Option<String>,
    pub rename_mode: bool,
    pub rename_input: String,
}

impl SessionPickerState {
    pub fn new() -> Self {
        Self {
            visible: false,
            sessions: Vec::new(),
            selected: 0,
            scroll_offset: 0,
            archive_pending: None,
            rename_mode: false,
            rename_input: String::new(),
        }
    }

    pub fn load_sessions(&mut self) {
        if let Ok(sessions) = SessionRecord::list() {
            self.sessions = sessions
                .into_iter()
                .filter(|s| s.status != zen_core::types::SessionStatus::Archived)
                .collect();
            self.selected = 0;
            self.scroll_offset = 0;
        }
    }

    pub fn show(&mut self) {
        self.load_sessions();
        self.visible = true;
    }

    pub fn dismiss(&mut self) {
        self.visible = false;
    }

    pub fn move_up(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        self.selected = if self.selected == 0 {
            self.sessions.len() - 1
        } else {
            self.selected - 1
        };
        self.update_scroll();
    }

    pub fn move_down(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.sessions.len();
        self.update_scroll();
    }

    fn update_scroll(&mut self) {
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + MAX_SESSION_ROWS {
            self.scroll_offset = self.selected - MAX_SESSION_ROWS + 1;
        }
    }

    pub fn selected_session(&self) -> Option<&SessionRecord> {
        self.sessions.get(self.selected)
    }

    pub fn visible_count(&self) -> usize {
        self.sessions.len().min(MAX_SESSION_ROWS)
    }

    pub fn start_archive(&mut self) {
        if let Some(session) = self.selected_session() {
            self.archive_pending = Some(session.id.clone());
        }
    }

    pub fn confirm_archive(&mut self) -> Option<String> {
        if let Some(pending_id) = self.archive_pending.take() {
            self.sessions.retain(|s| s.id != pending_id);
            if self.selected >= self.sessions.len() && !self.sessions.is_empty() {
                self.selected = self.sessions.len() - 1;
            }
            return Some(pending_id);
        }
        None
    }

    pub fn cancel_archive(&mut self) {
        self.archive_pending = None;
    }

    pub fn start_rename(&mut self) {
        if let Some(session) = self.selected_session() {
            self.rename_input = session.title.clone().unwrap_or_default();
            self.rename_mode = true;
        }
    }

    pub fn confirm_rename(&mut self) -> Option<(String, String)> {
        if self.rename_mode
            && let Some(session) = self.selected_session()
        {
            let session_id = session.id.clone();
            let new_title = self.rename_input.clone();
            self.rename_mode = false;
            self.rename_input.clear();
            return Some((session_id, new_title));
        }
        None
    }

    pub fn cancel_rename(&mut self) {
        self.rename_mode = false;
        self.rename_input.clear();
    }

    pub fn rename_input_char(&mut self, c: char) {
        if self.rename_mode {
            self.rename_input.push(c);
        }
    }

    pub fn rename_input_backspace(&mut self) {
        if self.rename_mode {
            self.rename_input.pop();
        }
    }
}

impl Default for SessionPickerState {
    fn default() -> Self {
        Self::new()
    }
}

fn format_time_ago(dt: &chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let duration = now.signed_duration_since(*dt);

    if duration.num_seconds() < 60 {
        "just now".to_string()
    } else if duration.num_minutes() < 60 {
        format!("{}m ago", duration.num_minutes())
    } else if duration.num_hours() < 24 {
        format!("{}h ago", duration.num_hours())
    } else if duration.num_days() < 7 {
        format!("{}d ago", duration.num_days())
    } else {
        dt.format("%Y-%m-%d").to_string()
    }
}

pub fn render_session_picker(
    frame: &mut ratatui::Frame,
    state: &SessionPickerState,
    current_session_id: Option<&str>,
    theme: &dyn OutputTheme,
) {
    if !state.visible || state.sessions.is_empty() {
        return;
    }

    let area = frame.area();
    let popup_width = (area.width * 70 / 100).clamp(40, 80);
    let popup_height = if state.rename_mode {
        (state.visible_count() as u16) + 7
    } else {
        (state.visible_count() as u16) + 4
    };
    let popup_x = (area.width.saturating_sub(popup_width)) / 2;
    let popup_y = (area.height.saturating_sub(popup_height)) / 2;
    let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

    frame.render_widget(Clear, popup_area);

    let muted = theme.text_muted();
    let accent_style = Style::default().fg(theme.info_accent());
    let archive_red = Style::default().fg(Color::Red);
    let bg_color = theme.bg();
    let area_bg = Style::default().bg(bg_color);

    let border_style = if state.archive_pending.is_some() {
        archive_red
    } else {
        accent_style
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(" Sessions ")
        .style(area_bg);

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let constraints = if state.rename_mode {
        vec![
            Constraint::Min(1),
            Constraint::Length(3),
            Constraint::Length(1),
        ]
    } else {
        vec![Constraint::Min(1), Constraint::Length(1)]
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    let visible_sessions =
        &state.sessions[state.scroll_offset..state.scroll_offset + state.visible_count()];

    let items: Vec<ListItem> = visible_sessions
        .iter()
        .enumerate()
        .map(|(idx, session)| {
            let global_idx = idx + state.scroll_offset;
            let is_selected = global_idx == state.selected;
            let is_current = current_session_id == Some(&session.id);
            let is_archive_pending = state.archive_pending.as_deref() == Some(&session.id);

            let title = session.title.as_deref().unwrap_or("(untitled)");
            let time_ago = format_time_ago(&session.updated_at);
            let fork_indicator = if session.parent_id.is_some() {
                " ⑂"
            } else {
                ""
            };
            let current_indicator = if is_current { " ●" } else { "" };

            let style = if is_archive_pending {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            } else if is_selected {
                accent_style.add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let display_title = if is_archive_pending {
                format!(" {} (press Ctrl+A again to confirm) ", title)
            } else {
                format!(" {}{}{} ", title, fork_indicator, current_indicator)
            };

            let line = Line::from(vec![
                Span::styled(display_title, style),
                Span::styled(time_ago, muted),
            ]);

            ListItem::new(line)
        })
        .collect();

    let list = List::new(items).highlight_style(
        Style::default()
            .fg(theme.info_accent())
            .add_modifier(Modifier::BOLD),
    );

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected - state.scroll_offset));

    frame.render_stateful_widget(list, chunks[0], &mut list_state);

    if state.rename_mode {
        let rename_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
            .title(" Rename Session ")
            .style(area_bg);
        let rename_inner = rename_block.inner(chunks[1]);
        frame.render_widget(rename_block, chunks[1]);

        let rename_text = Line::from(vec![
            Span::styled("Title: ", accent_style),
            Span::styled(
                format!("{}_", state.rename_input),
                Style::default().fg(Color::Yellow),
            ),
        ]);
        frame.render_widget(Paragraph::new(rename_text), rename_inner);

        let help_text = Line::from(vec![
            Span::styled("Enter", muted),
            Span::styled(" confirm ", accent_style),
            Span::styled("Esc", muted),
            Span::styled(" cancel", accent_style),
        ]);
        frame.render_widget(Paragraph::new(help_text), chunks[2]);
    } else {
        let help_text = Line::from(vec![
            Span::styled("↑↓", muted),
            Span::styled(" navigate ", accent_style),
            Span::styled("↵", muted),
            Span::styled(" resume ", accent_style),
            Span::styled("Ctrl+R", muted),
            Span::styled(" rename ", accent_style),
            Span::styled("Ctrl+A", muted),
            Span::styled(" archive ", accent_style),
            Span::styled("esc", muted),
            Span::styled(" close", accent_style),
        ]);
        frame.render_widget(Paragraph::new(help_text), chunks[1]);
    }
}
