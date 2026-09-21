//! Plain-text rendering of the picker for `workshop login` (no TUI) and for logs/tests.

use crate::{PickerState, PickerTab, XAI_CARD_ID};

/// Render both tabs as plain text. Used by the `workshop login` CLI and by PTY tests.
pub fn render(state: &PickerState) -> String {
    let mut out = String::new();
    out.push_str("Workshop — connect a model\n");
    out.push_str("Workshop never signs you in anywhere by default. Pick how it should reach a model.\n\n");

    out.push_str(&format!("[{}]\n", PickerTab::Models.title()));
    for (i, card) in state.models.iter().enumerate() {
        let marker = if state.tab == PickerTab::Models && i == state.models_selected {
            "›"
        } else {
            " "
        };
        let avail = card
            .availability()
            .map(|a| format!("  [{a}]"))
            .unwrap_or_default();
        let label = if card.id == XAI_CARD_ID {
            format!("{} — {}", card.title, card.summary)
        } else {
            card.title.to_owned()
        };
        out.push_str(&format!(
            "{marker} {:<2} {label}  ({}){avail}\n",
            i + 1,
            card.class.label()
        ));
        if card.id != XAI_CARD_ID {
            out.push_str(&format!("      {}\n", card.summary));
        }
    }

    out.push_str(&format!("\n[{}]\n", PickerTab::Subscriptions.title()));
    for (i, rail) in state.rails.iter().enumerate() {
        let marker = if state.tab == PickerTab::Subscriptions && i == state.rail_selected {
            "›"
        } else {
            " "
        };
        out.push_str(&format!(
            "{marker} {:<7} [{}]  {}\n",
            rail.id.name(),
            rail.pill.label(),
            rail.empty_copy()
        ));
    }

    out.push('\n');
    for line in state.detail_lines() {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// One-screen text for the `workshop login` CLI: the picker plus how to act on it.
pub fn cli_login_text(state: &PickerState) -> String {
    let mut out = render(state);
    out.push_str("\nNext steps\n");
    out.push_str("  • Edit the config file shown above for a Local or Direct API connection, then start `workshop`.\n");
    out.push_str("  • For a subscription, sign in with the official CLI in this terminal (claude auth login / codex login / cursor-agent login).\n");
    out.push_str("  • Optional xAI account login only: `workshop login --xai` (opens auth.x.ai). Not required.\n");
    out.push_str("  • In the TUI, /auth or /models opens this picker.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_picker_shows_tabs_rails_pills_and_xai_last() {
        let mut s = PickerState::new();
        s.run_detection();
        let t = render(&s);
        assert!(t.contains("[Models]"));
        assert!(t.contains("[Subscriptions]"));
        let claude = t.find("Claude ").unwrap();
        let codex = t.find("Codex ").unwrap();
        let cursor = t.find("Cursor ").unwrap();
        assert!(claude < codex && codex < cursor, "rail order Claude, Codex, Cursor");
        assert!(t.contains("[Sign in]"));
        let xai = t.find("xAI (optional)").unwrap();
        let add_later = t.find("Add a connection later").unwrap();
        assert!(add_later < xai, "xAI card is last");
        assert!(t.contains("Not required."));
        assert!(!t.contains("Login with grok.com"));
    }
}
