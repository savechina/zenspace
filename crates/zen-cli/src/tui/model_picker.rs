use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};
use zen_core::config::ZenConfig;

const MAX_ROWS: usize = 12;

#[derive(Clone, Copy, PartialEq)]
pub enum PickerStage {
    Provider,
    Model,
    Variant,
}

pub struct ModelPickerState {
    pub visible: bool,
    pub stage: PickerStage,
    pub selected: usize,
    pub scroll_offset: usize,
    pub provider_names: Vec<String>,
    pub model_names: Vec<String>,
    pub variant_names: Vec<String>,
    pub chosen_provider: Option<String>,
    pub chosen_model: Option<String>,
}

impl ModelPickerState {
    pub fn new() -> Self {
        Self {
            visible: false,
            stage: PickerStage::Provider,
            selected: 0,
            scroll_offset: 0,
            provider_names: Vec::new(),
            model_names: Vec::new(),
            variant_names: Vec::new(),
            chosen_provider: None,
            chosen_model: None,
        }
    }

    pub fn show(&mut self, config: &ZenConfig) {
        self.provider_names = config.providers.keys().cloned().collect();
        self.provider_names.sort();
        self.stage = PickerStage::Provider;
        self.selected = 0;
        self.scroll_offset = 0;
        self.chosen_provider = None;
        self.chosen_model = None;
        self.visible = true;
    }

    pub fn dismiss(&mut self) {
        self.visible = false;
    }

    fn current_items(&self) -> &[String] {
        match self.stage {
            PickerStage::Provider => &self.provider_names,
            PickerStage::Model => &self.model_names,
            PickerStage::Variant => &self.variant_names,
        }
    }

    pub fn move_up(&mut self) {
        let items = self.current_items();
        if items.is_empty() {
            return;
        }
        self.selected = if self.selected == 0 {
            items.len() - 1
        } else {
            self.selected - 1
        };
        self.update_scroll();
    }

    pub fn move_down(&mut self) {
        let items = self.current_items();
        if items.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % items.len();
        self.update_scroll();
    }

    fn update_scroll(&mut self) {
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + MAX_ROWS {
            self.scroll_offset = self.selected - MAX_ROWS + 1;
        }
    }

    /// Advance to next stage. Returns Some((provider, model, variant)) when done.
    pub fn advance(&mut self, config: &ZenConfig) -> Option<(String, String, Option<String>)> {
        match self.stage {
            PickerStage::Provider => {
                let name = self.provider_names.get(self.selected)?.clone();
                self.chosen_provider = Some(name.clone());
                self.model_names.clear();
                self.variant_names.clear();
                if let Some(p) = config.providers.get(&name) {
                    self.model_names = p.models.keys().cloned().collect();
                    self.model_names.sort();
                    if self.model_names.is_empty() {
                        // No model catalog — jump directly to done with default model
                        let model = p.default_model.clone().unwrap_or_else(|| "default".into());
                        self.dismiss();
                        return Some((name, model, None));
                    }
                }
                if self.model_names.is_empty() {
                    self.dismiss();
                    return None;
                }
                self.stage = PickerStage::Model;
                self.selected = 0;
                self.scroll_offset = 0;
                None
            }
            PickerStage::Model => {
                let name = self.model_names.get(self.selected)?.clone();
                self.chosen_model = Some(name.clone());
                self.variant_names.clear();
                if let Some(provider) = &self.chosen_provider
                    && let Some(p) = config.providers.get(provider)
                    && let Some(entry) = p.models.get(&name)
                {
                    self.variant_names = entry.variants.keys().cloned().collect();
                    self.variant_names.sort();
                }
                if self.variant_names.is_empty() {
                    // No variants — done
                    let provider = self.chosen_provider.take()?;
                    self.dismiss();
                    return Some((provider, name, None));
                }
                self.stage = PickerStage::Variant;
                self.selected = 0;
                self.scroll_offset = 0;
                None
            }
            PickerStage::Variant => {
                let variant = self.variant_names.get(self.selected).cloned();
                let provider = self.chosen_provider.take()?;
                let model = self.chosen_model.take()?;
                self.dismiss();
                Some((provider, model, variant))
            }
        }
    }

    pub fn go_back(&mut self) {
        match self.stage {
            PickerStage::Model => {
                self.stage = PickerStage::Provider;
                self.selected = 0;
                self.scroll_offset = 0;
            }
            PickerStage::Variant => {
                self.stage = PickerStage::Model;
                self.selected = 0;
                self.scroll_offset = 0;
            }
            _ => {}
        }
    }
}

impl Default for ModelPickerState {
    fn default() -> Self {
        Self::new()
    }
}

pub fn render_model_picker(
    frame: &mut ratatui::Frame,
    state: &ModelPickerState,
    theme: &dyn crate::tui::theme::OutputTheme,
) {
    if !state.visible {
        return;
    }

    let items = state.current_items();
    if items.is_empty() && state.stage == PickerStage::Provider {
        return;
    }

    let area = frame.area();
    let popup_width = (area.width * 60 / 100).clamp(30, 60);
    let visible_count = items.len().min(MAX_ROWS);
    let popup_height = (visible_count as u16) + 5;
    let popup_x = (area.width.saturating_sub(popup_width)) / 2;
    let popup_y = (area.height.saturating_sub(popup_height)) / 2;
    let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

    frame.render_widget(Clear, popup_area);

    let accent = Style::default().fg(theme.info_accent());
    let muted = theme.text_muted();
    let bg = Style::default().bg(theme.bg());

    let title = match state.stage {
        PickerStage::Provider => " Select Provider ",
        PickerStage::Model => " Select Model ",
        PickerStage::Variant => " Select Variant ",
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(accent)
        .title(title)
        .style(bg);

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    let visible_items = &items[state.scroll_offset..state.scroll_offset + visible_count];

    let list_items: Vec<ListItem> = visible_items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            let global_idx = idx + state.scroll_offset;
            let is_selected = global_idx == state.selected;
            let style = if is_selected {
                accent.add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(Span::styled(format!("  {item}"), style)))
        })
        .collect();

    let list = List::new(list_items).highlight_style(
        Style::default()
            .fg(theme.info_accent())
            .add_modifier(Modifier::BOLD),
    );

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected - state.scroll_offset));

    frame.render_stateful_widget(list, chunks[0], &mut list_state);

    let help = match state.stage {
        PickerStage::Provider => Line::from(vec![
            Span::styled("↑↓", muted),
            Span::styled(" nav ", accent),
            Span::styled("↵", muted),
            Span::styled(" pick ", accent),
            Span::styled("esc", muted),
            Span::styled(" cancel", accent),
        ]),
        PickerStage::Model | PickerStage::Variant => Line::from(vec![
            Span::styled("↑↓", muted),
            Span::styled(" nav ", accent),
            Span::styled("↵", muted),
            Span::styled(" pick ", accent),
            Span::styled("←", muted),
            Span::styled(" back ", accent),
            Span::styled("esc", muted),
            Span::styled(" cancel", accent),
        ]),
    };
    frame.render_widget(ratatui::widgets::Paragraph::new(help), chunks[1]);
}
