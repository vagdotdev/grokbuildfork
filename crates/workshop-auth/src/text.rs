//! Plain-text rendering of the picker for `workshop login` (no TUI) and for logs/tests.

use crate::{ModelsLine, ModelsRow, PickerState, RowKind};

/// Render the list as plain text: the same rows and state suffixes as the overlay, a signed-in
/// vendor's models indented under its row. Used by the `workshop login` CLI and by PTY tests.
pub fn render(state: &PickerState) -> String {
    let mut out = String::new();
    out.push_str("Workshop — models and subscriptions\n");
    out.push_str("Workshop starts on the OpenCode free model and never signs you in anywhere by default.\n\n");

    out.push_str(&format!("[{}]\n", state.title()));
    let mut i = 0;
    for line in state.models_lines() {
        match line {
            ModelsLine::Header(title) => out.push_str(&format!("  {title}\n")),
            ModelsLine::Row(row) => {
                let marker = if state.submenu.is_none() && i == state.selected {
                    "›"
                } else {
                    " "
                };
                out.push_str(&format!(
                    "{marker} {:<2} {}\n",
                    i + 1,
                    row_text(state, &row)
                ));
                if let RowKind::Vendor(rail) = row.kind {
                    for model in state
                        .rails
                        .iter()
                        .filter(|r| r.rail == rail && r.is_ready())
                        .flat_map(|r| r.models.iter())
                    {
                        out.push_str(&format!("       {}\n", model.display()));
                    }
                }
                i += 1;
            }
        }
    }
    let hidden = state.hidden_models();
    if hidden > 0 {
        out.push_str(&format!(
            "  ({hidden} non-chat models hidden: classifiers, routers; Ctrl+A in the app shows them)\n"
        ));
    }
    if let Some(summary) = state.catalog_summary() {
        out.push_str(&format!("  {summary}\n"));
    }
    if state.refresh_pending {
        out.push_str("  (refreshing the lists…)\n");
    }

    out.push('\n');
    for line in state.detail_lines() {
        out.push_str(&line);
        out.push('\n');
    }
    if let Some(status) = &state.status {
        out.push_str(&format!("\n{status}\n"));
    }
    out
}

/// `Big Pickle · free · active`, `Claude · ✓ Max ▸`, `xAI · optional · sign in · Uses xAI…`.
fn row_text(state: &PickerState, row: &ModelsRow) -> String {
    let suffix: String = state.row_suffix(row).into_iter().map(|(_, s)| s).collect();
    let mut text = row.title();
    if state.shows_provider_column() && !row.provider().is_empty() {
        text.push_str(&format!(" \u{b7} {}", row.provider()));
    }
    if !suffix.is_empty() {
        text.push_str(&format!(" \u{b7} {suffix}"));
    }
    if row.is_xai() {
        text.push_str(&format!(" \u{b7} {}", crate::XAI_CARD_COPY));
    }
    text
}

/// One-screen text for the `workshop login` CLI: the picker plus how to act on it.
pub fn cli_login_text(state: &PickerState) -> String {
    let mut out = render(state);
    out.push_str("\nNext steps\n");
    out.push_str("  • Start `workshop` and type: /model picks a model or a subscription, /auth jumps to the subscriptions.\n");
    out.push_str("  • For a subscription, sign in with the official CLI in this terminal (claude auth login / codex login / cursor-agent login).\n");
    out.push_str("  • Optional xAI account login only: `workshop login --xai` (opens auth.x.ai). Not required.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PickerSnapshot, models_rows};

    #[test]
    fn text_picker_shows_one_list_with_vendor_states_and_xai_last() {
        let mut s = PickerState::new();
        let rows = models_rows(&workshop_providers::Catalog::builtin(), |_| false, &[], &[]);
        let rails = workshop_detect::Rail::ALL
            .iter()
            .map(|r| {
                let mut st = workshop_detect::RailState::detecting(*r);
                st.installed = true;
                st.pill = workshop_detect::Pill::SignIn;
                st
            })
            .collect();
        s.apply_snapshot(PickerSnapshot {
            rows,
            rails,
            catalog_status: vec![
                workshop_providers::CatalogStatus::seed(crate::ENGINE_PROVIDER_ID, 1),
                workshop_providers::CatalogStatus::seed("kilo", 6),
            ],
            ..PickerSnapshot::default()
        });
        let t = render(&s);
        assert!(t.contains("[Models]"), "{t}");
        assert!(t.contains("  Subscriptions\n"), "{t}");
        assert!(t.contains("Big Pickle · free"), "{t}");
        assert!(
            t.contains("Lists: OpenCode cached list from 2026-09-21"),
            "{t}"
        );
        assert!(!t.contains("Kilo"), "Kilo is never named:\n{t}");
        let claude = t.find("Claude · sign in").unwrap();
        let codex = t.find("Codex · sign in").unwrap();
        let cursor = t.find("Cursor · sign in").unwrap();
        assert!(
            claude < codex && codex < cursor,
            "vendor order Claude, Codex, Cursor"
        );
        for pill in ["[Sign in]", "[Install]", "[Ready]", "[Detecting]"] {
            assert!(!t.contains(pill), "no pills: {t}");
        }
        let xai = t.find("xAI · optional · sign in").unwrap();
        let api_keys = t.find("API keys · ▸").unwrap();
        assert!(api_keys < xai, "xAI row is last");
        assert!(t.contains("Not required."));
        assert!(!t.contains("Login with grok.com"));
    }
}
