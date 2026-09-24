//! Workshop connection picker: the one overlay behind `/model` and `/auth`.
//!
//! Workshop starts with the OpenCode engine's default free model active, so the picker is never
//! the first screen. `/model` opens it on the active model; `/auth` (alias `/login`) opens the
//! same list on its Subscriptions section. There is one view: OpenCode's free models first (one
//! row per model; a model with effort levels opens a sub-menu of its levels), then any API-key
//! provider with a key configured (its own group), then the **Subscriptions** section — one row
//! per vendor, Claude / Codex / Cursor, whose row styling carries the state (`sign in`,
//! `install`, `✓ Max ▸`) and which opens into that vendor's real model list when signed in — then
//! `API keys ▸` (the providers to connect) and the labelled optional xAI row last.
//!
//! The picker never starts an OAuth flow by itself. The optional xAI row is the single entry
//! point to the inherited xAI OIDC flow, and only after the user selects it twice.
//!
//! Kilo Gateway is never listed ([`HIDDEN_PROVIDERS`]): it is Workshop's silent fallback when the
//! OpenCode model cannot answer, and the user only ever sees the answering model's name.
//!
//! Data sources: model rows come from [`workshop_providers`] (local servers, connected
//! providers) plus the OpenCode free catalog; vendor state comes from [`workshop_detect`] (the
//! single detection source). This crate holds the pure picker policy and the `[model.<key>]`
//! config writer; the host renders the state, feeds it [`PickerInput`], and executes
//! [`PickerOutcome`]s.
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

/// Where the picker opens: `/model` lands on the active model (with any typed text already in
/// the filter), `/auth` on the first subscription row. Same list either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerFocus {
    Models { filter: String },
    Subscriptions,
}

impl Default for PickerFocus {
    fn default() -> Self {
        Self::Models {
            filter: String::new(),
        }
    }
}

/// The picker's title; a sub-menu appends its name (`Models › Claude`).
pub const PICKER_TITLE: &str = "Models";
/// Group header over the vendor rows.
pub const SUBSCRIPTIONS_HEADER: &str = "Subscriptions";
/// Title of the row that opens the API-key providers.
pub const API_KEYS_TITLE: &str = "API keys";

pub const XAI_ROW_ID: &str = "xai_optional";
/// Copy on the optional xAI row, verbatim from the plan.
pub const XAI_CARD_COPY: &str = "Uses xAI accounts and auth.x.ai. Not required.";
/// Provider id of the OpenCode engine rows (free tier reachable only through the genuine client).
pub const ENGINE_PROVIDER_ID: &str = "opencode-engine";
/// Provider display name of the OpenCode engine rows and the composer label prefix.
pub const ENGINE_DISPLAY_NAME: &str = "OpenCode";
/// Where the engine rows come from when they are live: the running `opencode serve`.
pub const ENGINE_CATALOG_SOURCE: &str = "opencode serve /config/providers";
/// The one footer line under an OpenCode model: what it costs and where prompts go, no plumbing.
pub const ENGINE_ROW_NOTE: &str = "Free · no sign-in · the provider may log prompts";
/// Providers that exist in the catalog code but are never listed on `/model` or `/auth` and never
/// fetched for the picker (owner decision, v0.2.2): the Kilo Gateway community pool, which only
/// serves as the silent fallback behind the OpenCode default.
pub const HIDDEN_PROVIDERS: [&str; 1] = ["kilo"];

/// Whether `provider_id` is hidden from every picker surface.
pub fn is_hidden_provider(provider_id: &str) -> bool {
    HIDDEN_PROVIDERS.contains(&provider_id)
}

/// The model name as the composer shows it: no vendor prefix, no `(free)` suffix —
/// `NVIDIA: Nemotron 3 Super (free)` reads `Nemotron 3 Super`.
pub fn plain_model_name(name: &str) -> String {
    let mut out = name.trim();
    if let Some((vendor, rest)) = out.split_once(": ")
        && !vendor.is_empty()
        && !vendor.contains(' ')
    {
        out = rest.trim();
    }
    let lower = out.to_ascii_lowercase();
    if let Some(stripped) = lower.strip_suffix("(free)") {
        out = out[..stripped.len()].trim_end();
    }
    out.to_owned()
}

/// Name fragments (lowercase) of models that are not chat models: classifiers, guard rails,
/// routers, rerankers, embeddings. Matched against display names and ids.
const NON_CHAT_MARKERS: [&str; 10] = [
    "content safety",
    "safety",
    "guard",
    "router",
    "rerank",
    "embed",
    "classif",
    "moderation",
    "topic control",
    "jailbreak",
];

/// Whether a model name reads as a chat model (see [`NON_CHAT_MARKERS`]).
pub fn is_chat_model_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    !NON_CHAT_MARKERS.iter().any(|m| lower.contains(m))
}

/// A free model served through the OpenCode engine (`opencode serve`), mirrored from its catalog.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EngineModel {
    /// `opencode/<id>`, the form the engine's prompt API takes.
    pub model_ref: String,
    pub name: String,
    pub is_default: bool,
    pub tool_call: bool,
    pub context_limit: Option<u64>,
    /// The effort / reasoning levels OpenCode's catalog offers for this model (`low`, `medium`,
    /// `high`, …), lowest first; empty for a model without levels.
    #[serde(default)]
    pub variants: Vec<String>,
    /// The level the user picked, one of `variants`; `None` runs the model at its own default.
    /// Lives on the model so the active connection, the picker row and the prompt agree on it.
    #[serde(default)]
    pub effort: Option<String>,
    /// The model can see images (the engine catalog's `capabilities.input.image`).
    #[serde(default)]
    pub image_input: bool,
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
            variants: Vec::new(),
            effort: None,
            image_input: false,
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

    /// Picker row id of this model (`opencode-engine:opencode/<id>`, `…/<id>@<effort>` for a
    /// picked level).
    pub fn row_id(&self) -> String {
        match &self.effort {
            Some(effort) => format!("{ENGINE_PROVIDER_ID}:{}@{effort}", self.model_ref),
            None => format!("{ENGINE_PROVIDER_ID}:{}", self.model_ref),
        }
    }

    /// Row id of the model whatever level is picked (`opencode-engine:opencode/<id>`).
    pub fn base_row_id(&self) -> String {
        format!("{ENGINE_PROVIDER_ID}:{}", self.model_ref)
    }

    /// The composer / row text, upstream's format: `Big Pickle`, `Ling 3.0 Flash Fin Free (high)`.
    pub fn display(&self) -> String {
        match &self.effort {
            Some(effort) => format!("{} ({effort})", self.name),
            None => self.name.clone(),
        }
    }

    /// This model at `effort` (a level of `variants`), or at its own default for `None`.
    pub fn with_effort(&self, effort: Option<&str>) -> Self {
        Self {
            effort: effort.map(str::to_owned),
            ..self.clone()
        }
    }

    /// A newer catalog entry for the same model keeps the level the user picked, as long as the
    /// model still offers it; another model, or a dropped level, starts at the default.
    pub fn carrying_effort_from(mut self, previous: &EngineModel) -> Self {
        if previous.model_ref == self.model_ref
            && let Some(effort) = &previous.effort
            && self.variants.iter().any(|v| v == effort)
        {
            self.effort = Some(effort.clone());
        }
        self
    }
}

/// Picker row id of a subscription model (`claude:anthropic:sonnet`); the active connection
/// carries the same id so the picker can mark it.
pub fn rail_model_row_id(rail: Rail, model: &workshop_detect::ModelRef) -> String {
    format!("{}:{}", rail.vendor().id(), model.key())
}

/// One picker row.
#[derive(Debug, Clone, PartialEq)]
pub enum RowKind {
    /// A Direct API / Local catalog row. `locked` means the provider still needs a credential.
    Catalog { model: CatalogModel, locked: bool },
    /// A provider that has nothing selectable yet: connect it (sign in / paste key). Lives in the
    /// `API keys` sub-menu.
    ConnectProvider {
        provider_id: String,
        copy: String,
        credential_url: Option<String>,
    },
    /// A free model behind the OpenCode engine: one row per model (`effort` is `None` here; a
    /// model with levels opens the [`RowKind::Effort`] sub-menu).
    Engine(EngineModel),
    /// One level of an OpenCode model's effort sub-menu (`effort` set), or its own default (`None`).
    Effort(EngineModel),
    /// A model of a signed-in subscription CLI (its vendor's sub-menu, or the filtered flat list).
    RailModel {
        rail: Rail,
        model: workshop_detect::ModelRef,
    },
    /// A subscription vendor — Claude / Codex / Cursor. Its state comes from the picker's rails;
    /// signed in it opens into the CLI's own model list, signed out Enter runs the official login,
    /// not installed Enter runs the official installer.
    Vendor(Rail),
    /// The row that opens the API-key providers to connect.
    ApiKeys,
    /// The labeled optional xAI row (last).
    XaiOptional,
}

/// What a signed-in vendor shows while its CLI's model list has not arrived: the detect layer's
/// own copy.
pub const LOADING_MODELS: &str = workshop_detect::copy::LOADING_MODELS;

#[derive(Debug, Clone, PartialEq)]
pub struct ModelsRow {
    pub kind: RowKind,
    /// Group header the row sits under (`OpenCode`, a provider name, `Subscriptions`); rows of
    /// one group are contiguous.
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
            RowKind::Engine(m) => m.base_row_id(),
            RowKind::Effort(m) => m.row_id(),
            RowKind::RailModel { rail, model } => rail_model_row_id(*rail, model),
            RowKind::Vendor(rail) => format!("vendor:{}", rail.vendor().id()),
            RowKind::ApiKeys => "api-keys".into(),
            RowKind::XaiOptional => XAI_ROW_ID.into(),
        }
    }
    /// Row name: the model name; the vendor name; `Provider — Sign in` / `Provider — API key`
    /// for a provider that still has to be connected; a level (`high`) or `Default` in an
    /// effort sub-menu.
    pub fn title(&self) -> String {
        match &self.kind {
            RowKind::Catalog { model, .. } => model.display_name.clone(),
            RowKind::ConnectProvider { .. } => {
                format!("{} \u{2014} {}", self.group, self.connect_action())
            }
            RowKind::Engine(m) => m.name.clone(),
            RowKind::Effort(m) => m.effort.clone().unwrap_or_else(|| "Default".into()),
            RowKind::RailModel { model, .. } => model.display().to_owned(),
            RowKind::Vendor(rail) => rail.display_name().to_owned(),
            RowKind::ApiKeys => API_KEYS_TITLE.into(),
            RowKind::XaiOptional => "xAI".into(),
        }
    }
    /// How a connect row is acted on: a browser sign-in, or a pasted API key.
    pub fn connect_action(&self) -> &'static str {
        match &self.kind {
            RowKind::ConnectProvider { provider_id, .. } => match provider_id.as_str() {
                "openrouter" | "opencode" => "Sign in",
                _ => "API key",
            },
            RowKind::XaiOptional | RowKind::Vendor(_) => "Sign in",
            RowKind::Catalog { .. }
            | RowKind::Engine(_)
            | RowKind::Effort(_)
            | RowKind::RailModel { .. }
            | RowKind::ApiKeys => "",
        }
    }
    /// Provider column of the flat (filtered) list: where the row comes from.
    pub fn provider(&self) -> &str {
        match &self.kind {
            RowKind::Vendor(_) | RowKind::ApiKeys | RowKind::XaiOptional => "",
            _ => &self.group,
        }
    }
    /// Static badge of a model row: `free` / `free · key` / `key` / `key needed`. Rows whose state
    /// is live (vendors) are described by [`PickerState::row_suffix`].
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
            RowKind::XaiOptional => "optional",
            RowKind::Effort(_)
            | RowKind::RailModel { .. }
            | RowKind::ConnectProvider { .. }
            | RowKind::Vendor(_)
            | RowKind::ApiKeys => "",
        }
    }
    /// Whether a model row is a chat model a user would pick to talk to. Classifiers, guard /
    /// safety models, routers, rerankers and embedding models are hidden behind "show all".
    pub fn is_chat_model(&self) -> bool {
        match &self.kind {
            RowKind::Catalog { model, .. } => {
                is_chat_model_name(&model.display_name) && is_chat_model_name(&model.model_id)
            }
            RowKind::Engine(m) | RowKind::Effort(m) => {
                is_chat_model_name(&m.name) && is_chat_model_name(&m.model_ref)
            }
            RowKind::RailModel { .. }
            | RowKind::Vendor(_)
            | RowKind::ApiKeys
            | RowKind::ConnectProvider { .. }
            | RowKind::XaiOptional => true,
        }
    }
    /// Whether the row is a model the user can pick (a leaf), as opposed to a row that opens a
    /// sub-menu or connects something.
    pub fn is_model(&self) -> bool {
        matches!(
            self.kind,
            RowKind::Catalog { .. }
                | RowKind::Engine(_)
                | RowKind::Effort(_)
                | RowKind::RailModel { .. }
        )
    }
    pub fn is_xai(&self) -> bool {
        matches!(self.kind, RowKind::XaiOptional)
    }
    pub fn is_vendor(&self) -> bool {
        matches!(self.kind, RowKind::Vendor(_))
    }
    /// The provider whose model list this row belongs to (`opencode-engine` for engine rows, the
    /// vendor id for subscription rows); `None` for the xAI and API keys rows.
    pub fn provider_id(&self) -> Option<&str> {
        match &self.kind {
            RowKind::Catalog { model, .. } => Some(model.provider_id.as_str()),
            RowKind::ConnectProvider { provider_id, .. } => Some(provider_id.as_str()),
            RowKind::Engine(_) | RowKind::Effort(_) => Some(ENGINE_PROVIDER_ID),
            RowKind::RailModel { rail, .. } | RowKind::Vendor(rail) => Some(rail.vendor().id()),
            RowKind::ApiKeys | RowKind::XaiOptional => None,
        }
    }
}

/// Everything the host feeds into the picker once its async loaders finish.
#[derive(Debug, Clone, Default)]
pub struct PickerSnapshot {
    /// The engine rows, then hosted providers in manifest order (from `Catalog::picker_groups`),
    /// then the vendor rows, the connect rows and the xAI row (see [`models_rows`]).
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

/// Build every picker row for a snapshot: OpenCode's models (one per model), the connected
/// API-key / local providers' models, the three vendor rows, the connect rows (which
/// [`PickerState::apply_snapshot`] moves into the `API keys` sub-menu) and the xAI row.
///
/// `catalog` already contains detected local rows; `connected(provider_id)` comes from the broker;
/// `engine_models` is the cached / live engine catalog (empty → the Big Pickle seed row). The
/// vendors' models are not rows here: they are read from the picker's rails when a vendor opens.
pub fn models_rows(
    catalog: &workshop_providers::Catalog,
    connected: impl Fn(&str) -> bool,
    engine_models: &[EngineModel],
    _rails: &[RailState],
) -> Vec<ModelsRow> {
    let mut rows = Vec::new();
    // OpenCode free tier first: it is the first-run default. Only through the genuine client, so
    // it is an engine group, not Direct API. One row per model; its levels are a sub-menu.
    let engine: Vec<EngineModel> = if engine_models.is_empty() {
        vec![EngineModel::big_pickle_seed()]
    } else {
        engine_models.to_vec()
    };
    for m in engine {
        rows.push(ModelsRow {
            kind: RowKind::Engine(m.with_effort(None)),
            group: ENGINE_DISPLAY_NAME.into(),
            badge: "Free · no sign-in · OpenCode's shared pool".into(),
            class: ConnectionClass::AgentAdapter,
        });
    }
    let mut connect_rows = Vec::new();
    for group in catalog.picker_groups(&connected) {
        if is_hidden_provider(&group.provider_id) {
            continue;
        }
        let class = ConnectionClass::from_provider(group.class);
        // Hosted API-key providers are listed only once a key is configured; until then they
        // are a connect row in the `API keys` sub-menu.
        let listed = class == ConnectionClass::Local || connected(&group.provider_id);
        if group.rows.is_empty() || !listed {
            if let Some(copy) = group.connect_copy.clone() {
                let credential_url = workshop_providers::manifest(&group.provider_id)
                    .and_then(|m| m.credential_url.clone());
                connect_rows.push(ModelsRow {
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
            connect_rows.push(ModelsRow {
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
    // The Subscriptions section: every vendor, installed or not (a missing CLI is one keypress
    // from its official installer), then the API-key providers to connect, then xAI last.
    for rail in Rail::ALL {
        rows.push(ModelsRow {
            kind: RowKind::Vendor(rail),
            group: SUBSCRIPTIONS_HEADER.into(),
            badge: format!(
                "Subscription · through the official {} CLI",
                rail.vendor().display_name()
            ),
            class: ConnectionClass::AgentAdapter,
        });
    }
    if !connect_rows.is_empty() {
        rows.push(ModelsRow {
            kind: RowKind::ApiKeys,
            group: SUBSCRIPTIONS_HEADER.into(),
            badge: "Your own key or account with a hosted provider".into(),
            class: ConnectionClass::DirectApi,
        });
        rows.extend(connect_rows);
    }
    rows.push(ModelsRow {
        kind: RowKind::XaiOptional,
        group: SUBSCRIPTIONS_HEADER.into(),
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
    /// Open the highlighted row's sub-menu (`→`); nothing on a row without one.
    Open,
    /// Leave the open sub-menu (`←`); nothing at the top level.
    Left,
    Enter,
    /// Clear the filter, disarm the xAI row, leave the sub-menu, or close the picker (`Esc`), in
    /// that order; cancels an open key-entry prompt.
    Back,
    /// Text typed: a key-entry prompt takes it as the key, the list as its filter.
    Char(char),
    Backspace,
    /// Paste while a key-entry prompt is open.
    Paste(String),
    /// Refresh live catalogs / re-probe rails (`Ctrl+R`).
    Refresh,
    /// Show / hide the non-chat models (classifiers, routers…) (`Ctrl+A`).
    ToggleShowAll,
}

/// One line of the list as rendered: a group header or a selectable row.
#[derive(Debug, Clone, PartialEq)]
pub enum ModelsLine {
    Header(String),
    Row(Box<ModelsRow>),
}

/// How a piece of a row's state suffix is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Signed in, free, active: the success colour.
    Good,
    /// A state that asks for something (`sign in`, `install`), `optional`, the `▸` marker.
    Dim,
    /// The plan name and other plain facts.
    Plain,
}

/// What the host must do after a key press.
#[derive(Debug, Clone, PartialEq)]
pub enum PickerOutcome {
    /// Redraw only.
    Changed,
    /// Picker closed (Esc); host returns to the previous view.
    Close,
    /// User explicitly selected the labeled optional xAI row twice: the host may start the
    /// inherited xAI OIDC flow. This is the only outcome that leads to `auth.x.ai`.
    StartOptionalXaiLogin,
    /// A Direct API / Local row: write `[model.<key>]`, make it active.
    SelectCatalog(CatalogModel),
    /// A credential-requiring provider needs connecting; the host runs the provider's flow
    /// (OpenRouter PKCE sign-in, or the key-entry prompt the picker opened).
    ConnectProvider(String),
    /// The user pasted a key for `provider_id` (never logged; host saves it in the broker).
    SaveKey { provider_id: String, key: String },
    /// A free model behind the OpenCode engine (at the picked level, or its default): route turns
    /// through the engine.
    SelectEngine(EngineModel),
    /// A signed-out vendor's Enter: run the official CLI login in the user's terminal.
    RailConnect(Rail),
    /// Enter on a vendor whose official CLI is not installed: run the vendor's official installer,
    /// then its sign-in (`RailConnect`). Never started on its own.
    RailInstall(Rail),
    /// A model of a signed-in vendor: route turns through that vendor's adapter.
    SelectRailModel(Rail, workshop_detect::ModelRef),
    /// Re-run the loaders.
    Refresh,
    /// Enter on a signed-in vendor whose CLI could not list its models: ask the CLIs again (child
    /// processes only; the hosted lists are left alone).
    RetryRailModels,
}

/// An open key-entry prompt (value lives only here until saved; never rendered in full).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEntry {
    pub provider_id: String,
    pub label: String,
    pub buffer: String,
}

/// The open sub-menu, one level below the list.
#[derive(Debug, Clone, PartialEq)]
pub enum Submenu {
    /// A signed-in vendor's own model list.
    Vendor(Rail),
    /// An OpenCode model's effort levels.
    Effort(EngineModel),
    /// The API-key providers to connect.
    ApiKeys,
}

/// Picker state. Pure; the host renders it and feeds it [`PickerInput`].
#[derive(Debug, Clone, PartialEq)]
pub struct PickerState {
    /// The list: OpenCode models, connected providers' models, the vendor rows, `API keys`, xAI.
    pub rows: Vec<ModelsRow>,
    /// The `API keys` sub-menu: providers to connect (sign in / paste key).
    pub connect_rows: Vec<ModelsRow>,
    pub rails: Vec<RailState>,
    /// The open sub-menu, if any.
    pub submenu: Option<Submenu>,
    /// Selected index into [`Self::visible_rows`].
    pub selected: usize,
    /// Row id of the list row the open sub-menu came from (reselected on the way back).
    pub parent_id: Option<String>,
    /// The user acknowledged the xAI row once; a second Enter starts the flow.
    pub xai_armed: bool,
    /// Loaders still running (rows may be partial).
    pub loading: bool,
    /// A live refresh of the model lists is wanted (or running); the rows shown are the cached
    /// ones until the live snapshot lands.
    pub refresh_pending: bool,
    /// The refresh task has been started (a pending refresh waits for the cached load to land
    /// first, so the live rows always arrive last).
    pub refresh_in_flight: bool,
    /// Transient status line (errors, progress).
    pub status: Option<String>,
    pub key_entry: Option<KeyEntry>,
    /// Row id of the active connection (marked `active`, preselected).
    pub active_id: Option<String>,
    pub default_selection: Option<DefaultSelection>,
    pub secret_backend: Option<&'static str>,
    /// Freshness of every model list (see [`Self::catalog_note`]).
    pub catalog_status: Vec<CatalogStatus>,
    /// Type-to-filter text (case-insensitive, matched against name and provider). Esc clears it
    /// before it leaves the sub-menu or closes the picker.
    pub filter: String,
    /// Show every model row, including the non-chat ones hidden by default (`Ctrl+A`).
    pub show_all: bool,
    /// Opened by `/auth`: the first load keeps the selection on the Subscriptions section.
    pub focus_subscriptions: bool,
}

impl Default for PickerState {
    fn default() -> Self {
        Self::new()
    }
}

impl PickerState {
    /// Fresh picker before any loader ran: the engine seed row, the vendor rows (detecting), the
    /// connect rows, xAI.
    pub fn new() -> Self {
        let mut s = Self {
            rows: Vec::new(),
            connect_rows: Vec::new(),
            rails: Rail::ALL.iter().map(|r| RailState::detecting(*r)).collect(),
            submenu: None,
            selected: 0,
            parent_id: None,
            xai_armed: false,
            loading: true,
            refresh_pending: false,
            refresh_in_flight: false,
            status: None,
            key_entry: None,
            active_id: None,
            default_selection: None,
            secret_backend: None,
            catalog_status: Vec::new(),
            filter: String::new(),
            show_all: false,
            focus_subscriptions: false,
        };
        s.set_rows(models_rows(
            &workshop_providers::Catalog::default(),
            |_| false,
            &[],
            &[],
        ));
        s
    }

    /// Open on the active model (`/model`, with any typed filter) or on the first subscription
    /// row (`/auth`).
    pub fn with_focus(mut self, focus: PickerFocus) -> Self {
        self.focus(focus);
        self
    }

    /// Move an open picker to `focus` (a second `/auth` or `/model` while it is open).
    pub fn focus(&mut self, focus: PickerFocus) {
        self.submenu = None;
        self.parent_id = None;
        self.xai_armed = false;
        match focus {
            PickerFocus::Models { filter } => {
                self.focus_subscriptions = false;
                self.filter = filter;
                self.selected = 0;
                self.select_active();
            }
            PickerFocus::Subscriptions => {
                self.focus_subscriptions = true;
                self.filter.clear();
                self.select_first_vendor();
            }
        }
    }

    /// Mark (and preselect) the row of the active connection.
    pub fn with_active(mut self, active_id: Option<String>) -> Self {
        self.active_id = active_id;
        self.select_active();
        self
    }

    fn set_rows(&mut self, all: Vec<ModelsRow>) {
        let (connect, list): (Vec<_>, Vec<_>) = all
            .into_iter()
            .partition(|r| matches!(r.kind, RowKind::ConnectProvider { .. }));
        // A refresh that lands while the list is on screen must not reshuffle what the user is
        // reading: rows already shown keep their order, new rows join their group's band.
        self.rows = if self.loading || self.rows.is_empty() {
            list
        } else {
            stable_merge(&self.rows, list)
        };
        self.connect_rows = connect;
    }

    /// Index in [`Self::visible_rows`] of the active connection's row (an OpenCode model at any
    /// level, a vendor holding the active model, a catalog model).
    fn active_index(&self) -> Option<usize> {
        self.active_id.as_ref()?;
        self.visible_rows().iter().position(|r| self.is_active(r))
    }

    fn select_active(&mut self) {
        if let Some(idx) = self.active_index() {
            self.selected = idx;
        }
    }

    fn select_first_vendor(&mut self) {
        if let Some(idx) = self.visible_rows().iter().position(ModelsRow::is_vendor) {
            self.selected = idx;
        }
    }

    fn rail(&self, rail: Rail) -> Option<&RailState> {
        self.rails.iter().find(|r| r.rail == rail)
    }

    /// The rows of a signed-in vendor's sub-menu: the CLI's list, in its order.
    fn vendor_rows(&self, rail: Rail) -> Vec<ModelsRow> {
        let Some(state) = self.rail(rail) else {
            return Vec::new();
        };
        if !state.is_ready() {
            return Vec::new();
        }
        state
            .models
            .iter()
            .map(|model| ModelsRow {
                kind: RowKind::RailModel {
                    rail,
                    model: model.clone(),
                },
                group: rail.display_name().to_owned(),
                badge: format!(
                    "Subscription · through the official {} CLI",
                    rail.vendor().display_name()
                ),
                class: ConnectionClass::AgentAdapter,
            })
            .collect()
    }

    /// The rows of an OpenCode model's effort sub-menu: `Default`, then each level the catalog
    /// offers, lowest first.
    fn effort_rows(&self, model: &EngineModel) -> Vec<ModelsRow> {
        std::iter::once(None)
            .chain(model.variants.iter().map(|v| Some(v.as_str())))
            .map(|level| ModelsRow {
                kind: RowKind::Effort(model.with_effort(level)),
                group: ENGINE_DISPLAY_NAME.into(),
                badge: ENGINE_ROW_NOTE.into(),
                class: ConnectionClass::AgentAdapter,
            })
            .collect()
    }

    fn submenu_rows(&self, submenu: &Submenu) -> Vec<ModelsRow> {
        match submenu {
            Submenu::Vendor(rail) => self.vendor_rows(*rail),
            Submenu::Effort(model) => self.effort_rows(model),
            Submenu::ApiKeys => self.connect_rows.clone(),
        }
    }

    /// Whether Enter / `→` on `row` opens a sub-menu.
    pub fn is_expandable(&self, row: &ModelsRow) -> bool {
        match &row.kind {
            RowKind::Engine(m) => !m.variants.is_empty(),
            RowKind::Vendor(rail) => self
                .rail(*rail)
                .is_some_and(|r| r.is_ready() && !r.models.is_empty()),
            RowKind::ApiKeys => true,
            _ => false,
        }
    }

    fn matches_filter(&self, row: &ModelsRow) -> bool {
        let filter = self.filter.trim().to_ascii_lowercase();
        if filter.is_empty() {
            return true;
        }
        let hay = format!("{} {}", row.title(), row.provider()).to_ascii_lowercase();
        filter.split_whitespace().all(|word| hay.contains(word))
    }

    fn shown_by_default(&self, row: &ModelsRow) -> bool {
        self.show_all || self.is_active(row) || row.is_chat_model()
    }

    /// The rows in display order. At the top level without a filter: the list (non-chat models
    /// hidden unless `show_all`; the active row is always shown). Inside a sub-menu: its rows. With
    /// a filter typed: the flat list of everything that matches — the list rows plus every
    /// signed-in vendor's models and the connect rows — so a model is found wherever it lives.
    pub fn visible_rows(&self) -> Vec<ModelsRow> {
        let filtered = !self.filter.trim().is_empty();
        match &self.submenu {
            Some(submenu) => self
                .submenu_rows(submenu)
                .into_iter()
                .filter(|r| self.matches_filter(r))
                .collect(),
            None if !filtered => self
                .rows
                .iter()
                .filter(|r| self.shown_by_default(r))
                .cloned()
                .collect(),
            None => {
                let mut out = Vec::new();
                for row in self.rows.iter().filter(|r| self.shown_by_default(r)) {
                    if self.matches_filter(row) {
                        out.push(row.clone());
                    }
                    let children = match &row.kind {
                        RowKind::Vendor(rail) => self.vendor_rows(*rail),
                        RowKind::ApiKeys => self.connect_rows.clone(),
                        _ => Vec::new(),
                    };
                    out.extend(children.into_iter().filter(|r| self.matches_filter(r)));
                }
                out
            }
        }
    }

    /// The list as lines: one quiet header per group (`OpenCode`, a connected provider,
    /// `Subscriptions`) at the top level when no filter is typed, else the plain rows. Rows appear
    /// in [`Self::visible_rows`] order, so `selected` indexes the `Row` lines.
    pub fn models_lines(&self) -> Vec<ModelsLine> {
        let rows = self.visible_rows();
        let grouped = self.filter.trim().is_empty() && self.submenu.is_none();
        let mut lines = Vec::with_capacity(rows.len() + 4);
        let mut current: Option<String> = None;
        for row in rows {
            if grouped && current.as_deref() != Some(row.group.as_str()) {
                lines.push(ModelsLine::Header(row.group.clone()));
                current = Some(row.group.clone());
            }
            lines.push(ModelsLine::Row(Box::new(row)));
        }
        lines
    }

    /// Whether the rows are drawn with their provider column (the flat filtered list).
    pub fn shows_provider_column(&self) -> bool {
        !self.filter.trim().is_empty() && self.submenu.is_none()
    }

    /// How many model rows the default view hides (non-chat rows behind `Ctrl+A`).
    pub fn hidden_models(&self) -> usize {
        if self.show_all {
            return 0;
        }
        self.rows
            .iter()
            .filter(|r| !r.is_chat_model() && !self.is_active(r))
            .count()
    }

    /// The overlay title: `Models`, or `Models › <sub-menu>`.
    pub fn title(&self) -> String {
        match &self.submenu {
            None => PICKER_TITLE.to_owned(),
            Some(Submenu::Vendor(rail)) => format!("{PICKER_TITLE} › {}", rail.display_name()),
            Some(Submenu::Effort(m)) => format!("{PICKER_TITLE} › {}", m.name),
            Some(Submenu::ApiKeys) => format!("{PICKER_TITLE} › {API_KEYS_TITLE}"),
        }
    }

    /// The state drawn after a row's name, in pieces the host colours by [`Tone`]: `free`,
    /// `sign in`, `install`, `✓ Max ▸`, `optional · sign in`; ` · active` on the active row; ` ▸`
    /// on a row that opens a sub-menu.
    pub fn row_suffix(&self, row: &ModelsRow) -> Vec<(Tone, String)> {
        let mut out: Vec<(Tone, String)> = Vec::new();
        match &row.kind {
            RowKind::Engine(_) => out.push((Tone::Good, "free".into())),
            RowKind::Catalog { model, locked } => {
                let tone = if !*locked && model.is_keyless() {
                    Tone::Good
                } else {
                    Tone::Plain
                };
                out.push((tone, row.short_badge().into()));
            }
            RowKind::Vendor(rail) => {
                let Some(state) = self.rail(*rail) else {
                    return out;
                };
                if state.pill == Pill::Detecting {
                    out.push((Tone::Dim, "detecting\u{2026}".into()));
                } else if !state.installed {
                    out.push((Tone::Dim, "install".into()));
                } else if !state.is_ready() {
                    out.push((Tone::Dim, "sign in".into()));
                } else {
                    out.push((Tone::Good, "\u{2713}".into()));
                    match &state.subscription {
                        workshop_detect::RailModels::Loading => {
                            out.push((Tone::Dim, format!(" \u{b7} {LOADING_MODELS}")));
                        }
                        workshop_detect::RailModels::Failed { .. } => {
                            out.push((
                                Tone::Dim,
                                format!(" \u{b7} {}", workshop_detect::copy::MODELS_FAILED),
                            ));
                        }
                        workshop_detect::RailModels::Listed { list, .. } => {
                            if let Some(plan) =
                                list.account.as_ref().and_then(|a| a.plan.as_deref())
                            {
                                out.push((Tone::Plain, format!(" {}", plan_name(plan))));
                            }
                        }
                        workshop_detect::RailModels::NotReady => {
                            if let Some(copy) = state.empty_copy {
                                out.push((Tone::Dim, format!(" \u{b7} {copy}")));
                            }
                        }
                    }
                }
            }
            RowKind::XaiOptional => out.push((Tone::Dim, "optional \u{b7} sign in".into())),
            RowKind::ApiKeys
            | RowKind::Effort(_)
            | RowKind::RailModel { .. }
            | RowKind::ConnectProvider { .. } => {}
        }
        if self.is_active(row) {
            out.push((Tone::Good, " \u{b7} active".into()));
        }
        if self.is_expandable(row) {
            out.push((Tone::Dim, " \u{25b8}".into()));
        }
        // No leading space on the first piece.
        if let Some((_, first)) = out.first_mut() {
            let trimmed = first
                .trim_start_matches([' ', '\u{b7}'])
                .trim_start()
                .to_owned();
            *first = trimmed;
        }
        out
    }

    /// The verb after `Enter` in the key line, for the highlighted row.
    pub fn enter_verb(&self) -> &'static str {
        let Some(row) = self.selected_row() else {
            return "select";
        };
        match &row.kind {
            RowKind::Engine(m) if !m.variants.is_empty() => "open",
            RowKind::Engine(_) | RowKind::Effort(_) | RowKind::RailModel { .. } => "select",
            RowKind::Catalog { locked, .. } => {
                if *locked {
                    "connect"
                } else {
                    "select"
                }
            }
            RowKind::ConnectProvider { .. } => "connect",
            RowKind::ApiKeys => "open",
            RowKind::XaiOptional => "sign in",
            RowKind::Vendor(rail) => match self.rail(*rail) {
                Some(s) if s.pill == Pill::Detecting => "select",
                Some(s) if !s.installed => "install",
                Some(s) if !s.is_ready() => "sign in",
                Some(s) if !s.models.is_empty() => "open",
                Some(s) if matches!(s.subscription, workshop_detect::RailModels::Failed { .. }) => {
                    "retry"
                }
                Some(s) if s.show_connect => "sign in",
                _ => "select",
            },
        }
    }

    /// Replace rows and rails with a finished snapshot. Keeps the selection on the same row id
    /// when possible (first load: the active row, or the Subscriptions section for `/auth`), and
    /// never lands on the xAI row by itself.
    pub fn apply_snapshot(&mut self, snap: PickerSnapshot) {
        let first_load = self.loading;
        let prev = self.selected_row().map(|r| r.id());
        self.set_rows(snap.rows);
        if !snap.rails.is_empty() {
            self.rails = snap.rails;
        }
        self.default_selection = snap.default_selection;
        self.secret_backend = snap.secret_backend;
        self.catalog_status = snap.catalog_status;
        if snap.live {
            self.refresh_pending = false;
            self.refresh_in_flight = false;
        }
        self.loading = false;
        // A vendor sub-menu whose list went away (signed out meanwhile) falls back to the list,
        // on the vendor's row.
        if self.submenu.is_some() && self.visible_rows().is_empty() {
            self.close_submenu();
        } else {
            let kept = prev.and_then(|id| self.visible_rows().iter().position(|r| r.id() == id));
            self.selected = if first_load && !self.focus_subscriptions {
                self.active_index().or(kept).unwrap_or(0)
            } else {
                kept.unwrap_or(0)
            };
        }
        if first_load && self.focus_subscriptions {
            self.select_first_vendor();
        }
        self.selected = self
            .selected
            .min(self.visible_rows().len().saturating_sub(1));
        self.xai_armed = false;
    }

    pub fn selected_row(&self) -> Option<ModelsRow> {
        self.visible_rows().get(self.selected).cloned()
    }

    /// Whether `row` is (or holds) the active connection.
    pub fn is_active(&self, row: &ModelsRow) -> bool {
        let Some(active) = self.active_id.as_deref() else {
            return false;
        };
        match &row.kind {
            // The list row of an OpenCode model is active whatever level was picked.
            RowKind::Engine(m) => active.split('@').next().unwrap_or(active) == m.base_row_id(),
            RowKind::Vendor(rail) => active.starts_with(&format!("{}:", rail.vendor().id())),
            _ => active == row.id(),
        }
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

    /// One line naming every *listed* model list and its freshness, engine first, in snapshot
    /// order: `Lists: OpenCode fetched just now · OpenRouter fetched 2 min ago · …`. Hidden
    /// providers and providers without a row are left out.
    pub fn catalog_summary(&self) -> Option<String> {
        if self.catalog_status.is_empty() {
            return None;
        }
        let parts: Vec<String> = self
            .catalog_status
            .iter()
            .filter(|s| {
                s.provider_id == ENGINE_PROVIDER_ID
                    || (!is_hidden_provider(&s.provider_id)
                        && self
                            .rows
                            .iter()
                            .any(|r| r.provider_id() == Some(s.provider_id.as_str())))
            })
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

    fn open_submenu(&mut self, from: &ModelsRow, submenu: Submenu) {
        self.parent_id = Some(from.id());
        self.submenu = Some(submenu);
        self.filter.clear();
        self.xai_armed = false;
        self.selected = 0;
        self.select_active();
    }

    fn close_submenu(&mut self) {
        self.submenu = None;
        self.filter.clear();
        let parent = self.parent_id.take();
        self.selected = parent
            .and_then(|id| self.visible_rows().iter().position(|r| r.id() == id))
            .unwrap_or(0);
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
        match input {
            PickerInput::Refresh => PickerOutcome::Refresh,
            // Type-to-filter; every printable key is consumed here and none reaches the composer
            // behind the overlay.
            PickerInput::Char(c) if !c.is_control() => {
                self.filter.push(c);
                self.selected = 0;
                self.xai_armed = false;
                PickerOutcome::Changed
            }
            PickerInput::Backspace => {
                self.filter.pop();
                self.selected = 0;
                PickerOutcome::Changed
            }
            PickerInput::ToggleShowAll => {
                let keep = self.selected_row().map(|r| r.id());
                self.show_all = !self.show_all;
                self.selected = keep
                    .and_then(|id| self.visible_rows().iter().position(|r| r.id() == id))
                    .unwrap_or(0);
                PickerOutcome::Changed
            }
            PickerInput::Char(_) | PickerInput::Paste(_) => PickerOutcome::Changed,
            PickerInput::Up => {
                self.selected = self.selected.saturating_sub(1);
                self.xai_armed = false;
                PickerOutcome::Changed
            }
            PickerInput::Down => {
                if self.selected + 1 < self.visible_rows().len() {
                    self.selected += 1;
                }
                self.xai_armed = false;
                PickerOutcome::Changed
            }
            PickerInput::Left => {
                if self.submenu.is_some() {
                    self.close_submenu();
                }
                self.xai_armed = false;
                PickerOutcome::Changed
            }
            PickerInput::Open => {
                if let Some(row) = self.selected_row()
                    && self.is_expandable(&row)
                {
                    return self.enter_row(row);
                }
                PickerOutcome::Changed
            }
            PickerInput::Back => {
                if !self.filter.is_empty() {
                    self.filter.clear();
                    self.selected = 0;
                    self.select_active();
                    PickerOutcome::Changed
                } else if self.xai_armed {
                    self.xai_armed = false;
                    PickerOutcome::Changed
                } else if self.submenu.is_some() {
                    self.close_submenu();
                    PickerOutcome::Changed
                } else {
                    PickerOutcome::Close
                }
            }
            PickerInput::Enter => match self.selected_row() {
                Some(row) => self.enter_row(row),
                None => PickerOutcome::Changed,
            },
        }
    }

    /// Enter on a row (the same rules at the top level, in a sub-menu and in the filtered list).
    fn enter_row(&mut self, row: ModelsRow) -> PickerOutcome {
        match row.kind.clone() {
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
            RowKind::Engine(m) => {
                if m.variants.is_empty() {
                    PickerOutcome::SelectEngine(m)
                } else {
                    self.open_submenu(&row, Submenu::Effort(m));
                    PickerOutcome::Changed
                }
            }
            RowKind::Effort(m) => PickerOutcome::SelectEngine(m),
            RowKind::RailModel { rail, model } => PickerOutcome::SelectRailModel(rail, model),
            RowKind::ApiKeys => {
                self.open_submenu(&row, Submenu::ApiKeys);
                PickerOutcome::Changed
            }
            RowKind::Vendor(rail) => {
                let Some(state) = self.rail(rail).cloned() else {
                    return PickerOutcome::Changed;
                };
                if !state.installed && state.pill == Pill::Install {
                    PickerOutcome::RailInstall(rail)
                } else if state.is_ready() && !state.models.is_empty() {
                    self.open_submenu(&row, Submenu::Vendor(rail));
                    PickerOutcome::Changed
                } else if matches!(
                    state.subscription,
                    workshop_detect::RailModels::Failed { .. }
                ) {
                    PickerOutcome::RetryRailModels
                } else if state.show_connect {
                    PickerOutcome::RailConnect(rail)
                } else {
                    PickerOutcome::Changed
                }
            }
        }
    }

    /// One to three lines about the highlighted row: what it is, who is signed in, what Enter
    /// does — model names and accounts, never a transport or an endpoint. Never a secret.
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
        let Some(row) = self.selected_row() else {
            return Vec::new();
        };
        let mut lines = Vec::new();
        match &row.kind {
            RowKind::Engine(_) => {
                lines.push(match self.catalog_note(ENGINE_PROVIDER_ID) {
                    Some(note) if note.starts_with("fetched") => {
                        format!("{ENGINE_ROW_NOTE} \u{b7} list {note}")
                    }
                    Some(note) => format!("{ENGINE_ROW_NOTE} \u{b7} {note}"),
                    None => ENGINE_ROW_NOTE.to_owned(),
                });
            }
            RowKind::Effort(m) => lines.push(match &m.effort {
                Some(level) => format!("{} at effort {level}", m.name),
                None => format!("{} at its default effort", m.name),
            }),
            RowKind::Catalog { model, locked } => {
                lines.push(
                    match row.provider_id().and_then(|id| self.catalog_note(id)) {
                        Some(note) => format!("{} \u{b7} {note}", row.badge),
                        None => row.badge.clone(),
                    },
                );
                if *locked {
                    lines.push("Needs a credential first — Enter connects the provider.".into());
                } else if let Some(note) = &model.note {
                    lines.push(note.clone());
                }
            }
            RowKind::ConnectProvider {
                provider_id,
                credential_url,
                copy,
            } => {
                lines.push(format!("{copy} \u{b7} {}", row.badge));
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
            RowKind::RailModel { rail, .. } => {
                let mut line = format!("Your {} subscription", rail.display_name());
                match self.account(*rail) {
                    (Some(email), Some(plan)) => {
                        line.push_str(&format!(" \u{b7} signed in as {email} ({plan})"));
                    }
                    (Some(email), None) => line.push_str(&format!(" \u{b7} signed in as {email}")),
                    (None, Some(plan)) => line.push_str(&format!(" \u{b7} {plan}")),
                    (None, None) => {}
                }
                lines.push(line);
            }
            RowKind::Vendor(rail) => lines.extend(self.vendor_detail_lines(*rail)),
            RowKind::ApiKeys => {
                let providers: Vec<&str> =
                    self.connect_rows.iter().map(|r| r.group.as_str()).collect();
                lines.push(format!(
                    "Your own key or account with a provider: {}",
                    providers.join(", ")
                ));
            }
            RowKind::XaiOptional => {
                lines.push(XAI_CARD_COPY.into());
                lines.push(
                    "Enter opens auth.x.ai in your browser (the only Workshop path that contacts x.ai)."
                        .into(),
                );
                if self.xai_armed {
                    lines.push(
                        "Press Enter again to continue to auth.x.ai, or Esc to go back.".into(),
                    );
                }
            }
        }
        lines.into_iter().take(3).collect()
    }

    /// The email and plan name a listed vendor's CLI reported, when it did.
    fn account(&self, rail: Rail) -> (Option<String>, Option<String>) {
        let Some(workshop_detect::RailModels::Listed { list, .. }) =
            self.rail(rail).map(|r| &r.subscription)
        else {
            return (None, None);
        };
        let Some(account) = &list.account else {
            return (None, None);
        };
        (
            account.email.clone(),
            account.plan.as_deref().map(plan_name),
        )
    }

    fn vendor_detail_lines(&self, rail: Rail) -> Vec<String> {
        let Some(state) = self.rail(rail) else {
            return Vec::new();
        };
        let mut lines = Vec::new();
        if state.pill == Pill::Detecting {
            lines.push(
                "Looking for the official CLI on PATH and in the usual install folders…".into(),
            );
        } else if !state.installed {
            // Three short lines: the detail area must not wrap the command.
            lines.push(state.empty_copy.map(str::to_owned).unwrap_or_else(|| {
                format!(
                    "Enter installs {}, then signs you in",
                    rail.vendor().display_name()
                )
            }));
            match workshop_detect::official_install_command(rail.vendor()) {
                Some(cmd) => lines.push(format!("  {cmd}")),
                None => lines.push(format!(
                    "Workshop looks for `{}`.",
                    rail.vendor().binary_names().join("` or `")
                )),
            }
            lines.push(
                "Nothing installs on its own; Workshop never reads another app's login files."
                    .into(),
            );
        } else if !state.is_ready() {
            lines.push(format!(
                "Enter runs the official login in your terminal:  {}",
                login_command(rail.vendor())
            ));
        } else {
            match &state.subscription {
                workshop_detect::RailModels::Listed { list, .. } => {
                    let n = list.models.len();
                    let mut line = String::from("Signed in");
                    let (email, plan) = self.account(rail);
                    if let Some(email) = email {
                        line.push_str(&format!(" as {email}"));
                    }
                    if let Some(plan) = plan {
                        line.push_str(&format!(" \u{b7} {plan}"));
                    }
                    line.push_str(&format!(
                        " \u{b7} {n} model{}",
                        if n == 1 { "" } else { "s" }
                    ));
                    lines.push(line);
                }
                workshop_detect::RailModels::Loading => {
                    lines.push(format!("Signed in \u{b7} {LOADING_MODELS}"));
                }
                workshop_detect::RailModels::Failed { .. } => {
                    lines.push(format!(
                        "Signed in \u{b7} {}",
                        workshop_detect::copy::MODELS_FAILED
                    ));
                }
                workshop_detect::RailModels::NotReady => {
                    lines.push(match state.empty_copy {
                        Some(copy) => format!("Signed in \u{b7} {copy}"),
                        None => "Signed in".into(),
                    });
                    if state.show_connect {
                        lines.push(format!(
                            "Enter runs the official login in your terminal:  {}",
                            login_command(rail.vendor())
                        ));
                    }
                }
            }
        }
        lines
    }
}

/// The plan as a name: `max` → `Max`, `pro` → `Pro`.
fn plan_name(plan: &str) -> String {
    let mut chars = plan.trim().chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The documented login command as the user would type it (`claude auth login`, `codex login`).
fn login_command(vendor: workshop_detect::Vendor) -> String {
    let bin = vendor.binary_names().first().copied().unwrap_or("the CLI");
    format!("{bin} {}", workshop_detect::login_argv(vendor).join(" "))
}

/// Merge a refreshed row list into the one on screen: rows the user can already see keep their
/// relative order (updated in place from `fresh`), rows that disappeared are dropped, and new rows
/// are inserted after the last shown row of the same group (else appended).
fn stable_merge(shown: &[ModelsRow], fresh: Vec<ModelsRow>) -> Vec<ModelsRow> {
    let mut out: Vec<ModelsRow> = shown
        .iter()
        .filter_map(|old| fresh.iter().find(|new| new.id() == old.id()).cloned())
        .collect();
    for row in fresh {
        if out.iter().any(|r| r.id() == row.id()) {
            continue;
        }
        let after = out.iter().rposition(|r| r.group == row.group);
        match after {
            Some(i) => out.insert(i + 1, row),
            None => out.push(row),
        }
    }
    out
}

/// Path of the user config file the picker writes (`$WORKSHOP_HOME/config.toml`).
pub fn config_path() -> PathBuf {
    xai_dirs::resolve_grok_home()
        .unwrap_or_else(xai_dirs::default_grok_home)
        .join("config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use workshop_detect::{RailModels, copy};

    fn loaded() -> PickerState {
        let mut p = PickerState::new();
        let rows = models_rows(&workshop_providers::Catalog::builtin(), |_| false, &[], &[]);
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
        p.selected = p
            .visible_rows()
            .iter()
            .position(|r| r.title().contains(needle))
            .unwrap_or_else(|| panic!("row {needle:?}"));
    }

    fn titles(p: &PickerState) -> Vec<String> {
        p.visible_rows().iter().map(ModelsRow::title).collect()
    }

    fn rendered(p: &PickerState) -> Vec<String> {
        p.models_lines()
            .iter()
            .map(|l| match l {
                ModelsLine::Header(h) => format!("# {h}"),
                ModelsLine::Row(r) => r.title(),
            })
            .collect()
    }

    fn suffix_text(p: &PickerState, row: &ModelsRow) -> String {
        p.row_suffix(row).into_iter().map(|(_, s)| s).collect()
    }

    fn engine(name: &str, model_ref: &str, is_default: bool, tool_call: bool) -> EngineModel {
        EngineModel {
            model_ref: model_ref.into(),
            name: name.into(),
            is_default,
            tool_call,
            context_limit: None,
            variants: Vec::new(),
            effort: None,
            image_input: false,
        }
    }

    fn loaded_with_engine(models: &[EngineModel]) -> PickerState {
        let mut p = PickerState::new().with_active(Some(EngineModel::big_pickle_seed().row_id()));
        let rows = models_rows(
            &workshop_providers::Catalog::builtin(),
            |_| false,
            models,
            &[],
        );
        p.apply_snapshot(PickerSnapshot {
            rows,
            ..PickerSnapshot::default()
        });
        p
    }

    /// A rail as the detect layer reports it; a Ready rail carries the models its CLI listed
    /// (here: three Claude aliases, as a CLI would report them) and the account.
    fn rail(rail: Rail, installed: bool, ready: bool) -> RailState {
        let mut st = RailState::detecting(rail);
        st.installed = installed;
        st.pill = if ready {
            Pill::Ready
        } else if installed {
            Pill::SignIn
        } else {
            Pill::Install
        };
        st.models = if ready {
            let p = rail.provider_id();
            ["Claude Opus", "Claude Sonnet", "Claude Haiku"]
                .iter()
                .map(|name| {
                    workshop_detect::ModelRef::new(
                        p,
                        name.rsplit(' ').next().unwrap().to_ascii_lowercase(),
                    )
                    .with_display_name(*name)
                })
                .collect()
        } else {
            Vec::new()
        };
        st.subscription = if ready {
            RailModels::Listed {
                list: workshop_detect::models::SubscriptionModels {
                    rail,
                    models: st
                        .models
                        .iter()
                        .map(|m| workshop_detect::models::SubscriptionModel {
                            id: m.model.clone(),
                            label: m.display().to_owned(),
                            is_default: false,
                        })
                        .collect(),
                    account: Some(workshop_detect::models::Account {
                        email: Some("user@example.com".into()),
                        plan: Some("max".into()),
                    }),
                    documented_aliases: false,
                    fetched_at_secs: 0,
                },
                cached: false,
                error: None,
            }
        } else {
            RailModels::NotReady
        };
        st.empty_copy = (!ready).then(|| copy::empty_rail_copy(rail, installed, ready, false));
        st.show_connect = !ready;
        st
    }

    fn loaded_with_rails(rails: &[RailState]) -> PickerState {
        let mut p = PickerState::new().with_active(Some(EngineModel::big_pickle_seed().row_id()));
        let rows = models_rows(
            &workshop_providers::Catalog::builtin(),
            |_| false,
            &[
                engine("Big Pickle", "opencode/big-pickle", true, true),
                engine("MiMo Free", "opencode/mimo", false, true),
            ],
            rails,
        );
        p.apply_snapshot(PickerSnapshot {
            rows,
            rails: rails.to_vec(),
            ..PickerSnapshot::default()
        });
        p
    }

    /// The owner's layout: OpenCode's models, then the Subscriptions section — every vendor as
    /// one row whose suffix is its state, `API keys`, xAI last. No pills, no vendor models inline.
    #[test]
    fn one_list_opencode_then_subscriptions_with_state_suffixes() {
        let rails = [
            rail(Rail::Claude, true, true),
            rail(Rail::Codex, true, false),
            rail(Rail::Cursor, false, false),
        ];
        let p = loaded_with_rails(&rails);
        assert_eq!(
            rendered(&p),
            vec![
                "# OpenCode",
                "Big Pickle",
                "MiMo Free",
                "# Subscriptions",
                "Claude",
                "Codex",
                "Cursor",
                "API keys",
                "xAI",
            ],
            "{:?}",
            rendered(&p)
        );
        let by_title = |t: &str| {
            p.visible_rows()
                .into_iter()
                .find(|r| r.title() == t)
                .unwrap()
        };
        assert_eq!(
            suffix_text(&p, &by_title("Big Pickle")),
            "free \u{b7} active"
        );
        assert_eq!(suffix_text(&p, &by_title("MiMo Free")), "free");
        assert_eq!(
            suffix_text(&p, &by_title("Claude")),
            "\u{2713} Max \u{25b8}"
        );
        assert_eq!(suffix_text(&p, &by_title("Codex")), "sign in");
        assert_eq!(suffix_text(&p, &by_title("Cursor")), "install");
        assert_eq!(suffix_text(&p, &by_title("API keys")), "\u{25b8}");
        assert_eq!(suffix_text(&p, &by_title("xAI")), "optional \u{b7} sign in");
        assert!(
            !rendered(&p)
                .iter()
                .any(|l| l.contains("Opus") || l.contains("Sign in") || l.contains('[')),
            "no vendor model inline, no pill: {:?}",
            rendered(&p)
        );
        assert_eq!(p.title(), "Models");
        // The active model is preselected.
        assert_eq!(
            p.selected_row().map(|r| r.title()),
            Some("Big Pickle".into())
        );
    }

    /// Enter on a signed-in vendor opens its sub-menu with the CLI's list; Enter picks a model;
    /// Left / Esc go back to the vendor row.
    #[test]
    fn a_signed_in_vendor_opens_into_its_real_models() {
        let rails = [
            rail(Rail::Claude, true, true),
            rail(Rail::Codex, true, false),
            rail(Rail::Cursor, false, false),
        ];
        let mut p = loaded_with_rails(&rails);
        select_row(&mut p, "Claude");
        assert_eq!(p.enter_verb(), "open");
        assert_eq!(p.handle(PickerInput::Enter), PickerOutcome::Changed);
        assert_eq!(p.submenu, Some(Submenu::Vendor(Rail::Claude)));
        assert_eq!(p.title(), "Models › Claude");
        assert_eq!(
            titles(&p),
            vec!["Claude Opus", "Claude Sonnet", "Claude Haiku"]
        );
        assert!(
            !p.models_lines()
                .iter()
                .any(|l| matches!(l, ModelsLine::Header(_))),
            "a sub-menu has no group headers"
        );
        assert_eq!(
            p.detail_lines(),
            vec!["Your Claude subscription \u{b7} signed in as user@example.com (Max)"]
        );
        p.handle(PickerInput::Down);
        assert!(matches!(
            p.handle(PickerInput::Enter),
            PickerOutcome::SelectRailModel(Rail::Claude, m) if m.display() == "Claude Sonnet"
        ));
        // Back lands on the vendor row again.
        p.handle(PickerInput::Left);
        assert_eq!(p.submenu, None);
        assert_eq!(p.selected_row().map(|r| r.title()), Some("Claude".into()));
        // `→` opens too; Esc leaves the sub-menu before it closes the picker.
        assert_eq!(p.handle(PickerInput::Open), PickerOutcome::Changed);
        assert_eq!(p.submenu, Some(Submenu::Vendor(Rail::Claude)));
        assert_eq!(p.handle(PickerInput::Back), PickerOutcome::Changed);
        assert_eq!(p.submenu, None);
        assert_eq!(p.handle(PickerInput::Back), PickerOutcome::Close);
    }

    /// Enter on a signed-out vendor runs the official login; on a missing CLI the official
    /// installer; `→` does neither.
    #[test]
    fn signed_out_and_missing_vendors_sign_in_or_install_on_enter_only() {
        let rails = [
            rail(Rail::Claude, true, true),
            rail(Rail::Codex, true, false),
            rail(Rail::Cursor, false, false),
        ];
        let mut p = loaded_with_rails(&rails);
        select_row(&mut p, "Codex");
        assert_eq!(p.enter_verb(), "sign in");
        assert_eq!(p.handle(PickerInput::Open), PickerOutcome::Changed);
        assert_eq!(p.submenu, None);
        assert!(
            p.detail_lines()
                .iter()
                .any(|l| l.contains("in your terminal:  codex login")),
            "{:?}",
            p.detail_lines()
        );
        assert_eq!(
            p.handle(PickerInput::Enter),
            PickerOutcome::RailConnect(Rail::Codex)
        );
        select_row(&mut p, "Cursor");
        assert_eq!(p.enter_verb(), "install");
        let detail = p.detail_lines().join("\n");
        assert!(
            detail.contains(copy::INSTALL_CURSOR) && detail.contains("Nothing installs on its own"),
            "{detail}"
        );
        assert_eq!(p.handle(PickerInput::Open), PickerOutcome::Changed);
        assert_eq!(
            p.handle(PickerInput::Enter),
            PickerOutcome::RailInstall(Rail::Cursor)
        );
    }

    /// A signed-in vendor whose CLI has not listed its models (yet) says so on its row and
    /// offers nothing to pick; a failed list retries on Enter. Never an invented model.
    #[test]
    fn a_signed_in_vendor_without_its_list_shows_loading_or_retry() {
        let ready = |rail: Rail, subscription: RailModels, empty_copy| RailState {
            pill: Pill::Ready,
            installed: true,
            empty_copy: Some(empty_copy),
            subscription,
            ..RailState::detecting(rail)
        };
        let rails = vec![
            ready(Rail::Claude, RailModels::Loading, copy::LOADING_MODELS),
            ready(
                Rail::Codex,
                RailModels::Failed {
                    reason: "the CLI did not answer in time".into(),
                },
                copy::MODELS_FAILED,
            ),
            rail(Rail::Cursor, false, false),
        ];
        let mut p = loaded_with_rails(&rails);
        let claude = p
            .visible_rows()
            .into_iter()
            .find(|r| r.title() == "Claude")
            .unwrap();
        assert_eq!(
            suffix_text(&p, &claude),
            format!("\u{2713} \u{b7} {}", copy::LOADING_MODELS)
        );
        select_row(&mut p, "Claude");
        assert_eq!(p.handle(PickerInput::Enter), PickerOutcome::Changed);
        assert_eq!(p.submenu, None, "nothing to open while loading");
        assert_eq!(
            p.detail_lines(),
            vec![format!("Signed in \u{b7} {}", copy::LOADING_MODELS)]
        );
        select_row(&mut p, "Codex");
        let codex = p.selected_row().unwrap();
        assert!(suffix_text(&p, &codex).ends_with(copy::MODELS_FAILED));
        assert_eq!(p.enter_verb(), "retry");
        assert_eq!(p.handle(PickerInput::Enter), PickerOutcome::RetryRailModels);
        assert!(
            !rendered(&p)
                .iter()
                .any(|l| l.contains("Opus") || l.contains("Sign in")),
            "signed in is never `Sign in`, and no placeholder model: {:?}",
            rendered(&p)
        );
    }

    /// One row per OpenCode model; a model with levels opens its effort sub-menu (`Default`, then
    /// the levels), and picking a level carries it on the model. A model without levels selects
    /// at once.
    #[test]
    fn effort_levels_are_a_sub_menu_not_rows() {
        let mut ling = engine("Ling Free", "opencode/ling", false, true);
        ling.variants = vec!["low".into(), "medium".into(), "high".into()];
        let mut p = loaded_with_engine(&[
            engine("Big Pickle", "opencode/big-pickle", true, true),
            ling.clone(),
        ]);
        let opencode: Vec<String> = p
            .rows
            .iter()
            .filter(|r| matches!(r.kind, RowKind::Engine(_)))
            .map(ModelsRow::title)
            .collect();
        assert_eq!(opencode, ["Big Pickle", "Ling Free"]);
        select_row(&mut p, "Big Pickle");
        assert_eq!(p.enter_verb(), "select");
        assert!(matches!(
            p.handle(PickerInput::Enter),
            PickerOutcome::SelectEngine(m) if m.name == "Big Pickle" && m.effort.is_none()
        ));
        select_row(&mut p, "Ling Free");
        let row = p.selected_row().unwrap();
        assert_eq!(suffix_text(&p, &row), "free \u{25b8}");
        assert_eq!(p.enter_verb(), "open");
        assert_eq!(p.handle(PickerInput::Enter), PickerOutcome::Changed);
        assert_eq!(p.title(), "Models › Ling Free");
        assert_eq!(titles(&p), vec!["Default", "low", "medium", "high"]);
        assert_eq!(p.detail_lines(), vec!["Ling Free at its default effort"]);
        for _ in 0..3 {
            p.handle(PickerInput::Down);
        }
        assert_eq!(p.detail_lines(), vec!["Ling Free at effort high"]);
        let picked = match p.handle(PickerInput::Enter) {
            PickerOutcome::SelectEngine(m) => m,
            other => panic!("expected SelectEngine, got {other:?}"),
        };
        assert_eq!(picked.effort.as_deref(), Some("high"));
        assert_eq!(picked.display(), "Ling Free (high)");
        assert_eq!(picked.row_id(), "opencode-engine:opencode/ling@high");
        assert_eq!(EngineModel::big_pickle_seed().display(), "Big Pickle");

        // With that pick active, the list row and the level row are both marked; the list row
        // still reads as one model.
        p.active_id = Some(picked.row_id());
        p.handle(PickerInput::Left);
        select_row(&mut p, "Ling Free");
        let row = p.selected_row().unwrap();
        assert!(p.is_active(&row));
        assert_eq!(suffix_text(&p, &row), "free \u{b7} active \u{25b8}");
        p.handle(PickerInput::Open);
        assert_eq!(
            p.selected_row().map(|r| r.title()),
            Some("high".into()),
            "the sub-menu opens on the picked level"
        );

        // The live catalog re-reports the model: the pick survives while the level exists.
        let fresh = ling.clone().carrying_effort_from(&picked);
        assert_eq!(fresh.effort.as_deref(), Some("high"));
        let mut without_high = ling.clone();
        without_high.variants = vec!["low".into()];
        assert_eq!(without_high.carrying_effort_from(&picked).effort, None);
        assert_eq!(
            engine("Other", "opencode/other", false, true)
                .carrying_effort_from(&picked)
                .effort,
            None,
            "another model never inherits a level"
        );
    }

    /// `/auth` opens the same list on the first vendor row; the first load keeps it there.
    #[test]
    fn auth_focus_lands_on_the_subscriptions_section() {
        let mut p = PickerState::new()
            .with_active(Some(EngineModel::big_pickle_seed().row_id()))
            .with_focus(PickerFocus::Subscriptions);
        assert_eq!(p.selected_row().map(|r| r.title()), Some("Claude".into()));
        let rails = [
            rail(Rail::Claude, true, false),
            rail(Rail::Codex, true, false),
            rail(Rail::Cursor, false, false),
        ];
        p.apply_snapshot(PickerSnapshot {
            rows: models_rows(
                &workshop_providers::Catalog::builtin(),
                |_| false,
                &[],
                &rails,
            ),
            rails: rails.to_vec(),
            ..PickerSnapshot::default()
        });
        assert_eq!(p.selected_row().map(|r| r.title()), Some("Claude".into()));
        assert_eq!(p.title(), "Models", "one picker, one title");
        // `/model` afterwards goes back to the active model, with a typed filter kept.
        p.focus(PickerFocus::Models {
            filter: "pick".into(),
        });
        assert_eq!(p.filter, "pick");
        assert_eq!(titles(&p), vec!["Big Pickle"]);
    }

    /// The `API keys` row opens the connect rows (one vocabulary: `Provider — API key` /
    /// `Provider — Sign in`); Enter on one connects that provider.
    #[test]
    fn api_keys_open_the_connect_rows() {
        let mut p = loaded();
        select_row(&mut p, API_KEYS_TITLE);
        assert!(
            p.detail_lines()[0].contains("OpenRouter") && p.detail_lines()[0].contains("OpenAI"),
            "{:?}",
            p.detail_lines()
        );
        assert_eq!(p.handle(PickerInput::Enter), PickerOutcome::Changed);
        assert_eq!(p.submenu, Some(Submenu::ApiKeys));
        assert_eq!(p.title(), "Models › API keys");
        let titles = titles(&p);
        assert!(
            titles.iter().any(|t| t == "OpenRouter \u{2014} Sign in"),
            "{titles:?}"
        );
        assert!(
            titles.iter().any(|t| t == "OpenAI \u{2014} API key"),
            "{titles:?}"
        );
        assert!(
            titles.iter().all(|t| t.contains(" \u{2014} ")),
            "{titles:?}"
        );
        assert!(
            !titles.iter().any(|t| t.contains("xAI")),
            "xAI is not an API key"
        );
        select_row(&mut p, "OpenAI");
        assert_eq!(
            p.handle(PickerInput::Enter),
            PickerOutcome::ConnectProvider("openai".into())
        );
    }

    #[test]
    fn detail_lines_say_where_and_when_each_list_came_from() {
        let mut p = PickerState::new();
        // OpenRouter has a key configured, so its rows are listed; Kilo is hidden whatever it says.
        let rows = models_rows(
            &workshop_providers::Catalog::builtin(),
            |id| id == "openrouter",
            &[],
            &[],
        );
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
                    freshness: Freshness::Live,
                    fetched_at_secs: Some(now - 3 * 60),
                    rows: 5,
                    error: None,
                },
                CatalogStatus {
                    provider_id: "nvidia".into(),
                    freshness: Freshness::Seed,
                    fetched_at_secs: None,
                    rows: 5,
                    error: Some("HTTP 502".into()),
                },
            ],
            live: true,
            ..PickerSnapshot::default()
        });
        // Engine seed: one line, dated, no plumbing.
        select_row(&mut p, "Big Pickle");
        let lines = p.detail_lines();
        assert_eq!(
            lines,
            vec![format!(
                "{ENGINE_ROW_NOTE} \u{b7} cached list from 2026-09-21"
            )]
        );
        for word in ["engine", "opencode serve", "CLI", "endpoint", "opencode.ai"] {
            assert!(
                !lines.join(" ").to_ascii_lowercase().contains(word),
                "no plumbing word {word:?} under a model: {lines:?}"
            );
        }
        // Live rows of a connected provider: the badge line carries the age, no endpoint.
        let openrouter = p
            .visible_rows()
            .iter()
            .position(|r| r.provider_id() == Some("openrouter"))
            .expect("a connected provider's row is listed");
        p.selected = openrouter;
        let lines = p.detail_lines();
        assert!(lines[0].ends_with("· fetched 3 min ago"), "{lines:?}");
        assert!(
            !lines.iter().any(|l| l.contains("Endpoint")),
            "no endpoint under a model: {lines:?}"
        );
        assert_eq!(
            p.catalog_note("openrouter").as_deref(),
            Some("fetched 3 min ago")
        );
        // A failed refresh says so on the seed it fell back to.
        assert_eq!(
            p.catalog_note("nvidia").as_deref(),
            Some("cached list from 2026-09-21 · refresh failed")
        );
        assert!(p.catalog_note("google").is_none());
        // The summary names the listed lists only: never Kilo, not an unconnected provider.
        let summary = p.catalog_summary().unwrap();
        assert_eq!(
            summary, "Lists: OpenCode cached list from 2026-09-21 · OpenRouter fetched 3 min ago",
            "{summary}"
        );
        // An engine list that was fetched says so, briefly.
        p.catalog_status = vec![CatalogStatus {
            provider_id: ENGINE_PROVIDER_ID.into(),
            freshness: Freshness::Cached,
            fetched_at_secs: Some(now),
            rows: 7,
            error: None,
        }];
        select_row(&mut p, "Big Pickle");
        assert_eq!(
            p.detail_lines(),
            vec![format!("{ENGINE_ROW_NOTE} \u{b7} list fetched just now")]
        );
    }

    #[test]
    fn a_live_snapshot_clears_refresh_pending_and_a_cached_one_does_not() {
        let mut p = loaded();
        p.refresh_pending = true;
        let rows = models_rows(&workshop_providers::Catalog::builtin(), |_| false, &[], &[]);
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
        p.selected = p.visible_rows().len().saturating_sub(1);
        assert!(p.selected_row().is_some_and(|r| r.is_xai()));
    }

    #[test]
    fn list_is_engine_default_first_vendors_then_xai_last() {
        let p = loaded();
        assert!(
            matches!(p.rows.first().map(|r| &r.kind), Some(RowKind::Engine(m)) if m.is_default),
            "the engine default heads the list"
        );
        let vendors: Vec<_> = p
            .rows
            .iter()
            .filter_map(|r| match r.kind {
                RowKind::Vendor(rail) => Some(rail),
                _ => None,
            })
            .collect();
        assert_eq!(vendors, vec![Rail::Claude, Rail::Codex, Rail::Cursor]);
        assert!(p.rails.iter().all(|r| r.pill == Pill::Detecting));
        assert!(p.rows.last().is_some_and(ModelsRow::is_xai));
        assert!(
            p.connect_rows.iter().any(|r| matches!(&r.kind, RowKind::ConnectProvider { provider_id, .. } if provider_id == "openrouter")),
            "openrouter shows a connect row when no key is connected"
        );
        assert!(
            !p.rows
                .iter()
                .any(|r| matches!(r.kind, RowKind::ConnectProvider { .. })),
            "connect rows live in the API keys sub-menu"
        );
        assert_eq!(p.selected, 0, "never preselects the xAI row");
        // Fresh (unloaded) state: the seed row and the vendors are already there for the overlay.
        let fresh = PickerState::new();
        assert!(matches!(
            fresh.rows.first().map(|r| &r.kind),
            Some(RowKind::Engine(_))
        ));
        assert!(fresh.rows.iter().any(ModelsRow::is_vendor));
        let detecting = fresh.rows.iter().find(|r| r.is_vendor()).unwrap();
        assert_eq!(suffix_text(&fresh, detecting), "detecting\u{2026}");
    }

    /// Kilo Gateway is the silent fallback, never a row, and no unconnected API-key provider is
    /// listed as a model.
    #[test]
    fn kilo_is_never_listed_and_key_providers_wait_for_a_key() {
        let p = loaded();
        assert!(
            workshop_providers::Catalog::builtin()
                .models
                .iter()
                .any(|m| m.provider_id == "kilo"),
            "the catalog code still knows Kilo"
        );
        assert!(
            !p.rows.iter().any(|r| r.provider_id() == Some("kilo"))
                && !p
                    .connect_rows
                    .iter()
                    .any(|r| r.provider_id() == Some("kilo")),
            "Kilo is never a row: {:?}",
            titles(&p)
        );
        assert!(is_hidden_provider("kilo") && !is_hidden_provider("openrouter"));
        assert!(
            p.rows
                .iter()
                .filter(|r| r.is_model())
                .all(|r| matches!(r.kind, RowKind::Engine(_))),
            "with nothing connected, only OpenCode's models are listed: {:?}",
            titles(&p)
        );
        let engine = p.rows.first().expect("engine seed row");
        assert_eq!(engine.class, ConnectionClass::AgentAdapter);
        assert_eq!(engine.group, "OpenCode");
        assert_eq!(engine.title(), "Big Pickle");
        // Once a key is configured, that provider's rows appear as their own group.
        let connected = models_rows(
            &workshop_providers::Catalog::builtin(),
            |id| id == "openrouter",
            &[],
            &[],
        );
        assert!(
            connected
                .iter()
                .any(|r| r.provider_id() == Some("openrouter") && r.is_model()),
            "a connected provider's models are listed"
        );
        assert!(!connected.iter().any(|r| r.provider_id() == Some("kilo")));
    }

    #[test]
    fn plain_model_name_drops_vendor_prefix_and_free_suffix() {
        assert_eq!(
            plain_model_name("NVIDIA: Nemotron 3 Super (free)"),
            "Nemotron 3 Super"
        );
        assert_eq!(plain_model_name("Qwen: Qwen3.8 27B (free)"), "Qwen3.8 27B");
        assert_eq!(plain_model_name("Big Pickle"), "Big Pickle");
        assert_eq!(
            plain_model_name("Auto Free (rotates free models)"),
            "Auto Free (rotates free models)"
        );
        assert_eq!(plain_model_name("Claude Sonnet"), "Claude Sonnet");
    }

    #[test]
    fn active_row_is_marked_and_preselected() {
        let active = EngineModel::big_pickle_seed().row_id();
        let mut p = PickerState::new().with_active(Some(active.clone()));
        p.handle(PickerInput::Down);
        p.handle(PickerInput::Down);
        let rows = models_rows(&workshop_providers::Catalog::builtin(), |_| false, &[], &[]);
        p.apply_snapshot(PickerSnapshot {
            rows,
            ..PickerSnapshot::default()
        });
        assert_eq!(p.selected_row().map(|r| r.id()), Some(active));
        assert!(p.selected_row().is_some_and(|r| p.is_active(&r)));
    }

    /// The active subscription model marks its vendor row and, inside the sub-menu, itself.
    #[test]
    fn an_active_subscription_model_marks_its_vendor() {
        let rails = [
            rail(Rail::Claude, true, true),
            rail(Rail::Codex, true, false),
            rail(Rail::Cursor, false, false),
        ];
        let sonnet = rails[0].models[1].clone();
        let mut p = loaded_with_rails(&rails);
        p.active_id = Some(rail_model_row_id(Rail::Claude, &sonnet));
        p.selected = 0;
        p.select_active();
        assert_eq!(p.selected_row().map(|r| r.title()), Some("Claude".into()));
        let claude = p.selected_row().unwrap();
        assert_eq!(
            suffix_text(&p, &claude),
            "\u{2713} Max \u{b7} active \u{25b8}"
        );
        p.handle(PickerInput::Enter);
        assert_eq!(
            p.selected_row().map(|r| r.title()),
            Some("Claude Sonnet".into()),
            "the sub-menu opens on the active model"
        );
        let row = p.selected_row().unwrap();
        assert_eq!(suffix_text(&p, &row), "active");
    }

    #[test]
    fn xai_login_requires_two_explicit_enters_and_disarms_on_move() {
        let mut p = loaded();
        goto_xai(&mut p);
        assert_eq!(p.enter_verb(), "sign in");
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

    /// Every row but the xAI one yields something other than the xAI login — at the top level,
    /// in every sub-menu and in the filtered list.
    #[test]
    fn non_xai_rows_never_start_a_login() {
        let rails = [
            rail(Rail::Claude, true, true),
            rail(Rail::Codex, true, false),
            rail(Rail::Cursor, false, false),
        ];
        let mut p = loaded_with_rails(&rails);
        p.show_all = true;
        for i in 0..p.visible_rows().len() {
            p.submenu = None;
            p.selected = i;
            p.xai_armed = false;
            if p.selected_row().is_some_and(|r| r.is_xai()) {
                continue;
            }
            let out = p.handle(PickerInput::Enter);
            assert_ne!(out, PickerOutcome::StartOptionalXaiLogin, "row {i}");
            if p.submenu.is_some() {
                for j in 0..p.visible_rows().len() {
                    p.selected = j;
                    let out = p.handle(PickerInput::Enter);
                    assert_ne!(out, PickerOutcome::StartOptionalXaiLogin, "row {i}/{j}");
                }
                p.handle(PickerInput::Left);
            }
        }
        p.filter = "a".into();
        for i in 0..p.visible_rows().len() {
            p.selected = i;
            p.xai_armed = false;
            if p.selected_row().is_some_and(|r| r.is_xai()) {
                continue;
            }
            let out = p.handle(PickerInput::Enter);
            assert_ne!(
                out,
                PickerOutcome::StartOptionalXaiLogin,
                "filtered row {i}"
            );
            p.submenu = None;
            p.filter = "a".into();
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
    fn detail_is_one_to_three_lines_everywhere() {
        let rails = [
            rail(Rail::Claude, true, true),
            rail(Rail::Codex, true, false),
            rail(Rail::Cursor, false, false),
        ];
        let mut p = loaded_with_rails(&rails);
        p.show_all = true;
        for i in 0..p.visible_rows().len() {
            p.selected = i;
            let n = p.detail_lines().len();
            assert!((1..=3).contains(&n), "row {i}: {n} lines");
        }
        for submenu in [Submenu::Vendor(Rail::Claude), Submenu::ApiKeys] {
            p.submenu = Some(submenu.clone());
            for i in 0..p.visible_rows().len() {
                p.selected = i;
                let n = p.detail_lines().len();
                assert!((1..=3).contains(&n), "{submenu:?} row {i}: {n} lines");
            }
        }
    }

    #[test]
    fn esc_closes_the_picker() {
        let mut p = loaded();
        assert_eq!(p.handle(PickerInput::Back), PickerOutcome::Close);
    }

    /// Typing flattens everything that matches into one list — including a signed-in vendor's
    /// models and the connect rows — with the provider column; Esc clears the filter first.
    #[test]
    fn typing_filters_the_whole_tree_and_never_closes_it() {
        let rails = [
            rail(Rail::Claude, true, true),
            rail(Rail::Codex, true, false),
            rail(Rail::Cursor, false, false),
        ];
        let mut p = loaded_with_rails(&rails);
        let all = p.visible_rows().len();
        assert!(!p.shows_provider_column());
        for c in "sonnet".chars() {
            assert_eq!(
                p.handle(PickerInput::Char(c)),
                PickerOutcome::Changed,
                "{c}"
            );
        }
        assert_eq!(p.filter, "sonnet");
        assert!(p.shows_provider_column());
        assert_eq!(titles(&p), vec!["Claude Sonnet"]);
        assert_eq!(p.visible_rows()[0].provider(), "Claude");
        assert!(
            !p.models_lines()
                .iter()
                .any(|l| matches!(l, ModelsLine::Header(_))),
            "a search result has no groups"
        );
        assert!(matches!(
            p.handle(PickerInput::Enter),
            PickerOutcome::SelectRailModel(Rail::Claude, _)
        ));
        // A vendor is found by its name (to sign in), a provider by its connect row.
        p.filter = "codex".into();
        assert_eq!(titles(&p), vec!["Codex"]);
        p.filter = "openai".into();
        assert_eq!(titles(&p), vec!["OpenAI \u{2014} API key"]);
        p.filter = "pickle".into();
        assert_eq!(titles(&p), vec!["Big Pickle"]);
        // Esc clears the filter first; only an empty filter closes the picker.
        assert_eq!(p.handle(PickerInput::Back), PickerOutcome::Changed);
        assert!(p.filter.is_empty());
        assert_eq!(p.visible_rows().len(), all);
        assert_eq!(p.handle(PickerInput::Back), PickerOutcome::Close);
        // Backspace edits the filter; `q` is a letter, not a close key.
        p.handle(PickerInput::Char('q'));
        assert_eq!(p.filter, "q");
        p.handle(PickerInput::Backspace);
        assert!(p.filter.is_empty());
    }

    #[test]
    fn non_chat_models_hide_behind_show_all() {
        let models = vec![
            engine("Big Pickle", "opencode/big-pickle", true, true),
            engine(
                "Nemotron 3.5 Content Safety",
                "opencode/nemotron-safety",
                false,
                false,
            ),
            engine(
                "OpenRouter Free Models Router",
                "opencode/free-router",
                false,
                false,
            ),
            engine("Qwen3.8 27B Free", "opencode/qwen", false, true),
        ];
        let baseline_hidden = loaded_with_engine(&[]).hidden_models();
        let mut p = loaded_with_engine(&models);
        assert!(
            !titles(&p)
                .iter()
                .any(|t| t.contains("Content Safety") || t.contains("Router"))
        );
        assert_eq!(p.hidden_models(), baseline_hidden + 2);
        assert_eq!(p.handle(PickerInput::ToggleShowAll), PickerOutcome::Changed);
        assert!(p.show_all);
        assert!(titles(&p).iter().any(|t| t.contains("Content Safety")));
        assert_eq!(p.hidden_models(), 0);
        assert!(is_chat_model_name("Qwen3.8 27B (free)"));
        assert!(!is_chat_model_name(
            "nvidia/llama-3.1-nemoguard-8b-topic-control"
        ));
    }

    #[test]
    fn first_run_default_mirrors_the_engine_catalog() {
        assert_eq!(
            EngineModel::first_run_default(&[]),
            EngineModel::big_pickle_seed()
        );
        let live = vec![
            engine("Other", "opencode/other", false, true),
            engine("New Default", "opencode/new-default", true, true),
        ];
        assert_eq!(
            EngineModel::first_run_default(&live).model_ref,
            "opencode/new-default"
        );
    }

    #[test]
    fn a_refresh_while_open_keeps_the_shown_order() {
        let mut p = loaded_with_engine(&[
            engine("Big Pickle", "opencode/big-pickle", true, true),
            engine("Bravo Free", "opencode/bravo", false, true),
            engine("Alpha Free", "opencode/alpha", false, true),
        ]);
        let before: Vec<String> = p.rows.iter().map(ModelsRow::id).collect();
        // The live list comes back reordered, with a new row and one gone.
        let rows = models_rows(
            &workshop_providers::Catalog::builtin(),
            |_| false,
            &[
                engine("Alpha Free", "opencode/alpha", false, true),
                engine("Charlie Free", "opencode/charlie", false, true),
                engine("Big Pickle", "opencode/big-pickle", true, true),
            ],
            &[],
        );
        p.apply_snapshot(PickerSnapshot {
            rows,
            live: true,
            ..PickerSnapshot::default()
        });
        let after: Vec<String> = p.rows.iter().map(ModelsRow::id).collect();
        let big = after
            .iter()
            .position(|id| id.contains("big-pickle"))
            .unwrap();
        let alpha = after.iter().position(|id| id.contains("alpha")).unwrap();
        let charlie = after.iter().position(|id| id.contains("charlie")).unwrap();
        assert!(big < alpha, "shown rows keep their order: {after:?}");
        assert_eq!(
            charlie,
            alpha + 1,
            "a new engine row joins the engine band: {after:?}"
        );
        assert!(!after.iter().any(|id| id.contains("bravo")));
        assert_eq!(before.len(), after.len(), "one row left, one row joined");
    }

    /// A vendor that signs out while its sub-menu is open falls back to the list.
    #[test]
    fn a_vendor_sub_menu_closes_when_its_list_goes_away() {
        let mut rails = [
            rail(Rail::Claude, true, true),
            rail(Rail::Codex, true, false),
            rail(Rail::Cursor, false, false),
        ];
        let mut p = loaded_with_rails(&rails);
        select_row(&mut p, "Claude");
        p.handle(PickerInput::Enter);
        assert_eq!(p.submenu, Some(Submenu::Vendor(Rail::Claude)));
        rails[0] = rail(Rail::Claude, true, false);
        p.apply_snapshot(PickerSnapshot {
            rows: models_rows(
                &workshop_providers::Catalog::builtin(),
                |_| false,
                &[],
                &rails,
            ),
            rails: rails.to_vec(),
            ..PickerSnapshot::default()
        });
        assert_eq!(p.submenu, None);
        assert_eq!(p.selected_row().map(|r| r.title()), Some("Claude".into()));
        assert_eq!(suffix_text(&p, &p.selected_row().unwrap()), "sign in");
    }

    #[test]
    fn plan_names_are_capitalised() {
        assert_eq!(plan_name("max"), "Max");
        assert_eq!(plan_name("pro"), "Pro");
        assert_eq!(plan_name("Plus"), "Plus");
        assert_eq!(plan_name(""), "");
    }
}
