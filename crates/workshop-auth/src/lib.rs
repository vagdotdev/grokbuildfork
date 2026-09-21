//! Workshop connection picker: the only default auth surface.
//!
//! Login, the welcome `l` key, `/login`, `/auth`, `/models`, first run and `workshop login` all open
//! this picker. It never starts an OAuth flow by itself. The optional xAI card is the single entry
//! point to the inherited xAI OIDC flow, and only after the user selects it.
//!
//! Layout follows the Blackpen export (docs/workshop-production-plan.md section 12): two tabs,
//! **Models** and **Subscriptions**; Subscriptions is a left rail **Claude, Codex, Cursor** in that
//! order, each with one pill (Detecting / Ready / Sign in); the optional xAI card is the *last*
//! Models card and is never preselected.
//!
//! What this crate does **not** do (gate:no-theft): read any keychain item, any `auth.json`, any
//! Cursor SDK auth file, or spawn a vendor CLI. Detection is presence-only on `PATH` and a fixed
//! list of well-known install directories.

#![deny(clippy::indexing_slicing)]

pub mod detect;
pub mod text;

use std::path::PathBuf;

/// Connection class, always shown to the user (never present a subscription as an API base URL).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionClass {
    /// Workshop owns prompt, tools, loop and HTTP; credential is an API key issued for API use.
    DirectApi,
    /// Same, against a loopback OpenAI-compatible server.
    Local,
    /// Workshop supervises an official vendor CLI; the CLI owns its own login.
    AgentAdapter,
    /// The inherited xAI account login. Optional, labeled, last.
    OptionalXai,
}

impl ConnectionClass {
    pub fn label(self) -> &'static str {
        match self {
            Self::DirectApi => "Direct API",
            Self::Local => "Local",
            Self::AgentAdapter => "Agent adapter",
            Self::OptionalXai => "xAI account (optional)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerTab {
    Models,
    Subscriptions,
}

impl PickerTab {
    pub fn title(self) -> &'static str {
        match self {
            Self::Models => "Models",
            Self::Subscriptions => "Subscriptions",
        }
    }
    pub fn other(self) -> Self {
        match self {
            Self::Models => Self::Subscriptions,
            Self::Subscriptions => Self::Models,
        }
    }
}

/// The three subscription rails, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RailId {
    Claude,
    Codex,
    Cursor,
}

impl RailId {
    pub const ALL: [RailId; 3] = [RailId::Claude, RailId::Codex, RailId::Cursor];

    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::Cursor => "Cursor",
        }
    }
    /// The official CLI binary. A bare `agent` on `PATH` is **not** Cursor.
    pub fn binary(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Cursor => "cursor-agent",
        }
    }
    /// Official login command, run in the user's own terminal. Workshop never captures its output.
    pub fn login_command(self) -> &'static str {
        match self {
            Self::Claude => "claude auth login",
            Self::Codex => "codex login",
            Self::Cursor => "cursor-agent login",
        }
    }
    pub fn install_hint(self) -> &'static str {
        match self {
            Self::Claude => "Install Claude Code, then sign in",
            Self::Codex => "Install Codex, then sign in",
            Self::Cursor => "Install the Cursor Agent CLI (cursor-agent), then sign in",
        }
    }
}

/// Status pill on a subscription rail (Blackpen copy).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pill {
    Detecting,
    Ready,
    SignIn,
}

impl Pill {
    pub fn label(self) -> &'static str {
        match self {
            Self::Detecting => "Detecting",
            Self::Ready => "Ready",
            Self::SignIn => "Sign in",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionRail {
    pub id: RailId,
    pub pill: Pill,
    /// `None` until detection ran.
    pub installed: Option<bool>,
    /// Where the binary was found (display only).
    pub binary_path: Option<PathBuf>,
}

impl SubscriptionRail {
    fn new(id: RailId) -> Self {
        Self {
            id,
            pill: Pill::Detecting,
            installed: None,
            binary_path: None,
        }
    }
    /// Empty-state copy for the right pane.
    pub fn empty_copy(&self) -> String {
        match self.installed {
            None => "Detecting…".to_owned(),
            Some(false) => self.id.install_hint().to_owned(),
            Some(true) => format!("Sign in to see {} models", self.id.name()),
        }
    }
    /// Connect shows on a signed-out rail.
    pub fn shows_connect(&self) -> bool {
        self.pill != Pill::Ready
    }
}

/// One Models-tab card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelsCard {
    pub id: &'static str,
    pub title: &'static str,
    pub class: ConnectionClass,
    pub summary: &'static str,
    /// Env var whose *presence* is shown as `available`; the value is never read into the picker.
    pub env_key: Option<&'static str>,
    pub env_key_present: bool,
}

impl ModelsCard {
    pub fn availability(&self) -> Option<&'static str> {
        match (self.env_key, self.env_key_present) {
            (Some(_), true) => Some("available"),
            _ => None,
        }
    }
}

pub const XAI_CARD_ID: &str = "xai_optional";
pub const ADD_LATER_CARD_ID: &str = "add_later";
/// Copy on the optional xAI card, verbatim from the plan.
pub const XAI_CARD_COPY: &str = "Uses xAI accounts and auth.x.ai. Not required.";

fn env_present(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty())
}

/// The Models tab, in display order. xAI (optional) is always last.
pub fn models_cards() -> Vec<ModelsCard> {
    let card = |id, title, class, summary, env_key: Option<&'static str>| ModelsCard {
        id,
        title,
        class,
        summary,
        env_key,
        env_key_present: env_key.is_some_and(env_present),
    };
    vec![
        card(
            "local",
            "Local model",
            ConnectionClass::Local,
            "Ollama, LM Studio, llama.cpp or vLLM on this machine (OpenAI-compatible, loopback only)",
            None,
        ),
        card(
            "openai",
            "OpenAI API",
            ConnectionClass::DirectApi,
            "Your own OpenAI API key (Responses / Chat Completions)",
            Some("OPENAI_API_KEY"),
        ),
        card(
            "anthropic",
            "Anthropic API",
            ConnectionClass::DirectApi,
            "Your own Anthropic Console API key (Messages API)",
            Some("ANTHROPIC_API_KEY"),
        ),
        card(
            "openrouter",
            "OpenRouter",
            ConnectionClass::DirectApi,
            "One key, many models (OpenAI-compatible)",
            Some("OPENROUTER_API_KEY"),
        ),
        card(
            "custom",
            "Custom OpenAI-compatible endpoint",
            ConnectionClass::DirectApi,
            "Any server that speaks Chat Completions or Responses",
            None,
        ),
        card(
            ADD_LATER_CARD_ID,
            "Add a connection later",
            ConnectionClass::Local,
            "Keep Workshop offline for now; open this picker any time with /auth",
            None,
        ),
        card(
            XAI_CARD_ID,
            "xAI (optional)",
            ConnectionClass::OptionalXai,
            XAI_CARD_COPY,
            Some("XAI_API_KEY"),
        ),
    ]
}

/// A key press routed to the picker by the host UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerInput {
    Up,
    Down,
    /// Switch tabs (Tab / Left / Right).
    SwitchTab,
    Enter,
    /// Close the detail panel, or the picker when no panel is open.
    Back,
}

/// What the host must do after a key press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerOutcome {
    /// Redraw only.
    Changed,
    /// Picker closed (Esc / add later); host returns to the previous view.
    Close,
    /// User explicitly selected the labeled optional xAI card: the host may start the inherited
    /// xAI OIDC flow. This is the only outcome that leads to `auth.x.ai`.
    StartOptionalXaiLogin,
    /// A subscription rail was chosen: show the official login command; do not spawn from the picker.
    AdapterLogin(RailId),
}

/// Picker state. Pure; the host renders it and feeds it [`PickerInput`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerState {
    pub tab: PickerTab,
    pub models: Vec<ModelsCard>,
    pub rails: Vec<SubscriptionRail>,
    pub models_selected: usize,
    pub rail_selected: usize,
    /// Detail panel for the selected Models card (config snippet / instructions).
    pub detail_open: bool,
    /// The user acknowledged the xAI card once; a second Enter starts the flow.
    pub xai_armed: bool,
}

impl Default for PickerState {
    fn default() -> Self {
        Self::new()
    }
}

impl PickerState {
    /// Fresh picker: Models tab, first card selected, rails Detecting, xAI never preselected.
    pub fn new() -> Self {
        Self {
            tab: PickerTab::Models,
            models: models_cards(),
            rails: RailId::ALL.iter().map(|id| SubscriptionRail::new(*id)).collect(),
            models_selected: 0,
            rail_selected: 0,
            detail_open: false,
            xai_armed: false,
        }
    }

    /// Open directly on a tab (`/auth` → Models, `/models` → Models, subscriptions → Subscriptions).
    pub fn with_tab(mut self, tab: PickerTab) -> Self {
        self.tab = tab;
        self
    }

    /// Presence-only detection for the three rails. No auth-file reads, no process spawns.
    pub fn run_detection(&mut self) {
        for rail in &mut self.rails {
            let found = detect::find_official_binary(rail.id.binary());
            rail.installed = Some(found.is_some());
            rail.binary_path = found;
            // Ready requires a documented, non-secret status probe of the vendor CLI (milestone D).
            // Until then every detected rail is "Sign in": never claim readiness from presence.
            rail.pill = Pill::SignIn;
        }
    }

    pub fn selected_card(&self) -> Option<&ModelsCard> {
        self.models.get(self.models_selected)
    }

    pub fn selected_rail(&self) -> Option<&SubscriptionRail> {
        self.rails.get(self.rail_selected)
    }

    pub fn handle(&mut self, input: PickerInput) -> PickerOutcome {
        match (self.tab, input) {
            (_, PickerInput::SwitchTab) => {
                self.tab = self.tab.other();
                self.detail_open = false;
                self.xai_armed = false;
                PickerOutcome::Changed
            }
            (PickerTab::Models, PickerInput::Up) => {
                self.models_selected = self.models_selected.saturating_sub(1);
                self.detail_open = false;
                self.xai_armed = false;
                PickerOutcome::Changed
            }
            (PickerTab::Models, PickerInput::Down) => {
                if self.models_selected + 1 < self.models.len() {
                    self.models_selected += 1;
                }
                self.detail_open = false;
                self.xai_armed = false;
                PickerOutcome::Changed
            }
            (PickerTab::Subscriptions, PickerInput::Up) => {
                self.rail_selected = self.rail_selected.saturating_sub(1);
                self.detail_open = false;
                PickerOutcome::Changed
            }
            (PickerTab::Subscriptions, PickerInput::Down) => {
                if self.rail_selected + 1 < self.rails.len() {
                    self.rail_selected += 1;
                }
                self.detail_open = false;
                PickerOutcome::Changed
            }
            (_, PickerInput::Back) => {
                if self.detail_open {
                    self.detail_open = false;
                    self.xai_armed = false;
                    PickerOutcome::Changed
                } else {
                    PickerOutcome::Close
                }
            }
            (PickerTab::Models, PickerInput::Enter) => {
                let Some(card) = self.selected_card() else {
                    return PickerOutcome::Changed;
                };
                if card.id == ADD_LATER_CARD_ID {
                    return PickerOutcome::Close;
                }
                if card.id == XAI_CARD_ID {
                    // Two-step: first Enter shows the labeled copy, second Enter starts the flow.
                    if self.xai_armed {
                        return PickerOutcome::StartOptionalXaiLogin;
                    }
                    self.detail_open = true;
                    self.xai_armed = true;
                    return PickerOutcome::Changed;
                }
                self.detail_open = true;
                PickerOutcome::Changed
            }
            (PickerTab::Subscriptions, PickerInput::Enter) => {
                self.detail_open = true;
                match self.selected_rail() {
                    Some(rail) => PickerOutcome::AdapterLogin(rail.id),
                    None => PickerOutcome::Changed,
                }
            }
        }
    }

    /// Detail lines for the selected item (config snippet or next step). Never contains a secret.
    pub fn detail_lines(&self) -> Vec<String> {
        match self.tab {
            PickerTab::Models => self
                .selected_card()
                .map(|card| card_detail_lines(card, self.xai_armed))
                .unwrap_or_default(),
            PickerTab::Subscriptions => self
                .selected_rail()
                .map(rail_detail_lines)
                .unwrap_or_default(),
        }
    }
}

/// Path of the user config file shown in snippets (`$WORKSHOP_HOME/config.toml`).
pub fn config_path_display() -> String {
    xai_dirs::resolve_grok_home()
        .unwrap_or_else(xai_dirs::default_grok_home)
        .join("config.toml")
        .display()
        .to_string()
}

fn card_detail_lines(card: &ModelsCard, xai_armed: bool) -> Vec<String> {
    let cfg = config_path_display();
    let mut lines = vec![format!("{} · {}", card.title, card.class.label())];
    match card.id {
        "local" => lines.extend([
            "Point Workshop at a loopback OpenAI-compatible server. No key leaves this machine.".into(),
            format!("Add to {cfg}:"),
            "  default = \"local\"".into(),
            "  [model.local]".into(),
            "  model = \"<model name served locally>\"".into(),
            "  base_url = \"http://127.0.0.1:11434/v1\"   # Ollama; LM Studio uses :1234, vLLM :8000".into(),
            "  api_backend = \"chat_completions\"".into(),
            "  api_key = \"local\"".into(),
            "Then restart workshop.".into(),
        ]),
        "openai" => lines.extend([
            "Bring your own OpenAI API key. Workshop calls api.openai.com directly.".into(),
            env_line(card),
            format!("Add to {cfg}:"),
            "  default = \"openai\"".into(),
            "  [model.openai]".into(),
            "  model = \"gpt-5\"".into(),
            "  base_url = \"https://api.openai.com/v1\"".into(),
            "  api_backend = \"responses\"".into(),
            "  env_key = \"OPENAI_API_KEY\"".into(),
            "Then restart workshop.".into(),
        ]),
        "anthropic" => lines.extend([
            "Bring your own Anthropic Console API key (Messages API, anthropic-version header).".into(),
            env_line(card),
            format!("Add to {cfg}:"),
            "  default = \"anthropic\"".into(),
            "  [model.anthropic]".into(),
            "  model = \"claude-sonnet-4-5\"".into(),
            "  base_url = \"https://api.anthropic.com/v1\"".into(),
            "  api_backend = \"messages\"".into(),
            "  env_key = \"ANTHROPIC_API_KEY\"".into(),
            "Then restart workshop.".into(),
        ]),
        "openrouter" => lines.extend([
            "OpenRouter routes one key to many models (OpenAI-compatible).".into(),
            env_line(card),
            format!("Add to {cfg}:"),
            "  default = \"openrouter\"".into(),
            "  [model.openrouter]".into(),
            "  model = \"<provider/model>\"".into(),
            "  base_url = \"https://openrouter.ai/api/v1\"".into(),
            "  api_backend = \"chat_completions\"".into(),
            "  env_key = \"OPENROUTER_API_KEY\"".into(),
            "Then restart workshop.".into(),
        ]),
        "custom" => lines.extend([
            "Any OpenAI-compatible server. Plain http is accepted on loopback only.".into(),
            format!("Add to {cfg}:"),
            "  default = \"custom\"".into(),
            "  [model.custom]".into(),
            "  model = \"<model id>\"".into(),
            "  base_url = \"https://<host>/v1\"".into(),
            "  api_backend = \"chat_completions\"   # or \"responses\"".into(),
            "  env_key = \"MY_PROVIDER_API_KEY\"".into(),
            "Then restart workshop.".into(),
        ]),
        ADD_LATER_CARD_ID => lines.push("Workshop stays offline. Nothing is contacted.".into()),
        XAI_CARD_ID => {
            lines.push(XAI_CARD_COPY.into());
            lines.push(
                "Selecting this opens your browser at auth.x.ai and stores an xAI session in Workshop's home."
                    .into(),
            );
            lines.push(env_line(card));
            lines.push("This is the only Workshop path that contacts x.ai.".into());
            if xai_armed {
                lines.push("Press Enter again to continue to auth.x.ai, or Esc to go back.".into());
            }
        }
        _ => {}
    }
    lines
}

fn env_line(card: &ModelsCard) -> String {
    match (card.env_key, card.env_key_present) {
        (Some(k), true) => format!("{k} is set in this shell (value not shown)."),
        (Some(k), false) => format!("{k} is not set in this shell."),
        (None, _) => String::new(),
    }
}

fn rail_detail_lines(rail: &SubscriptionRail) -> Vec<String> {
    let mut lines = vec![format!(
        "{} · {} · {}",
        rail.id.name(),
        ConnectionClass::AgentAdapter.label(),
        rail.pill.label()
    )];
    match rail.installed {
        None => lines.push("Detecting…".into()),
        Some(false) => {
            lines.push(rail.id.install_hint().into());
            lines.push(format!(
                "Workshop looks for `{}` on PATH and in the usual install folders. It never reads another app's login files.",
                rail.id.binary()
            ));
        }
        Some(true) => {
            if let Some(p) = &rail.binary_path {
                lines.push(format!("Found {}", p.display()));
            }
            lines.push(format!(
                "Sign in with the official CLI in your terminal:  {}",
                rail.id.login_command()
            ));
            lines.push(
                "Workshop will spawn the official CLI for runs (milestone E); it does not read its credentials."
                    .into(),
            );
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xai_card_is_last_and_never_preselected() {
        let p = PickerState::new();
        assert_eq!(p.models.last().map(|c| c.id), Some(XAI_CARD_ID));
        assert_eq!(p.models_selected, 0);
        assert_ne!(p.selected_card().map(|c| c.id), Some(XAI_CARD_ID));
        assert_eq!(p.tab, PickerTab::Models);
    }

    #[test]
    fn rails_are_claude_codex_cursor_in_order_and_start_detecting() {
        let p = PickerState::new();
        let ids: Vec<_> = p.rails.iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![RailId::Claude, RailId::Codex, RailId::Cursor]);
        assert!(p.rails.iter().all(|r| r.pill == Pill::Detecting));
    }

    #[test]
    fn cursor_rail_uses_cursor_agent_not_agent() {
        assert_eq!(RailId::Cursor.binary(), "cursor-agent");
    }

    #[test]
    fn xai_login_requires_two_explicit_enters() {
        let mut p = PickerState::new();
        let last = p.models.len() - 1;
        for _ in 0..last {
            p.handle(PickerInput::Down);
        }
        assert_eq!(p.selected_card().map(|c| c.id), Some(XAI_CARD_ID));
        assert_eq!(p.handle(PickerInput::Enter), PickerOutcome::Changed);
        assert!(p.detail_open && p.xai_armed);
        assert_eq!(
            p.handle(PickerInput::Enter),
            PickerOutcome::StartOptionalXaiLogin
        );
    }

    #[test]
    fn moving_off_the_xai_card_disarms_it() {
        let mut p = PickerState::new();
        let last = p.models.len() - 1;
        for _ in 0..last {
            p.handle(PickerInput::Down);
        }
        p.handle(PickerInput::Enter);
        assert!(p.xai_armed);
        p.handle(PickerInput::Up);
        assert!(!p.xai_armed);
        // The card above xAI is "Add a connection later": Enter closes, never starts a login.
        assert_ne!(
            p.handle(PickerInput::Enter),
            PickerOutcome::StartOptionalXaiLogin
        );
        // Back on the xAI card, the first Enter only arms again.
        let mut p2 = PickerState::new();
        for _ in 0..last {
            p2.handle(PickerInput::Down);
        }
        p2.handle(PickerInput::Enter);
        p2.handle(PickerInput::Up);
        p2.handle(PickerInput::Down);
        assert!(!p2.xai_armed);
        assert_eq!(p2.handle(PickerInput::Enter), PickerOutcome::Changed);
    }

    #[test]
    fn non_xai_cards_never_start_a_login() {
        let mut p = PickerState::new();
        for i in 0..p.models.len() {
            p.models_selected = i;
            p.detail_open = false;
            p.xai_armed = false;
            let id = p.models[i].id;
            if id == XAI_CARD_ID {
                continue;
            }
            let out = p.handle(PickerInput::Enter);
            assert!(
                matches!(out, PickerOutcome::Changed | PickerOutcome::Close),
                "{id}: {out:?}"
            );
        }
    }

    #[test]
    fn esc_closes_detail_then_picker() {
        let mut p = PickerState::new();
        p.handle(PickerInput::Enter);
        assert!(p.detail_open);
        assert_eq!(p.handle(PickerInput::Back), PickerOutcome::Changed);
        assert_eq!(p.handle(PickerInput::Back), PickerOutcome::Close);
    }

    #[test]
    fn add_later_closes_without_login() {
        let mut p = PickerState::new();
        let idx = p
            .models
            .iter()
            .position(|c| c.id == ADD_LATER_CARD_ID)
            .unwrap();
        p.models_selected = idx;
        assert_eq!(p.handle(PickerInput::Enter), PickerOutcome::Close);
    }

    #[test]
    fn detail_lines_never_contain_an_env_value() {
        // Simulate a present key; the picker must only say it is set.
        // SAFETY: test-local env mutation; no other test in this crate reads this variable.
        unsafe { std::env::set_var("OPENAI_API_KEY", "sk-super-secret-value") };
        let mut p = PickerState::new();
        let idx = p.models.iter().position(|c| c.id == "openai").unwrap();
        p.models_selected = idx;
        let joined = p.detail_lines().join("\n");
        assert!(joined.contains("OPENAI_API_KEY is set"));
        assert!(!joined.contains("sk-super-secret-value"));
        unsafe { std::env::remove_var("OPENAI_API_KEY") };
    }

    #[test]
    fn detection_never_reports_ready_from_presence() {
        let mut p = PickerState::new();
        p.run_detection();
        assert!(p.rails.iter().all(|r| r.pill == Pill::SignIn));
        assert!(p.rails.iter().all(|r| r.installed.is_some()));
    }
}
