//! Workshop brand overlay for the Grok Build TUI.
//!
//! Holds the welcome-hero mark (a `v` monogram) and the product title so the upstream-owned pager
//! modules only swap a constant or a string for the items exported here.
//!
//! The mark is braille dot-art (U+2800..U+28FF, blank cells are U+2800) at the same grid sizes as
//! the upstream Grok logo it replaces, so the pager's layout math and theme recoloring apply
//! unchanged: the pager paints every cell in the theme's gray, shimmering toward the text color.
//! The dots are the stroke on every theme polarity. Regenerate the grids with `tools/monogram.py`.

use std::sync::OnceLock;

/// Geometric `v`, uniform stroke and a mitered apex, at the upstream full logo's grid:
/// 7 rows x 14 cols (28 x 28 dots).
pub const SANS_7X14: &str = include_str!("../assets/monogram-sans-7x14.txt");

/// Geometric `v` at the upstream small logo's grid: 5 rows x 10 cols (20 x 20 dots).
pub const SANS_5X10: &str = include_str!("../assets/monogram-sans-5x10.txt");

/// Serif `v`: a thick left downstroke tapering to the apex, a thin right arm, flat serifs; 7 x 14.
pub const SERIF_7X14: &str = include_str!("../assets/monogram-serif-7x14.txt");

/// Serif `v`, 5 x 10.
pub const SERIF_5X10: &str = include_str!("../assets/monogram-serif-5x10.txt");

/// Hairline `v`: a thin uniform stroke with a rounded apex; 7 x 14.
pub const HAIRLINE_7X14: &str = include_str!("../assets/monogram-hairline-7x14.txt");

/// Hairline `v`, 5 x 10.
pub const HAIRLINE_5X10: &str = include_str!("../assets/monogram-hairline-5x10.txt");

/// One art family at the welcome logo tiers.
pub struct HeroArt {
    /// Large tier (2x grid), tried first when the terminal is tall enough; `None` keeps the upstream tier chain.
    pub large: Option<&'static str>,
    /// Shade map for `large` (same grid, digits `0`-`2`); `None` paints every cell in the resting gray.
    pub large_shade: Option<&'static str>,
    /// Full tier (7 x 14), the upstream hero logo grid.
    pub full: &'static str,
    /// Compact tier (5 x 10), the upstream small logo grid.
    pub compact: &'static str,
}

/// The geometric `v`: the default mark.
pub const SANS: HeroArt = HeroArt {
    large: None,
    large_shade: None,
    full: SANS_7X14,
    compact: SANS_5X10,
};

/// The serif `v`.
pub const SERIF: HeroArt = HeroArt {
    full: SERIF_7X14,
    compact: SERIF_5X10,
    ..SANS
};

/// The hairline `v`.
pub const HAIRLINE: HeroArt = HeroArt {
    full: HAIRLINE_7X14,
    compact: HAIRLINE_5X10,
    ..SANS
};

/// Environment variable that picks the mark for a launch: `sans`, `serif`, `hairline`.
/// Anything else is the default, [`SANS`].
pub const HERO_ART_ENV: &str = "WORKSHOP_HERO_ART";

/// The art set for this launch, resolved once from [`HERO_ART_ENV`].
pub fn hero_art() -> &'static HeroArt {
    static ART: OnceLock<&'static HeroArt> = OnceLock::new();
    ART.get_or_init(|| hero_art_named(std::env::var(HERO_ART_ENV).ok().as_deref()))
}

fn hero_art_named(name: Option<&str>) -> &'static HeroArt {
    match name.map(str::trim) {
        Some("serif") => &SERIF,
        Some("hairline") => &HAIRLINE,
        _ => &SANS,
    }
}

/// Shade level of the cell at (`row`, `col`) in a shade map: `0` dark, `1` mid, `2` bright.
/// Missing or malformed cells read as `1`, the resting tone. No shipped mark carries a shade map
/// (every [`HeroArt::large_shade`] is `None`); the pager's tonal renderer keeps calling this.
pub fn shade_level(shade: &str, row: usize, col: usize) -> u8 {
    shade
        .lines()
        .filter(|l| !l.is_empty())
        .nth(row)
        .and_then(|line| line.as_bytes().get(col))
        .filter(|b| (b'0'..=b'2').contains(b))
        .map_or(1, |b| b - b'0')
}

/// The hero titles; one is picked per launch.
pub const TITLES: [&str; 2] = ["Vagdev's Workshop", "Workshop by Vagdev"];

/// Hero title for this launch (random pick from [`TITLES`]), stable across frames.
pub fn title() -> &'static str {
    static TITLE: OnceLock<&'static str> = OnceLock::new();
    TITLE.get_or_init(|| title_for(launch_seed()))
}

fn title_for(seed: u64) -> &'static str {
    TITLES[(seed & 1) as usize]
}

/// Random per process without an extra dependency: `RandomState` is seeded from the OS.
fn launch_seed() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
}

/// Subtitle under the hero title.
pub fn hero_subtitle() -> String {
    format!(
        "Thanks for trying {}, give feedback with /feedback!",
        title()
    )
}

/// Whether a remote announcement may take the welcome hero's info slot.
/// Only critical notices (outages, security) do; promos and product news are upstream marketing.
pub fn hero_shows_announcement(severity: Option<&str>) -> bool {
    severity == Some("critical")
}

/// The art as it is drawn on themes that paint dots dark (light themes).
///
/// The pager calls this for light themes because the photo portrait this monogram replaced used
/// dots for the light areas and had to be flipped there. The monogram's dots are its stroke on
/// either polarity, so the art comes back unchanged.
pub fn invert(art: &str) -> String {
    art.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARKS: [&HeroArt; 3] = [&SANS, &SERIF, &HAIRLINE];

    fn grid(art: &str) -> Vec<&str> {
        art.lines().filter(|l| !l.is_empty()).collect()
    }

    fn assert_grid(art: &str, rows: usize, cols: usize) {
        let lines = grid(art);
        assert_eq!(lines.len(), rows);
        for line in lines {
            assert_eq!(line.chars().count(), cols, "{line:?}");
            assert!(
                line.chars().all(|c| ('\u{2800}'..='\u{28FF}').contains(&c)),
                "non-braille glyph in {line:?}"
            );
        }
    }

    /// Unpack the braille cells into one bool per dot (rows x cols of dots).
    fn dots(art: &str) -> Vec<Vec<bool>> {
        // Braille bit for the dot at (dx, dy) inside a 2 x 4 cell
        const BITS: [[u32; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];
        let mut out = Vec::new();
        for line in grid(art) {
            let cells: Vec<u32> = line.chars().map(|c| u32::from(c) - 0x2800).collect();
            for bits in BITS {
                out.push(
                    cells
                        .iter()
                        .flat_map(|cell| bits.iter().map(move |bit| cell & bit != 0))
                        .collect(),
                );
            }
        }
        out
    }

    /// Leftmost and rightmost inked dot of a row.
    fn span(row: &[bool]) -> Option<(usize, usize)> {
        let first = row.iter().position(|d| *d)?;
        let last = row.iter().rposition(|d| *d)?;
        Some((first, last))
    }

    fn inked_runs(row: &[bool]) -> usize {
        row.iter()
            .zip(std::iter::once(&false).chain(row.iter()))
            .filter(|(now, before)| **now && !**before)
            .count()
    }

    #[test]
    fn monograms_match_the_upstream_logo_grids() {
        for art in MARKS {
            assert_grid(art.full, 7, 14);
            assert_grid(art.compact, 5, 10);
        }
    }

    #[test]
    fn no_mark_carries_a_large_tier_or_shade_map() {
        // The hero box stays at the upstream 7 rows: the pager only reaches its 2x tier through `large`
        for art in MARKS {
            assert!(art.large.is_none());
            assert!(art.large_shade.is_none());
        }
    }

    #[test]
    fn monograms_read_as_a_v() {
        for art in MARKS {
            for (name, tier) in [("full", art.full), ("compact", art.compact)] {
                let rows: Vec<Vec<bool>> = dots(tier)
                    .into_iter()
                    .filter(|r| r.iter().any(|d| *d))
                    .collect();
                let width = rows[0].len();
                assert!(
                    rows.len() >= width * 2 / 3,
                    "{name}: the mark fills the grid's height"
                );
                // Two arms at the top, one apex at the bottom
                assert_eq!(inked_runs(&rows[0]), 2, "{name}: top row {:?}", rows[0]);
                assert_eq!(inked_runs(rows.last().unwrap()), 1, "{name}: bottom row");
                let (top_l, top_r) = span(&rows[0]).unwrap();
                let (apex_l, apex_r) = span(rows.last().unwrap()).unwrap();
                assert!(
                    top_l < width / 4 && top_r >= width * 3 / 4,
                    "{name}: arms reach both sides"
                );
                let mid = width as f32 / 2.0;
                assert!(
                    ((apex_l + apex_r) as f32 / 2.0 - mid).abs() <= 2.0,
                    "{name}: apex is centered"
                );
                // The ink narrows on the way down and never leaves a gap between rows
                let mut prev = usize::MAX;
                for (i, row) in rows.iter().enumerate() {
                    let (l, r) = span(row).expect("no blank row inside the mark");
                    assert!(r - l <= prev, "{name}: row {i} widens");
                    prev = r - l;
                }
            }
        }
    }

    #[test]
    fn shade_level_reads_digits_and_defaults_to_mid() {
        let shade = "012\n201\n";
        assert_eq!(shade_level(shade, 0, 0), 0);
        assert_eq!(shade_level(shade, 0, 2), 2);
        assert_eq!(shade_level(shade, 1, 0), 2);
        assert_eq!(shade_level(shade, 1, 3), 1, "past the row end");
        assert_eq!(shade_level(shade, 5, 0), 1, "past the last row");
        assert_eq!(shade_level("x", 0, 0), 1, "non-digit");
    }

    #[test]
    fn art_sets_default_to_the_sans_monogram() {
        for name in [
            None,
            Some(""),
            Some("sans"),
            Some("bust-2x"),
            Some("nonsense"),
        ] {
            let art = hero_art_named(name);
            assert_eq!(art.full, SANS_7X14, "{name:?}");
            assert_eq!(art.compact, SANS_5X10, "{name:?}");
        }
        let serif = hero_art_named(Some(" serif "));
        assert_eq!(serif.full, SERIF_7X14);
        assert_eq!(serif.compact, SERIF_5X10);
        let hairline = hero_art_named(Some("hairline"));
        assert_eq!(hairline.full, HAIRLINE_7X14);
        assert_eq!(hairline.compact, HAIRLINE_5X10);
        assert!(
            std::ptr::eq(hero_art(), hero_art()),
            "resolved once per launch"
        );
    }

    #[test]
    fn title_alternates_by_seed_and_is_stable_per_launch() {
        assert_eq!(title_for(0), "Vagdev's Workshop");
        assert_eq!(title_for(1), "Workshop by Vagdev");
        assert!(TITLES.contains(&title()));
        assert_eq!(title(), title());
    }

    #[test]
    fn subtitle_carries_the_launch_title() {
        assert!(hero_subtitle().starts_with(&format!("Thanks for trying {}", title())));
    }

    #[test]
    fn only_critical_announcements_reach_the_hero() {
        assert!(hero_shows_announcement(Some("critical")));
        assert!(!hero_shows_announcement(Some("promo")));
        assert!(!hero_shows_announcement(Some("info")));
        assert!(!hero_shows_announcement(None));
    }

    #[test]
    fn invert_keeps_the_monogram_as_drawn() {
        // The stroke is ink on both polarities, so the light theme paints the same dots
        for art in MARKS {
            assert_eq!(invert(art.full), art.full);
            assert_eq!(invert(art.compact), art.compact);
        }
        assert_eq!(invert("a\n"), "a\n");
    }
}
