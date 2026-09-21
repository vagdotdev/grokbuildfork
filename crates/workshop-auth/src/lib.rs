//! Workshop connection picker: the only default auth surface.
//!
//! Login, the welcome `l` key, `/login`, `/auth`, `/models`, first run and `workshop login` all open
//! this picker. It never starts an OAuth flow by itself. The optional xAI card is the single entry
//! point to the inherited xAI OIDC flow, and only after the user selects it twice.
//!
//! Layout follows the Blackpen export (docs/workshop-production-plan.md section 12): two tabs,
//! **Models** and **Subscriptions**; Subscriptions is a left rail **Claude, Codex, Cursor** in that
//! order, each with one pill (Detecting / Ready / Sign in); the optional xAI card is the *last*
//! Models row and is never preselected.
//!
//! Data sources: Models rows come from [`workshop_providers`] (local servers, Kilo `:free`,
//! OpenRouter, Google, NVIDIA, OpenAI, Anthropic, OpenCode Zen, BYOK) plus the OpenCode engine's
//! free catalog; rail state comes from [`workshop_detect`] (the single detection source). This crate
//! holds the pure picker policy and the `[model.<key>]` config writer; the host renders the state,
//! feeds it [`PickerInput`], and executes [`PickerOutcome`]s.
//!
//! What this crate does **not** do (gate:no-theft): read any keychain item, any `auth.json`, any
//! Cursor SDK auth file, or spawn a vendor CLI for a turn.

#![deny(clippy::indexing_slicing)]

pub mod config_write;
pub mod text;

use std::path::PathBuf;

pub use workshop_detect::{Pill, Rail, RailState};
pub use workshop_providers::{CatalogModel, ConnectOption, DefaultSelection};

/// Connection class, always shown to the user (never present a subscription as an API base URL).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionClass {
    /// Workshop owns prompt, tools, loop and HTTP; credential is an API key issued for API use.
    DirectApi,
    /// Same, against a loopback OpenAI-compatible server.
    Local,
    /// Workshop supervises an official vendor CLI (or the OpenCode engine); the CLI owns its login.
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
    pub fn from_provider(class: workshop_providers::ProviderClass) -> Self {
        match class {
            workshop_providers::ProviderClass::Local => Self::Local,
            workshop_providers::ProviderClass::Direct => Self::DirectApi,
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

pub const XAI_ROW_ID: &str = "xai_optional";
pub const ADD_LATER_ROW_ID: &str = "add_later";
/// Copy on the optional xAI card, verbatim from the plan.
pub const XAI_CARD_COPY: &str = "Uses xAI accounts and auth.x.ai. Not required.";
/// Provider id of the OpenCode engine rows (free tier reachable only through the genuine client).
pub const ENGINE_PROVIDER_ID: &str = "opencode-engine";

/// A free model served through the OpenCode engine (`opencode serve`), mirrored from its catalog.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EngineModel {
    /// `opencode/<id>`, the form the engine's prompt API takes.
    pub model_ref: String,
    pub name: String,
    pub is_default: bool,
    pub tool_call: bool,
    pub context_limit: Option<u64>,
}

impl EngineModel {
    /// The one row shown before the engine has ever reported its catalog: OpenCode's documented
    /// default. Selecting it installs/starts the engine, which then refreshes the list.
    pub fn big_pickle_seed() -> Self {
        Self {
            model_ref: "opencode/big-pickle".into(),
            name: "Big Pickle".into(),
            is_default: true,
            tool_call: true,
            context_limit: Some(200_000),
        }
    }
}

/// One Models-tab row.
#[derive(Debug, Clone, PartialEq)]
pub enum RowKind {
    /// A Direct API / Local catalog row. `locked` means the provider still needs a credential.
    Catalog { model: CatalogModel, locked: bool },
    /// A provider that has nothing selectable yet: connect it (sign in / paste key / install).
    ConnectProvider {
        provider_id: String,
        copy: String,
        credential_url: Option<String>,
    },
    /// A free model behind the OpenCode engine.
    Engine(EngineModel),
    /// Keep Workshop offline for now.
    AddLater,
    /// The labeled optional xAI card.
    XaiOptional,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelsRow {
    pub kind: RowKind,
    /// Group header (provider display name); rows of one group are contiguous.
    pub group: String,
    /// Badge such as `Free · No sign-in · Shared pool · may log/train`.
    pub badge: String,
    pub class: ConnectionClass,
}

impl ModelsRow {
    pub fn id(&self) -> String {
        match &self.kind {
            RowKind::Catalog { model, .. } => model.key(),
            RowKind::ConnectProvider { provider_id, .. } => format!("connect:{provider_id}"),
            RowKind::Engine(m) => format!("{ENGINE_PROVIDER_ID}:{}", m.model_ref),
            RowKind::AddLater => ADD_LATER_ROW_ID.into(),
            RowKind::XaiOptional => XAI_ROW_ID.into(),
        }
    }
    pub fn title(&self) -> String {
        match &self.kind {
            RowKind::Catalog { model, .. } => model.display_name.clone(),
            RowKind::ConnectProvider { copy, .. } => copy.clone(),
            RowKind::Engine(m) => {
                if m.is_default {
                    format!("{} (OpenCode default)", m.name)
                } else {
                    m.name.clone()
                }
            }
            RowKind::AddLater => "Add a connection later".into(),
            RowKind::XaiOptional => "xAI (optional)".into(),
        }
    }
    pub fn is_xai(&self) -> bool {
        matches!(self.kind, RowKind::XaiOptional)
    }
}

/// Everything the host feeds into the picker once its async loaders finish.
#[derive(Debug, Clone, Default)]
pub struct PickerSnapshot {
    /// Local rows first, then hosted providers in manifest order (from `Catalog::picker_groups`).
    pub rows: Vec<ModelsRow>,
    pub rails: Vec<RailState>,
    /// The plan's first-run default, when nothing is configured yet.
    pub default_selection: Option<DefaultSelection>,
    /// Secret backend name for the review line (`keyring`, `file`, …).
    pub secret_backend: Option<&'static str>,
}

/// Build the Models rows for a snapshot.
///
/// `catalog` already contains detected local rows; `connected(provider_id)` comes from the broker;
/// `engine_models` is the cached / live engine catalog (empty → the Big Pickle seed row).
pub fn models_rows(
    catalog: &workshop_providers::Catalog,
    connected: impl Fn(&str) -> bool,
    engine_models: &[EngineModel],
) -> Vec<ModelsRow> {
    let mut rows = Vec::new();
    for group in catalog.picker_groups(&connected) {
        let class = ConnectionClass::from_provider(group.class);
        if group.rows.is_empty() {
            if let Some(copy) = group.connect_copy.clone() {
                let credential_url = workshop_providers::manifest(&group.provider_id)
                    .and_then(|m| m.credential_url.clone());
                rows.push(ModelsRow {
                    kind: RowKind::ConnectProvider {
                        provider_id: group.provider_id.clone(),
                        copy,
                        credential_url,
                    },
                    group: group.display_name.clone(),
                    badge: group.badge_line.clone(),
                    class,
                });
            }
            continue;
        }
        for model in group.rows {
            rows.push(ModelsRow {
                kind: RowKind::Catalog {
                    model: model.clone(),
                    locked: false,
                },
                group: group.display_name.clone(),
                badge: group.badge_line.clone(),
                class,
            });
        }
        if group.locked_rows > 0
            && let Some(copy) = group.connect_copy.clone()
        {
            let credential_url = workshop_providers::manifest(&group.provider_id)
                .and_then(|m| m.credential_url.clone());
            rows.push(ModelsRow {
                kind: RowKind::ConnectProvider {
                    provider_id: group.provider_id.clone(),
                    copy: format!("{copy} ({} more models)", group.locked_rows),
                    credential_url,
                },
                group: group.display_name.clone(),
                badge: group.badge_line.clone(),
                class,
            });
        }
    }
    // OpenCode free tier: only through the genuine client, so it is an engine group, not Direct API.
    let engine: Vec<EngineModel> = if engine_models.is_empty() {
        vec![EngineModel::big_pickle_seed()]
    } else {
        engine_models.to_vec()
    };
    for m in engine {
        rows.push(ModelsRow {
            kind: RowKind::Engine(m),
            group: "OpenCode free (engine)".into(),
            badge: "Free · Agent adapter · official opencode CLI · shared pool".into(),
            class: ConnectionClass::AgentAdapter,
        });
    }
    rows.push(ModelsRow {
        kind: RowKind::AddLater,
        group: "Later".into(),
        badge: "Nothing is contacted".into(),
        class: ConnectionClass::Local,
    });
    rows.push(ModelsRow {
        kind: RowKind::XaiOptional,
        group: "xAI (optional)".into(),
        badge: XAI_CARD_COPY.into(),
        class: ConnectionClass::OptionalXai,
    });
    rows
}

/// A key press routed to the picker by the host UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerInput {
    Up,
    Down,
    /// Switch tabs (Tab / Left / Right).
    SwitchTab,
    Enter,
    /// Close the detail panel / cancel key entry, or the picker when nothing is open.
    Back,
    /// Text typed while a key-entry prompt is open.
    Char(char),
    Backspace,
    /// Paste while a key-entry prompt is open.
    Paste(String),
    /// Refresh live catalogs / re-probe rails.
    Refresh,
}

/// What the host must do after a key press.
#[derive(Debug, Clone, PartialEq)]
pub enum PickerOutcome {
    /// Redraw only.
    Changed,
    /// Picker closed (Esc / add later); host returns to the previous view.
    Close,
    /// User explicitly selected the labeled optional xAI card twice: the host may start the
    /// inherited xAI OIDC flow. This is the only outcome that leads to `auth.x.ai`.
    StartOptionalXaiLogin,
    /// A Direct API / Local row: write `[model.<key>]`, make it active.
    SelectCatalog(CatalogModel),
    /// A credential-requiring provider needs connecting; the host runs the provider's flow
    /// (OpenRouter PKCE sign-in, or the key-entry prompt the picker opened).
    ConnectProvider(String),
    /// The user pasted a key for `provider_id` (never logged; host saves it in the broker).
    SaveKey { provider_id: String, key: String },
    /// A free model behind the OpenCode engine: route turns through the engine.
    SelectEngine(EngineModel),
    /// A subscription rail's Connect: run the official CLI login in the user's terminal.
    RailConnect(Rail),
    /// A model on a Ready rail: route turns through that vendor's adapter.
    SelectRailModel(Rail, workshop_detect::ModelRef),
    /// Re-run the loaders.
    Refresh,
}

/// An open key-entry prompt (value lives only here until saved; never rendered in full).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEntry {
    pub provider_id: String,
    pub label: String,
    pub buffer: String,
}

/// Picker state. Pure; the host renders it and feeds it [`PickerInput`].
#[derive(Debug, Clone, PartialEq)]
pub struct PickerState {
    pub tab: PickerTab,
    pub rows: Vec<ModelsRow>,
    pub rails: Vec<RailState>,
    pub models_selected: usize,
    pub rail_selected: usize,
    /// Selected model radio on the Subscriptions right pane.
    pub rail_model_selected: usize,
    /// Detail panel open for the selected item.
    pub detail_open: bool,
    /// The user acknowledged the xAI card once; a second Enter starts the flow.
    pub xai_armed: bool,
    /// Loaders still running (rows may be partial).
    pub loading: bool,
    /// Transient status line (errors, progress).
    pub status: Option<String>,
    pub key_entry: Option<KeyEntry>,
    pub default_selection: Option<DefaultSelection>,
    pub secret_backend: Option<&'static str>,
}

impl Default for PickerState {
    fn default() -> Self {
        Self::new()
    }
}

impl PickerState {
    /// Fresh picker before any loader ran: Models tab, "Add later" + xAI only, rails Detecting.
    pub fn new() -> Self {
        Self {
            tab: PickerTab::Models,
            rows: models_rows(&workshop_providers::Catalog::default(), |_| false, &[])
                .into_iter()
                .filter(|r| !matches!(r.kind, RowKind::Engine(_)))
                .collect(),
            rails: Rail::ALL.iter().map(|r| RailState::detecting(*r)).collect(),
            models_selected: 0,
            rail_selected: 0,
            rail_model_selected: 0,
            detail_open: false,
            xai_armed: false,
            loading: true,
            status: None,
            key_entry: None,
            default_selection: None,
            secret_backend: None,
        }
    }

    /// Open directly on a tab (`/auth` → Models, `/models` → Models).
    pub fn with_tab(mut self, tab: PickerTab) -> Self {
        self.tab = tab;
        self
    }

    /// Replace rows and rails with a finished snapshot. Keeps the selection on the same row id
    /// when possible and never lands on the xAI card.
    pub fn apply_snapshot(&mut self, snap: PickerSnapshot) {
        // Keep the selection across a refresh; the first load always lands on the first real row.
        let prev_id = if self.loading {
            None
        } else {
            self.selected_row().map(ModelsRow::id)
        };
        self.rows = snap.rows;
        if !snap.rails.is_empty() {
            self.rails = snap.rails;
        }
        self.default_selection = snap.default_selection;
        self.secret_backend = snap.secret_backend;
        self.loading = false;
        self.models_selected = prev_id
            .and_then(|id| self.rows.iter().position(|r| r.id() == id))
            .unwrap_or(0);
        if self.selected_row().is_some_and(ModelsRow::is_xai) {
            self.models_selected = 0;
        }
        self.rail_selected = self.rail_selected.min(self.rails.len().saturating_sub(1));
        self.rail_model_selected = 0;
        self.xai_armed = false;
    }

    pub fn selected_row(&self) -> Option<&ModelsRow> {
        self.rows.get(self.models_selected)
    }

    pub fn selected_rail(&self) -> Option<&RailState> {
        self.rails.get(self.rail_selected)
    }

    pub fn selected_rail_model(&self) -> Option<&workshop_detect::ModelRef> {
        self.selected_rail()
            .and_then(|r| r.models.get(self.rail_model_selected))
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some(msg.into());
    }

    /// Open the key-entry prompt for a provider (host calls this for paste-key connect flows).
    pub fn begin_key_entry(&mut self, provider_id: &str, label: &str) {
        self.key_entry = Some(KeyEntry {
            provider_id: provider_id.to_owned(),
            label: label.to_owned(),
            buffer: String::new(),
        });
        self.detail_open = true;
    }

    pub fn handle(&mut self, input: PickerInput) -> PickerOutcome {
        if let Some(entry) = self.key_entry.as_mut() {
            return match input {
                PickerInput::Char(c) if !c.is_control() => {
                    entry.buffer.push(c);
                    PickerOutcome::Changed
                }
                PickerInput::Paste(text) => {
                    entry.buffer.push_str(text.trim());
                    PickerOutcome::Changed
                }
                PickerInput::Backspace => {
                    entry.buffer.pop();
                    PickerOutcome::Changed
                }
                PickerInput::Enter => {
                    let key = entry.buffer.trim().to_owned();
                    if key.is_empty() {
                        return PickerOutcome::Changed;
                    }
                    let provider_id = entry.provider_id.clone();
                    self.key_entry = None;
                    PickerOutcome::SaveKey { provider_id, key }
                }
                PickerInput::Back => {
                    self.key_entry = None;
                    PickerOutcome::Changed
                }
                _ => PickerOutcome::Changed,
            };
        }
        match (self.tab, input) {
            (_, PickerInput::Refresh) => PickerOutcome::Refresh,
            (_, PickerInput::Char(_) | PickerInput::Backspace | PickerInput::Paste(_)) => {
                PickerOutcome::Changed
            }
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
                if self.models_selected + 1 < self.rows.len() {
                    self.models_selected += 1;
                }
                self.detail_open = false;
                self.xai_armed = false;
                PickerOutcome::Changed
            }
            (PickerTab::Subscriptions, PickerInput::Up) => {
                if self.detail_open && self.rail_model_selected > 0 {
                    self.rail_model_selected -= 1;
                } else {
                    self.rail_selected = self.rail_selected.saturating_sub(1);
                    self.rail_model_selected = 0;
                    self.detail_open = false;
                }
                PickerOutcome::Changed
            }
            (PickerTab::Subscriptions, PickerInput::Down) => {
                let models = self.selected_rail().map(|r| r.models.len()).unwrap_or(0);
                if self.detail_open && self.rail_model_selected + 1 < models {
                    self.rail_model_selected += 1;
                } else if !self.detail_open && self.rail_selected + 1 < self.rails.len() {
                    self.rail_selected += 1;
                    self.rail_model_selected = 0;
                }
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
                let Some(row) = self.selected_row().cloned() else {
                    return PickerOutcome::Changed;
                };
                match row.kind {
                    RowKind::AddLater => PickerOutcome::Close,
                    RowKind::XaiOptional => {
                        // Two-step: first Enter shows the labeled copy, second Enter starts the flow.
                        if self.xai_armed {
                            PickerOutcome::StartOptionalXaiLogin
                        } else {
                            self.detail_open = true;
                            self.xai_armed = true;
                            PickerOutcome::Changed
                        }
                    }
                    RowKind::Catalog { model, locked } => {
                        if locked {
                            PickerOutcome::ConnectProvider(model.provider_id)
                        } else {
                            PickerOutcome::SelectCatalog(model)
                        }
                    }
                    RowKind::ConnectProvider { provider_id, .. } => {
                        PickerOutcome::ConnectProvider(provider_id)
                    }
                    RowKind::Engine(m) => PickerOutcome::SelectEngine(m),
                }
            }
            (PickerTab::Subscriptions, PickerInput::Enter) => {
                let Some(rail) = self.selected_rail().cloned() else {
                    return PickerOutcome::Changed;
                };
                if rail.is_ready() && !rail.models.is_empty() {
                    if !self.detail_open {
                        self.detail_open = true;
                        return PickerOutcome::Changed;
                    }
                    match rail.models.get(self.rail_model_selected) {
                        Some(m) => PickerOutcome::SelectRailModel(rail.rail, m.clone()),
                        None => PickerOutcome::Changed,
                    }
                } else {
                    self.detail_open = true;
                    if rail.show_connect {
                        PickerOutcome::RailConnect(rail.rail)
                    } else {
                        PickerOutcome::Changed
                    }
                }
            }
        }
    }

    /// Detail lines for the selected item (what will happen, where traffic goes). Never a secret.
    pub fn detail_lines(&self) -> Vec<String> {
        if let Some(entry) = &self.key_entry {
            let masked = if entry.buffer.is_empty() {
                "(paste or type your key, Enter to save, Esc to cancel)".to_owned()
            } else {
                format!(
                    "{} characters entered — Enter saves to {}",
                    entry.buffer.chars().count(),
                    self.secret_backend.unwrap_or("the Workshop secret store")
                )
            };
            return vec![entry.label.clone(), masked];
        }
        match self.tab {
            PickerTab::Models => self
                .selected_row()
                .map(|row| row_detail_lines(row, self.xai_armed, self.default_selection.as_ref()))
                .unwrap_or_default(),
            PickerTab::Subscriptions => self
                .selected_rail()
                .map(|r| rail_detail_lines(r, self.rail_model_selected))
                .unwrap_or_default(),
        }
    }
}

/// Path of the user config file the picker writes (`$WORKSHOP_HOME/config.toml`).
pub fn config_path() -> PathBuf {
    xai_dirs::resolve_grok_home()
        .unwrap_or_else(xai_dirs::default_grok_home)
        .join("config.toml")
}

fn row_detail_lines(
    row: &ModelsRow,
    xai_armed: bool,
    default_selection: Option<&DefaultSelection>,
) -> Vec<String> {
    let cfg = config_path().display().to_string();
    let mut lines = vec![format!("{} · {}", row.title(), row.class.label())];
    match &row.kind {
        RowKind::Catalog { model, locked } => {
            lines.push(row.badge.clone());
            lines.push(format!("Endpoint: {}", model.endpoint()));
            if let Some(note) = &model.note {
                lines.push(note.clone());
            }
            if let Some(tools) = model.tools {
                lines.push(format!(
                    "Tool calling: {}",
                    if tools { "yes" } else { "not advertised" }
                ));
            }
            if let Some(ctx) = model.context_window {
                lines.push(format!("Context window: {ctx} tokens"));
            }
            lines.push(format!(
                "Catalog source: {} ({})",
                model.source.name, model.source.as_of
            ));
            if *locked {
                lines.push("This provider needs a credential first — press Enter to connect.".into());
            } else {
                lines.push(format!(
                    "Enter writes [model.{}] to {cfg} and makes it the active model. Turns run \
                     through Workshop's own agent loop; only {} is contacted.",
                    model.key(),
                    model.base_url
                ));
            }
            if let Some(DefaultSelection::KiloFree { primary, .. }) = default_selection
                && model.model_id == *primary
            {
                lines.push("First-run default: the free community pool (Direct API · Free · shared pool).".into());
            }
        }
        RowKind::ConnectProvider {
            provider_id,
            copy,
            credential_url,
        } => {
            lines.push(copy.clone());
            match provider_id.as_str() {
                "openrouter" => lines.push(
                    "Enter starts OpenRouter's PKCE sign-in in your browser; Workshop stores the key it issues in your OS keyring."
                        .into(),
                ),
                _ => lines.push("Enter opens a key prompt; the key goes to your OS keyring, never to config.toml.".into()),
            }
            if let Some(url) = credential_url {
                lines.push(format!("Get a key: {url}"));
            }
        }
        RowKind::Engine(m) => {
            lines.push(row.badge.clone());
            lines.push(format!(
                "Runs through the OpenCode engine: Workshop starts the official `opencode serve` on \
                 loopback and drives it over HTTP; opencode.ai is contacted by opencode itself. Model {}{}.",
                m.model_ref,
                if m.tool_call { ", tool calling" } else { "" }
            ));
            lines.push(
                "If opencode is missing, Enter installs the pinned version with the vendor's own installer into Workshop's home (no bundled binary)."
                    .into(),
            );
            lines.push("Shared free pool: prompts may be logged by the upstream provider.".into());
        }
        RowKind::AddLater => lines.push("Workshop stays offline. Nothing is contacted.".into()),
        RowKind::XaiOptional => {
            lines.push(XAI_CARD_COPY.into());
            lines.push(
                "Selecting this opens your browser at auth.x.ai and stores an xAI session in Workshop's home."
                    .into(),
            );
            lines.push("This is the only Workshop path that contacts x.ai.".into());
            if xai_armed {
                lines.push("Press Enter again to continue to auth.x.ai, or Esc to go back.".into());
            }
        }
    }
    lines
}

fn rail_detail_lines(rail: &RailState, selected_model: usize) -> Vec<String> {
    let mut lines = vec![format!(
        "{} · {} · {}",
        rail.rail.display_name(),
        ConnectionClass::AgentAdapter.label(),
        rail.pill.label()
    )];
    if let Some(copy) = rail.empty_copy {
        lines.push(copy.to_owned());
    }
    if rail.pill == Pill::Detecting {
        lines.push("Looking for the official CLI on PATH and in the usual install folders…".into());
    } else if !rail.installed {
        lines.push(format!(
            "Workshop looks for `{}`; it never reads another app's login files.",
            rail.rail.vendor().binary_names().join("` or `")
        ));
    } else if !rail.is_ready() {
        lines.push(format!(
            "Connect runs the official login in your terminal:  {}",
            workshop_detect::login_argv(rail.rail.vendor()).join(" ")
        ));
    } else {
        for (i, m) in rail.models.iter().enumerate() {
            lines.push(format!(
                "{} {}",
                if i == selected_model { "(•)" } else { "( )" },
                m.display()
            ));
        }
        lines.push("Enter routes the session through the official CLI in an isolated worktree.".into());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded() -> PickerState {
        let mut p = PickerState::new();
        let rows = models_rows(&workshop_providers::Catalog::builtin(), |_| false, &[]);
        p.apply_snapshot(PickerSnapshot {
            rows,
            rails: Vec::new(),
            default_selection: Some(workshop_providers::select_default(&[], true)),
            secret_backend: Some("memory"),
        });
        p
    }

    #[test]
    fn xai_row_is_last_and_never_preselected() {
        let p = loaded();
        assert!(p.rows.last().is_some_and(ModelsRow::is_xai));
        assert_eq!(p.models_selected, 0);
        assert!(!p.selected_row().is_some_and(ModelsRow::is_xai));
        assert_eq!(p.tab, PickerTab::Models);
        // Fresh (unloaded) state too.
        let fresh = PickerState::new();
        assert!(fresh.rows.last().is_some_and(ModelsRow::is_xai));
        assert!(!fresh.selected_row().is_some_and(ModelsRow::is_xai));
    }

    #[test]
    fn rails_are_claude_codex_cursor_in_order_and_start_detecting() {
        let p = PickerState::new();
        let ids: Vec<_> = p.rails.iter().map(|r| r.rail).collect();
        assert_eq!(ids, vec![Rail::Claude, Rail::Codex, Rail::Cursor]);
        assert!(p.rails.iter().all(|r| r.pill == Pill::Detecting));
    }

    #[test]
    fn kilo_free_is_a_selectable_row_and_openrouter_needs_connect() {
        let p = loaded();
        let kilo = p
            .rows
            .iter()
            .find(|r| matches!(&r.kind, RowKind::Catalog { model, .. } if model.model_id == "kilo-auto/free"))
            .expect("kilo row");
        assert!(matches!(kilo.kind, RowKind::Catalog { locked: false, .. }));
        assert!(
            p.rows.iter().any(|r| matches!(&r.kind, RowKind::ConnectProvider { provider_id, .. } if provider_id == "openrouter")),
            "openrouter shows a connect row when no key is connected"
        );
        let engine = p
            .rows
            .iter()
            .find(|r| matches!(r.kind, RowKind::Engine(_)))
            .expect("engine seed row");
        assert_eq!(engine.class, ConnectionClass::AgentAdapter);
    }

    #[test]
    fn xai_login_requires_two_explicit_enters_and_disarms_on_move() {
        let mut p = loaded();
        let last = p.rows.len() - 1;
        for _ in 0..last {
            p.handle(PickerInput::Down);
        }
        assert!(p.selected_row().is_some_and(ModelsRow::is_xai));
        assert_eq!(p.handle(PickerInput::Enter), PickerOutcome::Changed);
        assert!(p.detail_open && p.xai_armed);
        p.handle(PickerInput::Up);
        assert!(!p.xai_armed);
        p.handle(PickerInput::Down);
        assert_eq!(p.handle(PickerInput::Enter), PickerOutcome::Changed);
        assert_eq!(
            p.handle(PickerInput::Enter),
            PickerOutcome::StartOptionalXaiLogin
        );
    }

    #[test]
    fn non_xai_rows_never_start_a_login() {
        let mut p = loaded();
        for i in 0..p.rows.len() {
            p.models_selected = i;
            p.detail_open = false;
            p.xai_armed = false;
            if p.rows.get(i).is_some_and(ModelsRow::is_xai) {
                continue;
            }
            let out = p.handle(PickerInput::Enter);
            assert_ne!(out, PickerOutcome::StartOptionalXaiLogin, "row {i}");
        }
    }

    #[test]
    fn default_selection_is_never_xai_or_zen() {
        let sel = workshop_providers::select_default(&[], true);
        let json = serde_json::to_string(&sel).unwrap();
        assert!(!json.contains("xai") && !json.contains("opencode"));
        assert!(matches!(sel, DefaultSelection::KiloFree { .. }));
    }

    #[test]
    fn key_entry_never_echoes_the_secret() {
        let mut p = loaded();
        p.begin_key_entry("google", "Paste a Google AI Studio key");
        for c in "AIzaSySECRET".chars() {
            p.handle(PickerInput::Char(c));
        }
        let joined = p.detail_lines().join("\n");
        assert!(!joined.contains("AIzaSySECRET"));
        assert!(joined.contains("characters entered"));
        assert_eq!(
            p.handle(PickerInput::Enter),
            PickerOutcome::SaveKey {
                provider_id: "google".into(),
                key: "AIzaSySECRET".into()
            }
        );
        assert!(p.key_entry.is_none());
    }

    #[test]
    fn esc_closes_detail_then_picker_and_add_later_closes() {
        let mut p = loaded();
        p.handle(PickerInput::Enter);
        assert!(matches!(
            p.handle(PickerInput::Back),
            PickerOutcome::Changed | PickerOutcome::Close
        ));
        let mut q = loaded();
        let idx = q
            .rows
            .iter()
            .position(|r| matches!(r.kind, RowKind::AddLater))
            .unwrap();
        q.models_selected = idx;
        assert_eq!(q.handle(PickerInput::Enter), PickerOutcome::Close);
    }
}
