//! Workshop connection picker overlay (ratatui view over `workshop_auth::PickerState`).
//!
//! `/model` opens the **Models** view: one line per usable model (`name  provider  badge`), the
//! active one marked. `/auth` opens the **Subscriptions** view: the Claude / Codex / Cursor rails
//! with a Detecting / Ready / Sign in pill, then the API-key providers, then the optional xAI card
//! last. Both are one compact bordered box: list, one to three lines about the highlighted row,
//! one key line. `Tab` switches views, `Esc` closes. Nothing here starts a login: outcomes are
//! decided by the picker state in the dispatcher.
//!
//! Reusable: `render` draws the whole overlay into any `Rect`; the state it reads is the pure
//! `PickerState` from `workshop-auth`, so other menus (upstream's `/models`) can host it.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap};
use unicode_width::UnicodeWidthStr;
use workshop_auth::{ModelsLine, ModelsRow, PickerState, PickerTab, Pill, RowKind};

use crate::theme::Theme;

/// Widest the overlay gets; narrower terminals shrink it.
const MAX_WIDTH: u16 = 100;
/// Detail slot: one to three lines about the highlighted row.
const DETAIL_ROWS: u16 = 3;

/// Draw the overlay centered in `area` (inset by `h_margin` on both sides).
pub fn render(area: Rect, buf: &mut Buffer, theme: &Theme, picker: &PickerState, h_margin: u16) {
    let avail = Rect {
        x: area.x + h_margin,
        y: area.y,
        width: area.width.saturating_sub(h_margin * 2),
        height: area.height,
    };
    if avail.width < 30 || avail.height < 8 {
        Paragraph::new("Workshop: terminal too small for /model and /auth")
            .style(Style::default().fg(theme.text_primary))
            .render(avail, buf);
        return;
    }

    let entries = list_lines(theme, picker);
    // Borders (2) + list + separator + detail + key line, capped to the area.
    let wanted =
        2 + entries.len() as u16 + 1 + DETAIL_ROWS + 1 + u16::from(picker.status.is_some());
    let height = wanted.min(avail.height);
    let width = avail.width.min(MAX_WIDTH);
    // Anchored at the top of its area: a list that shrinks while the user types must not jump
    // around the screen the way a centered box would.
    let overlay = Rect {
        x: avail.x + (avail.width - width) / 2,
        y: avail.y,
        width,
        height,
    };
    Clear.render(overlay, buf);

    let other = picker.tab.other().title();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.gray_dim))
        .title(Line::from(vec![
            Span::styled(
                format!(" {} ", picker.tab.title()),
                Style::default()
                    .fg(theme.text_primary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("· Tab: {other} "),
                Style::default().fg(theme.gray_dim),
            ),
        ]));
    let inner = block.inner(overlay);
    block.render(overlay, buf);
    buf.set_style(inner, Style::default().bg(theme.bg_base));
    // One column of air between the border and the text.
    let inner = Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        ..inner
    };

    let status_rows = u16::from(picker.status.is_some());
    let [list_area, sep_area, detail_area, status_area, keys_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(DETAIL_ROWS.min(inner.height.saturating_sub(3))),
        Constraint::Length(status_rows),
        Constraint::Length(1),
    ])
    .areas(inner);

    // List, scrolled so the highlighted line stays visible.
    let selected_line = selected_line_index(picker);
    let visible = list_area.height as usize;
    let scroll = if visible == 0 || selected_line < visible {
        0
    } else {
        (selected_line + 1 - visible) as u16
    };
    Paragraph::new(entries)
        .scroll((scroll, 0))
        .render(list_area, buf);

    let rule: String = "─".repeat(sep_area.width as usize);
    Paragraph::new(Line::from(Span::styled(
        rule,
        Style::default().fg(theme.gray_dim),
    )))
    .render(sep_area, buf);

    let detail: Vec<Line> = picker
        .detail_lines()
        .into_iter()
        .enumerate()
        .map(|(i, l)| {
            let style = if i == 0 || l.starts_with("(•)") || l.starts_with("( )") {
                Style::default().fg(theme.text_primary)
            } else {
                Style::default().fg(theme.text_secondary)
            };
            Line::from(Span::styled(l, style))
        })
        .collect();
    Paragraph::new(detail)
        .wrap(Wrap { trim: false })
        .render(detail_area, buf);

    if let Some(status) = &picker.status {
        Paragraph::new(Line::from(Span::styled(
            status.clone(),
            Style::default().fg(theme.warning),
        )))
        .render(status_area, buf);
    }

    let keys = if picker.key_entry.is_some() {
        "Enter save · Esc cancel".to_owned()
    } else {
        let enter = match picker.tab {
            PickerTab::Models => "Enter select",
            PickerTab::Subscriptions => "Enter connect",
        };
        let refresh = if picker.loading {
            " · loading…"
        } else if picker.refresh_pending {
            " · refreshing lists…"
        } else {
            " · Ctrl+R refresh"
        };
        let esc = if picker.tab == PickerTab::Models && !picker.filter.is_empty() {
            "Esc clear"
        } else {
            "Esc close"
        };
        let show_all = match (picker.tab, picker.hidden_models(), picker.show_all) {
            (PickerTab::Models, hidden, false) if hidden > 0 => {
                format!(" · Ctrl+A show {hidden} hidden")
            }
            (PickerTab::Models, _, true) => " · Ctrl+A hide non-chat".to_owned(),
            _ => String::new(),
        };
        format!("↑↓ · {enter} · Tab {other} · {esc}{refresh}{show_all}")
    };
    Paragraph::new(Line::from(Span::styled(
        keys,
        Style::default().fg(theme.gray_bright),
    )))
    .render(keys_area, buf);
}

/// Index of the highlighted line within [`list_lines`].
fn selected_line_index(picker: &PickerState) -> usize {
    match picker.tab {
        // The search line, then group headers ahead of the selected row.
        PickerTab::Models => {
            let mut rows = 0;
            let mut line = 1;
            for entry in picker.models_lines() {
                match entry {
                    ModelsLine::Header(_) => line += 1,
                    ModelsLine::Row(_) => {
                        if rows == picker.models_selected {
                            return line;
                        }
                        rows += 1;
                        line += 1;
                    }
                }
            }
            line
        }
        // Rails, then a blank spacer line, then the connect rows / xAI card.
        PickerTab::Subscriptions => {
            if picker.rail_selected < picker.rails.len() || picker.auth_rows.is_empty() {
                picker.rail_selected
            } else {
                picker.rail_selected + 1
            }
        }
    }
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

fn badge_style(theme: &Theme, row: &ModelsRow) -> Style {
    match &row.kind {
        RowKind::XaiOptional | RowKind::RailSignIn(_) => Style::default().fg(theme.warning),
        RowKind::ConnectProvider { .. } => Style::default().fg(theme.accent_tool),
        RowKind::Engine(_) => Style::default().fg(theme.accent_success),
        RowKind::RailModel { .. } => Style::default().fg(theme.accent_model),
        RowKind::Catalog { model, locked } => {
            if !*locked && model.is_keyless() {
                Style::default().fg(theme.accent_success)
            } else {
                Style::default().fg(theme.accent_model)
            }
        }
    }
}

fn pill_style(theme: &Theme, pill: Pill) -> Style {
    match pill {
        Pill::Detecting => Style::default().fg(theme.gray_bright),
        Pill::Ready => Style::default().fg(theme.accent_success),
        Pill::SignIn => Style::default().fg(theme.warning),
    }
}

fn pad(s: &str, width: usize) -> String {
    let w = UnicodeWidthStr::width(s);
    if w > width {
        let mut out: String = s.chars().take(width.saturating_sub(1)).collect();
        out.push('…');
        return out;
    }
    format!("{s}{}", " ".repeat(width - w))
}

/// `name  provider  badge` for a model / connect / xAI row.
fn row_line<'a>(
    theme: &Theme,
    row: &ModelsRow,
    selected: bool,
    active: bool,
    name_w: usize,
    prov_w: usize,
) -> Line<'a> {
    let base = row_style(theme, selected);
    let marker = if selected { "› " } else { "  " };
    let mut spans = vec![
        Span::styled(marker.to_owned(), base),
        Span::styled(pad(&row.title(), name_w), base),
        Span::styled(" ".to_owned(), base),
        Span::styled(
            pad(row.provider(), prov_w),
            if selected {
                base
            } else {
                Style::default().fg(theme.text_secondary)
            },
        ),
        Span::styled(" ".to_owned(), base),
        Span::styled(row.short_badge().to_owned(), badge_style(theme, row)),
    ];
    if active {
        spans.push(Span::styled(
            " · active".to_owned(),
            Style::default()
                .fg(theme.accent_success)
                .add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(spans)
}

/// Column widths shared by every row line of a view.
fn columns(rows: &[ModelsRow]) -> (usize, usize) {
    let name_w = rows
        .iter()
        .map(|r| UnicodeWidthStr::width(r.title().as_str()))
        .max()
        .unwrap_or(10)
        .clamp(10, 40);
    let prov_w = rows
        .iter()
        .map(|r| UnicodeWidthStr::width(r.provider()))
        .max()
        .unwrap_or(8)
        .clamp(8, 24);
    (name_w, prov_w)
}

/// The type-to-filter line at the top of the Models view.
fn search_line<'a>(theme: &Theme, picker: &PickerState) -> Line<'a> {
    let mut spans = vec![Span::styled(
        "  \u{2315} ".to_owned(),
        Style::default().fg(theme.gray_bright),
    )];
    if picker.filter.is_empty() {
        spans.push(Span::styled(
            "type to filter".to_owned(),
            Style::default().fg(theme.gray_dim),
        ));
    } else {
        spans.push(Span::styled(
            picker.filter.clone(),
            Style::default()
                .fg(theme.text_primary)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            "\u{258f}".to_owned(),
            Style::default().fg(theme.gray_bright),
        ));
    }
    Line::from(spans)
}

fn header_line<'a>(theme: &Theme, title: &str) -> Line<'a> {
    Line::from(Span::styled(
        format!("  {title}"),
        Style::default()
            .fg(theme.text_secondary)
            .add_modifier(Modifier::BOLD),
    ))
}

fn list_lines<'a>(theme: &Theme, picker: &PickerState) -> Vec<Line<'a>> {
    let mut lines = Vec::new();
    match picker.tab {
        PickerTab::Models => {
            lines.push(search_line(theme, picker));
            let visible = picker.visible_models();
            let (name_w, prov_w) = columns(&picker.rows);
            let mut i = 0;
            for entry in picker.models_lines() {
                match entry {
                    ModelsLine::Header(title) => lines.push(header_line(theme, title)),
                    ModelsLine::Row(row) => {
                        lines.push(row_line(
                            theme,
                            row,
                            i == picker.models_selected,
                            picker.is_active(row),
                            name_w,
                            prov_w,
                        ));
                        i += 1;
                    }
                }
            }
            if picker.rows.is_empty() {
                lines.push(Line::from(Span::styled(
                    "  loading…",
                    Style::default().fg(theme.gray_bright),
                )));
            } else if visible.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!(
                        "  no model matches \u{201c}{}\u{201d}",
                        picker.filter.trim()
                    ),
                    Style::default().fg(theme.gray_bright),
                )));
            }
        }
        PickerTab::Subscriptions => {
            for (i, rail) in picker.rails.iter().enumerate() {
                let selected = i == picker.rail_selected;
                let base = row_style(theme, selected);
                let marker = if selected { "› " } else { "  " };
                let sub = match rail.empty_copy {
                    Some(copy) => copy.to_owned(),
                    None if rail.models.is_empty() => String::new(),
                    None => format!("{} models", rail.models.len()),
                };
                lines.push(Line::from(vec![
                    Span::styled(marker.to_owned(), base),
                    Span::styled(pad(rail.rail.display_name(), 8), base),
                    Span::styled(
                        pad(&format!("[{}]", rail.pill.label()), 12),
                        pill_style(theme, rail.pill),
                    ),
                    Span::styled(sub, Style::default().fg(theme.text_secondary)),
                ]));
            }
            if !picker.auth_rows.is_empty() {
                lines.push(Line::from(""));
                // One vocabulary for every connect row: `Provider — Sign in` / `Provider — API key`.
                let name_w = picker
                    .auth_rows
                    .iter()
                    .map(|r| UnicodeWidthStr::width(r.title().as_str()))
                    .max()
                    .unwrap_or(10)
                    .clamp(10, 48);
                for (i, row) in picker.auth_rows.iter().enumerate() {
                    let selected = picker.rails.len() + i == picker.rail_selected;
                    let base = row_style(theme, selected);
                    let marker = if selected { "› " } else { "  " };
                    lines.push(Line::from(vec![
                        Span::styled(marker.to_owned(), base),
                        Span::styled(pad(&row.title(), name_w), base),
                        Span::styled(" ".to_owned(), base),
                        Span::styled(row.short_badge().to_owned(), badge_style(theme, row)),
                    ]));
                }
            }
        }
    }
    lines
}
