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

use tokio::sync::{mpsc, watch};
use workshop_adapters::opencode_engine::{
    EngineOptions, InstallOptions, InstallProgress, OpenCodeEngine, PermissionHandler,
    PermissionReply, TurnHandle, TurnRequest, clear_quarantine, detect_opencode, format_bytes,
    install_opencode,
};

use crate::app::workshop_engine_state::{self as state, EngineState};
use workshop_adapters::supervisor::{RunHandle, SupervisorOptions, spawn};
use workshop_adapters::{
    AdapterEvent, AdapterId, DetectOptions, Detection, PermissionPolicy, RunRequest, detect,
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
    /// Picker row id of the active connection (Engine rows only; rails are not Models rows and a
    /// Shell connection is the shell's own default model).
    pub fn active_row_id(&self) -> Option<String> {
        match self {
            Self::Engine { model } => Some(model.row_id()),
            Self::Shell | Self::Adapter { .. } => None,
        }
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
pub fn activate_placeholder_session() -> Result<String, String> {
    workshop_auth::config_write::activate_placeholder_session(&workshop_auth::config_path())
        .map_err(|e| e.to_string())
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
}

/// What the UI thread learns as a turn streams. Mapped to scrollback `RenderBlock`s by the event
/// loop's Workshop `select!` arm (kept UI-agnostic so `workshop-adapters` never depends on the pager).
#[derive(Debug)]
pub enum WorkshopTurnMsg {
    /// One line of waiting status (`Thinking…`, or the first-time download progress); replaces the
    /// previous progress line, and the first real event clears it. Never silent while the user
    /// waits, and never a word about what runs underneath.
    Progress(String),
    /// The OpenCode engine started (lazily, on the first turn); cache it and the session so later
    /// turns reuse the same `opencode serve` and conversation. Engine turns only.
    EngineReady {
        engine: Arc<OpenCodeEngine>,
        session: String,
    },
    /// The first-keystroke warm-up finished: the engine is up before the first message.
    EngineWarm {
        engine: Arc<OpenCodeEngine>,
    },
    /// The engine's live catalog names a different default than the pinned seed the first run
    /// activated: the connection follows OpenCode's default (composer label, persisted file).
    EngineDefaultResolved {
        model: EngineModel,
    },
    Delta(String),
    Tool {
        name: String,
        summary: String,
    },
    ToolResult {
        ok: bool,
    },
    /// A permission ask the backend escalated; `decision` is what Workshop answered (asks are
    /// answered from the pager's permission mode — a synchronous vendor hook, so it is surfaced,
    /// not blocking-interactive).
    Permission {
        summary: String,
        decision: &'static str,
    },
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

/// The one `opencode serve` this process owns, shared by the first-keystroke warm-up and every
/// turn so two callers never start two servers: whoever holds the lock starts it, the other
/// reuses it. `phase` carries the live bring-up status so a turn that waits on the lock can show
/// what the other task is doing ("Installing…") instead of a generic line.
pub struct EngineSlotInner {
    engine: tokio::sync::Mutex<Option<Arc<OpenCodeEngine>>>,
    phase: watch::Sender<String>,
    /// The pager's always-approve mode as of the current turn; the engine's permission hook is
    /// installed once per `opencode serve` and reads this live instead of a start-time snapshot.
    always_approve: Arc<std::sync::atomic::AtomicBool>,
    /// The warm-up's failure, so a turn typed right after it reports that cause at once instead
    /// of silently repeating a 30 s bring-up that just failed. Cleared by the next attempt.
    recent_failure: std::sync::Mutex<Option<(std::time::Instant, String)>>,
}

pub type EngineSlot = Arc<EngineSlotInner>;

pub fn new_engine_slot() -> EngineSlot {
    Arc::new(EngineSlotInner {
        engine: tokio::sync::Mutex::new(None),
        phase: watch::channel(String::new()).0,
        always_approve: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        recent_failure: std::sync::Mutex::new(None),
    })
}

/// How long a warm-up failure is reported to the next turn as-is before it retries.
const RECENT_FAILURE_WINDOW: Duration = Duration::from_secs(60);
/// Hard ceiling on a turn's silence before its first event: past this the engine is up but the
/// model never answered, and the user gets the cause plus a way out instead of a spinner.
pub const FIRST_EVENT_TIMEOUT: Duration = Duration::from_secs(90);
/// Hard ceiling on `opencode serve` binding its port and passing its health check.
pub const ENGINE_START_TIMEOUT: Duration = Duration::from_secs(30);
/// The one thing the user sees while nothing has come back yet — whatever is happening behind it
/// (installing, starting, connecting, waiting for the model). No plumbing words, ever.
pub const THINKING: &str = "Thinking\u{2026}";
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

/// The first-run download, once the vendor script has started writing: still `Thinking…`, with
/// the honest byte count so a minute-long first message never looks hung.
pub fn install_progress_line(bytes: u64) -> String {
    format!(
        "{THINKING} \u{b7} first-time setup, {} downloaded",
        format_bytes(bytes)
    )
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
    /// The pager's always-approve mode; maps to `WorkspaceWrite` (else `ReadOnly`).
    pub always_approve: bool,
}

fn summarize_tool_input(input: &serde_json::Value) -> String {
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

/// Build the engine permission hook: surface each ask to the UI and answer it from the pager's
/// permission mode *at the time of the ask* (the hook outlives the turn that started the engine).
/// `PermissionHandler` is synchronous (it cannot await the user), so this is a surfaced
/// auto-decision, not a blocking prompt.
fn engine_permission_handler(
    tx: mpsc::UnboundedSender<WorkshopTurnMsg>,
    always_approve: Arc<std::sync::atomic::AtomicBool>,
) -> PermissionHandler {
    Arc::new(move |req| {
        let approve = always_approve.load(std::sync::atomic::Ordering::Relaxed);
        let decision = if approve {
            PermissionReply::Once
        } else {
            PermissionReply::Reject
        };
        let _ = tx.send(WorkshopTurnMsg::Permission {
            summary: format!("{} ({})", req.title, req.kind),
            decision: if approve { "allowed" } else { "rejected" },
        });
        decision
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
async fn start_engine(
    slot: &EngineSlot,
    workspace: &Path,
    tx: mpsc::UnboundedSender<WorkshopTurnMsg>,
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
    opts.permission_handler = Some(engine_permission_handler(tx, slot.always_approve.clone()));
    let sink_path = log.clone();
    opts.log_sink = Some(Arc::new(move |line: &str| {
        state::append_log(&sink_path, line)
    }));
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
    always_approve: bool,
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
    slot.always_approve
        .store(always_approve, std::sync::atomic::Ordering::Relaxed);
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
    match start_engine(slot, workspace, tx.clone()).await {
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
    let (quiet_tx, _quiet_rx) = mpsc::unbounded_channel();
    match acquire_engine(&slot, &workspace, &quiet_tx, false).await {
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

async fn build_stream(
    spec: &WorkshopTurnSpec,
    tx: &mpsc::UnboundedSender<WorkshopTurnMsg>,
    permission: PermissionPolicy,
) -> Result<TurnStream, TurnStartError> {
    match &spec.kind {
        WorkshopTurnKind::Engine {
            slot,
            session,
            model,
        } => {
            let mut model = model.clone();
            let engine = acquire_engine(slot, &spec.cwd, tx, spec.always_approve)
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
            let turn = engine.prompt(&session, req).await.map_err(|e| {
                TurnStartError::EngineUnavailable(format!("the prompt was refused: {e}"))
            })?;
            Ok(TurnStream::Engine(turn))
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
            Ok(TurnStream::Adapter(handle))
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
    // A coding tool edits code: the engine always runs OpenCode's `build` agent (the `plan` agent
    // cannot touch files), and each edit/command shows up in the scrollback as it happens. The
    // vendor CLIs keep their own read-only default until always-approve is on, as before.
    let permission = match spec.kind {
        WorkshopTurnKind::Engine { .. } => PermissionPolicy::WorkspaceWrite,
        WorkshopTurnKind::Adapter { .. } if spec.always_approve => PermissionPolicy::WorkspaceWrite,
        WorkshopTurnKind::Adapter { .. } => PermissionPolicy::ReadOnly,
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
    let mut stream = match built {
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
    let is_engine = matches!(spec.kind, WorkshopTurnKind::Engine { .. });
    let mut cancelled = false;
    let mut aborted_by_us = false;
    let mut first_event_at: Option<tokio::time::Instant> =
        Some(tokio::time::Instant::now() + FIRST_EVENT_TIMEOUT);
    loop {
        let silence = async {
            match first_event_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            _ = silence => {
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
                    let _ = tx.send(WorkshopTurnMsg::Delta(text));
                }
                Some(AdapterEvent::ToolCall { name, input, .. }) => {
                    first_event_at = None;
                    let _ = tx.send(WorkshopTurnMsg::Tool {
                        name,
                        summary: summarize_tool_input(&input),
                    });
                }
                Some(AdapterEvent::ToolResult { is_error, .. }) => {
                    let _ = tx.send(WorkshopTurnMsg::ToolResult { ok: !is_error });
                }
                Some(AdapterEvent::Error { message }) => {
                    let nothing_yet = first_event_at.is_some();
                    first_event_at = None;
                    // An abort we asked for (Ctrl-C, or the silence timeout above) is already
                    // reported; the backend's own "run cancelled" would only repeat it.
                    if !(aborted_by_us && message == "run cancelled") {
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
                Some(AdapterEvent::Thinking { .. }) => {
                    first_event_at = None;
                }
                Some(AdapterEvent::Usage(_)) | Some(AdapterEvent::Done { .. }) => {}
                None => break,
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
