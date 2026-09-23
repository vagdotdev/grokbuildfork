//! Workshop: the one masked prompt for a `sudo` password (see `app::workshop_askpass`). A small
//! bordered box just above the composer: what needs the password, a masked field, and the two
//! keys. The typed characters are never drawn; the field shows one dot per character.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};

use crate::app::workshop_askpass::PendingPassword;
use crate::theme::Theme;

/// Widest the box gets; narrower terminals shrink it.
const MAX_WIDTH: u16 = 100;
/// Borders (2) + title + field + key line.
pub const HEIGHT: u16 = 5;

/// Draw the box at the bottom of `area` (right above the composer), inset by `h_margin`.
pub fn render(area: Rect, buf: &mut Buffer, theme: &Theme, ask: &PendingPassword, h_margin: u16) {
    let avail = Rect {
        x: area.x + h_margin,
        y: area.y,
        width: area.width.saturating_sub(h_margin * 2),
        height: area.height,
    };
    if avail.width < 24 || avail.height < HEIGHT {
        return;
    }
    let width = avail.width.min(MAX_WIDTH);
    let overlay = Rect {
        x: avail.x + (avail.width - width) / 2,
        y: avail.y + avail.height - HEIGHT,
        width,
        height: HEIGHT,
    };
    Clear.render(overlay, buf);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.warning))
        .title(Span::styled(
            " Password ",
            Style::default()
                .fg(theme.text_primary)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(overlay);
    block.render(overlay, buf);
    buf.set_style(inner, Style::default().bg(theme.bg_base));
    let inner = Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    let title = fit(&ask.title, inner.width as usize);
    let dots: String =
        "\u{2022}".repeat(ask.typed_len().min(inner.width.saturating_sub(2) as usize));
    let lines = vec![
        Line::from(Span::styled(title, Style::default().fg(theme.text_primary))),
        Line::from(vec![
            Span::styled(dots, Style::default().fg(theme.text_primary)),
            Span::styled("\u{2588}", Style::default().fg(theme.warning)),
        ]),
        Line::from(Span::styled(
            "Enter: send to sudo \u{b7} Esc: skip \u{b7} goes only to sudo, never to the model, transcript or logs",
            Style::default().fg(theme.gray_dim),
        )),
    ];
    Paragraph::new(lines).render(inner, buf);
}

/// Cut `text` to `width` columns with an ellipsis, keeping the start (the command's verb).
fn fit(text: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(1);
        if used + w > width.saturating_sub(1) {
            out.push('\u{2026}');
            return out;
        }
        out.push(c);
        used += w;
    }
    out
}
