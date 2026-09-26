//! Workshop brand overlay for the Grok Build TUI.
//!
//! Holds the welcome-hero mark and the product title so the upstream-owned pager modules only
//! swap a constant or a string for the items exported here.
//!
//! The mark is an ASCII torus that spins on the welcome screen ([`donut`]), drawn at the same
//! grid sizes as the upstream Grok logo it replaces (7 x 14 and 5 x 10 cells), so the pager's
//! layout math applies unchanged; the pager maps the luminance ramp onto theme colours.

pub mod donut;

/// The one product name, everywhere a user reads it (hero, version line, exit card).
pub const TITLE: &str = "Vagdev's Workshop";

/// Hero title: always [`TITLE`]. One name per product, the same on every launch.
pub fn title() -> &'static str {
    TITLE
}

/// Composer placeholder: an invitation to type, with one concrete example.
pub const PROMPT_PLACEHOLDER: &str = "Ask anything\u{2026} \"add a test for multiply\"";

/// Subtitle under the hero title.
pub fn hero_subtitle() -> String {
    format!(
        "Thanks for trying {} \u{2014} /feedback saves a note and drafts a GitHub issue.",
        title()
    )
}

/// Workshop's own release notes, bundled so `/release-notes` works offline and without a CDN.
pub const RELEASE_NOTES: &str = include_str!("../assets/release-notes.md");

/// Public repository: issues and releases.
pub const REPO_URL: &str = "https://github.com/vagdotdev/grokbuildfork";

/// A prefilled "new issue" link for a feedback note (title and body URL-encoded, capped so the
/// URL stays within what browsers accept).
pub fn feedback_issue_url(text: &str, version: &str) -> String {
    let text = text.trim();
    let title: String = text
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(80)
        .collect();
    // Short enough to stay one readable line in a transcript; the full note is on disk. The
    // version the user ran is the one fact the maintainer always needs.
    let note: String = text.chars().take(600).collect();
    let body = format!("{note}\n\n— Workshop {version}");
    format!(
        "{REPO_URL}/issues/new?title={}&body={}",
        url_encode(&format!("Feedback: {title}")),
        url_encode(&body)
    )
}

/// Percent-encode every byte outside the unreserved set (RFC 3986), so the text survives inside a
/// query string in any browser.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
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

    #[test]
    fn one_product_name_on_every_launch() {
        assert_eq!(title(), "Vagdev's Workshop");
        assert_eq!(title(), TITLE);
        assert!(!TITLE.contains("by Vagdev"), "the alternate name is gone");
    }

    #[test]
    fn subtitle_carries_the_title_and_is_honest_about_feedback() {
        let subtitle = hero_subtitle();
        assert!(subtitle.starts_with(&format!("Thanks for trying {TITLE}")));
        assert!(subtitle.contains("GitHub issue"), "{subtitle}");
    }

    #[test]
    fn placeholder_invites_typing_with_an_example() {
        assert!(PROMPT_PLACEHOLDER.starts_with("Ask anything"));
        assert!(PROMPT_PLACEHOLDER.contains("add a test for multiply"));
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
    fn feedback_issue_url_is_prefilled_and_encoded() {
        let url = feedback_issue_url(
            "Picker closes on q\n\nTyping qwen leaves wen in the prompt.",
            "0.2.2",
        );
        assert!(url.starts_with("https://github.com/vagdotdev/grokbuildfork/issues/new?title="));
        assert!(url.contains("title=Feedback%3A%20Picker%20closes%20on%20q"));
        assert!(url.contains("&body=Picker%20closes%20on%20q%0A%0ATyping"));
        assert!(
            url.ends_with("Workshop%200.2.2"),
            "the version the user ran closes the body: {url}"
        );
        assert!(!url.contains(' ') && !url.contains('\n'));
        let long = "x".repeat(10_000);
        assert!(feedback_issue_url(&long, "0.2.2").len() < 1_000);
    }

    #[test]
    fn only_critical_announcements_reach_the_hero() {
        assert!(hero_shows_announcement(Some("critical")));
        assert!(!hero_shows_announcement(Some("promo")));
        assert!(!hero_shows_announcement(Some("info")));
        assert!(!hero_shows_announcement(None));
    }
}
