//! Loop workbench panel (005-agentic-loop, T022, US3).
//!
//! Renders cycle state, last counters, open gaps with 3-tier grouping:
//! Short (M0-M1: IngestNeverConsolidated, QuarantinedNote, LlmFailure),
//! Mid   (M2-M3: WikiPageWithoutEntities, OrphanEntity, UnresolvedRelationship, DuplicateEntityAlias),
//! Long  (M4-M5: DecisionBlocked, CommitmentOverdue, SelfCognitionBlocked, AntiTalkSuspect).
//! Reuses existing ratatui shell (DESIGN §2.1). Toggle 'L', hidden until first cycle.

use std::collections::BTreeMap;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use zen_core::paths::ZenPaths;
use zen_vault::distill::LoopCycleReport;

/// In-memory panel state — loaded on demand from `<logs>/loop-last-report.json` + `loop-gaps.jsonl`.
#[derive(Debug, Default)]
pub struct LoopPanelState {
    pub visible: bool,
    pub last_report: Option<LoopCycleReport>,
    pub gaps_by_kind: BTreeMap<String, u64>,
    pub gaps_total: u64,
}

fn tier_for(kind: &str) -> &'static str {
    match kind {
        "IngestNeverConsolidated" | "QuarantinedNote" | "LlmFailure" => "Short",
        "WikiPageWithoutEntities"
        | "OrphanEntity"
        | "UnresolvedRelationship"
        | "DuplicateEntityAlias" => "Mid",
        "DecisionBlocked" | "CommitmentOverdue" | "SelfCognitionBlocked" | "AntiTalkSuspect" => {
            "Long"
        }
        _ => "Mid",
    }
}

impl LoopPanelState {
    /// Load from vault paths. Returns default (hidden) on error or missing files.
    pub fn load() -> Self {
        let mut state = Self::default();
        let Ok(paths) = ZenPaths::detect() else {
            return state;
        };
        let logs = paths.logs();
        let last_path = logs.join("loop-last-report.json");
        if let Ok(raw) = std::fs::read_to_string(&last_path)
            && let Ok(report) = serde_json::from_str::<LoopCycleReport>(&raw)
        {
            state.last_report = Some(report);
            state.visible = true;
        }
        let gaps_path = logs.join("loop-gaps.jsonl");
        if let Ok(raw) = std::fs::read_to_string(&gaps_path) {
            for line in raw.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(line)
                    && let Some(k) = v.get("kind").and_then(|k| k.as_str())
                {
                    *state.gaps_by_kind.entry(k.to_string()).or_insert(0) += 1;
                    state.gaps_total += 1;
                }
            }
        }
        state
    }

    pub fn toggle(&mut self) {
        if self.last_report.is_none() && !self.visible {
            // Refresh from disk on first toggle — may have become available
            *self = Self::load();
            // If still no report, allow visible=true to show empty state per quickstart §4
            if self.last_report.is_none() {
                self.visible = !self.visible;
                return;
            }
        }
        self.visible = !self.visible;
    }

    #[allow(dead_code)]
    pub fn refresh(&mut self) {
        let fresh = Self::load();
        let was_visible = self.visible;
        *self = fresh;
        self.visible = was_visible;
    }
}

/// Render the panel as a centered overlay. Caller must `render_widget(Clear)` first.
pub fn render_loop_panel(frame: &mut Frame, state: &LoopPanelState, area: Rect) {
    // Centered overlay: 70% width, 60% height, min 60x20
    let width = (area.width as f32 * 0.72) as u16;
    let height = (area.height as f32 * 0.62) as u16;
    let width = width.clamp(60, area.width.saturating_sub(4));
    let height = height.clamp(18, area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let panel_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, panel_area);

    let title = " Loop Workbench (L to close, Short/Mid/Long) ";
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Cyan));

    let mut lines: Vec<Line> = Vec::new();

    if let Some(report) = &state.last_report {
        let outcome = report
            .outcome
            .map(|o| o.as_str().to_string())
            .unwrap_or_else(|| "unknown".into());
        lines.push(Line::from(vec![
            Span::styled(" Cycle: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!("{} · {}", report.cycle_id, outcome)),
        ]));
        if let Some(ts) = report.started_at {
            lines.push(Line::from(Span::styled(
                format!(" Started: {}", ts.to_rfc3339()),
                Style::default().fg(Color::DarkGray),
            )));
        }
        lines.push(Line::from(vec![
            Span::styled(" Counters: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!(
                "notes {} · pages {} · archived {} · gaps {}",
                report.notes_processed,
                report.pages_created,
                report.archived_count,
                report.gaps.len()
            )),
        ]));
        lines.push(Line::from(Span::raw("")));
    } else {
        lines.push(Line::from(Span::styled(
            " No cycle yet — run `zen wiki loop run` to seed the panel (hidden until first cycle)",
            Style::default().fg(Color::Yellow),
        )));
        lines.push(Line::from(Span::raw("")));
    }

    // 3-tier grouping
    let mut short: Vec<String> = Vec::new();
    let mut mid: Vec<String> = Vec::new();
    let mut long: Vec<String> = Vec::new();
    for (kind, count) in &state.gaps_by_kind {
        let entry = format!("{kind}: {count}");
        match tier_for(kind) {
            "Short" => short.push(entry),
            "Long" => long.push(entry),
            _ => mid.push(entry),
        }
    }
    if state.gaps_total == 0 {
        lines.push(Line::from(Span::styled(
            " Open gaps: none — vault healthy",
            Style::default().fg(Color::Green),
        )));
    } else {
        lines.push(Line::from(vec![
            Span::styled(
                " Open gaps: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("{} total", state.gaps_total)),
        ]));
        if !short.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("  Short (M0-M1): {}", short.join(", ")),
                Style::default().fg(Color::Cyan),
            )));
        }
        if !mid.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("  Mid   (M2-M3): {}", mid.join(", ")),
                Style::default().fg(Color::White),
            )));
        }
        if !long.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("  Long  (M4-M5): {}", long.join(", ")),
                Style::default().fg(Color::Magenta),
            )));
        }
    }

    lines.push(Line::from(Span::raw("")));
    lines.push(Line::from(Span::styled(
        " Tips: `zen wiki loop status --json` / `zen wiki loop gaps --json` for CLI parity (SC-005 <2s)",
        Style::default().fg(Color::DarkGray),
    )));

    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(para, panel_area);
}
