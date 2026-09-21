//! Plain-text rendering of the picker for `workshop login` (no TUI) and for logs/tests.

use crate::{ModelsRow, PickerState, PickerTab, RowKind};

/// Render both tabs as plain text. Used by the `workshop login` CLI and by PTY tests.
pub fn render(state: &PickerState) -> String {
    let mut out = String::new();
    out.push_str("Workshop — connect a model\n");
    out.push_str(
        "Workshop never signs you in anywhere by default. Pick how it should reach a model.\n\n",
    );

    out.push_str(&format!("[{}]\n", PickerTab::Models.title()));
    let mut last_group: Option<&str> = None;
    for (i, row) in state.rows.iter().enumerate() {
        if last_group != Some(row.group.as_str()) {
            out.push_str(&format!("  {}  — {}\n", row.group, row.badge));
            last_group = Some(row.group.as_str());
        }
        let marker = if state.tab == PickerTab::Models && i == state.models_selected {
            "›"
        } else {
            " "
        };
        out.push_str(&format!(
            "{marker} {:<2} {}  ({})\n",
            i + 1,
            row_title(row),
            row.class.label()
        ));
    }

    out.push_str(&format!("\n[{}]\n", PickerTab::Subscriptions.title()));
    for (i, rail) in state.rails.iter().enumerate() {
        let marker = if state.tab == PickerTab::Subscriptions && i == state.rail_selected {
            "›"
        } else {
            " "
        };
        let copy = rail
            .empty_copy
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{} models", rail.models.len()));
        out.push_str(&format!(
            "{marker} {:<7} [{}]  {}\n",
            rail.rail.display_name(),
            rail.pill.label(),
            copy
        ));
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

fn row_title(row: &ModelsRow) -> String {
    match &row.kind {
        RowKind::XaiOptional => format!("xAI (optional) — {}", crate::XAI_CARD_COPY),
        _ => row.title(),
    }
}

/// One-screen text for the `workshop login` CLI: the picker plus how to act on it.
pub fn cli_login_text(state: &PickerState) -> String {
    let mut out = render(state);
    out.push_str("\nNext steps\n");
    out.push_str("  • Start `workshop` and press l (or type /auth): Enter on a row writes it to config.toml and makes it the active model.\n");
    out.push_str("  • For a subscription, sign in with the official CLI in this terminal (claude auth login / codex login / cursor-agent login).\n");
    out.push_str("  • Optional xAI account login only: `workshop login --xai` (opens auth.x.ai). Not required.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PickerSnapshot, models_rows};

    #[test]
    fn text_picker_shows_tabs_rails_pills_and_xai_last() {
        let mut s = PickerState::new();
        let rows = models_rows(&workshop_providers::Catalog::builtin(), |_| false, &[]);
        let rails = workshop_detect::Rail::ALL
            .iter()
            .map(|r| {
                let mut st = workshop_detect::RailState::detecting(*r);
                st.pill = workshop_detect::Pill::SignIn;
                st
            })
            .collect();
        s.apply_snapshot(PickerSnapshot {
            rows,
            rails,
            default_selection: None,
            secret_backend: None,
        });
        let t = render(&s);
        assert!(t.contains("[Models]"));
        assert!(t.contains("[Subscriptions]"));
        let claude = t.find("Claude ").unwrap();
        let codex = t.find("Codex ").unwrap();
        let cursor = t.find("Cursor ").unwrap();
        assert!(
            claude < codex && codex < cursor,
            "rail order Claude, Codex, Cursor"
        );
        assert!(t.contains("[Sign in]"));
        let xai = t.find("xAI (optional)").unwrap();
        let add_later = t.find("Add a connection later").unwrap();
        assert!(add_later < xai, "xAI card is last");
        assert!(t.contains("Not required."));
        assert!(t.contains("Kilo"));
        assert!(!t.contains("Login with grok.com"));
    }
}
