//! Workshop brand overlay for the Grok Build TUI.
//!
//! Holds the welcome-hero portrait art and the product title so the upstream-owned pager modules
//! only swap a constant or a string for the items exported here.
//!
//! The art is braille dot-art (U+2800..U+28FF, blank cells are U+2800) at the same grid sizes as
//! the upstream Grok logo it replaces, so the pager's layout math and theme recoloring apply
//! unchanged. Dots mark the light areas of the photo; see [`invert`] for light themes.
//! Regenerate the grids with `tools/dotart.py`.

use std::sync::OnceLock;

/// Hero portrait at the upstream full logo's grid: 7 rows x 14 cols (28 x 28 dots).
pub const PORTRAIT: &str = include_str!("../assets/portrait-7x14.txt");

/// Compact portrait at the upstream small logo's grid: 5 rows x 10 cols (20 x 20 dots).
pub const PORTRAIT_COMPACT: &str = include_str!("../assets/portrait-5x10.txt");

/// 2x portrait: 14 rows x 28 cols (56 x 56 dots). Not wired by default: the hero box grows by
/// 7 rows with it, so the side-by-side layout needs a terminal of roughly 27+ rows before
/// the welcome screen falls back to the stacked layout.
pub const PORTRAIT_2X: &str = include_str!("../assets/portrait-14x28.txt");

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

/// Flip every braille dot so the portrait keeps its polarity where the theme paints dots dark
/// (light themes). Non-braille characters pass through unchanged.
pub fn invert(art: &str) -> String {
    art.chars()
        .map(|c| match u32::from(c) {
            v @ 0x2800..=0x28FF => char::from_u32(0x2800 | (0xFF ^ (v & 0xFF))).unwrap_or(c),
            _ => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn portraits_match_the_upstream_logo_grids() {
        assert_grid(PORTRAIT, 7, 14);
        assert_grid(PORTRAIT_COMPACT, 5, 10);
        assert_grid(PORTRAIT_2X, 14, 28);
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
    fn invert_flips_dots_and_round_trips() {
        assert_eq!(
            invert("\u{2800}\u{28FF}\u{2801}"),
            "\u{28FF}\u{2800}\u{28FE}"
        );
        assert_eq!(invert("a\n"), "a\n");
        assert_eq!(invert(&invert(PORTRAIT)), PORTRAIT);
        assert_grid(&invert(PORTRAIT), 7, 14);
    }
}
