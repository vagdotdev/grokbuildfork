//! Workshop connection picker: the `/model` and `/auth` overlays.
//!
//! Workshop starts with the OpenCode engine's default free model active, so the picker is never
//! the first screen. `/model` opens the **Models** view (one line per usable model); `/auth`
//! (alias `/login`) opens the **Subscriptions** view (the Claude / Codex / Cursor rails with a
//! Detecting / Ready / Sign in pill, the API-key providers, and the optional xAI card last).
//! `Tab` switches between the two; `Esc` closes.
//!
//! The picker never starts an OAuth flow by itself. The optional xAI card is the single entry
//! point to the inherited xAI OIDC flow, and only after the user selects it twice.
//!
//! Data sources: Models rows come from [`workshop_providers`] (local servers, Kilo `:free`,
//! connected providers) plus the OpenCode engine's free catalog; rail state comes from
//! [`workshop_detect`] (the single detection source). This crate holds the pure picker policy and
//! the `[model.<key>]` config writer; the host renders the state, feeds it [`PickerInput`], and
//! executes [`PickerOutcome`]s.
//!
//! What this crate does **not** do (gate:no-theft): read any keychain item, any `auth.json`, any
//! Cursor SDK auth file, or spawn a vendor CLI for a turn.

#![deny(clippy::indexing_slicing)]

pub mod config_write;
pub mod text;

use std::path::PathBuf;

pub use workshop_detect::{Pill, Rail, RailState};
pub use workshop_providers::{
    CatalogModel, CatalogStatus, ConnectOption, DefaultSelection, Freshness,
};

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
/// Copy on the optional xAI card, verbatim from the plan.
pub const XAI_CARD_COPY: &str = "Uses xAI accounts and auth.x.ai. Not required.";
/// Provider id of the OpenCode engine rows (free tier reachable only through the genuine client).
pub const ENGINE_PROVIDER_ID: &str = "opencode-engine";
/// Provider display name of the OpenCode engine rows and the composer label prefix.
pub const ENGINE_DISPLAY_NAME: &str = "OpenCode";
/// Where the engine rows come from when they are live: the running `opencode serve`.
pub const ENGINE_CATALOG_SOURCE: &str = "opencode serve /config/providers";

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

    /// The model Workshop activates on a first run: the engine's own default from its live
    /// catalog when one was cached, else the pinned seed. Mirrors OpenCode's default.
    pub fn first_run_default(catalog: &[EngineModel]) -> Self {
        catalog
            .iter()
            .find(|m| m.is_default)
            .cloned()
            .unwrap_or_else(Self::big_pickle_seed)
    }

    /// Picker row id of this model (`opencode-engine:opencode/<id>`).
    pub fn row_id(&self) -> String {
        format!("{ENGINE_PROVIDER_ID}:{}", self.model_ref)
    }
}

/// One picker row.
#[derive(Debug, Clone, PartialEq)]
pub enum RowKind {
    /// A Direct API / Local catalog row. `locked` means the provider still needs a credential.
    Catalog { model: CatalogModel, locked: bool },
    /// A provider that has nothing selectable yet: connect it (sign in / paste key). Shown on the
    /// Subscriptions view below the rails.
    ConnectProvider {
        provider_id: String,
        copy: String,
        credential_url: Option<String>,
    },
    /// A free model behind the OpenCode engine.
    Engine(EngineModel),
    /// The labeled optional xAI card (Subscriptions view, last).
    XaiOptional,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelsRow {
    pub kind: RowKind,
    /// Provider display name; rows of one provider are contiguous.
    pub group: String,
    /// Full badge such as `Free · No sign-in · Shared pool · may log/train` (first detail line).
    pub badge: String,
    pub class: ConnectionClass,
}

impl ModelsRow {
    pub fn id(&self) -> String {
        match &self.kind {
            RowKind::Catalog { model, .. } => model.key(),
            RowKind::ConnectProvider { provider_id, .. } => format!("connect:{provider_id}"),
            RowKind::Engine(m) => m.row_id(),
            RowKind::XaiOptional => XAI_ROW_ID.into(),
        }
    }
    /// Row name: the model name, or the connect action for a provider without a credential.
    pub fn title(&self) -> String {
        match &self.kind {
            RowKind::Catalog { model, .. } => model.display_name.clone(),
            RowKind::ConnectProvider { copy, .. } => copy.clone(),
            RowKind::Engine(m) => m.name.clone(),
            RowKind::XaiOptional => "xAI (optional)".into(),
        }
    }
    /// Provider column of the row line.
    pub fn provider(&self) -> &str {
        &self.group
    }
    /// Short badge for the row line: `free` / `free · key` / `key` / `connect` / `API key` /
    /// `optional`.
    pub fn short_badge(&self) -> &'static str {
        match &self.kind {
            RowKind::Catalog { model, locked } => {
                if *locked {
                    "key needed"
                } else if model.is_keyless() {
                    "free"
                } else if model.is_free() {
                    "free · key"
                } else {
                    "key"
                }
            }
            RowKind::Engine(_) => "free",
            RowKind::ConnectProvider { provider_id, .. } => match provider_id.as_str() {
                "openrouter" | "opencode" => "connect",
                _ => "API key",
            },
            RowKind::XaiOptional => "optional",
        }
    }
    /// Whether the row belongs to the Models view (selectable model) rather than Subscriptions.
    pub fn is_model(&self) -> bool {
        matches!(self.kind, RowKind::Catalog { .. } | RowKind::Engine(_))
    }
    pub fn is_xai(&self) -> bool {
        matches!(self.kind, RowKind::XaiOptional)
    }
    /// The provider whose model list this row belongs to (`opencode-engine` for engine rows);
    /// `None` for the xAI card.
    pub fn provider_id(&self) -> Option<&str> {
        match &self.kind {
            RowKind::Catalog { model, .. } => Some(model.provider_id.as_str()),
            RowKind::ConnectProvider { provider_id, .. } => Some(provider_id.as_str()),
            RowKind::Engine(_) => Some(ENGINE_PROVIDER_ID),
            RowKind::XaiOptional => None,
        }
    }
}

/// Everything the host feeds into the picker once its async loaders finish.
#[derive(Debug, Clone, Default)]
pub struct PickerSnapshot {
    /// Local rows first, then hosted providers in manifest order (from `Catalog::picker_groups`),
    /// then the engine rows, then the connect rows and the xAI card (see [`models_rows`]).
    pub rows: Vec<ModelsRow>,
    pub rails: Vec<RailState>,
    /// The plan's first-run default for Direct API connections (kept for the CLI text).
    pub default_selection: Option<DefaultSelection>,
    /// Secret backend name for the review line (`keyring`, `file`, …).
    pub secret_backend: Option<&'static str>,
    /// Where each model list came from and when (hosted providers and the engine, keyed by
    /// provider id); rendered as `fetched 3 min ago` / `cached list from <date>` with the rows.
    pub catalog_status: Vec<CatalogStatus>,
    /// This snapshot is the result of a live refresh (clears the picker's `refresh_pending`).
    pub live: bool,
}

/// Build every picker row for a snapshot; [`PickerState::apply_snapshot`] splits them into the
/// Models view (catalog + engine models) and the Subscriptions view (connect rows + xAI card).
///
/// `catalog` already contains detected local rows; `connected(provider_id)` comes from the broker;
/// `engine_models` is the cached / live engine catalog (empty → the Big Pickle seed row).
pub fn models_rows(
    catalog: &workshop_providers::Catalog,
    connected: impl Fn(&str) -> bool,
    engine_models: &[EngineModel],
) -> Vec<ModelsRow> {
    let mut rows = Vec::new();
    // OpenCode free tier first: it is the first-run default. Only through the genuine client, so
    // it is an engine group, not Direct API.
    let engine: Vec<EngineModel> = if engine_models.is_empty() {
        vec![EngineModel::big_pickle_seed()]
    } else {
        engine_models.to_vec()
    };
    for m in engine {
        rows.push(ModelsRow {
            kind: RowKind::Engine(m),
            group: ENGINE_DISPLAY_NAME.into(),
            badge: "Free · Agent adapter · official opencode CLI · shared pool".into(),
            class: ConnectionClass::AgentAdapter,
        });
    }
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
    rows.push(ModelsRow {
        kind: RowKind::XaiOptional,
        group: "xAI".into(),
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
    /// Switch views (Tab / Left / Right).
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
    /// Picker closed (Esc); host returns to the previous view.
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
    /// Models view: one row per usable model (catalog + engine).
    pub rows: Vec<ModelsRow>,
    /// Subscriptions view, below the rails: providers to connect and the optional xAI card.
    pub auth_rows: Vec<ModelsRow>,
    pub rails: Vec<RailState>,
    /// Selected index into `rows`.
    pub models_selected: usize,
    /// Selected index on the Subscriptions view: `0..rails.len()` is a rail, then `auth_rows`.
    pub rail_selected: usize,
    /// Selected model radio of a Ready rail.
    pub rail_model_selected: usize,
    /// A Ready rail's model radios are open (↑/↓ move between them).
    pub detail_open: bool,
    /// The user acknowledged the xAI card once; a second Enter starts the flow.
    pub xai_armed: bool,
    /// Loaders still running (rows may be partial).
    pub loading: bool,
    /// A live refresh of the model lists is in flight; the rows shown are the cached ones.
    pub refresh_pending: bool,
    /// Transient status line (errors, progress).
    pub status: Option<String>,
    pub key_entry: Option<KeyEntry>,
    /// Row id of the active connection (marked `active`, preselected on the Models view).
    pub active_id: Option<String>,
    pub default_selection: Option<DefaultSelection>,
    pub secret_backend: Option<&'static str>,
    /// Freshness of every model list on the Models view (see [`Self::catalog_note`]).
    pub catalog_status: Vec<CatalogStatus>,
}

impl Default for PickerState {
    fn default() -> Self {
        Self::new()
    }
}

impl PickerState {
    /// Fresh picker before any loader ran: the engine seed row, the connect rows, rails Detecting.
    pub fn new() -> Self {
        let mut s = Self {
            tab: PickerTab::Models,
            rows: Vec::new(),
            auth_rows: Vec::new(),
            rails: Rail::ALL.iter().map(|r| RailState::detecting(*r)).collect(),
            models_selected: 0,
            rail_selected: 0,
            rail_model_selected: 0,
            detail_open: false,
            xai_armed: false,
            loading: true,
            refresh_pending: false,
            status: None,
            key_entry: None,
            active_id: None,
            default_selection: None,
            secret_backend: None,
            catalog_status: Vec::new(),
        };
        s.set_rows(models_rows(
            &workshop_providers::Catalog::default(),
            |_| false,
            &[],
        ));
        s
    }

    /// Open directly on a view (`/model` → Models, `/auth` → Subscriptions).
    pub fn with_tab(mut self, tab: PickerTab) -> Self {
        self.tab = tab;
        self
    }

    /// Mark (and preselect) the row of the active connection.
    pub fn with_active(mut self, active_id: Option<String>) -> Self {
        self.active_id = active_id;
        self.select_active();
        self
    }

    fn set_rows(&mut self, all: Vec<ModelsRow>) {
        let (models, auth): (Vec<_>, Vec<_>) = all.into_iter().partition(ModelsRow::is_model);
        self.rows = models;
        self.auth_rows = auth;
    }

    fn select_active(&mut self) {
        if let Some(id) = &self.active_id
            && let Some(idx) = self.rows.iter().position(|r| r.id() == *id)
        {
            self.models_selected = idx;
        }
    }

    /// Replace rows and rails with a finished snapshot. Keeps the selection on the same row id
    /// when possible (first load: the active row), and never lands on the xAI card.
    pub fn apply_snapshot(&mut self, snap: PickerSnapshot) {
        let first_load = self.loading;
        let prev_model = self.selected_row().map(ModelsRow::id);
        let prev_auth = self.selected_auth_row().map(ModelsRow::id);
        self.set_rows(snap.rows);
        if !snap.rails.is_empty() {
            self.rails = snap.rails;
        }
        self.default_selection = snap.default_selection;
        self.secret_backend = snap.secret_backend;
        self.catalog_status = snap.catalog_status;
        if snap.live {
            self.refresh_pending = false;
        }
        self.loading = false;
        self.models_selected = prev_model
            .filter(|_| !first_load)
            .and_then(|id| self.rows.iter().position(|r| r.id() == id))
            .unwrap_or(0);
        if first_load {
            self.select_active();
        }
        if let Some(id) = prev_auth
            && let Some(idx) = self.auth_rows.iter().position(|r| r.id() == id)
        {
            self.rail_selected = self.rails.len() + idx;
        }
        self.rail_selected = self
            .rail_selected
            .min(self.subscriptions_len().saturating_sub(1));
        if self.selected_auth_row().is_some_and(ModelsRow::is_xai) && first_load {
            self.rail_selected = 0;
        }
        self.rail_model_selected = 0;
        self.xai_armed = false;
    }

    pub fn selected_row(&self) -> Option<&ModelsRow> {
        self.rows.get(self.models_selected)
    }

    /// Number of entries on the Subscriptions view (rails + connect rows + xAI).
    pub fn subscriptions_len(&self) -> usize {
        self.rails.len() + self.auth_rows.len()
    }

    /// The selected rail, when the Subscriptions selection is on one.
    pub fn selected_rail(&self) -> Option<&RailState> {
        self.rails.get(self.rail_selected)
    }

    /// The selected connect row / xAI card, when the Subscriptions selection is below the rails.
    pub fn selected_auth_row(&self) -> Option<&ModelsRow> {
        self.rail_selected
            .checked_sub(self.rails.len())
            .and_then(|i| self.auth_rows.get(i))
    }

    pub fn selected_rail_model(&self) -> Option<&workshop_detect::ModelRef> {
        self.selected_rail()
            .and_then(|r| r.models.get(self.rail_model_selected))
    }

    /// Whether `row` is the active connection.
    pub fn is_active(&self, row: &ModelsRow) -> bool {
        self.active_id.as_deref() == Some(row.id().as_str())
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some(msg.into());
    }

    /// Freshness of `provider_id`'s model list (`opencode-engine` for the engine rows); `None`
    /// for providers without a list.
    pub fn catalog_status(&self, provider_id: &str) -> Option<&CatalogStatus> {
        self.catalog_status
            .iter()
            .find(|s| s.provider_id == provider_id)
    }

    /// The freshness note for `provider_id`'s list: `fetched 3 min ago`, or `cached list from
    /// <date>` while the compiled seed stands in (` · refresh failed` when a fetch was tried).
    pub fn catalog_note(&self, provider_id: &str) -> Option<String> {
        self.catalog_status(provider_id).map(CatalogStatus::note)
    }

    /// One line naming every model list and its freshness, engine first, in snapshot order:
    /// `Lists: OpenCode fetched just now · Kilo Gateway fetched 2 min ago · …`.
    pub fn catalog_summary(&self) -> Option<String> {
        if self.catalog_status.is_empty() {
            return None;
        }
        let parts: Vec<String> = self
            .catalog_status
            .iter()
            .map(|s| {
                let name = if s.provider_id == ENGINE_PROVIDER_ID {
                    ENGINE_DISPLAY_NAME.to_owned()
                } else {
                    workshop_providers::manifest(&s.provider_id)
                        .map(|m| m.display_name)
                        .unwrap_or_else(|| s.provider_id.clone())
                };
                format!("{name} {}", s.note())
            })
            .collect();
        Some(format!("Lists: {}", parts.join(" · ")))
    }

    /// Open the key-entry prompt for a provider (host calls this for paste-key connect flows).
    pub fn begin_key_entry(&mut self, provider_id: &str, label: &str) {
        self.key_entry = Some(KeyEntry {
            provider_id: provider_id.to_owned(),
            label: label.to_owned(),
            buffer: String::new(),
        });
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
                PickerOutcome::Changed
            }
            (PickerTab::Models, PickerInput::Down) => {
                if self.models_selected + 1 < self.rows.len() {
                    self.models_selected += 1;
                }
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
                self.xai_armed = false;
                PickerOutcome::Changed
            }
            (PickerTab::Subscriptions, PickerInput::Down) => {
                let models = self.selected_rail().map(|r| r.models.len()).unwrap_or(0);
                if self.detail_open && self.rail_model_selected + 1 < models {
                    self.rail_model_selected += 1;
                } else if !self.detail_open && self.rail_selected + 1 < self.subscriptions_len() {
                    self.rail_selected += 1;
                    self.rail_model_selected = 0;
                }
                self.xai_armed = false;
                PickerOutcome::Changed
            }
            (_, PickerInput::Back) => {
                if self.detail_open || self.xai_armed {
                    self.detail_open = false;
                    self.xai_armed = false;
                    PickerOutcome::Changed
                } else {
                    PickerOutcome::Close
                }
            }
            (PickerTab::Models, PickerInput::Enter) => match self.selected_row().cloned() {
                Some(row) => self.enter_row(row),
                None => PickerOutcome::Changed,
            },
            (PickerTab::Subscriptions, PickerInput::Enter) => {
                if let Some(row) = self.selected_auth_row().cloned() {
                    return self.enter_row(row);
                }
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
                } else if rail.show_connect {
                    PickerOutcome::RailConnect(rail.rail)
                } else {
                    PickerOutcome::Changed
                }
            }
        }
    }

    /// Enter on a model / connect / xAI row (same rules on both views).
    fn enter_row(&mut self, row: ModelsRow) -> PickerOutcome {
        match row.kind {
            RowKind::XaiOptional => {
                // Two-step: first Enter shows the labeled copy, second Enter starts the flow.
                if self.xai_armed {
                    PickerOutcome::StartOptionalXaiLogin
                } else {
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

    /// One to three lines about the highlighted entry (what happens, where traffic goes).
    /// Never a secret.
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
        let note = |row: &ModelsRow| row.provider_id().and_then(|id| self.catalog_note(id));
        let lines = match self.tab {
            PickerTab::Models => self
                .selected_row()
                .map(|row| row_detail_lines(row, self.xai_armed, note(row)))
                .unwrap_or_default(),
            PickerTab::Subscriptions => match self.selected_auth_row() {
                Some(row) => row_detail_lines(row, self.xai_armed, note(row)),
                None => self
                    .selected_rail()
                    .map(|r| rail_detail_lines(r, self.rail_model_selected))
                    .unwrap_or_default(),
            },
        };
        lines.into_iter().take(3).collect()
    }
}

/// Path of the user config file the picker writes (`$WORKSHOP_HOME/config.toml`).
pub fn config_path() -> PathBuf {
    xai_dirs::resolve_grok_home()
        .unwrap_or_else(xai_dirs::default_grok_home)
        .join("config.toml")
}

/// `list_note` is the freshness of the row's model list (`fetched 3 min ago` / `cached list from
/// <date>`), shown with the badge so every row says where and when its list came from.
fn row_detail_lines(row: &ModelsRow, xai_armed: bool, list_note: Option<String>) -> Vec<String> {
    let mut lines = Vec::new();
    match &row.kind {
        RowKind::Catalog { model, locked } => {
            lines.push(match &list_note {
                Some(note) => format!("{} · list {note}", row.badge),
                None => row.badge.clone(),
            });
            let mut facts = vec![format!("Endpoint: {}", model.endpoint())];
            if let Some(tools) = model.tools {
                facts.push(format!(
                    "tool calling: {}",
                    if tools { "yes" } else { "not advertised" }
                ));
            }
            if let Some(ctx) = model.context_window {
                facts.push(format!("{ctx} tokens"));
            }
            lines.push(facts.join(" · "));
            if *locked {
                lines.push("Needs a credential first — Enter connects the provider.".into());
            } else if let Some(note) = &model.note {
                lines.push(note.clone());
            }
        }
        RowKind::ConnectProvider {
            provider_id,
            credential_url,
            ..
        } => {
            lines.push(row.badge.clone());
            match provider_id.as_str() {
                "openrouter" => lines.push(
                    "Enter starts OpenRouter's sign-in in your browser; the key it issues goes to your OS keyring."
                        .into(),
                ),
                _ => lines.push(
                    "Enter opens a key prompt; the key goes to your OS keyring, never to config.toml."
                        .into(),
                ),
            }
            if let Some(url) = credential_url {
                lines.push(format!("Get a key: {url}"));
            }
        }
        RowKind::Engine(_) => {
            lines.push(
                "Free shared pool via the official opencode CLI, installed on your first message."
                    .into(),
            );
            lines.push(
                "Loopback only; opencode.ai is contacted by opencode itself. Prompts may be logged upstream."
                    .into(),
            );
            if let Some(note) = &list_note {
                lines.push(match note.starts_with("fetched") {
                    true => format!("Model list {note} from {ENGINE_CATALOG_SOURCE}."),
                    false => {
                        format!("Model list: {note}; the live list arrives when the engine starts.")
                    }
                });
            }
        }
        RowKind::XaiOptional => {
            lines.push(XAI_CARD_COPY.into());
            lines.push(
                "Enter opens auth.x.ai in your browser (the only Workshop path that contacts x.ai)."
                    .into(),
            );
            if xai_armed {
                lines.push("Press Enter again to continue to auth.x.ai, or Esc to go back.".into());
            }
        }
    }
    lines
}

fn rail_detail_lines(rail: &RailState, selected_model: usize) -> Vec<String> {
    let mut lines = Vec::new();
    if rail.pill == Pill::Detecting {
        lines.push("Looking for the official CLI on PATH and in the usual install folders…".into());
    } else if !rail.installed {
        lines.push(format!(
            "Workshop looks for `{}`; it never reads another app's login files.",
            rail.rail.vendor().binary_names().join("` or `")
        ));
    } else if !rail.is_ready() {
        lines.push(format!(
            "Enter runs the official login in your terminal:  {}",
            workshop_detect::login_argv(rail.rail.vendor()).join(" ")
        ));
    } else {
        let radios: Vec<String> = rail
            .models
            .iter()
            .enumerate()
            .map(|(i, m)| {
                format!(
                    "{} {}",
                    if i == selected_model { "(•)" } else { "( )" },
                    m.display()
                )
            })
            .collect();
        lines.push(radios.join("  "));
        lines.push(
            "Enter picks the model; turns run through the official CLI in an isolated worktree."
                .into(),
        );
    }
    if let Some(copy) = rail.empty_copy {
        lines.push(copy.to_owned());
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
            ..PickerSnapshot::default()
        });
        p
    }

    fn select_row(p: &mut PickerState, needle: &str) {
        p.models_selected = p
            .rows
            .iter()
            .position(|r| r.title().contains(needle))
            .unwrap_or_else(|| panic!("row {needle:?}"));
    }

    #[test]
    fn detail_lines_say_where_and_when_each_list_came_from() {
        let mut p = PickerState::new();
        let rows = models_rows(&workshop_providers::Catalog::builtin(), |_| false, &[]);
        let now = workshop_providers::catalog::fetch::now_secs();
        p.apply_snapshot(PickerSnapshot {
            rows,
            catalog_status: vec![
                CatalogStatus::seed(ENGINE_PROVIDER_ID, 1),
                CatalogStatus {
                    provider_id: "kilo".into(),
                    freshness: Freshness::Live,
                    fetched_at_secs: Some(now - 3 * 60),
                    rows: 21,
                    error: None,
                },
                CatalogStatus {
                    provider_id: "openrouter".into(),
                    freshness: Freshness::Seed,
                    fetched_at_secs: None,
                    rows: 5,
                    error: Some("HTTP 502".into()),
                },
            ],
            live: true,
            ..PickerSnapshot::default()
        });
        // Engine seed: dated, and honest about when the live list comes.
        select_row(&mut p, "Big Pickle");
        let lines = p.detail_lines();
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(
            lines[2].contains("cached list from 2026-09-21")
                && lines[2].contains("live list arrives when the engine starts"),
            "{lines:?}"
        );
        // Live Kilo rows: the badge line carries the age.
        select_row(&mut p, "Auto Free");
        let lines = p.detail_lines();
        assert!(lines[0].ends_with("· list fetched 3 min ago"), "{lines:?}");
        assert_eq!(p.catalog_note("kilo").as_deref(), Some("fetched 3 min ago"));
        // A failed refresh says so on the seed it fell back to.
        assert_eq!(
            p.catalog_note("openrouter").as_deref(),
            Some("cached list from 2026-09-21 · refresh failed")
        );
        assert!(p.catalog_note("google").is_none());
        let summary = p.catalog_summary().unwrap();
        assert!(
            summary.starts_with("Lists: OpenCode cached list from 2026-09-21 · Kilo Gateway fetched 3 min ago · OpenRouter cached list from 2026-09-21 · refresh failed"),
            "{summary}"
        );
        // An engine list that was fetched names its source.
        p.catalog_status = vec![CatalogStatus {
            provider_id: ENGINE_PROVIDER_ID.into(),
            freshness: Freshness::Cached,
            fetched_at_secs: Some(now),
            rows: 7,
            error: None,
        }];
        select_row(&mut p, "Big Pickle");
        let lines = p.detail_lines();
        assert_eq!(
            lines[2],
            format!("Model list fetched just now from {ENGINE_CATALOG_SOURCE}.")
        );
    }

    #[test]
    fn a_live_snapshot_clears_refresh_pending_and_a_cached_one_does_not() {
        let mut p = loaded();
        p.refresh_pending = true;
        let rows = models_rows(&workshop_providers::Catalog::builtin(), |_| false, &[]);
        p.apply_snapshot(PickerSnapshot {
            rows: rows.clone(),
            ..PickerSnapshot::default()
        });
        assert!(p.refresh_pending, "the cached snapshot lands first");
        p.apply_snapshot(PickerSnapshot {
            rows,
            live: true,
            ..PickerSnapshot::default()
        });
        assert!(!p.refresh_pending);
        assert!(p.catalog_summary().is_none(), "no status, no summary line");
    }

    fn goto_xai(p: &mut PickerState) {
        if p.tab != PickerTab::Subscriptions {
            p.handle(PickerInput::SwitchTab);
        }
        for _ in 0..p.subscriptions_len() {
            p.handle(PickerInput::Down);
        }
        assert!(p.selected_auth_row().is_some_and(ModelsRow::is_xai));
    }

    #[test]
    fn models_view_lists_models_only_and_engine_default_first() {
        let p = loaded();
        assert!(p.rows.iter().all(ModelsRow::is_model));
        assert!(
            matches!(p.rows.first().map(|r| &r.kind), Some(RowKind::Engine(m)) if m.is_default),
            "the engine default heads the Models view"
        );
        assert!(!p.rows.iter().any(ModelsRow::is_xai));
        // Fresh (unloaded) state: the seed row is already there for the overlay.
        let fresh = PickerState::new();
        assert!(matches!(
            fresh.rows.first().map(|r| &r.kind),
            Some(RowKind::Engine(_))
        ));
    }

    #[test]
    fn subscriptions_view_is_rails_then_connect_rows_then_xai_last() {
        let p = loaded();
        let ids: Vec<_> = p.rails.iter().map(|r| r.rail).collect();
        assert_eq!(ids, vec![Rail::Claude, Rail::Codex, Rail::Cursor]);
        assert!(p.rails.iter().all(|r| r.pill == Pill::Detecting));
        assert!(p.auth_rows.last().is_some_and(ModelsRow::is_xai));
        assert!(
            p.auth_rows.iter().any(|r| matches!(&r.kind, RowKind::ConnectProvider { provider_id, .. } if provider_id == "openrouter")),
            "openrouter shows a connect row when no key is connected"
        );
        assert_eq!(p.subscriptions_len(), 3 + p.auth_rows.len());
        assert_eq!(p.rail_selected, 0, "never preselects the xAI card");
    }

    #[test]
    fn kilo_free_is_a_selectable_model_row() {
        let p = loaded();
        let kilo = p
            .rows
            .iter()
            .find(|r| matches!(&r.kind, RowKind::Catalog { model, .. } if model.model_id == "kilo-auto/free"))
            .expect("kilo row");
        assert!(matches!(kilo.kind, RowKind::Catalog { locked: false, .. }));
        assert_eq!(kilo.short_badge(), "free");
        let engine = p.rows.first().expect("engine seed row");
        assert_eq!(engine.class, ConnectionClass::AgentAdapter);
        assert_eq!(engine.provider(), "OpenCode");
        assert_eq!(engine.title(), "Big Pickle");
    }

    #[test]
    fn active_row_is_marked_and_preselected() {
        let active = EngineModel::big_pickle_seed().row_id();
        let mut p = PickerState::new().with_active(Some(active.clone()));
        p.handle(PickerInput::Down);
        p.handle(PickerInput::Down);
        let rows = models_rows(&workshop_providers::Catalog::builtin(), |_| false, &[]);
        p.apply_snapshot(PickerSnapshot {
            rows,
            ..PickerSnapshot::default()
        });
        assert_eq!(p.selected_row().map(ModelsRow::id), Some(active));
        assert!(p.selected_row().is_some_and(|r| p.is_active(r)));
    }

    #[test]
    fn xai_login_requires_two_explicit_enters_and_disarms_on_move() {
        let mut p = loaded();
        goto_xai(&mut p);
        assert_eq!(p.handle(PickerInput::Enter), PickerOutcome::Changed);
        assert!(p.xai_armed);
        assert!(
            p.detail_lines()
                .iter()
                .any(|l| l.contains("Press Enter again"))
        );
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
    fn esc_disarms_xai_before_closing() {
        let mut p = loaded();
        goto_xai(&mut p);
        p.handle(PickerInput::Enter);
        assert_eq!(p.handle(PickerInput::Back), PickerOutcome::Changed);
        assert!(!p.xai_armed);
        assert_eq!(p.handle(PickerInput::Back), PickerOutcome::Close);
    }

    #[test]
    fn non_xai_rows_never_start_a_login() {
        let mut p = loaded();
        for i in 0..p.rows.len() {
            p.models_selected = i;
            p.xai_armed = false;
            let out = p.handle(PickerInput::Enter);
            assert_ne!(out, PickerOutcome::StartOptionalXaiLogin, "row {i}");
        }
        p.handle(PickerInput::SwitchTab);
        for i in 0..p.subscriptions_len() {
            p.rail_selected = i;
            p.detail_open = false;
            p.xai_armed = false;
            if p.selected_auth_row().is_some_and(ModelsRow::is_xai) {
                continue;
            }
            let out = p.handle(PickerInput::Enter);
            assert_ne!(out, PickerOutcome::StartOptionalXaiLogin, "entry {i}");
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
    fn first_run_default_mirrors_the_engine_catalog() {
        assert_eq!(
            EngineModel::first_run_default(&[]),
            EngineModel::big_pickle_seed()
        );
        let live = vec![
            EngineModel {
                model_ref: "opencode/other".into(),
                name: "Other".into(),
                is_default: false,
                tool_call: true,
                context_limit: None,
            },
            EngineModel {
                model_ref: "opencode/new-default".into(),
                name: "New Default".into(),
                is_default: true,
                tool_call: true,
                context_limit: None,
            },
        ];
        assert_eq!(
            EngineModel::first_run_default(&live).model_ref,
            "opencode/new-default"
        );
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
    fn connect_row_on_subscriptions_opens_provider_connect() {
        let mut p = loaded();
        p.handle(PickerInput::SwitchTab);
        for _ in 0..p.rails.len() {
            p.handle(PickerInput::Down);
        }
        let row = p.selected_auth_row().expect("first connect row").clone();
        let RowKind::ConnectProvider { provider_id, .. } = &row.kind else {
            panic!("expected a connect row, got {row:?}");
        };
        assert_eq!(
            p.handle(PickerInput::Enter),
            PickerOutcome::ConnectProvider(provider_id.clone())
        );
    }

    #[test]
    fn detail_is_at_most_three_lines_everywhere() {
        let mut p = loaded();
        for i in 0..p.rows.len() {
            p.models_selected = i;
            let n = p.detail_lines().len();
            assert!((1..=3).contains(&n), "row {i}: {n} lines");
        }
        p.handle(PickerInput::SwitchTab);
        for i in 0..p.subscriptions_len() {
            p.rail_selected = i;
            let n = p.detail_lines().len();
            assert!((1..=3).contains(&n), "entry {i}: {n} lines");
        }
    }

    #[test]
    fn esc_closes_the_picker() {
        let mut p = loaded();
        assert_eq!(p.handle(PickerInput::Back), PickerOutcome::Close);
    }
}
