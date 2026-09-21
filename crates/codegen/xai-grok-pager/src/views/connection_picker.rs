//! Workshop connection picker (ratatui view over `workshop_auth::PickerState`).
//!
//! Layout follows the Blackpen export: a title, two tabs (Models / Subscriptions), a left list
//! (Models rows grouped by provider, or the Claude / Codex / Cursor rails with a Detecting / Ready /
//! Sign in pill), and a right detail pane (or the model radios of a Ready rail). The optional xAI
//! card is last and never preselected; its pane carries the plan's copy verbatim. Nothing here
//! starts a login: outcomes are decided by the picker state in the dispatcher.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};
use workshop_auth::{ConnectionClass, PickerState, PickerTab, Pill, RowKind};

use crate::theme::Theme;

const LEFT_WIDTH: u16 = 44;

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

    let [title_area, tabs_area, body_area, status_area, footer_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(4),
        Constraint::Length(1),
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
    let mut tab_line = vec![
        tab_span(PickerTab::Models),
        Span::raw("  "),
        tab_span(PickerTab::Subscriptions),
        Span::styled("   Tab switches · r refresh", Style::default().fg(theme.gray_dim)),
    ];
    if picker.loading {
        tab_line.push(Span::styled(
            "   detecting…",
            Style::default().fg(theme.gray_bright),
        ));
    }
    Paragraph::new(Line::from(tab_line)).render(tabs_area, buf);

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

    if let Some(status) = &picker.status {
        Paragraph::new(Line::from(Span::styled(
            status.clone(),
            Style::default().fg(theme.warning),
        )))
        .render(status_area, buf);
    }

    let footer = match picker.tab {
        PickerTab::Models => "↑↓ move · Enter select/connect · Esc back/close · Tab: Subscriptions",
        PickerTab::Subscriptions => "↑↓ move · Enter connect/choose model · Esc back/close · Tab: Models",
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

fn class_style(theme: &Theme, class: ConnectionClass) -> Style {
    match class {
        ConnectionClass::Local => Style::default().fg(theme.accent_success),
        ConnectionClass::DirectApi => Style::default().fg(theme.accent_model),
        ConnectionClass::AgentAdapter => Style::default().fg(theme.accent_tool),
        ConnectionClass::OptionalXai => Style::default().fg(theme.warning),
    }
}

fn render_models_list(area: Rect, buf: &mut Buffer, theme: &Theme, picker: &PickerState) {
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(theme.gray_dim));
    let inner = block.inner(area);
    block.render(area, buf);
    // Keep the selected row visible: one header + one row per entry, scrolled to the cursor.
    let mut lines: Vec<Line> = Vec::new();
    let mut selected_line = 0usize;
    let mut last_group: Option<&str> = None;
    for (i, row) in picker.rows.iter().enumerate() {
        if last_group != Some(row.group.as_str()) {
            lines.push(Line::from(Span::styled(
                row.group.clone(),
                Style::default()
                    .fg(theme.gray_bright)
                    .add_modifier(Modifier::BOLD),
            )));
            last_group = Some(row.group.as_str());
        }
        let selected = i == picker.models_selected;
        if selected {
            selected_line = lines.len();
        }
        let marker = if selected { "› " } else { "  " };
        let mut spans = vec![
            Span::styled(marker, row_style(theme, selected)),
            Span::styled(row.title(), row_style(theme, selected)),
        ];
        let tag = match &row.kind {
            RowKind::Catalog { model, locked } => {
                if *locked {
                    " locked"
                } else if model.is_keyless() {
                    " free · no key"
                } else if model.is_free() {
                    " free"
                } else {
                    ""
                }
            }
            RowKind::ConnectProvider { .. } => " connect",
            RowKind::Engine(m) if m.is_default => " free · default",
            RowKind::Engine(_) => " free",
            RowKind::AddLater => "",
            RowKind::XaiOptional => " optional",
        };
        if !tag.is_empty() {
            spans.push(Span::styled(tag, class_style(theme, row.class)));
        }
        lines.push(Line::from(spans));
    }
    let visible = inner.height as usize;
    let scroll = if visible == 0 || selected_line < visible {
        0
    } else {
        (selected_line + 1 - visible) as u16
    };
    Paragraph::new(lines).scroll((scroll, 0)).render(inner, buf);
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
            Span::styled(
                format!("{:<8}", rail.rail.display_name()),
                row_style(theme, selected),
            ),
            Span::styled(format!("[{}]", rail.pill.label()), pill_style(theme, rail.pill)),
        ]));
        let sub = match rail.empty_copy {
            Some(copy) => copy.to_owned(),
            None => format!("{} models", rail.models.len()),
        };
        lines.push(Line::from(Span::styled(
            format!("    {sub}"),
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
    let show_rest = picker.detail_open
        || picker.key_entry.is_some()
        || picker.tab == PickerTab::Subscriptions
        || picker.selected_row().is_some_and(|r| !matches!(r.kind, RowKind::XaiOptional));
    if show_rest {
        for l in rest {
            let style = if l.starts_with("  ") || l.starts_with("(•)") || l.starts_with("( )") {
                Style::default().fg(theme.command)
            } else {
                Style::default().fg(theme.text_secondary)
            };
            lines.push(Line::from(Span::styled(l.clone(), style)));
        }
        if picker.tab == PickerTab::Subscriptions
            && picker.selected_rail().is_some_and(|r| r.show_connect)
        {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "[ Connect ]  Enter runs the official CLI login in your terminal",
                Style::default()
                    .fg(theme.text_primary)
                    .add_modifier(Modifier::BOLD),
            )));
        }
    } else if let Some(row) = picker.selected_row() {
        lines.push(Line::from(Span::styled(
            row.badge.clone(),
            Style::default().fg(theme.text_secondary),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Press Enter for details.",
            Style::default().fg(theme.gray_bright),
        )));
    }
    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .render(inner, buf);
}
