//! The logo is hidden entirely on legacy Windows consoles: the ConHost raster fonts do not cover the U+2800 braille block, so it renders as tofu.

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::render::color::blend_color;
use crate::theme::Theme;

/// Height at or above which the small logo is shown (below it, no logo).
const SMALL_LOGO_MIN_HEIGHT: u16 = 22;
/// Height at or above which the full logo is shown.
const FULL_LOGO_MIN_HEIGHT: u16 = 26;
/// Height at or above which the 2x art is shown, when the brand art set carries one (it is 7 rows taller than the full logo).
const LARGE_LOGO_MIN_HEIGHT: u16 = 33;

/// Which logo art the stacked column shows.
/// The terminal height picks the tier; the stacked layout steps it down only while the column would not fit beside the draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogoTier {
    /// The 2x art; only reachable while [`workshop_brand::HeroArt::large`] is set.
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

    fn art(self) -> Option<&'static str> {
        let art = workshop_brand::hero_art();
        match self {
            Self::Large => art.large,
            Self::Full => Some(art.full),
            Self::Compact => Some(art.compact),
            Self::Hidden => None,
        }
    }

    pub fn rows(self) -> u16 {
        self.art().map_or(0, count_lines)
    }

    /// Columns the art spans; 0 when the tier paints nothing.
    pub fn visual_width(self) -> u16 {
        self.art().map_or(0, visual_width)
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

fn large_enabled() -> bool {
    workshop_brand::hero_art().large.is_some()
}

fn pick_logo(window_height: u16) -> Option<&'static str> {
    pick_logo_for(window_height, logo_hidden())
}

fn pick_logo_for(window_height: u16, hidden: bool) -> Option<&'static str> {
    LogoTier::for_height_and_hidden(window_height, hidden, large_enabled()).art()
}

/// The braille art has no ASCII stand-in; see the module doc.
fn logo_hidden() -> bool {
    crate::glyphs::is_legacy_windows_console()
}

fn non_empty_lines(logo: &str) -> impl Iterator<Item = &str> {
    logo.lines().filter(|l| !l.is_empty())
}

fn count_lines(logo: &str) -> u16 {
    non_empty_lines(logo).count() as u16
}

fn visual_width(logo: &str) -> u16 {
    non_empty_lines(logo)
        .map(unicode_width::UnicodeWidthStr::width)
        .max()
        .unwrap_or(24) as u16
}

/// Animation phase in seconds since the first render.
/// The phase is wall-clock based so the shimmer speed is independent of the frame rate.
fn anim_phase_secs() -> f32 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f32()
}

/// Shimmer redraw cadence in frames per second.
/// The sweep is slow, so a few fps looks smooth while sparing the long-lived welcome screen from full-rate repaints.
const SHIMMER_FPS: f32 = 12.0;

/// Quantized shimmer frame for the current wall-clock phase.
/// The welcome screen redraws only when this advances, throttling the animation to ~`SHIMMER_FPS` rather than the full event-loop tick rate.
/// The frame is pinned to 0 when the logo is hidden.
pub fn shimmer_frame() -> u64 {
    if logo_hidden() {
        return 0;
    }
    (anim_phase_secs() * SHIMMER_FPS) as u64
}

/// Per-glyph shine opacity in `[0, 1]` at normalized diagonal position `diag` (0 is bottom-left, 1 is top-right) and animation time `secs`.
/// A raised-cosine band sweeps from bottom-left to top-right and parks off-screen between sweeps; a gentle global pulse breathes underneath it.
/// 0 keeps the resting gray, 1 is full bright.
fn shine_opacity(diag: f32, secs: f32) -> f32 {
    const BAND: f32 = 0.38; // half-width of the shine band; wider means a more gradual falloff
    const CYCLE: f32 = 4.0; // seconds for one sweep plus its rest
    const SWEEP_FRAC: f32 = 0.32; // portion of the cycle spent sweeping (~1.3s glint, rest idles)
    const SHINE: f32 = 0.33; // peak shine strength
    const PULSE: f32 = 0.06; // global breathing amount
    const PULSE_SECS: f32 = 5.0; // breathing period

    let p = (secs % CYCLE) / CYCLE;
    let q = (p / SWEEP_FRAC).min(1.0); // parks the band off-screen during the rest
    let band_pos = -BAND + q * (1.0 + 2.0 * BAND);
    let pulse = PULSE * (0.5 - 0.5 * (std::f32::consts::TAU * secs / PULSE_SECS).cos());

    let d = (diag - band_pos).abs();
    let shine = if d < BAND {
        0.5 * (1.0 + (std::f32::consts::PI * d / BAND).cos())
    } else {
        0.0
    };
    (pulse + SHINE * shine).clamp(0.0, 1.0)
}

fn render_into(area: Rect, buf: &mut Buffer, theme: &Theme, logo: &str) {
    // Light themes paint the dots dark, so flip the portrait to keep it a positive image
    let ink = (!theme.is_dark()).then(|| workshop_brand::invert(logo));
    let logo = ink.as_deref().unwrap_or(logo);
    let lines: Vec<&str> = non_empty_lines(logo).collect();
    let rows = lines.len().max(1) as f32;
    let cols = lines
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(1)
        .max(1) as f32;
    let secs = anim_phase_secs();

    // Blend each glyph from the resting gray toward the bright text color by its shine opacity, so a sheen sweeps across the braille art
    // Adjacent glyphs that land on the same blended color share one Span to hold down the per-frame allocation
    let base = theme.gray;
    let hilite = theme.text_primary;
    let logo_lines: Vec<Line> = lines
        .iter()
        .enumerate()
        .map(|(row, line)| {
            let mut spans: Vec<Span> = Vec::new();
            let mut run = String::new();
            let mut run_color: Option<Color> = None;
            for (col, ch) in line.chars().enumerate() {
                // Sweep along the diagonal from bottom-left to top-right: the coordinate grows as col increases and row decreases
                let diag = (col as f32 + (rows - 1.0 - row as f32)) / (cols + rows);
                let color = blend_color(base, hilite, shine_opacity(diag, secs)).unwrap_or(base);
                if run_color != Some(color) {
                    if let Some(prev) = run_color {
                        spans.push(Span::styled(
                            std::mem::take(&mut run),
                            Style::default().fg(prev),
                        ));
                    }
                    run_color = Some(color);
                }
                run.push(ch);
            }
            if let Some(prev) = run_color {
                spans.push(Span::styled(run, Style::default().fg(prev)));
            }
            Line::from(spans).alignment(Alignment::Center)
        })
        .collect();
    Paragraph::new(logo_lines).render(area, buf);
}

pub fn logo_line_count(window_height: u16) -> u16 {
    pick_logo(window_height).map_or(0, count_lines)
}

pub fn logo_visual_width(window_height: u16) -> u16 {
    pick_logo(window_height).map_or(24, visual_width)
}

pub fn render_logo(area: Rect, buf: &mut Buffer, theme: &Theme, window_height: u16) {
    if let Some(logo) = pick_logo(window_height) {
        render_into(area, buf, theme, logo);
    }
}

/// Paint the tier the layout reserved rows for, so the art can never outgrow its slot.
pub fn render_logo_tier(area: Rect, buf: &mut Buffer, theme: &Theme, tier: LogoTier) {
    if let Some(logo) = tier.art() {
        render_into(area, buf, theme, logo);
    }
}

/// Line count of the small logo used in minimal's committed welcome card (0 on a legacy Windows console, where the braille art is suppressed).
pub fn compact_logo_line_count() -> u16 {
    if logo_hidden() {
        0
    } else {
        LogoTier::Compact.rows()
    }
}

/// Render the small braille logo (centered) into `area` for minimal's welcome card.
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
    }

    // The braille art has no legacy-safe stand-in, so every height tier must collapse to no logo when the legacy-console flag is set
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
        assert!(LogoTier::Full.rows() > LogoTier::Compact.rows());
        assert!(LogoTier::Full.visual_width() > LogoTier::Compact.visual_width());
        assert_eq!(LogoTier::Hidden.rows(), 0);
        assert_eq!(LogoTier::Hidden.visual_width(), 0);
        if let Some(large) = workshop_brand::hero_art().large {
            assert_eq!(LogoTier::Large.rows(), count_lines(large));
            assert!(LogoTier::Large.rows() > LogoTier::Full.rows());
            assert!(LogoTier::Large.visual_width() > LogoTier::Full.visual_width());
        } else {
            assert_eq!(LogoTier::Large.rows(), 0);
        }
    }

    #[test]
    fn hero_tiers_try_the_tallest_art_first_and_end_on_full() {
        let tiers = hero_logo_tiers();
        assert_eq!(tiers.last(), Some(&LogoTier::Full));
        assert!(!tiers.contains(&LogoTier::Compact));
        assert!(!tiers.contains(&LogoTier::Hidden));
        assert_eq!(
            tiers.contains(&LogoTier::Large),
            large_enabled() && !logo_hidden()
        );
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

    #[test]
    fn shine_opacity_stays_in_unit_range() {
        let mut secs = 0.0;
        while secs < 10.0 {
            for i in 0..=20 {
                let diag = i as f32 / 20.0;
                let op = shine_opacity(diag, secs);
                assert!(
                    (0.0..=1.0).contains(&op),
                    "opacity {op} out of range at diag {diag}, secs {secs}"
                );
            }
            secs += 0.13;
        }
    }

    #[test]
    fn shine_band_sweeps_across() {
        // The brightest point along the diagonal advances from left to right as the sweep progresses through its active phase
        let brightest = |secs: f32| -> f32 {
            (0..=100)
                .map(|i| i as f32 / 100.0)
                .max_by(|a, b| {
                    shine_opacity(*a, secs)
                        .partial_cmp(&shine_opacity(*b, secs))
                        .unwrap()
                })
                .unwrap()
        };
        let early = brightest(0.1);
        let mid = brightest(0.4);
        let late = brightest(0.7);
        assert!(early < mid, "early {early} should precede mid {mid}");
        assert!(mid < late, "mid {mid} should precede late {late}");
    }

    #[test]
    fn shine_rests_dim_between_sweeps() {
        // During the rest phase the band is parked off-screen, so an interior glyph falls back to at most the gentle pulse, never full bright
        let op = shine_opacity(0.5, 6.0); // secs % 4.0 = 2.0, past SWEEP_FRAC, in the rest phase
        assert!(op < 0.2, "resting opacity {op} should stay dim");
    }
}
