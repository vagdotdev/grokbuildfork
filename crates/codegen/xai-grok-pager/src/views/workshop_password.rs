//! Workshop: the one masked prompt for a `sudo` password (see `app::workshop_askpass`), drawn in
//! the style of the permission prompt (`views::permission_view`): the raised card background, the
//! accent bar on the left, a bold title, a `❯` input row with the block caret, and the key hints.
//! The typed characters are never drawn; the input shows one dot per character.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::workshop_askpass::PendingPassword;
use crate::theme::Theme;

/// One blank row, the title, the input row, the key line, one blank row.
pub const HEIGHT: u16 = 5;

/// Draw the card across the bottom of `area` (right above the composer).
pub fn render(area: Rect, buf: &mut Buffer, theme: &Theme, ask: &PendingPassword) {
    if area.width < 24 || area.height < HEIGHT {
        return;
    }
    let card = Rect {
        x: area.x,
        y: area.y + area.height - HEIGHT,
        width: area.width,
        height: HEIGHT,
    };
    buf.set_style(card, Style::default().bg(theme.bg_light));
    let accent = Style::default().fg(theme.accent_user);
    for row in card.y..card.y + card.height {
        if let Some(cell) = buf.cell_mut((card.x, row)) {
            cell.set_symbol(crate::glyphs::accent_bar());
            cell.set_style(accent);
        }
    }
    let content_x = card.x + 3;
    let content_width = card.width.saturating_sub(5);

    let title = fit(&ask.title, content_width as usize);
    buf.set_line(
        content_x,
        card.y + 1,
        &Line::from(Span::styled(
            title,
            Style::default()
                .fg(theme.text_primary)
                .add_modifier(Modifier::BOLD),
        )),
        content_width,
    );

    // `❯ ••••••█`: the permission prompt's input row, with dots for the characters.
    buf.set_span(content_x, card.y + 2, &Span::styled("\u{276f} ", accent), 2);
    let window = content_width.saturating_sub(3) as usize;
    let dots = ask.typed_len().min(window);
    let text_style = Style::default().fg(theme.text_primary);
    for col in 0..dots {
        buf.set_span(
            content_x + 2 + col as u16,
            card.y + 2,
            &Span::styled("\u{2022}", text_style),
            1,
        );
    }
    let caret_style = if theme.is_bandless() {
        theme.block_cursor_over(theme.bg_light)
    } else {
        Style::default().fg(theme.bg_light).bg(theme.accent_user)
    };
    if dots < window {
        buf.set_span(
            content_x + 2 + dots as u16,
            card.y + 2,
            &Span::styled(" ", caret_style),
            1,
        );
    }

    let dim = Style::default()
        .fg(theme.text_secondary)
        .add_modifier(Modifier::DIM);
    let sep = Span::styled("  \u{00b7}  ", dim);
    let hints = Line::from(vec![
        Span::styled("Enter", accent),
        Span::styled(" send to sudo", dim),
        sep.clone(),
        Span::styled("Esc", accent),
        Span::styled(" skip", dim),
        sep,
        Span::styled(
            "only sudo sees it \u{2014} never the model, transcript or logs",
            dim,
        ),
    ]);
    buf.set_line(content_x, card.y + 3, &hints, content_width);
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
