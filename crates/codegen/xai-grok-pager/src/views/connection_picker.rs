//! Workshop connection picker overlay (ratatui view over `workshop_auth::PickerState`).
//!
//! One list behind `/model` and `/auth`: `⌕ type to filter`, quiet group headers (`OpenCode`, a
//! connected provider, `Subscriptions`), one line per row — `name  state` — with the row's state
//! as a coloured suffix (`free`, `sign in`, `install`, `✓ Max ▸`, `optional · sign in`) and the
//! active one marked. A row with `▸` opens a sub-menu (a vendor's models, a model's effort
//! levels, the API-key providers) whose name joins the title: `Models › Claude`. Under the list:
//! a rule, one to three detail lines about the highlighted row, one key line. Nothing here
//! starts a login: outcomes are decided by the picker state in the dispatcher.
//!
//! Reusable: `render` draws the whole overlay into any `Rect`; the state it reads is the pure
//! `PickerState` from `workshop-auth`, so other menus (upstream's `/models`) can host it.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap};
use unicode_width::UnicodeWidthStr;
use workshop_auth::{ModelsLine, ModelsRow, PickerState, Tone};

use crate::theme::Theme;

/// Widest the overlay gets; narrower terminals shrink it.
const MAX_WIDTH: u16 = 100;
/// Detail slot: at most three lines about the highlighted row.
const MAX_DETAIL_ROWS: u16 = 3;

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
    let detail = detail_lines(theme, picker);
    let detail_rows = (detail.len() as u16).clamp(1, MAX_DETAIL_ROWS);
    // Borders (2) + list + separator + detail + key line, capped to the area.
    let wanted =
        2 + entries.len() as u16 + 1 + detail_rows + 1 + u16::from(picker.status.is_some());
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

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.gray_dim))
        .title(Line::from(Span::styled(
            format!(" {} ", picker.title()),
            Style::default()
                .fg(theme.text_primary)
                .add_modifier(Modifier::BOLD),
        )));
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
        Constraint::Length(detail_rows.min(inner.height.saturating_sub(3))),
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

    Paragraph::new(detail)
        .wrap(Wrap { trim: false })
        .render(detail_area, buf);

    if let Some(status) = &picker.status {
        Paragraph::new(Line::from(Span::styled(
            status.clone(),
            Style::default().fg(theme.text_secondary),
        )))
        .render(status_area, buf);
    }

    let keys = if picker.key_entry.is_some() {
        "Enter save · Esc cancel".to_owned()
    } else {
        let enter = format!("Enter {}", picker.enter_verb());
        let back = if !picker.filter.is_empty() {
            "Esc clear"
        } else if picker.submenu.is_some() {
            "← back · Esc back"
        } else {
            "Esc close"
        };
        let refresh = if picker.loading {
            " · loading…"
        } else if picker.refresh_pending {
            " · refreshing lists…"
        } else {
            " · Ctrl+R refresh"
        };
        let show_all = match (picker.submenu.is_some(), picker.hidden_models(), picker.show_all) {
            (false, hidden, false) if hidden > 0 => format!(" · Ctrl+A show {hidden} hidden"),
            (false, _, true) => " · Ctrl+A hide non-chat".to_owned(),
            _ => String::new(),
        };
        format!("↑↓ · {enter} · {back}{refresh}{show_all}")
    };
    Paragraph::new(Line::from(Span::styled(
        keys,
        Style::default().fg(theme.gray_bright),
    )))
    .render(keys_area, buf);
}

/// Index of the highlighted line within [`list_lines`]: the search line, then the headers ahead
/// of the selected row.
fn selected_line_index(picker: &PickerState) -> usize {
    let mut rows = 0;
    let mut line = 1;
    for entry in picker.models_lines() {
        match entry {
            ModelsLine::Header(_) => line += 1,
            ModelsLine::Row(_) => {
                if rows == picker.selected {
                    return line;
                }
                rows += 1;
                line += 1;
            }
        }
    }
    line
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

fn tone_style(theme: &Theme, tone: Tone, text: &str) -> Style {
    match tone {
        Tone::Good if text.contains("active") => Style::default()
            .fg(theme.accent_success)
            .add_modifier(Modifier::BOLD),
        Tone::Good => Style::default().fg(theme.accent_success),
        Tone::Dim => Style::default().fg(theme.gray_bright),
        Tone::Plain => Style::default().fg(theme.text_secondary),
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

/// `name  [provider]  state` for one row.
fn row_line<'a>(
    theme: &Theme,
    picker: &PickerState,
    row: &ModelsRow,
    selected: bool,
    name_w: usize,
    prov_w: Option<usize>,
) -> Line<'a> {
    let base = row_style(theme, selected);
    let marker = if selected { "› " } else { "  " };
    let mut spans = vec![
        Span::styled(marker.to_owned(), base),
        Span::styled(pad(&row.title(), name_w), base),
        Span::styled(" ".to_owned(), base),
    ];
    if let Some(prov_w) = prov_w {
        spans.push(Span::styled(
            pad(row.provider(), prov_w),
            if selected {
                base
            } else {
                Style::default().fg(theme.text_secondary)
            },
        ));
        spans.push(Span::styled(" ".to_owned(), base));
    }
    for (tone, text) in picker.row_suffix(row) {
        let style = tone_style(theme, tone, &text);
        spans.push(Span::styled(text, style));
    }
    Line::from(spans)
}

/// Column widths shared by every row line: the name column, and the provider column in the
/// flat filtered list.
fn columns(picker: &PickerState, rows: &[ModelsRow]) -> (usize, Option<usize>) {
    let name_w = rows
        .iter()
        .map(|r| UnicodeWidthStr::width(r.title().as_str()))
        .max()
        .unwrap_or(10)
        .clamp(10, 40);
    let prov_w = picker.shows_provider_column().then(|| {
        rows.iter()
            .map(|r| UnicodeWidthStr::width(r.provider()))
            .max()
            .unwrap_or(8)
            .clamp(8, 24)
    });
    (name_w, prov_w)
}

/// The type-to-filter line at the top of the list.
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
    let mut lines = vec![search_line(theme, picker)];
    let visible = picker.visible_rows();
    let (name_w, prov_w) = columns(picker, &visible);
    let mut i = 0;
    for entry in picker.models_lines() {
        match entry {
            ModelsLine::Header(title) => lines.push(header_line(theme, &title)),
            ModelsLine::Row(row) => {
                lines.push(row_line(
                    theme,
                    picker,
                    &row,
                    i == picker.selected,
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
        let what = if picker.filter.trim().is_empty() {
            "  nothing to pick here".to_owned()
        } else {
            format!("  no match for \u{201c}{}\u{201d}", picker.filter.trim())
        };
        lines.push(Line::from(Span::styled(
            what,
            Style::default().fg(theme.gray_bright),
        )));
    }
    lines
}

fn detail_lines<'a>(theme: &Theme, picker: &PickerState) -> Vec<Line<'a>> {
    picker
        .detail_lines()
        .into_iter()
        .enumerate()
        .map(|(i, l)| {
            let style = if i == 0 {
                Style::default().fg(theme.text_primary)
            } else {
                Style::default().fg(theme.text_secondary)
            };
            Line::from(Span::styled(l, style))
        })
        .collect()
}
