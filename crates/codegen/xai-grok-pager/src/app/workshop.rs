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
    PermissionHandler, PermissionReply, PermissionRequest, PromptFile, QuestionAnswers,
    QuestionHandler, QuestionRequest, TurnHandle, TurnRequest,
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
    /// Composer label in upstream's format, `<model> (<effort>)`: `Big Pickle`,
    /// `Ling 3.0 Flash Fin Free (high)`, `Claude Sonnet` — the effort only when the model has
    /// levels and one is picked, never a provider or runtime name. The mode (`· plan`,
    /// `· always-approve`) is appended by the composer's own mode flags, as upstream does.
    pub fn composer_label(&self) -> Option<String> {
        match self {
            Self::Shell => None,
            Self::Engine { model } => Some(model.display()),
            Self::Adapter { model, .. } => Some(model.display().to_owned()),
        }
    }
    /// The model name for the failure line (`Couldn't reach {model}`); `Shell` has none.
    pub fn model_name(&self) -> Option<String> {
        match self {
            Self::Shell => None,
            Self::Engine { model } => Some(model.name.clone()),
            Self::Adapter { model, .. } => Some(model.display().to_owned()),
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

/// Stamp every agent with the active connection's composer label (the answering model's while
/// the silent fallback carries the session) and context meter numbers (after a connection
/// change, a resolved live default, or a new usage report).
pub fn sync_agent_views(app: &mut crate::app::app_view::AppView) {
    let label = app.workshop_label();
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

/// Test hook: point the silent fallback at a loopback OpenAI-compatible server instead of the
/// Kilo Gateway (the PTY gate proves the fallback without network).
pub const KILO_BASE_URL_ENV: &str = "WORKSHOP_KILO_BASE_URL";

/// The keyless Direct API model Workshop silently falls back to when the OpenCode model cannot
/// start or answer: the first *concrete* model of the Kilo community pool's default chain (not
/// the auto-router, so the composer can name the model that actually answered). Never listed on
/// `/model`; the user only ever sees the model name.
pub fn kilo_fallback_model() -> Option<workshop_providers::CatalogModel> {
    let catalog = Catalog::builtin();
    let mut model = workshop_providers::KILO_DEFAULT_CHAIN
        .iter()
        .filter_map(|id| catalog.get(&format!("kilo:{id}")))
        .find(|m| workshop_auth::is_chat_model_name(&m.model_id) && !m.model_id.contains("auto"))
        .cloned()?;
    if let Some(base) = std::env::var(KILO_BASE_URL_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
    {
        model.base_url = base.trim().trim_end_matches('/').to_owned();
    }
    Some(model)
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
            variants: m.variants.clone(),
            effort: None,
            image_input: m.image_input,
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
    let manifests = listed_refresh_providers(&default_broker());
    let hosted = async {
        if manifests.is_empty() {
            live_catalogs::load_cached(&cache_dir)
        } else {
            live_catalogs::refresh_with(&cache_dir, &manifests, opts).await
        }
    };
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

/// The hosted providers whose model list is worth fetching: never a hidden one, and an API-key
/// provider only once its key is configured (an unconnected provider has no row to show).
fn listed_refresh_providers(
    broker: &CredentialBroker,
) -> Vec<workshop_providers::ProviderManifest> {
    live_catalogs::REFRESH_PROVIDERS
        .iter()
        .filter(|id| !workshop_auth::is_hidden_provider(id))
        .filter(|id| {
            workshop_providers::manifest(id)
                .is_some_and(|m| !m.requires_credential() || broker.is_connected(id))
        })
        .filter_map(|id| workshop_providers::manifest(id))
        .collect()
}

async fn build_picker_snapshot(
    hosted: Option<HostedCatalogs>,
    engine_error: Option<String>,
    rail_models: workshop_detect::Refresh,
) -> PickerSnapshot {
    let local = workshop_providers::probe_all_local_servers(Duration::from_millis(600)).await;
    let rails: Vec<workshop_detect::RailState> = tokio::task::spawn_blocking(move || {
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
    let rows = models_rows(&catalog, |id| broker.is_connected(id), &engine, &rails);
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
    activate_catalog_model_with(model, |_| {})
}

/// `$WORKSHOP_HOME/tmp`: the engine's `TMPDIR`, so its scratch files (OpenCode's own temp dir is
/// `<tmpdir>/opencode`) stay under the Workshop home.
pub fn engine_scratch_dir() -> PathBuf {
    workshop_home().join("tmp")
}

/// Rewrite the engine's scratch paths for the screen: `<$WORKSHOP_HOME/tmp>/opencode` becomes
/// `~/.workshop/tmp`, so no "opencode" path reaches a tool row. Any other text is returned as is.
pub fn scrub_scratch_paths(text: &str) -> String {
    let dir = engine_scratch_dir();
    let actual = format!("{}/opencode", dir.display());
    if !text.contains(&actual) {
        return text.to_owned();
    }
    let shown = crate::app::workshop_permissions::shorten_home(&dir.display().to_string());
    text.replace(&actual, &shown)
}

/// [`scrub_scratch_paths`] over every string in a JSON value (a tool's input or metadata).
pub fn scrub_scratch_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(s) => serde_json::Value::String(scrub_scratch_paths(s)),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(scrub_scratch_json).collect())
        }
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), scrub_scratch_json(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Request attempts upstream's sampler makes for the silent pool fallback before a step fails
/// (`[model.<key>] max_retries`; 15 by default, with a backoff that grows to 30 s — a quarter of
/// an hour of `Retrying…` offline). Two means one immediate retry on a rebuilt client, about a
/// second per step. Upstream's turn loop still resubmits a failed step three times (2, 10 and
/// 30 s apart, `MAX_TRANSIENT_TURN_RETRIES`), so offline the fallback gives up after roughly
/// 45 s, then the user gets the plain line and the composer is theirs again.
pub const FALLBACK_MAX_RETRIES: u32 = 2;

/// [`activate_catalog_model`] for the silent pool fallback: the same entry with its retries
/// capped at [`FALLBACK_MAX_RETRIES`].
pub fn activate_fallback_model(
    model: &workshop_providers::CatalogModel,
) -> Result<ActivationPlan, String> {
    activate_catalog_model_with(model, |spec| spec.max_retries = Some(FALLBACK_MAX_RETRIES))
}

fn activate_catalog_model_with(
    model: &workshop_providers::CatalogModel,
    adjust: impl FnOnce(&mut workshop_providers::ModelEntrySpec),
) -> Result<ActivationPlan, String> {
    let broker = default_broker();
    let mut spec = resolve_model_entry(model, &broker).map_err(|e| e.to_string())?;
    adjust(&mut spec);
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
    let client =
        workshop_providers::oauth::openrouter::exchange_client().map_err(|e| e.to_string())?;
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
    /// The engine ended this turn at its idle ceiling (set before its final `Error` arrives).
    pub fn stalled(&self) -> bool {
        match self {
            Self::Engine(t) => t.stalled(),
            Self::Adapter(_) => false,
        }
    }
    /// The model is composing a tool call (the engine said so and is silent until it is done).
    pub fn composing(&self) -> bool {
        match self {
            Self::Engine(t) => t.composing(),
            Self::Adapter(_) => false,
        }
    }
}

/// What the UI thread learns as a turn streams. Mapped to scrollback `RenderBlock`s by the event
/// loop's Workshop `select!` arm (kept UI-agnostic so `workshop-adapters` never depends on the pager).
#[derive(Debug)]
pub enum WorkshopTurnMsg {
    /// The bring-up's phase for the pager's turn-status row: [`THINKING`] for the plain wait for
    /// the model, or the first-time download progress ([`install_progress_line`]). Never a word
    /// about what runs underneath.
    Progress(String),
    /// The OpenCode engine started (lazily, on the first turn); cache it and the session so later
    /// turns reuse the same `opencode serve` and conversation. Engine turns only.
    EngineReady {
        engine: Arc<OpenCodeEngine>,
        session: String,
    },
    /// The rest of this turn is answered by `model` (it can see the images the turn opened): the
    /// composer names it until the turn ends; the next turn goes back to the picked model.
    Answering { model: EngineModel },
    /// The launch warm-up finished: the engine is up before the first message.
    EngineWarm { engine: Arc<OpenCodeEngine> },
    /// The engine's live catalog names a different default than the pinned seed the first run
    /// activated: the connection follows OpenCode's default (composer label, persisted file).
    EngineDefaultResolved {
        model: EngineModel,
    },
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
    /// The agent asks the user something (OpenCode's `question` tool): the UI thread opens Grok
    /// Build's question view and sends the answers (or `None`, declined) on `reply`.
    QuestionAsk {
        request: QuestionRequest,
        reply: oneshot::Sender<QuestionAnswers>,
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
    /// The plain failure line (`Couldn't reach <model> — …`); the cause is already in the log.
    Error(String),
    /// OpenCode's model could not start or answer this turn (offline, installer failed, no
    /// verified `opencode`, the prompt refused, nothing back before the first-event ceiling).
    /// `text` is the prompt that never ran; the UI silently resends it through the keyless pool.
    /// Followed by `Done`.
    EngineUnavailable {
        reason: String,
        text: String,
    },
    /// `sudo` in one of the engine's commands needs the user's password: the askpass helper is
    /// waiting on `reply` (`Some(bytes)` = the password, `None` = skipped). The UI shows one
    /// masked prompt; the bytes go to the helper only. Any turn, any time.
    PasswordAsk {
        prompt: String,
        reply: tokio::sync::oneshot::Sender<Option<Vec<u8>>>,
    },
    /// The model started answering and then went silent for [`STALL_TIMEOUT`] (no output, no
    /// tool running, no prompt waiting on the user); the engine aborted the turn. `text` is the
    /// prompt to resend: an engine turn goes through the pool fallback, else the user gets
    /// [`stall_line`] with Enter to retry. Followed by `Done`.
    Stalled {
        model: String,
        text: String,
        engine: bool,
    },
    /// The turn ended; `session_id` is persisted per workspace for resume.
    Done {
        session_id: Option<String>,
        cancelled: bool,
    },
}

/// Why a turn could not start.
enum TurnStartError {
    /// `opencode` could not be detected, installed or started, or would not take the prompt: the
    /// silent fallback answers instead.
    EngineUnavailable(String),
    /// A vendor CLI turn could not start: the plain failure line.
    Other(String),
}

/// The one `opencode serve` this process owns, shared by the launch warm-up and every
/// turn so two callers never start two servers: whoever holds the lock starts it, the other
/// reuses it. `phase` carries the live bring-up status so a turn that waits on the lock can show
/// what the other task is doing ("Installing…") instead of a generic line.
pub struct EngineSlotInner {
    engine: tokio::sync::Mutex<Option<Arc<OpenCodeEngine>>>,
    phase: watch::Sender<String>,
    /// The warm-up's failure, so a turn typed right after it reports that cause at once instead
    /// of silently repeating a 30 s bring-up that just failed. Cleared by the next attempt.
    recent_failure: std::sync::Mutex<Option<(std::time::Instant, String)>>,
    /// The `sudo` askpass listener (one per process, started with the first engine); `None`
    /// until then, or when the home could not hold it.
    askpass: std::sync::Mutex<Option<Arc<crate::app::workshop_askpass::AskpassServer>>>,
}

pub type EngineSlot = Arc<EngineSlotInner>;

pub fn new_engine_slot() -> EngineSlot {
    Arc::new(EngineSlotInner {
        engine: tokio::sync::Mutex::new(None),
        phase: watch::channel(String::new()).0,
        recent_failure: std::sync::Mutex::new(None),
        askpass: std::sync::Mutex::new(None),
    })
}

/// The process's askpass listener, started on first use with the live loop's channel.
fn askpass_server(
    slot: &EngineSlot,
    ui_tx: &mpsc::UnboundedSender<WorkshopTurnMsg>,
) -> Option<Arc<crate::app::workshop_askpass::AskpassServer>> {
    let mut guard = slot.askpass.lock().ok()?;
    if guard.is_none() {
        *guard = crate::app::workshop_askpass::start(ui_tx.clone()).map(Arc::new);
    }
    guard.clone()
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
/// Mid-turn ceiling: once the model has started answering, this long without any event from the
/// engine about the turn (output, a tool starting or finishing, a step boundary, permission
/// traffic) means it stopped (an upstream 504 the engine retries silently, a dropped stream). The
/// engine aborts the turn and Workshop recovers. `WORKSHOP_STALL_TIMEOUT_SECS` overrides it (gates
/// run it in seconds).
pub const STALL_TIMEOUT: Duration = Duration::from_secs(90);
/// The ceiling while the model is composing a tool call: the engine publishes the call once, when
/// it starts, and nothing more until its input is complete — a whole page in one `write` at high
/// effort is minutes of that silence, and cutting it hands the turn to another model mid-file.
/// Only a dead stream lasts this long. `WORKSHOP_COMPOSE_TIMEOUT_SECS` overrides it.
pub const COMPOSE_TIMEOUT: Duration = Duration::from_secs(600);

/// [`STALL_TIMEOUT`], or the `WORKSHOP_STALL_TIMEOUT_SECS` override.
pub fn stall_timeout() -> Duration {
    seconds_from_env("WORKSHOP_STALL_TIMEOUT_SECS").unwrap_or(STALL_TIMEOUT)
}

/// [`COMPOSE_TIMEOUT`], or the `WORKSHOP_COMPOSE_TIMEOUT_SECS` override.
pub fn compose_timeout() -> Duration {
    seconds_from_env("WORKSHOP_COMPOSE_TIMEOUT_SECS").unwrap_or(COMPOSE_TIMEOUT)
}

fn seconds_from_env(name: &str) -> Option<Duration> {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .map(Duration::from_secs)
}

/// The one line shown when a model stopped mid-answer and no fallback could take over.
pub fn stall_line(model: &str) -> String {
    format!("{model} stopped responding \u{2014} Enter to retry \u{b7} /model to switch")
}
/// Hard ceiling on `opencode serve` binding its port and passing its health check.
pub const ENGINE_START_TIMEOUT: Duration = Duration::from_secs(30);
/// The bring-up's "nothing to add" progress: the turn-status row shows the pager's own wait for
/// the model (`Waiting for response…`) — whatever happens behind it (installing, starting,
/// connecting). No plumbing words, ever.
pub const THINKING: &str = "Thinking\u{2026}";
/// The one described step of a first run, for the turn-status row (`First-time setup, 12 MB
/// downloaded…` with its own timer): the row's phase identity while the byte count refines it.
pub const FIRST_TIME_SETUP: &str = "first-time setup";

/// The first-run download, once the vendor script has started writing: the honest byte count so a
/// minute-long first message never looks hung (the status row adds the spinner and the seconds).
pub fn install_progress_line(bytes: u64) -> String {
    format!("First-time setup, {} downloaded", format_bytes(bytes))
}

/// The one failure line a user sees when a model could not be reached or did not answer (after
/// the silent fallback, if any, failed too): plain, no runtime names, and the two ways out.
pub fn failure_line(model: &str) -> String {
    format!("Couldn't reach {model} \u{2014} Enter to retry \u{b7} /model to switch")
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
    /// Images pasted or attached with the prompt (engine turns send them as file parts).
    pub images: Vec<PromptFile>,
    /// The agent's permission mode when the prompt was sent. Engine: Plan → the read-only
    /// `plan` agent, everything else → `build` with the engine asking before edits/commands.
    /// Vendor CLIs: AlwaysApprove → `WorkspaceWrite`, else their read-only default.
    pub mode: WorkshopPermissionMode,
}

/// One-line summary of a tool call for its transcript row: the path, command, pattern or URL.
pub fn summarize_tool_input(input: &serde_json::Value) -> String {
    // `description` last: the `task` tool (a subagent) has no path or command, only what it was
    // asked to do — that is the row (`Run Extract Wikimedia image candidates`), not `Run task`.
    for key in [
        "filePath",
        "path",
        "command",
        "pattern",
        "query",
        "url",
        "description",
    ] {
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
    let with_messages =
        local_summaries_for_cwd_sync(&cwd.to_string_lossy(), RecentSessionSelection::Interactive)
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

/// Start the background voice setup (the `voice-engine` helper, then this machine's speech model,
/// from the release mirror only) unless it is off (`voice.auto_download`, `WORKSHOP_VOICE_AUTO=0`),
/// voice is disabled, already running this process, or already in place. `delay` holds it back
/// on a returning launch so the first half minute is the user's.
pub fn maybe_start_voice_prefetch(
    app: &mut crate::app::app_view::AppView,
    delay: Duration,
) -> Vec<crate::app::actions::Effect> {
    if !workshop_voice::prefetch::auto_enabled(app.voice_config.auto_download) {
        return vec![];
    }
    start_voice_prefetch(app, delay)
}

/// The setup itself, regardless of the automatic-download flag (a `/voice` press asked for it).
fn start_voice_prefetch(
    app: &mut crate::app::app_view::AppView,
    delay: Duration,
) -> Vec<crate::app::actions::Effect> {
    if app.workshop_voice_prefetch.is_some()
        || !app.voice_mode_enabled
        || !xai_grok_voice::AUDIO_SUPPORTED
    {
        return vec![];
    }
    let voice_dir = workshop_voice::store::default_dir();
    let engine_override = app.voice_config.engine_path.as_deref().map(Path::new);
    if workshop_voice::prefetch::is_ready(
        &voice_dir,
        app.voice_config.model.as_deref(),
        engine_override,
    ) {
        return vec![];
    }
    let shared = workshop_voice::prefetch::shared();
    app.workshop_voice_prefetch = Some(shared.clone());
    vec![crate::app::actions::Effect::WorkshopVoicePrefetch {
        shared,
        delay,
        home: workshop_home(),
        voice_dir,
        tier: app.voice_config.model.clone(),
    }]
}

/// `/voice` while voice is not ready: the one line to show instead of starting the microphone
/// (`Voice is getting ready — 62%`), plus the setup to start if it is not running (a failed
/// attempt is tried again). `None` when the helper and the model are in place.
pub fn voice_getting_ready(
    app: &mut crate::app::app_view::AppView,
) -> Option<(String, Vec<crate::app::actions::Effect>)> {
    use workshop_voice::prefetch::Phase;
    let voice_dir = workshop_voice::store::default_dir();
    let engine_override = app.voice_config.engine_path.as_deref().map(Path::new);
    if workshop_voice::prefetch::is_ready(
        &voice_dir,
        app.voice_config.model.as_deref(),
        engine_override,
    ) {
        return None;
    }
    let status = app
        .workshop_voice_prefetch
        .as_ref()
        .and_then(|s| s.lock().ok().map(|s| s.clone()));
    match status {
        Some(status) if !matches!(status.phase, Phase::Failed(_) | Phase::Ready) => {
            Some((status.line(), vec![]))
        }
        _ => {
            // Not started (or turned off for the background), done but something is missing
            // again, or failed: the press asked for voice, so one try now.
            app.workshop_voice_prefetch = None;
            let effects = start_voice_prefetch(app, Duration::ZERO);
            let line = if effects.is_empty() {
                "Voice isn't set up on this machine \u{2014} /doctor shows why".to_owned()
            } else {
                workshop_voice::prefetch::Status::default().line()
            };
            Some((line, effects))
        }
    }
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

/// A pasted image as the engine takes it: its bytes as a `data:` URL, or the file it was saved to.
pub fn prompt_file(image: &crate::prompt_images::PastedImage) -> Option<PromptFile> {
    use base64::Engine as _;
    let ext = image.mime_type.rsplit('/').next().unwrap_or("png");
    let url = match &image.encoded_bytes {
        Some(bytes) => format!(
            "data:{};base64,{}",
            image.mime_type,
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ),
        None => [&image.session_image_path, &image.staged_temp_path, &image.source_path]
            .into_iter()
            .flatten()
            .find(|p| p.is_file())
            .and_then(|p| url::Url::from_file_path(p).ok())?
            .to_string(),
    };
    Some(PromptFile {
        mime: image.mime_type.clone(),
        url,
        filename: format!("image-{}.{ext}", image.display_number),
    })
}

/// The engine's questions as Grok Build's question view shows them (the `header` is OpenCode's
/// short tab label; the view titles each question by its text).
pub fn engine_questions(
    request: &QuestionRequest,
) -> Vec<xai_grok_tools::implementations::grok_build::ask_user_question::Question> {
    use xai_grok_tools::implementations::grok_build::ask_user_question::{Question, QuestionOption};
    request
        .questions
        .iter()
        .map(|q| Question {
            question: q.question.clone(),
            options: q
                .options
                .iter()
                .map(|o| QuestionOption {
                    label: o.label.clone(),
                    description: o.description.clone(),
                    preview: None,
                    id: None,
                })
                .collect(),
            multi_select: Some(q.multiple),
            id: None,
        })
        .collect()
}

/// The engine's answers from the question view: per question, in order, the chosen labels, with
/// "Other" replaced by what the user typed. Anything but an accepted answer declines.
pub fn engine_question_answers(
    request: &QuestionRequest,
    response: &xai_grok_tools::implementations::grok_build::ask_user_question::AskUserQuestionExtResponse,
) -> QuestionAnswers {
    use xai_grok_tools::implementations::grok_build::ask_user_question::AskUserQuestionExtResponse;
    let AskUserQuestionExtResponse::Accepted {
        answers,
        annotations,
    } = response
    else {
        return None;
    };
    Some(
        request
            .questions
            .iter()
            .map(|q| {
                let typed = annotations
                    .as_ref()
                    .and_then(|a| a.get(&q.question))
                    .and_then(|a| a.notes.clone())
                    .filter(|n| !n.trim().is_empty());
                answers
                    .get(&q.question)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|label| match (&typed, label.as_str()) {
                        (Some(text), "Other") => text.clone(),
                        _ => label,
                    })
                    .collect()
            })
            .collect(),
    )
}

fn engine_question_handler(tx: mpsc::UnboundedSender<WorkshopTurnMsg>) -> QuestionHandler {
    Arc::new(move |req| {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = tx.send(WorkshopTurnMsg::QuestionAsk {
            request: req.clone(),
            reply: reply_tx,
        });
        reply_rx
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

/// Record a turn failure's technical cause where `workshop doctor` and the log can show it; the
/// user sees [`failure_line`] only.
pub fn log_failure_cause(cause: &str) {
    let cause = cause.split_whitespace().collect::<Vec<_>>().join(" ");
    tracing::warn!("workshop turn failed: {cause}");
    state::append_log(&engine_log_path(), &format!("turn failed: {cause}"));
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
            engine_phase(slot, &tx, THINKING);
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
                format!(
                    "`{}` is not a usable OpenCode binary ({reason})",
                    path.display()
                ),
            ));
        }
        Detection::NotInstalled => {
            engine_phase(slot, &tx, THINKING);
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
    engine_phase(slot, &tx, THINKING);

    let mut opts = EngineOptions::new(workspace);
    // The engine asks before edits and commands; what happens next is the agent's permission
    // mode (Plan/Normal prompt, Auto/Always-approve allow), decided on the UI thread per ask.
    opts.permission = Some(ask_before_edit_and_bash());
    // `sudo` in the engine's commands has no terminal to ask on. Grok Build's shell tool defers
    // to the user's own `SUDO_ASKPASS` helper when one is set; so does the engine (the variable
    // passes through). With none, Workshop is the helper: `SUDO_ASKPASS` → this process's socket
    // → one masked prompt, never the model.
    if std::env::var_os(crate::app::workshop_askpass::HELPER_ENV).is_none_or(|v| v.is_empty())
        && let Some(askpass) = askpass_server(slot, &ui_tx)
    {
        opts.extra_env = askpass.env();
    }
    // The engine's scratch space (OpenCode keeps it at `<tmpdir>/opencode`) lives under the
    // Workshop home, and the tool rows show it as `~/.workshop/tmp` (see `scrub_scratch_paths`).
    let scratch = engine_scratch_dir();
    if std::fs::create_dir_all(&scratch).is_ok() {
        opts.extra_env
            .push(("TMPDIR".into(), scratch.into_os_string()));
    }
    opts.permission_handler = Some(engine_permission_handler(ui_tx.clone()));
    opts.question_handler = Some(engine_question_handler(ui_tx));
    // The models answer as Workshop's assistant, not as "opencode".
    opts.config = Some(engine_config(&log));
    let sink_path = log.clone();
    opts.log_sink = Some(Arc::new(move |line: &str| {
        state::append_log(&sink_path, line)
    }));
    // `opencode serve` is up in a couple of seconds on any laptop; a server that has not bound
    // its port after this long is broken, and the user should hear so instead of waiting.
    opts.startup_timeout = ENGINE_START_TIMEOUT;
    // A model that stops mid-answer is aborted after this and the turn recovers (see `Stalled`);
    // one composing a tool call (silent by design) gets the longer ceiling.
    opts.idle_timeout = Some(stall_timeout());
    opts.compose_timeout = Some(compose_timeout());

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
                    THINKING.to_owned()
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

/// Warm-up in the background as soon as the composer opens with an engine model active (or, if
/// the engine model is picked later, on the first typed character): install (first run) and
/// start `opencode serve` while the user is still reading or typing, so the first message only
/// waits for the model. Also resolves OpenCode's live default model. Failures are recorded for
/// `workshop doctor` and surface on the first real turn, which reports the cause and falls back.
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
    /// The model answering can see images.
    image_input: bool,
}

/// The free models that can see images, in the order a turn that needs to see one is handed to
/// them: Muse Spark 1.3 sorted the owner's Panthera photos best and fastest, 1.2 is its backup,
/// MiMo last (a fresh engine often does not list it yet).
const VISION_MODELS: [&str; 3] = [
    "opencode/muse-spark-1.3-contributor-free",
    "opencode/muse-spark-1.2-contributor-free",
    "opencode/mimo-v2.6-flash-free",
];

/// The first of [`VISION_MODELS`] the running engine lists with image input.
fn vision_model(live: &[EngineModel]) -> Option<EngineModel> {
    VISION_MODELS.iter().find_map(|model_ref| {
        live.iter()
            .find(|m| m.model_ref == *model_ref && m.image_input)
            .cloned()
    })
}

/// The follow-up sent (never shown) when a turn moves to a model that can see what it opened.
const VISION_CONTINUE_PROMPT: &str =
    "Continue my request. You can now see the image files you opened.";

/// The follow-up sent (never shown) when a model that cannot see images downloaded some: a model
/// that can checks them before the turn ends.
const VISION_CHECK_PROMPT: &str = "Continue my request: open each image you downloaded with your \
read tool, check that it is a real photo of what I asked for and that no two are the same picture \
(an edited, cropped or resized version of one photo counts as the same), replace any that fail, \
then finish.";

/// A tool call that fetches image files (a `curl`/`wget`/script download of .jpg/.png/…).
fn downloads_images(tool: &str, input: &serde_json::Value) -> bool {
    let Some(command) = input.get("command").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let command = command.to_ascii_lowercase();
    tool == "bash"
        && [".jpg", ".jpeg", ".png", ".webp", ".gif"]
            .iter()
            .any(|ext| command.contains(ext))
        && ["curl", "wget", "urllib", "requests", "download"]
            .iter()
            .any(|fetch| command.contains(fetch))
}

/// A `read` of an image file: the engine attaches the picture to the tool result.
fn reads_an_image(tool: &str, input: &serde_json::Value) -> bool {
    const IMAGE_EXTENSIONS: [&str; 9] = [
        "jpg", "jpeg", "png", "webp", "gif", "heic", "bmp", "tif", "tiff",
    ];
    tool == "read"
        && input
            .get("filePath")
            .or_else(|| input.get("path"))
            .and_then(serde_json::Value::as_str)
            .and_then(|p| Path::new(p).extension())
            .and_then(|e| e.to_str())
            .is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
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
                // The user's picked effort level survives the swap while the model offers it.
                let live = live.carrying_effort_from(&model);
                if live.model_ref != model.model_ref || live.effort != model.effort {
                    let _ = tx.send(WorkshopTurnMsg::EngineDefaultResolved {
                        model: live.clone(),
                    });
                }
                model = live;
            }
            // A prompt that carries images goes to a model that can see them from the start.
            let sees = |m: &EngineModel| {
                cached_engine_models()
                    .iter()
                    .find(|c| c.model_ref == m.model_ref)
                    .map_or(m.image_input, |c| c.image_input)
            };
            if !spec.images.is_empty()
                && !sees(&model)
                && let Some(vision) = vision_model(&cached_engine_models())
            {
                state::append_log(
                    &engine_log_path(),
                    &format!(
                        "vision: the prompt carries {} image(s) {} cannot see; {} answers this turn",
                        spec.images.len(),
                        model.model_ref,
                        vision.model_ref
                    ),
                );
                let _ = tx.send(WorkshopTurnMsg::Answering {
                    model: vision.clone(),
                });
                model = vision;
            }
            engine_progress(tx, THINKING);
            // A model that is up but will not take the prompt cannot answer either: the silent
            // fallback answers instead (the cause is logged by the fallback dispatch).
            let session = match session {
                Some(s) if engine.session_exists(s).await.unwrap_or(false) => s.clone(),
                _ => engine.create_session(Some("Workshop")).await.map_err(|e| {
                    TurnStartError::EngineUnavailable(format!("could not open a session: {e}"))
                })?,
            };
            let _ = tx.send(WorkshopTurnMsg::EngineReady {
                engine: engine.clone(),
                session: session.clone(),
            });
            let mut req = TurnRequest::new(spec.text.clone());
            req.model = Some(model.model_ref.clone());
            // The picked effort level reaches OpenCode as the prompt's `variant`.
            req.variant = model.effort.clone();
            req.permission = permission;
            req.files = spec.images.clone();
            let turn = engine.prompt(&session, req).await.map_err(|e| {
                TurnStartError::EngineUnavailable(format!("the prompt was refused: {e}"))
            })?;
            // The live catalog knows what the model can see even when the saved pick predates it.
            let image_input = sees(&model);
            let follow_up = EngineFollowUp {
                engine,
                session,
                model_ref: model.model_ref,
                image_input,
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
                    log_failure_cause(&format!("{} could not be verified: {reason}", adapter.id()));
                    return Err(TurnStartError::Other(failure_line(
                        model.as_deref().unwrap_or(&adapter.id().to_string()),
                    )));
                }
                Detection::NotInstalled => {
                    log_failure_cause(&format!("{} is not installed", adapter.id()));
                    return Err(TurnStartError::Other(failure_line(
                        model.as_deref().unwrap_or(&adapter.id().to_string()),
                    )));
                }
            };
            let mut req = RunRequest::new(spec.text.clone(), &spec.cwd);
            req.model = model.clone();
            req.resume = resume.clone();
            req.permission = permission;
            let handle = spawn(&*adapter, &cli, req, &SupervisorOptions::default())
                .await
                .map_err(|e| {
                    log_failure_cause(&e.to_string());
                    TurnStartError::Other(failure_line(
                        model.as_deref().unwrap_or(&adapter.id().to_string()),
                    ))
                })?;
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
    // The waiting line is up from the first moment on every backend (the engine bring-up refines
    // it with its own phases) and stays under the latest block until the turn ends.
    engine_progress(&tx, THINKING);
    // Ctrl-C must work while the engine is still installing or starting, not only once events
    // flow: race the bring-up against the cancel signal.
    let built = tokio::select! {
        built = build_stream(&spec, &tx, permission) => built,
        _ = wait_cancelled(&mut cancel_rx) => {
            finish(&tx, true);
            return;
        }
    };
    let (mut stream, mut follow_up) = match built {
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

    let mut model_name = match &spec.kind {
        WorkshopTurnKind::Engine { model, .. } => model.name.clone(),
        WorkshopTurnKind::Adapter { adapter_id, .. } => adapter_id.to_string(),
    };
    let is_engine = matches!(spec.kind, WorkshopTurnKind::Engine { .. });
    let mut cancelled = false;
    let mut aborted_by_us = false;
    let mut first_event_at: Option<tokio::time::Instant> =
        Some(tokio::time::Instant::now() + FIRST_EVENT_TIMEOUT);
    // The backend's detail for a tool call (title, exit code, diff) arrives right before its
    // result; hold it so the UI gets one message per finished call.
    let mut tool_details: std::collections::HashMap<String, (Option<String>, serde_json::Value)> =
        std::collections::HashMap::new();
    // What the model wrote since its last tool call, and whether the turn failed: a turn that
    // ends on an announced action, or that pasted a requested file instead of writing it (once),
    // is continued (engine, not Plan), at most MAX_AUTO_CONTINUES times in all.
    let mut tail = String::new();
    let mut errored = false;
    let mut stalled = false;
    let mut continues = 0;
    let asks_for_files = asks_to_write_files(&spec.text);
    let mut wrote_a_file = false;
    let mut continued_to_write = false;
    // A model that cannot see images and opens one hands the rest of the turn to one that can
    // (once a turn): the turn is stopped right after that read and continued on the same engine
    // conversation, whose history carries the picture.
    let mut image_reads: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut vision_switch: Option<EngineModel> = None;
    let mut switched = false;
    // A model that cannot see and downloads images hands them, before the turn ends, to one that
    // can to check them (once a turn).
    let mut downloaded_images = false;
    loop {
        let silence = async {
            match first_event_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            _ = silence => {
                // A model whose first move is a tool call has answered — the engine announced the
                // call and is silent while its input streams; the engine's own ceilings own the
                // wait from here (nothing reaches this stream until the call is complete).
                if stream.composing() {
                    first_event_at = None;
                    continue;
                }
                // The model accepted the prompt but nothing came back: stop waiting. OpenCode's
                // model cannot answer, so the silent fallback answers instead; a vendor CLI gets
                // the plain failure line.
                stream.cancel();
                first_event_at = None;
                aborted_by_us = true;
                let cause = format!(
                    "no answer from {model_name} after {} s",
                    FIRST_EVENT_TIMEOUT.as_secs()
                );
                if is_engine {
                    let _ = tx.send(WorkshopTurnMsg::EngineUnavailable {
                        reason: cause,
                        text: spec.text.clone(),
                    });
                    break;
                }
                log_failure_cause(&cause);
                let _ = tx.send(WorkshopTurnMsg::Error(failure_line(&model_name)));
            }
            ev = stream.next_event() => match ev {
                Some(AdapterEvent::TextDelta { text }) => {
                    first_event_at = None;
                    tail.push_str(&text);
                    let _ = tx.send(WorkshopTurnMsg::Delta(text));
                }
                Some(AdapterEvent::ToolCall { id, name, input }) => {
                    first_event_at = None;
                    tail.clear();
                    wrote_a_file |= FILE_WRITE_TOOLS.contains(&name.as_str());
                    if reads_an_image(&name, &input) {
                        image_reads.insert(id.clone());
                    }
                    downloaded_images |= downloads_images(&name, &input);
                    let _ = tx.send(WorkshopTurnMsg::Tool { id, name, input });
                }
                Some(AdapterEvent::ToolDetail { id, title, metadata }) => {
                    tool_details.insert(id, (title, metadata));
                }
                Some(AdapterEvent::ToolResult { id, output, is_error }) => {
                    let opened_an_image = image_reads.remove(&id) && !is_error;
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
                    engine_progress(&tx, THINKING);
                    if opened_an_image
                        && !switched
                        && vision_switch.is_none()
                        && !aborted_by_us
                        && let Some(f) = &follow_up
                        && !f.image_input
                        && let Some(vision) = vision_model(&cached_engine_models())
                    {
                        state::append_log(
                            &engine_log_path(),
                            &format!(
                                "vision: {} cannot see the image it opened on {}; {} answers the rest of the turn",
                                f.model_ref, f.session, vision.model_ref
                            ),
                        );
                        vision_switch = Some(vision);
                        stream.cancel();
                    }
                }
                Some(AdapterEvent::Error { message }) => {
                    let nothing_yet = first_event_at.is_some();
                    first_event_at = None;
                    // The stop that hands the turn to a vision model is not a failure.
                    if vision_switch.is_some() {
                        continue;
                    }
                    errored = true;
                    if stream.stalled() {
                        // The engine's idle ceiling ended the turn: reported as `Stalled` below,
                        // with the recovery, not as a bare error.
                        stalled = true;
                        state::append_log(
                            &engine_log_path(),
                            &format!("stall: {model_name} stopped responding ({message}); recovering"),
                        );
                    } else if !(aborted_by_us && message == "run cancelled") {
                        // An abort we asked for (Ctrl-C, or the silence timeout above) is already
                        // reported; the backend's own "run cancelled" would only repeat it.
                        // OpenCode's model failing before a word came back cannot answer: the
                        // silent fallback answers instead. After output started, or on a vendor
                        // CLI, the plain failure line.
                        if is_engine && nothing_yet {
                            stream.cancel();
                            let _ = tx.send(WorkshopTurnMsg::EngineUnavailable {
                                reason: message,
                                text: spec.text.clone(),
                            });
                            break;
                        }
                        log_failure_cause(&message);
                        let _ = tx.send(WorkshopTurnMsg::Error(failure_line(&model_name)));
                    }
                }
                Some(AdapterEvent::Thinking { text }) => {
                    first_event_at = None;
                    let _ = tx.send(WorkshopTurnMsg::Thinking(text));
                }
                Some(AdapterEvent::Usage(usage)) => {
                    let _ = tx.send(WorkshopTurnMsg::Usage(usage));
                }
                Some(AdapterEvent::Done { .. }) => {}
                None => {
                    // Images a model that cannot see downloaded are checked by one that can.
                    let mut checking = false;
                    if vision_switch.is_none()
                        && downloaded_images
                        && !switched
                        && !aborted_by_us
                        && !errored
                        && permission == PermissionPolicy::WorkspaceWrite
                        && let Some(f) = &follow_up
                        && !f.image_input
                        && let Some(vision) = vision_model(&cached_engine_models())
                    {
                        state::append_log(
                            &engine_log_path(),
                            &format!(
                                "vision: {} downloaded images it cannot see on {}; {} checks them",
                                f.model_ref, f.session, vision.model_ref
                            ),
                        );
                        vision_switch = Some(vision);
                        checking = true;
                    }
                    if let Some(vision) = vision_switch.take()
                        && !aborted_by_us
                        && let Some(f) = follow_up.as_mut()
                    {
                        let prompt = if checking {
                            VISION_CHECK_PROMPT
                        } else {
                            VISION_CONTINUE_PROMPT
                        };
                        let mut req = TurnRequest::new(prompt);
                        req.model = Some(vision.model_ref.clone());
                        req.permission = permission;
                        match f.engine.prompt(&f.session, req).await {
                            Ok(turn) => {
                                stream = TurnStream::Engine(turn);
                                f.model_ref = vision.model_ref.clone();
                                f.image_input = true;
                                switched = true;
                                tail.clear();
                                model_name = vision.name.clone();
                                let _ = tx.send(WorkshopTurnMsg::Answering { model: vision });
                                engine_progress(&tx, THINKING);
                                continue;
                            }
                            Err(e) => {
                                log_failure_cause(&format!(
                                    "vision: {} could not take over the images: {e}",
                                    vision.model_ref
                                ));
                                let _ = tx.send(WorkshopTurnMsg::Error(failure_line(&vision.name)));
                            }
                        }
                    }
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
                                engine_progress(&tx, THINKING);
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
    if stalled && !cancelled {
        let _ = tx.send(WorkshopTurnMsg::Stalled {
            model: model_name.clone(),
            text: spec.text.clone(),
            engine: matches!(spec.kind, WorkshopTurnKind::Engine { .. }),
        });
    }
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

/// A vendor CLI installer the user started from an `Install` rail, while it runs: one at a time,
/// its latest output line shared with the runner task for the picker's status line.
#[derive(Debug, Clone)]
pub struct RailInstall {
    pub rail: workshop_detect::Rail,
    pub started: std::time::Instant,
    pub progress: Arc<std::sync::Mutex<String>>,
}

impl RailInstall {
    pub fn new(rail: workshop_detect::Rail) -> Self {
        Self {
            rail,
            started: std::time::Instant::now(),
            progress: Arc::new(std::sync::Mutex::new(String::new())),
        }
    }

    /// `Installing Claude Code… 12s · <latest installer line>` — one line, no plumbing.
    pub fn status_line(&self) -> String {
        let name = self.rail.vendor().display_name();
        let secs = self.started.elapsed().as_secs();
        let latest = self
            .progress
            .lock()
            .map(|l| l.clone())
            .unwrap_or_default();
        let latest: String = latest.chars().take(60).collect();
        match (secs >= 3, latest.is_empty()) {
            (false, true) => format!("Installing {name}\u{2026}"),
            (true, true) => format!("Installing {name}\u{2026} {secs}s"),
            (false, false) => format!("Installing {name}\u{2026} \u{b7} {latest}"),
            (true, false) => format!("Installing {name}\u{2026} {secs}s \u{b7} {latest}"),
        }
    }
}

/// Run `rail`'s official installer to completion (blocking; the effect runs it on the blocking
/// pool), streaming its lines into `progress` and appending them to
/// `<workshop home>/logs/install-<vendor>.log`. `Err` is the plain reason for the status line;
/// it names the log, never the command's internals.
pub fn run_rail_installer(
    rail: workshop_detect::Rail,
    progress: Arc<std::sync::Mutex<String>>,
) -> Result<(), String> {
    use std::io::Write as _;
    use workshop_detect::install::InstallOutcome;
    let vendor = rail.vendor();
    let command = workshop_detect::install_command(vendor)
        .ok_or_else(|| format!("{} has no installer to run", vendor.display_name()))?;
    let log_path = workshop_detect::install_log_path(&workshop_home(), vendor);
    if let Some(dir) = log_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("could not open {}: {e}", log_path.display()))?;
    let _ = writeln!(log, "$ {command}");
    tracing::info!(vendor = vendor.id(), log = %log_path.display(), "workshop: running the official installer");
    let outcome = workshop_detect::run_installer(
        &command,
        workshop_detect::install::INSTALL_TIMEOUT,
        |line| {
            let _ = writeln!(log, "{line}");
            if let Ok(mut p) = progress.lock() {
                *p = line.to_owned();
            }
        },
    )?;
    let _ = writeln!(log, "[workshop] {outcome:?}");
    match outcome {
        InstallOutcome::Installed => Ok(()),
        InstallOutcome::Failed { status } => Err(format!(
            "the installer exited with {} (details: {})",
            status
                .map(|c| format!("status {c}"))
                .unwrap_or_else(|| "a signal".to_owned()),
            log_path.display()
        )),
        InstallOutcome::Hung => Err(format!(
            "the installer stopped responding (details: {})",
            log_path.display()
        )),
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
        .unwrap_or_else(|| {
            vendor
                .binary_names()
                .first()
                .copied()
                .unwrap_or("")
                .to_owned()
        });
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
    use super::{
        announces_unfinished_action, asks_to_write_files, downloads_images, ends_with_code_block,
        engine_question_answers, engine_questions,
    };

    #[test]
    fn image_downloads_are_recognised() {
        let bash = |command: &str| downloads_images("bash", &serde_json::json!({ "command": command }));
        assert!(bash("mkdir -p ~/Desktop/panthera && curl -fsSL -o lion_1.jpg https://upload.wikimedia.org/x.jpg"));
        assert!(bash("python3 -c \"import urllib.request; urllib.request.urlretrieve(u, 'tiger.png')\""));
        assert!(!bash("ls ~/Desktop/*.jpg"));
        assert!(!bash("curl -fsSL https://example.org/api.json"));
        assert!(!downloads_images("read", &serde_json::json!({ "filePath": "a.jpg" })));
    }
    use workshop_adapters::opencode_engine::{QuestionChoice, QuestionPrompt, QuestionRequest};
    use xai_grok_tools::implementations::grok_build::ask_user_question::{
        AskUserQuestionExtResponse, QuestionAnnotation,
    };

    fn install_question() -> QuestionRequest {
        let choice = |label: &str| QuestionChoice {
            label: label.into(),
            description: String::new(),
        };
        QuestionRequest {
            id: "que_1".into(),
            session_id: "ses_1".into(),
            questions: vec![
                QuestionPrompt {
                    question: "How should Ghostty be installed?".into(),
                    header: "Install".into(),
                    options: vec![choice("PPA (Recommended)"), choice(".deb")],
                    multiple: false,
                },
                QuestionPrompt {
                    question: "Where should it go?".into(),
                    header: "Where".into(),
                    options: vec![choice("/usr/bin")],
                    multiple: false,
                },
            ],
            call_id: Some("call_9".into()),
        }
    }

    #[test]
    fn engine_questions_open_in_the_question_view_and_answers_go_back_in_order() {
        let req = install_question();
        let shown = engine_questions(&req);
        assert_eq!(shown.len(), 2);
        assert_eq!(shown[0].options[0].label, "PPA (Recommended)");
        let mut answers = indexmap::IndexMap::new();
        answers.insert("Where should it go?".to_owned(), vec!["Other".to_owned()]);
        answers.insert(
            "How should Ghostty be installed?".to_owned(),
            vec!["PPA (Recommended)".to_owned()],
        );
        let mut notes = std::collections::HashMap::new();
        notes.insert(
            "Where should it go?".to_owned(),
            QuestionAnnotation {
                preview: None,
                notes: Some("~/.local/bin".into()),
            },
        );
        let accepted = AskUserQuestionExtResponse::Accepted {
            answers,
            annotations: Some(notes),
        };
        assert_eq!(
            engine_question_answers(&req, &accepted),
            Some(vec![
                vec!["PPA (Recommended)".to_owned()],
                vec!["~/.local/bin".to_owned()]
            ])
        );
        assert_eq!(
            engine_question_answers(&req, &AskUserQuestionExtResponse::Cancelled),
            None
        );
    }

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
