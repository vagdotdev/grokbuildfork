//! The welcome logo: Workshop's ASCII donut ([`workshop_brand::donut`]) at the upstream logo grids.
//!
//! The hero box spins it (one precomputed frame per slow tick while the welcome screen is up and
//! focused); every other surface — the stacked narrow layout, the login and consent screens,
//! minimal's welcome card — paints the resting frame. The logo is hidden entirely on legacy
//! Windows consoles, as upstream hides its braille art there.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Color;

use crate::render::color::blend_color;
use crate::theme::Theme;
use workshop_brand::donut::{self, Size};

/// Height at or above which the small logo is shown (below it, no logo).
const SMALL_LOGO_MIN_HEIGHT: u16 = 22;
/// Height at or above which the full logo is shown.
const FULL_LOGO_MIN_HEIGHT: u16 = 26;
/// Height at or above which a 2x art would be shown; the donut ships no 2x tier, so this is never reached.
const LARGE_LOGO_MIN_HEIGHT: u16 = 33;

/// Which logo art the stacked column shows.
/// The terminal height picks the tier; the stacked layout steps it down only while the column would not fit beside the draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogoTier {
    /// A 2x art; the donut has none, so this tier paints nothing and is never chosen.
    Large,
    Full,
    Compact,
    Hidden,
}

impl LogoTier {
    pub fn for_height(window_height: u16) -> Self {
        Self::for_height_and_hidden(window_height, logo_hidden(), large_enabled())
    }

    /// Takes the legacy-console and 2x flags as parameters so tests can drive them directly.
    fn for_height_and_hidden(window_height: u16, hidden: bool, large: bool) -> Self {
        if hidden || window_height < SMALL_LOGO_MIN_HEIGHT {
            Self::Hidden
        } else if window_height < FULL_LOGO_MIN_HEIGHT {
            Self::Compact
        } else if large && window_height >= LARGE_LOGO_MIN_HEIGHT {
            Self::Large
        } else {
            Self::Full
        }
    }

    /// The donut grid this tier paints; `None` paints nothing.
    fn size(self) -> Option<Size> {
        match self {
            Self::Full => Some(Size::Full),
            Self::Compact => Some(Size::Compact),
            Self::Large | Self::Hidden => None,
        }
    }

    pub fn rows(self) -> u16 {
        self.size().map_or(0, |s| s.rows() as u16)
    }

    /// Columns the art spans; 0 when the tier paints nothing.
    pub fn visual_width(self) -> u16 {
        self.size().map_or(0, |s| s.cols() as u16)
    }

    /// The next smaller tier; `None` once hidden.
    pub fn step_down(self) -> Option<Self> {
        match self {
            Self::Large => Some(Self::Full),
            Self::Full => Some(Self::Compact),
            Self::Compact => Some(Self::Hidden),
            Self::Hidden => None,
        }
    }
}

/// Tiers the hero box tries in order, tallest first; each must fit the box before the next is considered.
/// Hidden is not a candidate: on a legacy console the full tier already paints nothing and spans 0 columns.
pub fn hero_logo_tiers() -> &'static [LogoTier] {
    if large_enabled() && !logo_hidden() {
        &[LogoTier::Large, LogoTier::Full]
    } else {
        &[LogoTier::Full]
    }
}

/// The donut ships no 2x art, so the tier chain tops out at the full logo.
fn large_enabled() -> bool {
    false
}

fn pick_logo(window_height: u16) -> Option<Size> {
    pick_logo_for(window_height, logo_hidden())
}

fn pick_logo_for(window_height: u16, hidden: bool) -> Option<Size> {
    LogoTier::for_height_and_hidden(window_height, hidden, large_enabled()).size()
}

/// Upstream hides its logo on legacy Windows consoles; the donut keeps that so the welcome layout stays the same there.
fn logo_hidden() -> bool {
    crate::glyphs::is_legacy_windows_console()
}

/// Whether this terminal gets the spinning hero at all: a logo to spin, and colour on (no
/// `NO_COLOR`, no dumb terminal — those read as a request for a quiet screen, and get the
/// resting frame). The user's `[ui] hero_animation` and the terminal's focus are checked by the
/// app, the layout by the welcome renderer.
pub fn hero_animation_supported() -> bool {
    !logo_hidden() && crate::theme::color_support::detect().has_color()
}

/// How far the shadow shade sinks from the resting gray toward the background, and the lit shade rises toward the text color.
/// Both stay theme-derived so every palette (and polarity) keeps its own contrast.
const SHADE_WEAK_MIX: f32 = 0.5;
const SHADE_STRONG_MIX: f32 = 0.55;

/// Resting colors for three luminance bands: shadow, mid (the plain logo gray), lit.
fn shade_palette(theme: &Theme) -> [Color; 3] {
    let mid = theme.gray;
    let weak = blend_color(mid, theme.bg_base, SHADE_WEAK_MIX).unwrap_or(mid);
    let strong = blend_color(mid, theme.text_primary, SHADE_STRONG_MIX).unwrap_or(mid);
    [weak, mid, strong]
}

/// Theme colour for a ramp level: `.,-` shadow, `~:;` mid gray, `=!*` lit, `#$@` the text colour.
/// The lit side always carries the most contrast against the canvas, on either polarity.
fn band_color(level: u8, [weak, mid, strong]: [Color; 3], hilite: Color) -> Color {
    match level {
        0..=2 => weak,
        3..=5 => mid,
        6..=8 => strong,
        _ => hilite,
    }
}

/// Paint one donut frame with its top-left at the area's top row, centred horizontally.
fn render_into(area: Rect, buf: &mut Buffer, theme: &Theme, frame: &donut::Frame) {
    let palette = shade_palette(theme);
    let hilite = theme.text_primary;
    let size = frame.size();
    let cols = (size.cols() as u16).min(area.width);
    let rows = (size.rows() as u16).min(area.height);
    let x0 = area.x + area.width.saturating_sub(cols) / 2;
    for row in 0..rows {
        for col in 0..cols {
            let Some(cell) = buf.cell_mut(Position::new(x0 + col, area.y + row)) else {
                continue;
            };
            match frame.level(usize::from(row), usize::from(col)) {
                Some(level) => {
                    cell.set_char(frame.glyph(usize::from(row), usize::from(col)))
                        .set_fg(band_color(level, palette, hilite));
                }
                None => {
                    cell.set_char(' ');
                }
            }
        }
    }
}

pub fn logo_line_count(window_height: u16) -> u16 {
    pick_logo(window_height).map_or(0, |s| s.rows() as u16)
}

pub fn logo_visual_width(window_height: u16) -> u16 {
    pick_logo(window_height).map_or(24, |s| s.cols() as u16)
}

/// The resting logo for the terminal height (login and consent screens).
pub fn render_logo(area: Rect, buf: &mut Buffer, theme: &Theme, window_height: u16) {
    render_logo_tier(area, buf, theme, LogoTier::for_height(window_height));
}

/// Paint the tier the layout reserved rows for, resting: the first frame of the loop.
pub fn render_logo_tier(area: Rect, buf: &mut Buffer, theme: &Theme, tier: LogoTier) {
    render_logo_frame(area, buf, theme, tier, 0);
}

/// Paint frame `frame` of the spin in the tier the layout reserved rows for, so the art can never outgrow its slot.
pub fn render_logo_frame(area: Rect, buf: &mut Buffer, theme: &Theme, tier: LogoTier, frame: u32) {
    if let Some(size) = tier.size() {
        render_into(area, buf, theme, donut::frame(size, frame as usize));
    }
}

/// Line count of the small logo used in minimal's committed welcome card (0 on a legacy Windows console, where the logo is suppressed).
pub fn compact_logo_line_count() -> u16 {
    if logo_hidden() {
        0
    } else {
        LogoTier::Compact.rows()
    }
}

/// Render the small logo (centered) into `area` for minimal's welcome card.
/// No-op when the logo is hidden.
pub fn render_compact_logo(area: Rect, buf: &mut Buffer, theme: &Theme) {
    if !logo_hidden() {
        render_logo_tier(area, buf, theme, LogoTier::Compact);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tier_for(window_height: u16, hidden: bool, large: bool) -> LogoTier {
        LogoTier::for_height_and_hidden(window_height, hidden, large)
    }

    #[test]
    fn logo_sizes_by_height() {
        assert_eq!(
            tier_for(SMALL_LOGO_MIN_HEIGHT - 1, false, false),
            LogoTier::Hidden
        );
        assert_eq!(
            tier_for(SMALL_LOGO_MIN_HEIGHT, false, false),
            LogoTier::Compact
        );
        assert_eq!(
            tier_for(FULL_LOGO_MIN_HEIGHT - 1, false, false),
            LogoTier::Compact
        );
        assert_eq!(tier_for(FULL_LOGO_MIN_HEIGHT, false, false), LogoTier::Full);
        // Without a 2x art set the chain tops out at the full logo, however tall the terminal
        assert_eq!(
            tier_for(LARGE_LOGO_MIN_HEIGHT, false, false),
            LogoTier::Full
        );
        assert_eq!(tier_for(u16::MAX, false, false), LogoTier::Full);
    }

    #[test]
    fn large_tier_needs_the_2x_art_and_the_height() {
        assert_eq!(
            tier_for(LARGE_LOGO_MIN_HEIGHT - 1, false, true),
            LogoTier::Full
        );
        assert_eq!(
            tier_for(LARGE_LOGO_MIN_HEIGHT, false, true),
            LogoTier::Large
        );
        // Stepping down walks the whole chain, so an overflowing column lands on the same tiers as before
        assert_eq!(LogoTier::Large.step_down(), Some(LogoTier::Full));
        assert_eq!(LogoTier::Full.step_down(), Some(LogoTier::Compact));
        assert_eq!(LogoTier::Compact.step_down(), Some(LogoTier::Hidden));
        assert_eq!(LogoTier::Hidden.step_down(), None);
        // The donut has no 2x art: the tier paints nothing and the hero chain never offers it
        assert_eq!(LogoTier::Large.rows(), 0);
        assert_eq!(LogoTier::Large.visual_width(), 0);
        assert!(!large_enabled());
    }

    // Every height tier must collapse to no logo when the legacy-console flag is set
    #[test]
    fn logo_hidden_on_legacy_console_at_every_height() {
        for h in [
            0,
            SMALL_LOGO_MIN_HEIGHT,
            FULL_LOGO_MIN_HEIGHT,
            LARGE_LOGO_MIN_HEIGHT,
            u16::MAX,
        ] {
            assert_eq!(tier_for(h, true, true), LogoTier::Hidden, "height {h}");
            assert!(pick_logo_for(h, true).is_none(), "height {h}");
        }
    }

    #[test]
    fn tiers_shrink_down_the_chain() {
        // The hero box lays the art beside the menu, so each tier must be strictly smaller than the one above it in both axes
        if logo_hidden() {
            return;
        }
        assert_eq!(LogoTier::Full.rows(), 7);
        assert_eq!(LogoTier::Full.visual_width(), 14);
        assert_eq!(LogoTier::Compact.rows(), 5);
        assert_eq!(LogoTier::Compact.visual_width(), 10);
        assert_eq!(LogoTier::Hidden.rows(), 0);
        assert_eq!(LogoTier::Hidden.visual_width(), 0);
    }

    #[test]
    fn hero_tiers_try_the_tallest_art_first_and_end_on_full() {
        let tiers = hero_logo_tiers();
        assert_eq!(tiers, &[LogoTier::Full]);
    }

    #[test]
    fn compact_logo_line_count_matches_small_logo_when_visible() {
        // The minimal welcome card budgets exactly the small logo's rows
        if !logo_hidden() {
            assert_eq!(compact_logo_line_count(), LogoTier::Compact.rows());
            assert!(compact_logo_line_count() < LogoTier::Full.rows());
            assert!(compact_logo_line_count() > 0);
        } else {
            assert_eq!(compact_logo_line_count(), 0);
        }
    }

    fn luminance(color: Color) -> f32 {
        match color {
            Color::Rgb(r, g, b) => 0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32,
            other => panic!("expected an RGB theme color, got {other:?}"),
        }
    }

    /// The unquantized palettes of both polarities and the default theme, so the assertions see RGB regardless of `NO_COLOR` or the terminal under test.
    fn palettes() -> [crate::theme::Theme; 3] {
        [
            crate::theme::Theme::oscura_midnight(),
            crate::theme::Theme::groknight(),
            crate::theme::Theme::grokday(),
        ]
    }

    #[test]
    fn shade_palette_steps_from_the_background_toward_the_text() {
        // The three resting shades must be ordered by contrast against the canvas on both polarities, or tone shading would invert
        for theme in palettes() {
            let [weak, mid, strong] = shade_palette(&theme);
            assert_eq!(mid, theme.gray);
            let bg = luminance(theme.bg_base);
            let (w, m, s) = (luminance(weak), luminance(mid), luminance(strong));
            assert!(
                (w - bg).abs() < (m - bg).abs() && (m - bg).abs() < (s - bg).abs(),
                "weak {w} mid {m} strong {s} against bg {bg}"
            );
        }
    }

    #[test]
    fn ramp_bands_map_onto_four_theme_colours() {
        for theme in palettes() {
            let palette = shade_palette(&theme);
            let hilite = theme.text_primary;
            let bands: Vec<Color> = (0..12u8)
                .map(|level| band_color(level, palette, hilite))
                .collect();
            assert_eq!(bands.first(), Some(&palette[0]));
            assert_eq!(bands.get(3), Some(&palette[1]));
            assert_eq!(bands.get(6), Some(&palette[2]));
            assert_eq!(bands.last(), Some(&hilite));
            // Brighter ramp levels never lose contrast against the canvas
            let bg = luminance(theme.bg_base);
            let contrast: Vec<f32> = bands.iter().map(|c| (luminance(*c) - bg).abs()).collect();
            assert!(
                contrast.windows(2).all(|w| w[0] <= w[1]),
                "{contrast:?} on {:?}",
                theme.bg_base
            );
        }
    }

    #[test]
    fn frame_zero_is_painted_in_theme_colours_and_frames_differ() {
        let theme = crate::theme::Theme::oscura_midnight();
        let area = Rect::new(0, 0, 14, 7);
        let mut resting = Buffer::empty(area);
        render_logo_tier(area, &mut resting, &theme, LogoTier::Full);
        let frame0 = donut::frame(Size::Full, 0);
        let palette = shade_palette(&theme);
        for row in 0..7u16 {
            for col in 0..14u16 {
                let cell = resting.cell((col, row)).unwrap();
                let level = frame0.level(usize::from(row), usize::from(col));
                assert_eq!(
                    cell.symbol(),
                    frame0.glyph(usize::from(row), usize::from(col)).to_string(),
                    "glyph at {row},{col}"
                );
                if let Some(level) = level {
                    assert_eq!(cell.fg, band_color(level, palette, theme.text_primary));
                }
            }
        }
        // The lit top of the torus is in the text colour; the row under it is the resting gray
        assert_eq!(resting.cell((5, 0)).unwrap().fg, theme.text_primary);
        assert_eq!(resting.cell((5, 1)).unwrap().symbol(), "!");
        assert_eq!(resting.cell((5, 1)).unwrap().fg, palette[2]);

        let mut spun = Buffer::empty(area);
        render_logo_frame(area, &mut spun, &theme, LogoTier::Full, 40);
        assert_ne!(resting, spun, "a later frame paints different cells");

        // The hidden tier and the 2x tier leave the buffer untouched
        let mut blank = Buffer::empty(area);
        render_logo_frame(area, &mut blank, &theme, LogoTier::Hidden, 3);
        render_logo_frame(area, &mut blank, &theme, LogoTier::Large, 3);
        assert_eq!(blank, Buffer::empty(area));
    }

    #[test]
    fn the_logo_is_centred_in_a_wider_area_and_clipped_to_a_smaller_one() {
        let theme = crate::theme::Theme::groknight();
        let wide = Rect::new(0, 0, 40, 7);
        let mut buf = Buffer::empty(wide);
        render_logo(wide, &mut buf, &theme, FULL_LOGO_MIN_HEIGHT);
        let frame0 = donut::frame(Size::Full, 0);
        // (40 - 14) / 2 = 13 columns of margin on the left
        assert_eq!(buf.cell((13 + 5, 0)).unwrap().symbol(), "$");
        assert_eq!(buf.cell((13 + 5, 1)).unwrap().symbol(), "!");
        assert_eq!(buf.cell((0, 0)).unwrap().symbol(), " ");
        assert_eq!(frame0.glyph(1, 5), '!');

        let small = Rect::new(0, 0, 6, 3);
        let mut clipped = Buffer::empty(small);
        render_logo_tier(small, &mut clipped, &theme, LogoTier::Full);
        assert_eq!(clipped.cell((5, 0)).unwrap().symbol(), "$");
    }
}
