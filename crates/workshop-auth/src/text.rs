//! Plain-text rendering of the picker for `workshop login` (no TUI) and for logs/tests.

use crate::{ModelsRow, PickerState, PickerTab, RowKind};

/// Render both views as plain text. Used by the `workshop login` CLI and by PTY tests.
pub fn render(state: &PickerState) -> String {
    let mut out = String::new();
    out.push_str("Workshop — connect a model\n");
    out.push_str("Workshop starts on the OpenCode free model and never signs you in anywhere by default.\n\n");

    out.push_str(&format!("[{}]\n", PickerTab::Models.title()));
    let mut i = 0;
    for line in state.models_lines() {
        match line {
            crate::ModelsLine::Header(title) => out.push_str(&format!("  {title}\n")),
            crate::ModelsLine::Row(row) => {
                let marker = if state.tab == PickerTab::Models && i == state.models_selected {
                    "›"
                } else {
                    " "
                };
                let active = if state.is_active(row) {
                    " · active"
                } else {
                    ""
                };
                out.push_str(&format!(
                    "{marker} {:<2} {} · {} · {}{active}\n",
                    i + 1,
                    row.title(),
                    row.provider(),
                    row.short_badge(),
                ));
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
    for (i, row) in state.auth_rows.iter().enumerate() {
        let marker = if state.tab == PickerTab::Subscriptions
            && state.rails.len() + i == state.rail_selected
        {
            "›"
        } else {
            " "
        };
        out.push_str(&format!("{marker} {}\n", row_title(row)));
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
        RowKind::XaiOptional => format!(
            "{} · {} · {}",
            row.title(),
            row.short_badge(),
            crate::XAI_CARD_COPY
        ),
        _ => row.title(),
    }
}

/// One-screen text for the `workshop login` CLI: the picker plus how to act on it.
pub fn cli_login_text(state: &PickerState) -> String {
    let mut out = render(state);
    out.push_str("\nNext steps\n");
    out.push_str("  • Start `workshop` and type: /model switches the model, /auth connects a subscription or API key.\n");
    out.push_str("  • For a subscription, sign in with the official CLI in this terminal (claude auth login / codex login / cursor-agent login).\n");
    out.push_str("  • Optional xAI account login only: `workshop login --xai` (opens auth.x.ai). Not required.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PickerSnapshot, models_rows};

    #[test]
    fn text_picker_shows_views_rails_pills_and_xai_last() {
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
            catalog_status: vec![
                workshop_providers::CatalogStatus::seed(crate::ENGINE_PROVIDER_ID, 1),
                workshop_providers::CatalogStatus::seed("kilo", 6),
            ],
            ..PickerSnapshot::default()
        });
        let t = render(&s);
        assert!(t.contains("[Models]"));
        assert!(t.contains("[Subscriptions]"));
        assert!(t.contains("Big Pickle · OpenCode · free"));
        assert!(
            t.contains(
                "Lists: OpenCode cached list from 2026-09-21 · Kilo Gateway cached list from 2026-09-21"
            ),
            "{t}"
        );
        let claude = t.find("Claude ").unwrap();
        let codex = t.find("Codex ").unwrap();
        let cursor = t.find("Cursor ").unwrap();
        assert!(
            claude < codex && codex < cursor,
            "rail order Claude, Codex, Cursor"
        );
        assert!(t.contains("[Sign in]"));
        let xai = t.find("xAI \u{2014} Sign in · optional").unwrap();
        let openrouter = t.find("OpenRouter \u{2014} Sign in").unwrap();
        assert!(t.contains("OpenAI \u{2014} API key"), "{t}");
        assert!(openrouter < xai, "xAI card is last");
        assert!(t.contains("Not required."));
        assert!(t.contains("Kilo"));
        assert!(!t.contains("Login with grok.com"));
    }
}
