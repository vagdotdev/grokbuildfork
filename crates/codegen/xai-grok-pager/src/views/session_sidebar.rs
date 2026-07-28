//! Compact local-session navigation shown beside wide agent views.

use crate::app::agent::AgentId;
use crate::app::agent_view::AgentView;
use crate::render::line_utils::truncate_str;
use crate::theme::Theme;
use crate::views::dashboard::{RowState, classify_top_level};
use indexmap::IndexMap;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

pub const SIDEBAR_WIDTH: u16 = 26;
pub const DIVIDER_WIDTH: u16 = 1;
pub const MIN_AGENT_WIDTH: u16 = 60;
const HEADER_HEIGHT: u16 = 1;

/// Absolute hit-test geometry from the last rendered frame.
#[derive(Debug, Default)]
pub struct SessionSidebarState {
    pub area: Option<Rect>,
    pub row_rects: Vec<(AgentId, Rect)>,
    pub hovered: Option<AgentId>,
}

impl SessionSidebarState {
    pub fn clear(&mut self) {
        self.area = None;
        self.row_rects.clear();
        self.hovered = None;
    }

    pub fn target_at(&self, col: u16, row: u16) -> Option<AgentId> {
        self.row_rects
            .iter()
            .find_map(|(id, rect)| rect.contains((col, row).into()).then_some(*id))
    }

    /// Update row hover from cached absolute geometry. Returns whether it changed.
    pub fn update_hover(&mut self, col: u16, row: u16) -> bool {
        let hovered = self.target_at(col, row);
        let changed = hovered != self.hovered;
        self.hovered = hovered;
        changed
    }
}

/// Split a wide agent surface into sidebar (including divider) and agent areas.
pub fn split_area(area: Rect, session_count: usize) -> Option<(Rect, Rect)> {
    let sidebar_width = SIDEBAR_WIDTH + DIVIDER_WIDTH;
    if session_count < 2 || area.height == 0 || area.width < sidebar_width + MIN_AGENT_WIDTH {
        return None;
    }
    Some((
        Rect::new(area.x, area.y, sidebar_width, area.height),
        Rect::new(
            area.x + sidebar_width,
            area.y,
            area.width - sidebar_width,
            area.height,
        ),
    ))
}

fn visible_start(total: usize, active_index: usize, capacity: usize) -> usize {
    if total <= capacity || capacity == 0 {
        return 0;
    }
    active_index
        .saturating_sub(capacity.saturating_sub(1))
        .min(total - capacity)
}

/// Render in `IndexMap` order and cache only the visible absolute row rectangles.
pub fn render(
    buf: &mut Buffer,
    area: Rect,
    agents: &IndexMap<AgentId, AgentView>,
    active: AgentId,
    state: &mut SessionSidebarState,
    mouse_pos: Option<(u16, u16)>,
) {
    state.area = Some(area);
    state.row_rects.clear();

    let theme = Theme::current();
    let content = Rect::new(area.x, area.y, SIDEBAR_WIDTH, area.height);
    let divider = Rect::new(area.x + SIDEBAR_WIDTH, area.y, DIVIDER_WIDTH, area.height);
    buf.set_style(content, Style::default().bg(theme.bg_base));
    buf.set_style(
        divider,
        Style::default().fg(theme.gray_dim).bg(theme.bg_base),
    );
    for row in divider.y..divider.y.saturating_add(divider.height) {
        buf.set_string(divider.x, row, "│", Style::default().fg(theme.gray_dim));
    }

    let header_height = HEADER_HEIGHT.min(content.height.saturating_sub(1));
    let capacity = content.height.saturating_sub(header_height) as usize;
    let active_index = agents.get_index_of(&active).unwrap_or(0);
    let start = visible_start(agents.len(), active_index, capacity);
    let end = (start + capacity).min(agents.len());

    if header_height > 0 {
        let overflow = match (start > 0, end < agents.len()) {
            (true, true) => " ↑↓",
            (true, false) => " ↑",
            (false, true) => " ↓",
            (false, false) => "",
        };
        let label = format!(" SESSIONS{overflow}");
        buf.set_stringn(
            content.x,
            content.y,
            label,
            content.width as usize,
            Style::default()
                .fg(theme.gray_dim)
                .add_modifier(Modifier::BOLD),
        );
    }

    for (visible_index, (id, _)) in agents.iter().skip(start).take(capacity).enumerate() {
        let rect = Rect::new(
            content.x,
            content.y + header_height + visible_index as u16,
            content.width,
            1,
        );
        state.row_rects.push((*id, rect));
    }

    if let Some((col, row)) = mouse_pos {
        state.hovered = state.target_at(col, row);
    } else if state
        .hovered
        .is_some_and(|id| !state.row_rects.iter().any(|(row_id, _)| *row_id == id))
    {
        state.hovered = None;
    }

    for ((id, agent), (_, rect)) in agents
        .iter()
        .skip(start)
        .take(capacity)
        .zip(state.row_rects.iter())
    {
        let is_active = *id == active;
        let is_hovered = state.hovered == Some(*id);
        let mut row_style = Style::default().fg(theme.text_primary).bg(theme.bg_base);
        if is_active {
            row_style = row_style
                .bg(theme.bg_highlight)
                .add_modifier(Modifier::BOLD);
        } else if is_hovered {
            row_style = row_style.bg(theme.bg_hover);
        }
        buf.set_style(*rect, row_style);

        let status = classify_top_level(agent);
        let (icon, color) = match status {
            RowState::NeedsInput => (crate::glyphs::diamond_filled(), theme.warning),
            RowState::Working => (crate::glyphs::diamond_filled(), theme.accent_running),
            _ => (crate::glyphs::diamond_hollow(), theme.gray_dim),
        };
        buf.set_string(rect.x + 1, rect.y, icon, Style::default().fg(color));
        let title = crate::views::session_title::entry_title(agent);
        let title = truncate_str(&title, rect.width.saturating_sub(4) as usize);
        buf.set_stringn(
            rect.x + 3,
            rect.y,
            title,
            rect.width.saturating_sub(3) as usize,
            row_style,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_requires_two_sessions_and_full_agent_width() {
        let area = Rect::new(4, 2, SIDEBAR_WIDTH + DIVIDER_WIDTH + MIN_AGENT_WIDTH, 20);
        assert!(split_area(area, 1).is_none());
        let (sidebar, agent) = split_area(area, 2).expect("wide multi-session layout");
        assert_eq!(sidebar, Rect::new(4, 2, 27, 20));
        assert_eq!(agent, Rect::new(31, 2, 60, 20));
        assert!(split_area(Rect::new(0, 0, area.width - 1, 20), 2).is_none());
    }

    #[test]
    fn overflow_keeps_active_row_visible() {
        assert_eq!(visible_start(10, 0, 4), 0);
        assert_eq!(visible_start(10, 5, 4), 2);
        assert_eq!(visible_start(10, 9, 4), 6);
    }
}
