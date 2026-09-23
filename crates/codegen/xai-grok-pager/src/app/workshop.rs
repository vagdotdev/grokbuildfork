//! Workshop overlay: connection picker data loading and selection activation.
//!
//! Models rows come from `workshop-providers`, rail state from `workshop-detect`, and the OpenCode
//! free catalog from the engine cache. Activating a Direct API / Local row writes
//! `[model.<key>]` + `default` into `$WORKSHOP_HOME/config.toml`, exports a saved key as its
//! `WORKSHOP_<PROVIDER>_API_KEY` env var (never into the file), asks the shell to reload its model
//! list, and authenticates with the non-interactive method. Nothing here starts an OAuth flow.
//!
//! Model lists are live, not compiled in: [`load_picker_snapshot`] reads the last fetched lists
//! from `$WORKSHOP_HOME/catalog-cache/` (never the network), [`refresh_picker_snapshot`] fetches
//! the keyless hosted lists and the engine's `/config/providers` and then reloads. A refresh runs
//! only after the user acted — an active connection at startup, `/model`, the picker's `r` — so
//! the first-run, `/login` and `/auth` screens stay hermetic.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use tokio::sync::{mpsc, oneshot, watch};
use workshop_adapters::opencode_engine::{
    EngineOptions, InstallOptions, InstallProgress, OpenCodeEngine, PermissionDecision,
    PermissionHandler, PermissionReply, PermissionRequest, TurnHandle, TurnRequest,
    WORKSHOP_AGENT_PROMPT, agent_prompts, ask_before_edit_and_bash, clear_quarantine,
    detect_opencode, format_bytes, install_opencode, instructions_config,
};

use crate::app::workshop_engine_state::{self as state, EngineState};
use workshop_adapters::supervisor::{RunHandle, SupervisorOptions, spawn};
use workshop_adapters::{
    AdapterEvent, AdapterId, DetectOptions, Detection, PermissionPolicy, RunRequest, Usage,
    detect,
};
use workshop_auth::{ENGINE_PROVIDER_ID, EngineModel, PickerSnapshot, models_rows};
use workshop_providers::catalog::live::{self as live_catalogs, HostedCatalogs};
use workshop_providers::{
    Catalog, CatalogStatus, CredentialBroker, CredentialInjection, FileSecretStore, Freshness,
    KeyringSecretStore, LayeredSecretStore, RefreshOptions, resolve_model_entry, select_default,
};

/// Which runtime a prompt is routed through.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkshopConnection {
    /// The shell's own agent loop (Direct API / Local `[model.<key>]`).
    Shell,
    /// A free model behind the OpenCode engine (`opencode serve`); the first-run default.
    Engine { model: EngineModel },
    /// A vendor CLI adapter on a Ready subscription rail.
    Adapter {
        rail: workshop_detect::Rail,
        model: workshop_detect::ModelRef,
    },
}

impl WorkshopConnection {
    /// Composer label: `OpenCode · {model}` for the engine, `Claude · {model}` for rails.
    pub fn composer_label(&self) -> Option<String> {
        match self {
            Self::Shell => None,
            Self::Engine { model } => Some(format!(
                "{} · {}",
                workshop_auth::ENGINE_DISPLAY_NAME,
                model.name
            )),
            Self::Adapter { rail, model } => Some(workshop_detect::composer_label(*rail, model)),
        }
    }
    pub fn is_shell(&self) -> bool {
        matches!(self, Self::Shell)
    }
    pub fn is_engine(&self) -> bool {
        matches!(self, Self::Engine { .. })
    }
    /// The live model's context window, from the engine's catalog (`200K` for Big Pickle).
    /// `None` when the connection does not report one — the meter then stays hidden instead of
    /// showing a guess.
    pub fn context_limit(&self) -> Option<u64> {
        match self {
            Self::Engine { model } => model.context_limit.filter(|n| *n > 0),
            Self::Shell | Self::Adapter { .. } => None,
        }
    }
    /// The model's plain display name (`Big Pickle`, `claude-sonnet-4-5`) for places that show
    /// one name, not a composer label. `None` for Shell (the shell model's own name shows).
    pub fn model_display_name(&self) -> Option<String> {
        match self {
            Self::Shell => None,
            Self::Engine { model } => Some(model.name.clone()),
            Self::Adapter { model, .. } => Some(
                model
                    .display_name
                    .clone()
                    .unwrap_or_else(|| model.model.clone()),
            ),
        }
    }
    /// Picker row id of the active connection (Engine rows only; rails are not Models rows and a
    /// Shell connection is the shell's own default model).
    pub fn active_row_id(&self) -> Option<String> {
        match self {
            Self::Engine { model } => Some(model.row_id()),
            Self::Shell | Self::Adapter { .. } => None,
        }
    }
}

/// The context meter's numbers for the active connection: `None` for Shell (the shell's own
/// numbers show), else `(engine usage so far, live model's context window)`.
pub fn context_meter(app: &crate::app::app_view::AppView) -> Option<(Option<u64>, Option<u64>)> {
    if app.workshop_connection.is_shell() {
        None
    } else {
        Some((
            app.workshop_context_used,
            app.workshop_connection.context_limit(),
        ))
    }
}

/// The `/context` text for an Engine/Adapter connection: the live model's usage against its
/// window when the engine has reported one, else that it is not known yet — never the shell
/// placeholder's numbers. `None` for Shell (the shell's own snapshot shows). Lines starting with
/// `·` render muted.
pub fn context_lines(app: &crate::app::app_view::AppView) -> Option<Vec<String>> {
    use crate::views::context_bar::fmt_tokens;
    let (used, limit) = context_meter(app)?;
    let model = app
        .workshop_connection
        .composer_label()
        .unwrap_or_else(|| "this connection".to_owned());
    let mut lines = vec!["Context".to_owned(), String::new()];
    match (used, limit) {
        (Some(used), Some(limit)) => {
            let pct = if limit > 0 {
                used as f64 * 100.0 / limit as f64
            } else {
                0.0
            };
            lines.push(format!(
                "{} / {} tokens ({pct:.1}%)",
                fmt_tokens(used),
                fmt_tokens(limit)
            ));
            lines.push(model);
            lines.push(String::new());
            lines.push("· Engine-reported after the last step (prompt + cached + output).".to_owned());
        }
        (None, Some(limit)) => {
            lines.push(format!("{model} · {} token window", fmt_tokens(limit)));
            lines.push(String::new());
            lines.push("· No usage yet — the engine reports it after the first message.".to_owned());
        }
        (_, None) => {
            lines.push(model);
            lines.push(String::new());
            lines.push("· This connection does not report context usage.".to_owned());
        }
    }
    Some(lines)
}

/// Stamp every agent with the active connection's composer label and context meter numbers
/// (after a connection change, a resolved live default, or a new usage report).
pub fn sync_agent_views(app: &mut crate::app::app_view::AppView) {
    let label = app.workshop_connection.composer_label();
    let context = context_meter(app);
    for agent in app.agents.values_mut() {
        agent.workshop_model_label = label.clone();
        agent.workshop_context = context;
    }
}

/// `$WORKSHOP_HOME` (created if missing).
pub fn workshop_home() -> PathBuf {
    xai_dirs::grok_home()
}

fn active_connection_path() -> PathBuf {
    workshop_home().join("active-connection.json")
}

/// The connection this home last activated. Engine/Adapter selections are not shell models, so
/// they live here rather than in config.toml; `Shell` (or no file) means the shell's default model.
pub fn load_active_connection() -> WorkshopConnection {
    std::fs::read_to_string(active_connection_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(WorkshopConnection::Shell)
}

pub fn save_active_connection(conn: &WorkshopConnection) {
    let path = active_connection_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_vec_pretty(conn) {
        let _ = workshop_providers::atomic_write_private(&path, &json);
    }
}

/// True until something has been connected: no `[model.*]` entry in `$WORKSHOP_HOME/config.toml`.
/// A first run lands in the composer with [`first_run_connection`] active and never shows a picker.
pub fn is_first_run() -> bool {
    let Ok(text) = std::fs::read_to_string(workshop_auth::config_path()) else {
        return true;
    };
    let Ok(doc) = text.parse::<toml::Table>() else {
        return true;
    };
    doc.get("model")
        .and_then(toml::Value::as_table)
        .is_none_or(|models| models.is_empty())
}

/// The first-run connection: the OpenCode engine's own default free model. Before the engine has
/// ever run there is no live catalog, so this is the cached one when this home has seen one, else
/// the pinned seed (Big Pickle today); the first engine start replaces it with whatever the live
/// catalog marks as default (`live_engine_default`), and the pinned name stands only offline.
pub fn first_run_connection() -> WorkshopConnection {
    WorkshopConnection::Engine {
        model: EngineModel::first_run_default(&cached_engine_models()),
    }
}

/// The keyless Direct API row Workshop falls back to when the OpenCode engine cannot start
/// (offline, installer failed): the Kilo community pool.
pub fn kilo_fallback_model() -> Option<workshop_providers::CatalogModel> {
    Catalog::builtin()
        .get(&format!(
            "kilo:{}",
            workshop_providers::KILO_DEFAULT_CHAIN[0]
        ))
        .cloned()
}

/// The credential broker over the OS keyring with the owner-only file fallback under the home.
pub fn default_broker() -> CredentialBroker {
    let home = workshop_home();
    let tag = home
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "workshop".into());
    let store = LayeredSecretStore::new(
        Box::new(KeyringSecretStore::for_home(&tag)),
        FileSecretStore::new(home.join("secrets")),
    );
    CredentialBroker::new(Arc::new(store), home.join("connections.json"))
}

/// `$WORKSHOP_HOME/catalog-cache`: the hosted lists (`<provider>.json`, written by
/// `workshop_providers::catalog::live`), the engine list (`opencode-engine.json`), and each
/// subscription's last listed models (`<rail>-models.json`, `workshop_detect::ModelsCache`).
fn catalog_cache_dir() -> PathBuf {
    workshop_providers::catalog::fetch::default_cache_dir(&workshop_home())
}

fn engine_cache_path() -> PathBuf {
    catalog_cache_dir().join("opencode-engine.json")
}

/// The last engine catalog this home saw (written after every engine launch).
pub fn cached_engine_models() -> Vec<EngineModel> {
    std::fs::read_to_string(engine_cache_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn store_engine_models(models: &[EngineModel]) {
    let path = engine_cache_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_vec_pretty(models) {
        let _ = workshop_providers::atomic_write_private(&path, &json);
    }
}

/// Where the engine rows come from: the cache file's write time when the engine list was ever
/// fetched (`fetched <age>`), else the pinned seed (`cached list from <date>`). `error` is the
/// reason the last live read failed, when one was attempted.
fn engine_catalog_status(models: &[EngineModel], error: Option<String>) -> CatalogStatus {
    let fetched_at = std::fs::metadata(engine_cache_path())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    match fetched_at {
        Some(at) if !models.is_empty() => CatalogStatus {
            provider_id: ENGINE_PROVIDER_ID.into(),
            freshness: Freshness::Cached,
            fetched_at_secs: Some(at),
            rows: models.len(),
            error,
        },
        _ => CatalogStatus {
            error,
            ..CatalogStatus::seed(ENGINE_PROVIDER_ID, 1)
        },
    }
}

/// Read the engine's live free catalog (`GET /config/providers` on loopback) and cache it for
/// `/model`. `Err` when the catalog cannot be read (old engine, server gone).
pub async fn refresh_engine_catalog(engine: &OpenCodeEngine) -> Result<Vec<EngineModel>, String> {
    let catalog = engine.free_models().await.map_err(|e| e.to_string())?;
    let models: Vec<EngineModel> = catalog
        .models
        .iter()
        .map(|m| EngineModel {
            model_ref: m.model_ref.clone(),
            name: m.name.clone(),
            is_default: m.is_default,
            tool_call: m.tool_call,
            context_limit: m.context_limit,
        })
        .collect();
    if models.is_empty() {
        return Err("opencode reported no free models".into());
    }
    store_engine_models(&models);
    Ok(models)
}

/// Build the picker snapshot: loopback local-server probe, the CLI rail probe (child processes,
/// so on the blocking pool), then the model lists — the last fetched hosted lists and engine list
/// from `catalog-cache/` (seeds where nothing was fetched yet), never the network — and the broker
/// connection state.
///
/// This is the single data entry point of the `/model` + `/auth` overlay: everything the two views
/// list comes from the `PickerSnapshot` returned here (`workshop_auth::models_rows` builds the rows
/// from a `Catalog` plus the engine models; `PickerState::apply_snapshot` takes it). The lists are
/// read last so a snapshot built while [`refresh_picker_snapshot`] runs still sees what it cached.
/// A signed-in rail shows its last listed models, or `Loading models…` until
/// [`refresh_rail_models_snapshot`] (or the `/model` refresh) asks its CLI.
pub async fn load_picker_snapshot() -> PickerSnapshot {
    build_picker_snapshot(None, None, workshop_detect::Refresh::CacheOnly).await
}

/// [`load_picker_snapshot`], but every signed-in rail asks its own CLI for its models (unless it
/// listed them in the last minute). Child processes only: Workshop itself makes no request.
pub async fn refresh_rail_models_snapshot() -> PickerSnapshot {
    let refresh = workshop_detect::Refresh::Live {
        max_age: workshop_detect::models::FRESH_FOR,
    };
    build_picker_snapshot(None, None, refresh).await
}

/// Refresh the model lists from their live sources, then build the snapshot: the keyless hosted
/// lists (Kilo, OpenRouter, NVIDIA; concurrently, short deadline, cached on success), when an
/// engine is up its `/config/providers`, and each signed-in rail's CLI. `force` ignores the cache
/// age (the picker's Ctrl+R). Only ever called after the user acted; failures leave the last
/// cached list or the dated seed.
pub async fn refresh_picker_snapshot(
    engine: Option<Arc<OpenCodeEngine>>,
    force: bool,
) -> PickerSnapshot {
    let opts = if force {
        RefreshOptions::forced()
    } else {
        RefreshOptions::default()
    };
    let cache_dir = catalog_cache_dir();
    let hosted = live_catalogs::refresh(&cache_dir, opts);
    let engine_result = async {
        match engine {
            Some(engine) => refresh_engine_catalog(&engine).await.err(),
            None => None,
        }
    };
    let (hosted, engine_error) = tokio::join!(hosted, engine_result);
    let rails = workshop_detect::Refresh::Live {
        max_age: if force {
            Duration::ZERO
        } else {
            workshop_detect::models::FRESH_FOR
        },
    };
    let mut snap = build_picker_snapshot(Some(hosted), engine_error, rails).await;
    snap.live = true;
    snap
}

async fn build_picker_snapshot(
    hosted: Option<HostedCatalogs>,
    engine_error: Option<String>,
    rail_models: workshop_detect::Refresh,
) -> PickerSnapshot {
    let local = workshop_providers::probe_all_local_servers(Duration::from_millis(600)).await;
    let rails = tokio::task::spawn_blocking(move || {
        let cache = workshop_detect::ModelsCache::new(catalog_cache_dir());
        workshop_detect::picker_rails(
            &workshop_detect::DetectConfig::default(),
            &cache,
            rail_models,
        )
        .to_vec()
    })
    .await
    .unwrap_or_else(|_| {
        workshop_detect::Rail::ALL
            .iter()
            .map(|r| workshop_detect::RailState::detecting(*r))
            .collect()
    });
    let HostedCatalogs {
        mut catalog,
        status: hosted_status,
    } = hosted.unwrap_or_else(|| live_catalogs::load_cached(&catalog_cache_dir()));
    let as_of = "live";
    for status in &local {
        if status.is_reachable()
            && let Some(m) = workshop_providers::manifest(&status.provider_id)
        {
            catalog.replace_provider(
                &status.provider_id,
                workshop_providers::local::local_rows(&m, status, as_of),
            );
        }
    }
    let broker = default_broker();
    let secret_backend = broker.secret_backend();
    let engine = cached_engine_models();
    let rows = models_rows(&catalog, |id| broker.is_connected(id), &engine);
    let default_selection = Some(select_default(&local, true));
    let mut catalog_status = vec![engine_catalog_status(&engine, engine_error)];
    catalog_status.extend(hosted_status);
    PickerSnapshot {
        rows,
        rails,
        default_selection,
        secret_backend: Some(secret_backend),
        catalog_status,
        live: false,
    }
}

/// Write a keyless placeholder model + make it the shell default, then return its config key.
/// Engine/Adapter connections don't use a shell model — but the shell needs *a* model + the
/// non-interactive auth method to open an ACP session (the agent view that renders the streamed
/// turn). The placeholder is never contacted: `dispatch_workshop_turn` intercepts prompts and
/// routes them to the engine/adapter. Delegates to `workshop-auth` (which owns the config schema).
pub fn activate_placeholder_session(conn: &WorkshopConnection) -> Result<String, String> {
    let placeholder = workshop_auth::config_write::PlaceholderModel {
        display_name: conn
            .model_display_name()
            .unwrap_or_else(|| "Workshop connection".to_owned()),
        context_window: conn.context_limit(),
    };
    workshop_auth::config_write::activate_placeholder_session(
        &workshop_auth::config_path(),
        &placeholder,
    )
    .map_err(|e| e.to_string())
}

/// `workshop models` for an Engine/Adapter connection: the live model and the models the
/// connection actually offers, instead of the shell's placeholder ids. Returns `None` for a
/// Shell connection (the shell's own list applies).
pub fn connection_models_text() -> Option<String> {
    let conn = load_active_connection();
    let mut out = String::new();
    match &conn {
        WorkshopConnection::Shell => return None,
        WorkshopConnection::Engine { model } => {
            out.push_str(&format!(
                "Model: {} ({}) — free, no key\n\nAvailable models ({}):\n",
                model.name,
                workshop_auth::ENGINE_DISPLAY_NAME,
                workshop_auth::ENGINE_DISPLAY_NAME
            ));
            let mut rows = cached_engine_models();
            if rows.is_empty() {
                rows.push(model.clone());
            }
            for m in rows {
                let marker = if m.model_ref == model.model_ref {
                    "*"
                } else {
                    "-"
                };
                let active = if m.model_ref == model.model_ref {
                    " (active)"
                } else {
                    ""
                };
                out.push_str(&format!("  {marker} {} — {}{active}\n", m.model_ref, m.name));
            }
            out.push_str("\nSwitch with /model inside workshop.\n");
        }
        WorkshopConnection::Adapter { rail, model } => {
            out.push_str(&format!(
                "Model: {}\n\nSwitch with /model inside workshop.\n",
                workshop_detect::composer_label(*rail, model)
            ));
        }
    }
    Some(out)
}

/// What activating a Direct API / Local row needs the process to do.
#[derive(Debug, Clone)]
pub struct ActivationPlan {
    /// `[model.<key>]` key written to config.toml (and set as `default`).
    pub key: String,
    pub display_name: String,
    pub base_url: String,
    /// Env var exports the shell needs before it reloads (saved-key providers only).
    pub env: Vec<(String, String)>,
}

/// Resolve `model` against the broker, write it to config.toml, and describe the follow-up.
pub fn activate_catalog_model(
    model: &workshop_providers::CatalogModel,
) -> Result<ActivationPlan, String> {
    let broker = default_broker();
    let spec = resolve_model_entry(model, &broker).map_err(|e| e.to_string())?;
    let mut env = Vec::new();
    if let CredentialInjection::FromBroker { provider_id, var } = &spec.credential {
        let handle = broker.resolve(provider_id).map_err(|e| e.to_string())?;
        let value = handle
            .authorize(&model.endpoint())
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no saved credential for {provider_id}"))?
            .to_owned();
        env.push((var.clone(), value));
    }
    let key = workshop_auth::config_write::activate_model(&workshop_auth::config_path(), &spec)
        .map_err(|e| e.to_string())?;
    Ok(ActivationPlan {
        key,
        display_name: spec.name.clone(),
        base_url: spec.base_url.clone(),
        env,
    })
}

/// Export the saved-key env vars in this process (the agent runs in-process and reads them on reload).
pub fn export_env(env: &[(String, String)]) {
    for (k, v) in env {
        // SAFETY: single-threaded UI thread mutating process env before the agent's reload request
        // is sent; readers (`std::env::var`) tolerate concurrent reads on Unix targets we ship.
        unsafe { std::env::set_var(k, v) };
    }
}

/// Persist a pasted key for `provider_id` in the broker (keyring, else the 0600 file fallback).
pub fn save_provider_key(provider_id: &str, key: &str) -> Result<&'static str, String> {
    let broker = default_broker();
    broker
        .save_api_key(provider_id, key)
        .map_err(|e| e.to_string())?;
    Ok(broker.secret_backend())
}

/// Key-entry prompt label for a provider's paste-key connect flow.
pub fn key_prompt_label(provider_id: &str) -> String {
    match workshop_providers::manifest(provider_id) {
        Some(m) => match &m.credential_url {
            Some(url) => format!("Paste your {} API key (get one at {url})", m.display_name),
            None => format!("Paste your {} API key", m.display_name),
        },
        None => format!("Paste your {provider_id} API key"),
    }
}

/// OpenRouter PKCE sign-in: opens the browser, waits for the loopback callback, exchanges the code,
/// saves the key. Returns the secret backend used.
pub async fn openrouter_sign_in() -> Result<&'static str, String> {
    use workshop_providers::{OpenRouterSignIn, SignInMode};
    let signin = OpenRouterSignIn::start(SignInMode::Loopback)
        .await
        .map_err(|e| e.to_string())?;
    let url = signin.authorize_url();
    if webbrowser::open(&url).is_err() {
        tracing::warn!("could not open a browser for OpenRouter sign-in; url: {url}");
    }
    let client = workshop_providers::oauth::openrouter::exchange_client().map_err(|e| e.to_string())?;
    let key = signin
        .complete_loopback(&client, Duration::from_secs(300))
        .await
        .map_err(|e| e.to_string())?;
    save_provider_key("openrouter", &key)
}

/// A live turn stream from either backend. `OpenCodeEngine::prompt` (free tier) and the CLI
/// supervisor (`spawn`, subscriptions) share the same `AdapterEvent` contract — `next_event`,
/// `cancel`, `session_id` — so one consumer loop in the TUI serves both.
pub enum TurnStream {
    Engine(TurnHandle),
    Adapter(RunHandle),
}

impl TurnStream {
    pub async fn next_event(&mut self) -> Option<AdapterEvent> {
        match self {
            Self::Engine(t) => t.next_event().await,
            Self::Adapter(r) => r.next_event().await,
        }
    }
    pub fn cancel(&self) {
        match self {
            Self::Engine(t) => t.cancel(),
            Self::Adapter(r) => r.cancel(),
        }
    }
    pub fn session_id(&self) -> Option<String> {
        match self {
            Self::Engine(t) => Some(t.session_id().to_string()),
            Self::Adapter(r) => r.session_id(),
        }
    }
}

/// What the UI thread learns as a turn streams. Mapped to scrollback `RenderBlock`s by the event
/// loop's Workshop `select!` arm (kept UI-agnostic so `workshop-adapters` never depends on the pager).
#[derive(Debug)]
pub enum WorkshopTurnMsg {
    /// One line of bring-up status ("Installing the OpenCode engine…"); replaces the previous
    /// progress line, and the first real event clears it. Never silent while the user waits.
    Progress(String),
    /// The OpenCode engine started (lazily, on the first turn); cache it and the session so later
    /// turns reuse the same `opencode serve` and conversation. Engine turns only.
    EngineReady {
        engine: Arc<OpenCodeEngine>,
        session: String,
    },
    /// The first-keystroke warm-up finished: the engine is up before the first message.
    EngineWarm { engine: Arc<OpenCodeEngine> },
    /// The engine's live catalog names a different default than the pinned seed the first run
    /// activated: the connection follows OpenCode's default (composer label, persisted file).
    EngineDefaultResolved { model: EngineModel },
    Delta(String),
    /// A chunk of the model's reasoning: rendered as the pager's collapsed thinking block, never
    /// as part of the answer.
    Thinking(String),
    /// The agent started a tool call; `input` is the tool's full argument object (the path and
    /// content of a `write`, the `command` of a `bash`), from which the row's summary and, once
    /// the result lands, its expandable body are built.
    Tool {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// The tool call finished: `output` is the tool's text (stdout+stderr for `bash`), `title`
    /// and `metadata` the backend's detail (`exit`, `diff`, `filediff`, …) when it reports any.
    ToolResult {
        id: String,
        ok: bool,
        output: String,
        title: Option<String>,
        metadata: serde_json::Value,
    },
    /// The engine asks before an edit or a command (its `ask` policy): the UI thread decides
    /// from the agent's live permission mode — auto-approve modes answer at once, Normal and Plan
    /// show the approval prompt — and sends the answer on `reply`. Dropping `reply` is a reject.
    PermissionAsk {
        request: PermissionRequest,
        reply: oneshot::Sender<PermissionReply>,
    },
    /// The user answered a prompt for tool call `call_id`; the same call's next ask (the engine
    /// asks `external_directory` and then `bash` for one out-of-folder command) gets the same
    /// answer without a second prompt.
    PermissionDecided {
        call_id: String,
        decision: PermissionReply,
    },
    /// Token accounting the backend reported for a finished step (the engine's `step-finish`);
    /// the last one of a turn is the model's current context usage.
    Usage(Usage),
    /// A message the user typed while declining a permission ("No, and tell Workshop what to do
    /// differently"): sent as the next turn once this one ends.
    FollowUp(String),
    Error(String),
    /// The OpenCode engine could not be started for this turn (offline, installer failed, no
    /// verified `opencode`). `text` is the prompt that never ran; the UI falls back to the Kilo
    /// keyless pool and resends it. Followed by `Done`.
    EngineUnavailable {
        reason: String,
        text: String,
    },
    /// The turn ended; `session_id` is persisted per workspace for resume.
    Done {
        session_id: Option<String>,
        cancelled: bool,
    },
}

/// Why a turn could not start.
enum TurnStartError {
    /// `opencode` could not be detected, installed, or started.
    EngineUnavailable(String),
    Other(String),
}

/// The one `opencode serve` this process owns, shared by the first-keystroke warm-up and every
/// turn so two callers never start two servers: whoever holds the lock starts it, the other
/// reuses it. `phase` carries the live bring-up status so a turn that waits on the lock can show
/// what the other task is doing ("Installing…") instead of a generic line.
pub struct EngineSlotInner {
    engine: tokio::sync::Mutex<Option<Arc<OpenCodeEngine>>>,
    phase: watch::Sender<String>,
    /// The warm-up's failure, so a turn typed right after it reports that cause at once instead
    /// of silently repeating a 30 s bring-up that just failed. Cleared by the next attempt.
    recent_failure: std::sync::Mutex<Option<(std::time::Instant, String)>>,
}

pub type EngineSlot = Arc<EngineSlotInner>;

pub fn new_engine_slot() -> EngineSlot {
    Arc::new(EngineSlotInner {
        engine: tokio::sync::Mutex::new(None),
        phase: watch::channel(String::new()).0,
        recent_failure: std::sync::Mutex::new(None),
    })
}

/// The pager's permission mode as it applies to one Engine/Adapter turn (Shift+Tab cycle,
/// `/plan`, `/auto`, `/always-approve`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkshopPermissionMode {
    /// Read-only: the engine runs OpenCode's `plan` agent, which cannot edit files or run
    /// destructive commands.
    Plan,
    /// The engine asks before every edit and command; the user answers in the approval prompt.
    Normal,
    /// Asks are approved without a prompt (the shell's classifier does not run on engine turns).
    Auto,
    /// Asks are approved without a prompt.
    AlwaysApprove,
}

impl WorkshopPermissionMode {
    /// Whether asks are answered without showing a prompt.
    pub fn auto_approves(self) -> bool {
        matches!(self, Self::Auto | Self::AlwaysApprove)
    }
}

/// How long a warm-up failure is reported to the next turn as-is before it retries.
const RECENT_FAILURE_WINDOW: Duration = Duration::from_secs(60);
/// Hard ceiling on a turn's silence before its first event: past this the engine is up but the
/// model never answered, and the user gets the cause plus a way out instead of a spinner.
pub const FIRST_EVENT_TIMEOUT: Duration = Duration::from_secs(90);
/// Hard ceiling on `opencode serve` binding its port and passing its health check.
pub const ENGINE_START_TIMEOUT: Duration = Duration::from_secs(30);
/// A model that has not sent anything back after this long is said to still be connecting: the
/// waiting line changes so the user knows the wait is the network's, not a hang.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The waiting line shows its elapsed seconds only once the wait is long enough to feel like one.
pub const ELAPSED_AFTER: Duration = Duration::from_secs(3);
/// Frames of the animated mark in front of the waiting line.
pub const WAIT_SPINNER: [char; 10] = [
    '\u{280B}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283C}', '\u{2834}', '\u{2826}', '\u{2827}',
    '\u{2807}', '\u{280F}',
];

/// The one line a user sees while a turn has produced nothing yet: an animated mark, the phase
/// ("Installing…", "Waiting for Big Pickle…"), the elapsed seconds after [`ELAPSED_AFTER`], and
/// how to stop waiting. Repainted every tick by the UI so the mark moves and the seconds count.
pub fn waiting_line(text: &str, elapsed: Duration, frame: usize) -> String {
    let mark = WAIT_SPINNER
        .get(frame % WAIT_SPINNER.len())
        .copied()
        .unwrap_or(' ');
    let mut line = format!("{mark} {text}");
    if elapsed >= ELAPSED_AFTER {
        line.push_str(&format!(" \u{b7} {}s", elapsed.as_secs()));
    }
    line.push_str(" \u{b7} Ctrl+C to cancel");
    line
}

/// The install phase with byte progress, once the vendor script has started writing.
pub fn install_progress_line(bytes: u64) -> String {
    format!(
        "Installing the OpenCode engine (first time only)\u{2026} {} downloaded",
        format_bytes(bytes)
    )
}

/// Which backend a submitted prompt should run on.
pub enum WorkshopTurnKind {
    Engine {
        slot: EngineSlot,
        session: Option<String>,
        model: EngineModel,
    },
    Adapter {
        adapter_id: AdapterId,
        resume: Option<String>,
        model: Option<String>,
    },
}

/// One submitted prompt to route off the ACP path.
pub struct WorkshopTurnSpec {
    pub kind: WorkshopTurnKind,
    pub cwd: PathBuf,
    pub text: String,
    /// The agent's permission mode when the prompt was sent. Engine: Plan → the read-only
    /// `plan` agent, everything else → `build` with the engine asking before edits/commands.
    /// Vendor CLIs: AlwaysApprove → `WorkspaceWrite`, else their read-only default.
    pub mode: WorkshopPermissionMode,
}

/// One-line summary of a tool call for its transcript row: the path, command, pattern or URL.
pub fn summarize_tool_input(input: &serde_json::Value) -> String {
    for key in ["filePath", "path", "command", "pattern", "query", "url"] {
        if let Some(v) = input.get(key).and_then(serde_json::Value::as_str) {
            return v.to_owned();
        }
    }
    String::new()
}

fn resume_store_path() -> PathBuf {
    workshop_home().join("adapter-sessions.json")
}

/// The vendor/engine session id last seen for `(backend, workspace)`, for resume.
pub fn load_resume_id(backend: &str, cwd: &Path) -> Option<String> {
    let map: serde_json::Value = std::fs::read_to_string(resume_store_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())?;
    map.get(format!("{backend}:{}", cwd.display()))
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

/// Whether `cwd` has anything to come back to: a saved session with messages, or an engine /
/// adapter conversation id recorded for this workspace. The welcome menu reads it once per launch
/// to decide whether to offer "Resume session" at all.
pub fn has_resumable_sessions(cwd: &Path) -> bool {
    use xai_grok_shell::session::persistence::{
        RecentSessionSelection, local_summaries_for_cwd_sync,
    };
    let with_messages = local_summaries_for_cwd_sync(
        &cwd.to_string_lossy(),
        RecentSessionSelection::Interactive,
    )
    .map(|list| {
        list.iter()
            .any(|s| s.num_messages > 0 || s.num_chat_messages > 0)
    })
    .unwrap_or(false);
    with_messages
        || load_resume_id("opencode", cwd).is_some()
        || workshop_detect::Rail::ALL
            .iter()
            .any(|rail| load_resume_id(rail.vendor().id(), cwd).is_some())
}

/// Persist the session id for `(backend, workspace)` so the next turn resumes the conversation.
pub fn save_resume_id(backend: &str, cwd: &Path, session_id: &str) {
    let path = resume_store_path();
    let mut map: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    map.insert(
        format!("{backend}:{}", cwd.display()),
        serde_json::Value::String(session_id.to_owned()),
    );
    if let Ok(bytes) = serde_json::to_vec_pretty(&map) {
        let _ = workshop_providers::atomic_write_private(&path, &bytes);
    }
}

/// Build the engine permission hook: every ask goes to the UI thread, which answers from the
/// agent's permission mode *at the time of the ask* (the hook outlives the turn that started the
/// engine): auto-approve modes reply at once, Normal/Plan show the approval prompt and reply
/// when the user picks. A UI that is gone (channel closed) fails closed: the ask is rejected.
fn engine_permission_handler(tx: mpsc::UnboundedSender<WorkshopTurnMsg>) -> PermissionHandler {
    Arc::new(move |req| {
        let (reply_tx, reply_rx) = oneshot::channel();
        match tx.send(WorkshopTurnMsg::PermissionAsk {
            request: req.clone(),
            reply: reply_tx,
        }) {
            Ok(()) => PermissionDecision::Pending(reply_rx),
            Err(_) => PermissionDecision::Reply(PermissionReply::Reject),
        }
    })
}

/// The session topic shown in the terminal title: the first line of the first prompt, trimmed to
/// a tab-width string.
pub fn session_topic(prompt: &str) -> String {
    const MAX: usize = 40;
    let line = prompt
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    let mut topic: String = line.chars().take(MAX).collect();
    if line.chars().count() > MAX {
        topic = topic.trim_end().to_owned();
        topic.push('\u{2026}');
    }
    topic
}

/// The ways out appended to every failure line: retry the same prompt, switch model, diagnose.
pub const ERROR_WAYS_OUT: &str = "Enter retries · /model switches model · /doctor checks the setup";

/// The one line a user sees when the engine path fails after the engine is up: the cause, the
/// ways out, and where the details went. (A failure *before* the engine is up goes through the
/// Kilo fallback instead — see `TurnStartError::EngineUnavailable`.)
pub fn engine_failure_line(cause: &str) -> String {
    format!(
        "OpenCode engine: {cause} — {ERROR_WAYS_OUT}; log: {}",
        engine_log_path().display()
    )
}

/// A backend error as the user sees it: the message (one line) with the ways out, unless the
/// message already carries them (engine failure lines do).
pub fn actionable_error_line(message: &str) -> String {
    let message = message.trim();
    if message.contains(ERROR_WAYS_OUT) {
        message.to_owned()
    } else {
        format!("{message} — {ERROR_WAYS_OUT}")
    }
}

/// `$WORKSHOP_HOME/logs/opencode-engine.log`, the `opencode serve` stdout/stderr capture.
pub fn engine_log_path() -> PathBuf {
    state::log_path(&workshop_home())
}

/// `$WORKSHOP_HOME/engine/instructions.md`: the identity the engine's models are given.
pub fn engine_instructions_path() -> PathBuf {
    workshop_home().join("engine").join("instructions.md")
}

/// What the engine's models are told about where they run. Appended by OpenCode to every system
/// prompt it builds for Workshop's server (all agents), like a project `AGENTS.md` — but kept
/// under `$WORKSHOP_HOME`, so nothing is written into the user's project. It follows OpenCode's
/// environment block, which names the model by its provider-qualified id (`opencode/big-pickle`).
pub const ENGINE_INSTRUCTIONS: &str = "\
# Workshop

You are Workshop's coding assistant. Workshop is the terminal application the user launched; you \
are the model working inside it. When asked who or what you are, or who made you, say you are \
Workshop's coding assistant and call the model you are running as by its short name only: for a \
model ID of the form provider/name, say just the name. The provider part of the model ID only says \
where the model is hosted, not who made you or Workshop, so never mention it. Do not describe the \
software you run on and do not introduce yourself by any other product name.

Everything else about how you work — tools, conventions, permissions — is as instructed above.
";

/// The engine's inline config: Workshop's base prompt for the agents its turns run on, and the
/// identity file for this launch. A home that cannot be written drops only the file; the cause
/// goes to the engine log.
fn engine_config(log: &Path) -> serde_json::Value {
    let mut config = match write_engine_instructions(log) {
        Some(path) => instructions_config(&[path]),
        None => serde_json::json!({}),
    };
    if let Some(fields) = config.as_object_mut() {
        fields.insert("agent".into(), agent_prompts(WORKSHOP_AGENT_PROMPT));
    }
    config
}

/// Write the identity file (idempotent); `None` when the home cannot be written.
fn write_engine_instructions(log: &Path) -> Option<PathBuf> {
    let path = engine_instructions_path();
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        state::append_log(log, &format!("instructions: cannot create {}: {e}", parent.display()));
        return None;
    }
    if std::fs::read_to_string(&path).ok().as_deref() != Some(ENGINE_INSTRUCTIONS)
        && let Err(e) = workshop_providers::atomic_write_private(&path, ENGINE_INSTRUCTIONS.as_bytes())
    {
        state::append_log(log, &format!("instructions: cannot write {}: {e}", path.display()));
        return None;
    }
    Some(path)
}

/// The waiting line's text while `model` works and nothing new is on screen.
fn waiting_for(model: &str) -> String {
    format!("Waiting for {model}\u{2026}")
}

fn engine_progress(tx: &mpsc::UnboundedSender<WorkshopTurnMsg>, text: impl Into<String>) {
    let _ = tx.send(WorkshopTurnMsg::Progress(text.into()));
}

/// Progress that also updates the slot's live phase for whoever is waiting on the lock.
fn engine_phase(slot: &EngineSlot, tx: &mpsc::UnboundedSender<WorkshopTurnMsg>, text: &str) {
    let _ = slot.phase.send(text.to_owned());
    engine_progress(tx, text);
}

/// Record a failed bring-up for `workshop doctor` and return the cause with the log pointer.
fn engine_fail(st: &mut EngineState, home: &Path, cause: String) -> String {
    // One line: installer/serve stderr can carry newlines, and the fallback notice keeps the
    // first line only.
    let cause = cause.split_whitespace().collect::<Vec<_>>().join(" ");
    st.last_error = Some(cause.clone());
    st.save(home);
    format!("{cause}; log: {}", state::log_path(home).display())
}

/// Bring the engine up, one visible step at a time: detect → (install) → start. Every step has a
/// hard timeout (installer 300 s, `serve` ready [`ENGINE_START_TIMEOUT`]), every failure returns
/// one line with the actual cause, and each phase is recorded in `$WORKSHOP_HOME/engine/`.
/// `tx` carries the bring-up progress lines; `ui_tx` is the interactive loop's channel, where the
/// server's permission asks go for as long as this `opencode serve` lives.
async fn start_engine(
    slot: &EngineSlot,
    workspace: &Path,
    tx: mpsc::UnboundedSender<WorkshopTurnMsg>,
    ui_tx: mpsc::UnboundedSender<WorkshopTurnMsg>,
) -> Result<OpenCodeEngine, String> {
    let home = workshop_home();
    let log = state::log_path(&home);
    let mut st = EngineState::load(&home).unwrap_or_default();
    st.last_start_unix = Some(state::now_unix());
    st.last_error = None;
    st.log_path = Some(log.clone());
    st.last_phase = Some("detect".into());
    st.save(&home);

    // Detect an `opencode` on PATH / known dirs / the Workshop tools tree; install the pinned
    // version via the vendor's own script only if absent (never a bundled binary). The download
    // reports its bytes so the first minute is never a static line.
    let install = InstallOptions {
        progress: Some(InstallProgress::new({
            let slot = slot.clone();
            let tx = tx.clone();
            move |bytes| engine_phase(&slot, &tx, &install_progress_line(bytes))
        })),
        ..InstallOptions::default()
    };
    let detect_opts = DetectOptions::default();
    let run_installer = |st: &mut EngineState, log: &Path| {
        st.last_phase = Some("install".into());
        st.save(&home);
        state::append_log(log, "install: running the official OpenCode installer");
    };
    let cli = match detect_opencode(&detect_opts, Some(&install)).await {
        Detection::Installed(cli) => cli,
        Detection::Unverified { path, reason }
            if workshop_adapters::opencode_engine::is_workshop_managed(&path) =>
        {
            // Our own install no longer runs (a quarantine flag from an older Workshop, a
            // half-written binary): clear the flag and re-check, else reinstall over it.
            engine_phase(slot, &tx, "Repairing the OpenCode engine install…");
            state::append_log(
                &log,
                &format!("repair: `{}` failed verification: {reason}", path.display()),
            );
            clear_quarantine(&path);
            match detect_opencode(&detect_opts, Some(&install)).await {
                Detection::Installed(cli) => cli,
                _ => {
                    let _ = std::fs::remove_file(&path);
                    run_installer(&mut st, &log);
                    match install_opencode(&install).await {
                        Ok(cli) => cli,
                        Err(e) => {
                            state::append_log(&log, &format!("reinstall failed: {e}"));
                            return Err(engine_fail(
                                &mut st,
                                &home,
                                format!("reinstall failed: {e}"),
                            ));
                        }
                    }
                }
            }
        }
        Detection::Unverified { path, reason } => {
            return Err(engine_fail(
                &mut st,
                &home,
                format!("`{}` is not a usable OpenCode binary ({reason})", path.display()),
            ));
        }
        Detection::NotInstalled => {
            engine_phase(
                slot,
                &tx,
                "Installing the OpenCode engine (first time only, about a minute)…",
            );
            run_installer(&mut st, &log);
            match install_opencode(&install).await {
                Ok(cli) => cli,
                Err(e) => {
                    state::append_log(&log, &format!("install failed: {e}"));
                    return Err(engine_fail(&mut st, &home, format!("install failed: {e}")));
                }
            }
        }
    };
    st.binary = Some(cli.path.clone());
    st.version = Some(cli.version.clone());
    st.last_phase = Some("start".into());
    st.save(&home);
    engine_phase(
        slot,
        &tx,
        &format!("Starting the OpenCode engine ({})…", cli.version),
    );

    let mut opts = EngineOptions::new(workspace);
    // The engine asks before edits and commands; what happens next is the agent's permission
    // mode (Plan/Normal prompt, Auto/Always-approve allow), decided on the UI thread per ask.
    opts.permission = Some(ask_before_edit_and_bash());
    opts.permission_handler = Some(engine_permission_handler(ui_tx));
    // The models answer as Workshop's assistant, not as "opencode".
    opts.config = Some(engine_config(&log));
    let sink_path = log.clone();
    opts.log_sink = Some(Arc::new(move |line: &str| state::append_log(&sink_path, line)));
    // `opencode serve` is up in a couple of seconds on any laptop; a server that has not bound
    // its port after this long is broken, and the user should hear so instead of waiting.
    opts.startup_timeout = ENGINE_START_TIMEOUT;
    match OpenCodeEngine::start(&cli, opts).await {
        Ok(engine) => {
            st.last_phase = Some("ready".into());
            st.save(&home);
            Ok(engine)
        }
        Err(e) => {
            state::append_log(&log, &format!("start failed: {e}"));
            Err(engine_fail(&mut st, &home, format!("did not start: {e}")))
        }
    }
}

/// The process-wide engine: reuse it if it is up, wait if another task is starting it, start it
/// otherwise. The wait mirrors the other task's live phase so a turn typed during the warm-up is
/// never silent. `Err` is the cause (with the log pointer) for the Kilo fallback.
async fn acquire_engine(
    slot: &EngineSlot,
    workspace: &Path,
    tx: &mpsc::UnboundedSender<WorkshopTurnMsg>,
    ui_tx: &mpsc::UnboundedSender<WorkshopTurnMsg>,
) -> Result<Arc<OpenCodeEngine>, String> {
    let mut guard = match slot.engine.try_lock() {
        Ok(guard) => guard,
        Err(_) => {
            let mut phase = slot.phase.subscribe();
            let current = phase.borrow_and_update().clone();
            engine_progress(
                tx,
                if current.is_empty() {
                    "Starting the OpenCode engine…".to_owned()
                } else {
                    current
                },
            );
            loop {
                tokio::select! {
                    guard = slot.engine.lock() => break guard,
                    changed = phase.changed() => {
                        if changed.is_ok() {
                            let text = phase.borrow_and_update().clone();
                            if !text.is_empty() {
                                engine_progress(tx, text);
                            }
                        }
                    }
                }
            }
        }
    };
    if let Some(engine) = guard.as_ref() {
        return Ok(engine.clone());
    }
    let recent = slot
        .recent_failure
        .lock()
        .ok()
        .and_then(|mut f| f.take())
        .filter(|(at, _)| at.elapsed() < RECENT_FAILURE_WINDOW);
    if let Some((_, line)) = recent {
        return Err(line);
    }
    match start_engine(slot, workspace, tx.clone(), ui_tx.clone()).await {
        Ok(engine) => {
            let engine = Arc::new(engine);
            // Every engine start refreshes the engine's free list for `/model` (a loopback GET);
            // the default-model resolution below and the picker read the cached result.
            if let Err(e) = refresh_engine_catalog(&engine).await {
                tracing::warn!("opencode free catalog not read after start: {e}");
            }
            *guard = Some(engine.clone());
            Ok(engine)
        }
        Err(line) => {
            if let Ok(mut f) = slot.recent_failure.lock() {
                *f = Some((std::time::Instant::now(), line.clone()));
            }
            Err(line)
        }
    }
}

/// Warm-up on the user's first typed character (never on launch): install (first run) and start
/// `opencode serve` while the user is still typing, so the first message only waits for the
/// model. Also resolves OpenCode's live default model. Failures are recorded for `workshop
/// doctor` and surface on the first real turn, which reports the cause and falls back.
pub async fn warm_engine(
    slot: EngineSlot,
    workspace: PathBuf,
    tx: mpsc::UnboundedSender<WorkshopTurnMsg>,
) {
    // Progress lines of the warm-up have no turn to attach to; only the outcome is reported.
    // The server's permission asks still need the live loop, so `tx` is what the engine keeps.
    let (quiet_tx, _quiet_rx) = mpsc::unbounded_channel();
    match acquire_engine(&slot, &workspace, &quiet_tx, &tx).await {
        Ok(engine) => {
            if let Some(live) = live_engine_default(&cached_engine_models()) {
                let _ = tx.send(WorkshopTurnMsg::EngineDefaultResolved { model: live });
            }
            let _ = tx.send(WorkshopTurnMsg::EngineWarm { engine });
        }
        Err(e) => tracing::warn!("workshop engine warm-up failed: {e}"),
    }
}

/// OpenCode's current default model out of the last live catalog the engine reported (the model
/// it marks default, else the first free one). `None` while no catalog was ever read.
fn live_engine_default(models: &[EngineModel]) -> Option<EngineModel> {
    models
        .iter()
        .find(|m| m.is_default)
        .or_else(|| models.first())
        .cloned()
        .map(|mut m| {
            m.is_default = true;
            m
        })
}

/// What a silent continuation needs to prompt the same engine conversation again.
struct EngineFollowUp {
    engine: Arc<OpenCodeEngine>,
    session: String,
    model_ref: String,
}

/// How many times one turn is continued after it ended on an action it announced but never took.
const MAX_AUTO_CONTINUES: u32 = 2;

/// The follow-up sent (never shown) when a turn ends right after announcing an action.
const AUTO_CONTINUE_PROMPT: &str = "Continue: your last message said what you would do next but \
ended before doing it. Do it now, then finish every remaining part of my request.";

/// The follow-up sent (never shown, at most once a turn) when a requested file was shown in the
/// chat instead of written.
const AUTO_WRITE_PROMPT: &str = "Continue: you showed the file contents in the chat instead of \
writing them. Write them to the file(s) with your tools, run or test the result if I asked, then \
finish every remaining part of my request.";

/// The engine's tools that create or change files.
const FILE_WRITE_TOOLS: [&str; 5] = ["write", "edit", "multiedit", "patch", "apply_patch"];

/// True when the prompt asks for files to be made ("build … todo.py", "write a script that …").
fn asks_to_write_files(prompt: &str) -> bool {
    let prompt = prompt.to_lowercase();
    let words: Vec<&str> = prompt
        .split(|c: char| c.is_whitespace() || ",;:()[]`'\"!?".contains(c))
        .map(|w| w.trim_end_matches('.'))
        .filter(|w| !w.is_empty())
        .collect();
    let makes = words.iter().any(|w| {
        ["create", "write", "build", "make", "generate", "save", "scaffold"]
            .iter()
            .any(|v| w.starts_with(v))
    });
    let file_name = |w: &str| {
        w.rsplit_once('.').is_some_and(|(stem, ext)| {
            stem.len() >= 2 && (1..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric())
        })
    };
    let names_a_file = words.iter().any(|w| {
        file_name(w)
            || ["file", "script", "app", "program", "module", "project"]
                .contains(&w.trim_end_matches('s'))
    });
    makes && names_a_file
}

/// True when the turn's closing text ends with a fenced code block.
fn ends_with_code_block(tail: &str) -> bool {
    tail.trim_end()
        .strip_suffix("```")
        .is_some_and(|body| body.contains("```"))
}

/// True when the turn's closing text (what the model wrote after its last tool call) announces a
/// step it never took: it ends on ":" ("…the official installer for Ubuntu:"), or its last
/// sentence is an "I'll …" / "Let me …" that no tool call followed.
fn announces_unfinished_action(tail: &str) -> bool {
    let mut text = tail.trim_end();
    // A command shown in a fence instead of run: judge the words before the fence. A colon before
    // a fence usually introduces a finished result ("…Desktop/Panthera:" and the folder tree, "run
    // this yourself:" and the command), so there only an "I'll …" sentence counts.
    let mut fenced = false;
    if let Some(body) = text.strip_suffix("```")
        && let Some(before) = body.rfind("```").and_then(|open| body.get(..open))
    {
        text = before.trim_end();
        fenced = true;
    }
    if text.ends_with(':') && !fenced {
        return true;
    }
    if text.ends_with('?') {
        return false;
    }
    let sentence = text
        .rsplit(['\n', '.', '!', '?'])
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or_default()
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    let sentence = ["now, ", "now ", "next, ", "next ", "then "]
        .iter()
        .find_map(|p| sentence.strip_prefix(p))
        .unwrap_or(&sentence);
    let announces = ["i'll ", "i will ", "let me ", "i'm going to ", "i am going to "]
        .iter()
        .any(|p| sentence.starts_with(p));
    let hands_back = ["let me know", "if you", "once you", "when you", "wait"]
        .iter()
        .any(|p| sentence.contains(p));
    announces && !hands_back
}

async fn build_stream(
    spec: &WorkshopTurnSpec,
    tx: &mpsc::UnboundedSender<WorkshopTurnMsg>,
    permission: PermissionPolicy,
) -> Result<(TurnStream, Option<EngineFollowUp>), TurnStartError> {
    match &spec.kind {
        WorkshopTurnKind::Engine {
            slot,
            session,
            model,
        } => {
            let mut model = model.clone();
            let engine = acquire_engine(slot, &spec.cwd, tx, tx)
                .await
                .map_err(TurnStartError::EngineUnavailable)?;
            // The default engine model is whichever model OpenCode's live catalog (read at
            // every engine start) marks as default; the pinned seed only stands in while that
            // catalog is unreachable.
            if model.is_default
                && let Some(live) = live_engine_default(&cached_engine_models())
            {
                if live.model_ref != model.model_ref {
                    let _ = tx.send(WorkshopTurnMsg::EngineDefaultResolved {
                        model: live.clone(),
                    });
                }
                model = live;
            }
            engine_progress(tx, waiting_for(&model.name));
            let session = match session {
                Some(s) if engine.session_exists(s).await.unwrap_or(false) => s.clone(),
                _ => engine.create_session(Some("Workshop")).await.map_err(|e| {
                    TurnStartError::Other(engine_failure_line(&format!(
                        "could not open a session: {e}"
                    )))
                })?,
            };
            let _ = tx.send(WorkshopTurnMsg::EngineReady {
                engine: engine.clone(),
                session: session.clone(),
            });
            let mut req = TurnRequest::new(spec.text.clone());
            req.model = Some(model.model_ref.clone());
            req.permission = permission;
            let turn = engine.prompt(&session, req).await.map_err(|e| {
                TurnStartError::Other(engine_failure_line(&format!(
                    "the prompt was refused: {e}"
                )))
            })?;
            let follow_up = EngineFollowUp {
                engine,
                session,
                model_ref: model.model_ref,
            };
            Ok((TurnStream::Engine(turn), Some(follow_up)))
        }
        WorkshopTurnKind::Adapter {
            adapter_id,
            resume,
            model,
        } => {
            let adapter = workshop_adapters::vendors::by_id(*adapter_id);
            let cli = match detect(&*adapter, &DetectOptions::default()).await {
                Detection::Installed(cli) => cli,
                Detection::Unverified { reason, .. } => {
                    return Err(TurnStartError::Other(format!(
                        "{} could not be verified: {reason}",
                        adapter.id()
                    )));
                }
                Detection::NotInstalled => {
                    return Err(TurnStartError::Other(format!(
                        "{} is not installed",
                        adapter.id()
                    )));
                }
            };
            let mut req = RunRequest::new(spec.text.clone(), &spec.cwd);
            req.model = model.clone();
            req.resume = resume.clone();
            req.permission = permission;
            let handle = spawn(&*adapter, &cli, req, &SupervisorOptions::default())
                .await
                .map_err(|e| TurnStartError::Other(e.to_string()))?;
            Ok((TurnStream::Adapter(handle), None))
        }
    }
}

/// Drive one turn on the chosen backend, forwarding stream events to the UI over `tx` and honoring
/// a `cancel` signal (Esc / Ctrl-C → abort/kill). Runs as a detached task; the UI thread owns the
/// receiver and renders. Never touches the ACP path.
pub async fn run_workshop_turn(
    spec: WorkshopTurnSpec,
    tx: mpsc::UnboundedSender<WorkshopTurnMsg>,
    mut cancel_rx: watch::Receiver<bool>,
) {
    // The agent's mode is real on the engine: Plan runs OpenCode's read-only `plan` agent (it
    // cannot edit files or run destructive commands); every other mode runs `build`, where the
    // server asks before each edit/command and the UI answers per its mode (Normal prompts,
    // Auto/Always-approve allow). The vendor CLIs keep their own read-only default until
    // always-approve is on, as before.
    let permission = match (&spec.kind, spec.mode) {
        (WorkshopTurnKind::Engine { .. }, WorkshopPermissionMode::Plan) => {
            PermissionPolicy::ReadOnly
        }
        (WorkshopTurnKind::Engine { .. }, _) => PermissionPolicy::WorkspaceWrite,
        (WorkshopTurnKind::Adapter { .. }, WorkshopPermissionMode::AlwaysApprove) => {
            PermissionPolicy::WorkspaceWrite
        }
        (WorkshopTurnKind::Adapter { .. }, _) => PermissionPolicy::ReadOnly,
    };
    let finish = |tx: &mpsc::UnboundedSender<WorkshopTurnMsg>, cancelled: bool| {
        let _ = tx.send(WorkshopTurnMsg::Done {
            session_id: None,
            cancelled,
        });
    };
    // Ctrl-C must work while the engine is still installing or starting, not only once events
    // flow: race the bring-up against the cancel signal.
    let built = tokio::select! {
        built = build_stream(&spec, &tx, permission) => built,
        _ = wait_cancelled(&mut cancel_rx) => {
            finish(&tx, true);
            return;
        }
    };
    let (mut stream, follow_up) = match built {
        Ok(s) => s,
        Err(error) => {
            let _ = tx.send(match error {
                TurnStartError::EngineUnavailable(reason) => WorkshopTurnMsg::EngineUnavailable {
                    reason,
                    text: spec.text.clone(),
                },
                TurnStartError::Other(message) => WorkshopTurnMsg::Error(message),
            });
            finish(&tx, false);
            return;
        }
    };

    let model_name = match &spec.kind {
        WorkshopTurnKind::Engine { model, .. } => model.name.clone(),
        WorkshopTurnKind::Adapter { adapter_id, .. } => adapter_id.to_string(),
    };
    let mut cancelled = false;
    let mut aborted_by_us = false;
    let mut first_event_at: Option<tokio::time::Instant> =
        Some(tokio::time::Instant::now() + FIRST_EVENT_TIMEOUT);
    // Ten seconds without a first byte is long enough to tell the user the wait is the
    // connection's; the hard ceiling above still ends the turn with the cause.
    let mut still_connecting_at: Option<tokio::time::Instant> =
        Some(tokio::time::Instant::now() + CONNECT_TIMEOUT);
    // The backend's detail for a tool call (title, exit code, diff) arrives right before its
    // result; hold it so the UI gets one message per finished call.
    let mut tool_details: std::collections::HashMap<String, (Option<String>, serde_json::Value)> =
        std::collections::HashMap::new();
    // What the model wrote since its last tool call, and whether the turn failed: a turn that
    // ends on an announced action, or that pasted a requested file instead of writing it (once),
    // is continued (engine, not Plan), at most MAX_AUTO_CONTINUES times in all.
    let mut tail = String::new();
    let mut errored = false;
    let mut continues = 0;
    let asks_for_files = asks_to_write_files(&spec.text);
    let mut wrote_a_file = false;
    let mut continued_to_write = false;
    loop {
        let silence = async {
            match first_event_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending::<()>().await,
            }
        };
        let still_connecting = async {
            match still_connecting_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            _ = still_connecting => {
                still_connecting_at = None;
                engine_progress(&tx, format!("Still connecting to {model_name}\u{2026} nothing back yet"));
            }
            _ = silence => {
                // The engine accepted the prompt but nothing came back: stop waiting, say why.
                stream.cancel();
                let _ = tx.send(WorkshopTurnMsg::Error(engine_failure_line(&format!(
                    "no answer from {model_name} after {} s",
                    FIRST_EVENT_TIMEOUT.as_secs()
                ))));
                first_event_at = None;
                aborted_by_us = true;
            }
            ev = stream.next_event() => match ev {
                Some(AdapterEvent::TextDelta { text }) => {
                    first_event_at = None;
                    still_connecting_at = None;
                    tail.push_str(&text);
                    let _ = tx.send(WorkshopTurnMsg::Delta(text));
                }
                Some(AdapterEvent::ToolCall { id, name, input }) => {
                    first_event_at = None;
                    still_connecting_at = None;
                    tail.clear();
                    wrote_a_file |= FILE_WRITE_TOOLS.contains(&name.as_str());
                    let _ = tx.send(WorkshopTurnMsg::Tool { id, name, input });
                }
                Some(AdapterEvent::ToolDetail { id, title, metadata }) => {
                    tool_details.insert(id, (title, metadata));
                }
                Some(AdapterEvent::ToolResult { id, output, is_error }) => {
                    let (title, metadata) = tool_details
                        .remove(&id)
                        .unwrap_or((None, serde_json::Value::Null));
                    let _ = tx.send(WorkshopTurnMsg::ToolResult {
                        id,
                        ok: !is_error,
                        output,
                        title,
                        metadata,
                    });
                    // The model is at work again (thinking, hidden by default, or writing): the
                    // waiting line says so until its next output.
                    engine_progress(&tx, waiting_for(&model_name));
                }
                Some(AdapterEvent::Error { message }) => {
                    first_event_at = None;
                    still_connecting_at = None;
                    errored = true;
                    // An abort we asked for (Ctrl-C, or the silence timeout above) is already
                    // reported; the backend's own "run cancelled" would only repeat it.
                    if !(aborted_by_us && message == "run cancelled") {
                        let _ = tx.send(WorkshopTurnMsg::Error(message));
                    }
                }
                Some(AdapterEvent::Thinking { text }) => {
                    first_event_at = None;
                    still_connecting_at = None;
                    let _ = tx.send(WorkshopTurnMsg::Thinking(text));
                }
                Some(AdapterEvent::Usage(usage)) => {
                    let _ = tx.send(WorkshopTurnMsg::Usage(usage));
                }
                Some(AdapterEvent::Done { .. }) => {}
                None => {
                    let pasted = asks_for_files
                        && !wrote_a_file
                        && !continued_to_write
                        && ends_with_code_block(&tail);
                    if let Some(f) = &follow_up
                        && permission == PermissionPolicy::WorkspaceWrite
                        && !aborted_by_us
                        && !errored
                        && continues < MAX_AUTO_CONTINUES
                        && (pasted || announces_unfinished_action(&tail))
                    {
                        continues += 1;
                        continued_to_write |= pasted;
                        let (why, prompt) = if pasted {
                            ("the reply showed a requested file in the chat and wrote no file", AUTO_WRITE_PROMPT)
                        } else {
                            ("the turn ended on an announced action with no tool call after it", AUTO_CONTINUE_PROMPT)
                        };
                        let chars: Vec<char> = tail.trim().chars().collect();
                        let ending: String =
                            chars.iter().skip(chars.len().saturating_sub(120)).collect();
                        state::append_log(
                            &engine_log_path(),
                            &format!(
                                "auto-continue {continues}/{MAX_AUTO_CONTINUES} on {}: {why}: {ending:?}",
                                f.session
                            ),
                        );
                        let mut req = TurnRequest::new(prompt);
                        req.model = Some(f.model_ref.clone());
                        req.permission = permission;
                        match f.engine.prompt(&f.session, req).await {
                            Ok(turn) => {
                                stream = TurnStream::Engine(turn);
                                tail.clear();
                                engine_progress(&tx, waiting_for(&model_name));
                                continue;
                            }
                            Err(e) => state::append_log(
                                &engine_log_path(),
                                &format!("auto-continue refused: {e}"),
                            ),
                        }
                    }
                    break;
                }
            },
            _ = wait_cancelled(&mut cancel_rx), if !aborted_by_us => {
                cancelled = true;
                aborted_by_us = true;
                stream.cancel();
            }
        }
    }

    let session_id = stream.session_id();
    let _ = tx.send(WorkshopTurnMsg::Done {
        session_id,
        cancelled,
    });
}

/// Resolve once the cancel flag is raised (or its sender is gone, which counts as a cancel).
async fn wait_cancelled(cancel_rx: &mut watch::Receiver<bool>) {
    loop {
        if *cancel_rx.borrow() {
            return;
        }
        if cancel_rx.changed().await.is_err() {
            return;
        }
    }
}

/// Map a subscription rail to its `workshop-adapters` CLI adapter id.
pub fn rail_adapter_id(rail: workshop_detect::Rail) -> AdapterId {
    match rail.vendor() {
        workshop_detect::Vendor::Claude => AdapterId::Claude,
        workshop_detect::Vendor::Codex => AdapterId::Codex,
        workshop_detect::Vendor::Cursor => AdapterId::Cursor,
        workshop_detect::Vendor::OpenCode => AdapterId::OpenCode,
    }
}

/// The official CLI login command for a rail, to run attached to the user's terminal.
pub fn rail_login_argv(rail: workshop_detect::Rail) -> Vec<String> {
    let vendor = rail.vendor();
    let cfg = workshop_detect::DetectConfig::default();
    let probe = workshop_detect::probe_vendor(vendor, &cfg);
    let bin = probe
        .binary
        .as_ref()
        .map(|b| b.path.to_string_lossy().to_string())
        .unwrap_or_else(|| vendor.binary_names().first().copied().unwrap_or("").to_owned());
    let mut argv = vec![bin];
    argv.extend(
        workshop_detect::login_argv(vendor)
            .iter()
            .map(|s| s.to_string()),
    );
    argv
}

#[cfg(test)]
mod tests {
    use super::{announces_unfinished_action, asks_to_write_files, ends_with_code_block};

    #[test]
    fn file_requests_are_recognised() {
        for prompt in [
            "build a command line todo app in python: todo.py with add, list and done commands, saving the todos in todos.json. test it when you're done",
            "create add.py with a function add(a, b)",
            "write a script that renames my photos",
            "make a small flask app",
        ] {
            assert!(asks_to_write_files(prompt), "{prompt:?}");
        }
        for prompt in [
            "hello, who are you?",
            "show me an example of a python loop",
            "create a folder on my desktop called iBooks and download two open-source classic books inside of it, and install Ghostty",
            "what does e.g. mean",
        ] {
            assert!(!asks_to_write_files(prompt), "{prompt:?}");
        }
    }

    #[test]
    fn a_closing_fence_is_a_pasted_block() {
        assert!(ends_with_code_block(
            "I'll build it.\n\n**todo.py**\n```python\nprint('x')\n```\n"
        ));
        assert!(!ends_with_code_block("Wrote todo.py and ran it: 3 passed."));
        assert!(!ends_with_code_block("```"));
    }

    #[test]
    fn announced_actions_are_unfinished() {
        for tail in [
            "Ubuntu 24.04 doesn't have Ghostty in its repos yet, so I'll use the official community `.deb` installer for Ubuntu:\n",
            "Books downloaded. I'll run:",
            "Now I'll install it with apt.",
            "Let me run the tests.",
            "Next, I'll create the file",
            "I'll run this:\n```bash\nsudo apt install ghostty\n```",
        ] {
            assert!(announces_unfinished_action(tail), "{tail:?}");
        }
    }

    #[test]
    fn finished_answers_and_questions_are_not() {
        for tail in [
            "",
            "Done.",
            "Installed Ghostty 1.3.1 and downloaded both books.",
            "Let me know if you want anything else.",
            "I'll wait for your go-ahead.",
            "Should I install it with snap or the .deb?",
            "I'll install it if you confirm.",
            "Here are the files:\n- a.epub\n- b.epub",
            "Sorted all 15 photos into ~/Desktop/Panthera:\n```\nPanthera/\n  lion/\n```",
            "Please run this yourself in a terminal:\n```\nsudo apt install htop\n```",
        ] {
            assert!(!announces_unfinished_action(tail), "{tail:?}");
        }
    }
}
