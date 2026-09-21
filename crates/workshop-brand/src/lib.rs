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

/// Head-and-shoulders bust at the upstream full logo's grid: 7 rows x 14 cols (28 x 28 dots).
pub const BUST_7X14: &str = include_str!("../assets/portrait-7x14.txt");

/// Bust at the upstream small logo's grid: 5 rows x 10 cols (20 x 20 dots).
pub const BUST_5X10: &str = include_str!("../assets/portrait-5x10.txt");

/// 2x bust: 14 rows x 28 cols (56 x 56 dots). Only shown by a `-2x` art set: the hero box grows
/// by 7 rows with it, so the side-by-side layout needs roughly 27 terminal rows.
pub const BUST_14X28: &str = include_str!("../assets/portrait-14x28.txt");

/// Passport-style face crop (eyes, nose, mouth fill the square) on an empty background, 7 x 14.
pub const FACE_7X14: &str = include_str!("../assets/face-7x14.txt");

/// Face crop, 5 x 10.
pub const FACE_5X10: &str = include_str!("../assets/face-5x10.txt");

/// Face crop, 14 x 28.
pub const FACE_14X28: &str = include_str!("../assets/face-14x28.txt");

/// One art family at the welcome logo tiers.
pub struct HeroArt {
    /// 2x tier (14 x 28), tried first when the terminal is tall enough; `None` keeps the upstream tier chain.
    pub large: Option<&'static str>,
    /// Full tier (7 x 14), the upstream hero logo grid.
    pub full: &'static str,
    /// Compact tier (5 x 10), the upstream small logo grid.
    pub compact: &'static str,
}

/// The bust, 1x only (the default).
pub const BUST: HeroArt = HeroArt {
    large: None,
    full: BUST_7X14,
    compact: BUST_5X10,
};

/// The bust with the 2x tier enabled.
pub const BUST_2X: HeroArt = HeroArt {
    large: Some(BUST_14X28),
    ..BUST
};

/// The face crop, 1x only.
pub const FACE: HeroArt = HeroArt {
    large: None,
    full: FACE_7X14,
    compact: FACE_5X10,
};

/// The face crop with the 2x tier enabled.
pub const FACE_2X: HeroArt = HeroArt {
    large: Some(FACE_14X28),
    ..FACE
};

/// Environment variable that picks the art set for a launch: `bust`, `bust-2x`, `face`, `face-2x`.
/// Anything else is the default, [`BUST`].
pub const HERO_ART_ENV: &str = "WORKSHOP_HERO_ART";

/// The art set for this launch, resolved once from [`HERO_ART_ENV`].
pub fn hero_art() -> &'static HeroArt {
    static ART: OnceLock<&'static HeroArt> = OnceLock::new();
    ART.get_or_init(|| hero_art_named(std::env::var(HERO_ART_ENV).ok().as_deref()))
}

fn hero_art_named(name: Option<&str>) -> &'static HeroArt {
    match name.map(str::trim) {
        Some("bust-2x") => &BUST_2X,
        Some("face") => &FACE,
        Some("face-2x") => &FACE_2X,
        _ => &BUST,
    }
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
        for art in [&BUST, &BUST_2X, &FACE, &FACE_2X] {
            assert_grid(art.full, 7, 14);
            assert_grid(art.compact, 5, 10);
            if let Some(large) = art.large {
                assert_grid(large, 14, 28);
            }
        }
    }

    #[test]
    fn art_sets_default_to_the_1x_bust() {
        for name in [None, Some(""), Some("bust"), Some("nonsense")] {
            let art = hero_art_named(name);
            assert!(art.large.is_none(), "{name:?}");
            assert_eq!(art.full, BUST_7X14, "{name:?}");
        }
        let two_x = hero_art_named(Some(" bust-2x "));
        assert_eq!(two_x.large, Some(BUST_14X28));
        assert_eq!(two_x.full, BUST_7X14);
        assert_eq!(two_x.compact, BUST_5X10);
        let face = hero_art_named(Some("face"));
        assert!(face.large.is_none());
        assert_eq!(face.full, FACE_7X14);
        assert_eq!(face.compact, FACE_5X10);
        assert_eq!(hero_art_named(Some("face-2x")).large, Some(FACE_14X28));
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
        assert_eq!(invert(&invert(BUST_7X14)), BUST_7X14);
        assert_grid(&invert(FACE_7X14), 7, 14);
    }
}
