//! Workshop brand overlay for the Grok Build TUI.
//!
//! Holds the welcome-hero mark and the product title so the upstream-owned pager modules only
//! swap a constant or a string for the items exported here.
//!
//! The mark is an ASCII torus that spins on the welcome screen ([`donut`]), with the `v` monogram
//! rising through it and sinking back once a loop ([`hero`]), drawn at the same grid sizes as the
//! upstream Grok logo it replaces (7 x 14 and 5 x 10 cells), so the pager's layout math applies
//! unchanged; the pager maps the luminance ramp onto theme colours.

pub mod donut;
pub mod hero;

/// The one product name, everywhere a user reads it (hero, version line, exit card).
pub const TITLE: &str = "Workshop";

/// Hero title: always [`TITLE`]. One name per product, the same on every launch.
pub fn title() -> &'static str {
    TITLE
}

/// Composer placeholder: an invitation to type, with one concrete example — an everyday task,
/// not a programming exercise (Workshop is for anything a person wants done on their computer).
pub const PROMPT_PLACEHOLDER: &str = "Ask anything\u{2026} \"tidy up my Downloads folder\"";

/// Subtitle under the hero title: the one product name, and where a note goes.
pub fn hero_subtitle() -> String {
    "Thanks for trying Workshop \u{2014} /feedback saves a note.".to_owned()
}

/// Workshop's own release notes, bundled so `/release-notes` works offline and without a CDN.
pub const RELEASE_NOTES: &str = include_str!("../assets/release-notes.md");

/// Public repository: issues and releases.
pub const REPO_URL: &str = "https://github.com/vagdotdev/grokbuildfork";

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

    #[test]
    fn one_product_name_on_every_launch() {
        assert_eq!(title(), "Workshop");
        assert_eq!(title(), TITLE);
        assert!(!TITLE.contains("Vagdev"), "the hero title is the bare name");
    }

    #[test]
    fn subtitle_thanks_the_user_and_points_at_feedback() {
        assert_eq!(
            hero_subtitle(),
            "Thanks for trying Workshop \u{2014} /feedback saves a note."
        );
        assert!(
            !hero_subtitle().contains("Vagdev"),
            "one product name: the subtitle uses the same one as the title"
        );
        assert!(
            !hero_subtitle().to_ascii_lowercase().contains("github"),
            "a note is saved, nothing is drafted anywhere"
        );
    }

    #[test]
    fn placeholder_invites_typing_with_an_everyday_example() {
        assert!(PROMPT_PLACEHOLDER.starts_with("Ask anything"));
        assert!(PROMPT_PLACEHOLDER.contains("tidy up my Downloads folder"));
        assert!(
            !PROMPT_PLACEHOLDER.contains("test"),
            "the example is not a programming exercise"
        );
    }

    #[test]
    fn release_notes_are_bundled_and_de_branded() {
        assert!(RELEASE_NOTES.contains("# Workshop release notes"));
        assert!(RELEASE_NOTES.contains("0.2.2"));
        let lower = RELEASE_NOTES.to_ascii_lowercase();
        assert!(
            !lower.contains("grok build"),
            "release notes name the product"
        );
    }

    #[test]
    fn only_critical_announcements_reach_the_hero() {
        assert!(hero_shows_announcement(Some("critical")));
        assert!(!hero_shows_announcement(Some("promo")));
        assert!(!hero_shows_announcement(Some("info")));
        assert!(!hero_shows_announcement(None));
    }
}
