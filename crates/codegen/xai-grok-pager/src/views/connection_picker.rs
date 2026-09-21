//! Workshop connection picker (ratatui view over `workshop_auth::PickerState`).
//!
//! Layout follows the Blackpen export: a title, two tabs (Models / Subscriptions), a left list
//! (Models cards, or the Claude / Codex / Cursor rails with a Detecting / Ready / Sign in pill),
//! and a right detail pane. The optional xAI card is last and never preselected; the pane for it
//! carries the plan's copy verbatim. Nothing here starts a login: outcomes are decided by the
//! picker state in the dispatcher.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};
use workshop_auth::{PickerState, PickerTab, Pill, XAI_CARD_ID};

use crate::theme::Theme;

const LEFT_WIDTH: u16 = 40;

pub fn render(area: Rect, buf: &mut Buffer, theme: &Theme, picker: &PickerState, h_margin: u16) {
    let area = Rect {
        x: area.x + h_margin,
        y: area.y,
        width: area.width.saturating_sub(h_margin * 2),
        height: area.height,
    };
    if area.width < 20 || area.height < 8 {
        Paragraph::new("Workshop: terminal too small for the connection picker")
            .style(Style::default().fg(theme.text_primary))
            .render(area, buf);
        return;
    }

    let [title_area, tabs_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(4),
        Constraint::Length(2),
    ])
    .areas(area);

    Paragraph::new(vec![
        Line::from(Span::styled(
            "Workshop — connect a model",
            Style::default()
                .fg(theme.text_primary)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "Workshop never signs you in anywhere by default. Pick how it should reach a model.",
            Style::default().fg(theme.gray_bright),
        )),
    ])
    .render(title_area, buf);

    // Tabs
    let tab_span = |tab: PickerTab| {
        let active = picker.tab == tab;
        let style = if active {
            Style::default()
                .fg(theme.text_primary)
                .bg(theme.bg_highlight)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.gray_bright)
        };
        Span::styled(format!(" {} ", tab.title()), style)
    };
    Paragraph::new(Line::from(vec![
        tab_span(PickerTab::Models),
        Span::raw("  "),
        tab_span(PickerTab::Subscriptions),
        Span::styled("   Tab switches", Style::default().fg(theme.gray_dim)),
    ]))
    .render(tabs_area, buf);

    let narrow = body_area.width < LEFT_WIDTH + 30;
    let (left, right) = if narrow {
        let [l, r] = Layout::vertical([Constraint::Percentage(55), Constraint::Min(3)]).areas(body_area);
        (l, r)
    } else {
        let [l, r] =
            Layout::horizontal([Constraint::Length(LEFT_WIDTH), Constraint::Min(20)]).areas(body_area);
        (l, r)
    };

    match picker.tab {
        PickerTab::Models => render_models_list(left, buf, theme, picker),
        PickerTab::Subscriptions => render_rails(left, buf, theme, picker),
    }
    render_detail(right, buf, theme, picker);

    let footer = match picker.tab {
        PickerTab::Models => "↑↓ move · Enter details · Esc back/close · Tab: Subscriptions",
        PickerTab::Subscriptions => "↑↓ move · Enter connect · Esc back/close · Tab: Models",
    };
    Paragraph::new(vec![
        Line::from(Span::styled(footer, Style::default().fg(theme.gray_bright))),
        Line::from(Span::styled(
            "Connection classes: Direct API · Local · Agent adapter. Subscriptions are never used as API base URLs.",
            Style::default().fg(theme.gray_dim),
        )),
    ])
    .render(footer_area, buf);
}

fn row_style(theme: &Theme, selected: bool) -> Style {
    if selected {
        Style::default()
            .fg(theme.text_primary)
            .bg(theme.bg_highlight)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.text_primary)
    }
}

fn render_models_list(area: Rect, buf: &mut Buffer, theme: &Theme, picker: &PickerState) {
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(theme.gray_dim));
    let inner = block.inner(area);
    block.render(area, buf);
    let mut lines: Vec<Line> = Vec::new();
    for (i, card) in picker.models.iter().enumerate() {
        let selected = i == picker.models_selected;
        let marker = if selected { "› " } else { "  " };
        let mut spans = vec![
            Span::styled(marker, row_style(theme, selected)),
            Span::styled(card.title, row_style(theme, selected)),
        ];
        if let Some(a) = card.availability() {
            spans.push(Span::styled(
                format!("  {a}"),
                Style::default().fg(theme.accent_success),
            ));
        }
        if card.id == XAI_CARD_ID {
            spans.push(Span::styled(
                "  optional",
                Style::default().fg(theme.warning),
            ));
        }
        lines.push(Line::from(spans));
        lines.push(Line::from(Span::styled(
            format!("    {}", card.class.label()),
            Style::default().fg(theme.gray_dim),
        )));
    }
    Paragraph::new(lines).render(inner, buf);
}

fn pill_style(theme: &Theme, pill: Pill) -> Style {
    match pill {
        Pill::Detecting => Style::default().fg(theme.gray_bright),
        Pill::Ready => Style::default().fg(theme.accent_success),
        Pill::SignIn => Style::default().fg(theme.warning),
    }
}

fn render_rails(area: Rect, buf: &mut Buffer, theme: &Theme, picker: &PickerState) {
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(theme.gray_dim));
    let inner = block.inner(area);
    block.render(area, buf);
    let mut lines: Vec<Line> = Vec::new();
    for (i, rail) in picker.rails.iter().enumerate() {
        let selected = i == picker.rail_selected;
        let marker = if selected { "› " } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(marker, row_style(theme, selected)),
            Span::styled(format!("{:<8}", rail.id.name()), row_style(theme, selected)),
            Span::styled(format!("[{}]", rail.pill.label()), pill_style(theme, rail.pill)),
        ]));
        lines.push(Line::from(Span::styled(
            format!("    {}", rail.empty_copy()),
            Style::default().fg(theme.gray_dim),
        )));
    }
    Paragraph::new(lines).render(inner, buf);
}

fn render_detail(area: Rect, buf: &mut Buffer, theme: &Theme, picker: &PickerState) {
    let inner = Rect {
        x: area.x + 1,
        y: area.y,
        width: area.width.saturating_sub(1),
        height: area.height,
    };
    let mut lines: Vec<Line> = Vec::new();
    let detail = picker.detail_lines();
    let (head, rest) = match detail.split_first() {
        Some((h, r)) => (h.clone(), r),
        None => (String::new(), &[][..]),
    };
    lines.push(Line::from(Span::styled(
        head,
        Style::default()
            .fg(theme.text_primary)
            .add_modifier(Modifier::BOLD),
    )));
    if picker.detail_open || picker.tab == PickerTab::Subscriptions {
        for l in rest {
            let style = if l.starts_with("  ") {
                Style::default().fg(theme.command)
            } else {
                Style::default().fg(theme.text_secondary)
            };
            lines.push(Line::from(Span::styled(l.clone(), style)));
        }
        if picker.tab == PickerTab::Subscriptions
            && picker.selected_rail().is_some_and(|r| r.shows_connect())
        {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "[ Connect ]  runs the official CLI login in your terminal",
                Style::default()
                    .fg(theme.text_primary)
                    .add_modifier(Modifier::BOLD),
            )));
        }
    } else if let Some(card) = picker.selected_card() {
        lines.push(Line::from(Span::styled(
            card.summary,
            Style::default().fg(theme.text_secondary),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Press Enter for setup details.",
            Style::default().fg(theme.gray_bright),
        )));
    }
    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .render(inner, buf);
}
