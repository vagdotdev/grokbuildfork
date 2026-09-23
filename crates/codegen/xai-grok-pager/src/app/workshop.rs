//! Workshop overlay: connection picker data loading and selection activation.
//!
//! Models rows come from `workshop-providers`, rail state from `workshop-detect`, and the OpenCode
//! free catalog from the engine cache. Activating a Direct API / Local row writes
//! `[model.<key>]` + `default` into `$WORKSHOP_HOME/config.toml`, exports a saved key as its
//! `WORKSHOP_<PROVIDER>_API_KEY` env var (never into the file), asks the shell to reload its model
//! list, and authenticates with the non-interactive method. Nothing here starts an OAuth flow.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use workshop_adapters::opencode_engine::{
    EngineOptions, EngineState, InstallOptions, OpenCodeEngine, PermissionHandler,
    PermissionReply, TurnHandle, TurnRequest, detect_opencode, install_opencode, state,
};
use workshop_adapters::supervisor::{RunHandle, SupervisorOptions, spawn};
use workshop_adapters::{
    AdapterEvent, AdapterId, DetectOptions, Detection, PermissionPolicy, RunRequest, detect,
};
use workshop_auth::{EngineModel, PickerSnapshot, models_rows};
use workshop_providers::{
    Catalog, CredentialBroker, CredentialInjection, FileSecretStore, KeyringSecretStore,
    LayeredSecretStore, resolve_model_entry, select_default,
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

fn engine_cache_path() -> PathBuf {
    workshop_home()
        .join("catalog-cache")
        .join("opencode-engine.json")
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

/// Build the picker snapshot: loopback local-server probe, builtin + cached catalogs, broker
/// connection state, engine cache, and the CLI rail probe (child processes, so on the blocking pool).
///
/// This is the single data entry point of the `/model` + `/auth` overlay: everything the two views
/// list comes from the `PickerSnapshot` returned here (`workshop_auth::models_rows` builds the rows
/// from a `Catalog` plus the engine models; `PickerState::apply_snapshot` takes it). Live provider
/// catalogs plug in by widening the `Catalog` / engine models handed to `models_rows`, not by
/// touching the views.
pub async fn load_picker_snapshot() -> PickerSnapshot {
    let local = workshop_providers::probe_all_local_servers(Duration::from_millis(600)).await;
    let mut catalog = Catalog::builtin();
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
    let rails = tokio::task::spawn_blocking(|| {
        let probe = workshop_detect::probe_all(&workshop_detect::DetectConfig::default());
        workshop_detect::rails(&probe, workshop_detect::model::default_models).to_vec()
    })
    .await
    .unwrap_or_else(|_| {
        workshop_detect::Rail::ALL
            .iter()
            .map(|r| workshop_detect::RailState::detecting(*r))
            .collect()
    });
    PickerSnapshot {
        rows,
        rails,
        default_selection,
        secret_backend: Some(secret_backend),
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
    Tool { name: String, summary: String },
    ToolResult { ok: bool },
    /// A permission ask the backend escalated; `decision` is what Workshop answered (asks are
    /// answered from the pager's permission mode — a synchronous vendor hook, so it is surfaced,
    /// not blocking-interactive).
    Permission { summary: String, decision: &'static str },
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

/// The one line a user sees when the engine path fails after the engine is up: the cause, the
/// way out, and where the details went. (A failure *before* the engine is up goes through the
/// Kilo fallback instead — see `TurnStartError::EngineUnavailable`.)
pub fn engine_failure_line(cause: &str) -> String {
    format!(
        "OpenCode engine: {cause} — /model switches to another model; log: {}",
        engine_log_path().display()
    )
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
    // version via the vendor's own script only if absent (never a bundled binary).
    let install = InstallOptions::default();
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
            state::clear_quarantine(&path);
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
    opts.permission_handler = Some(engine_permission_handler(tx, slot.always_approve.clone()));
    opts.log_path = Some(log.clone());
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
            if let Some(live) = live_engine_default(&engine).await {
                let _ = tx.send(WorkshopTurnMsg::EngineDefaultResolved { model: live });
            }
            let _ = tx.send(WorkshopTurnMsg::EngineWarm { engine });
        }
        Err(e) => tracing::warn!("workshop engine warm-up failed: {e}"),
    }
}

/// Fetch the engine's live free catalog, cache it for `/model`, and return OpenCode's current
/// default model. `None` when the catalog cannot be read (offline, old engine).
async fn live_engine_default(engine: &OpenCodeEngine) -> Option<EngineModel> {
    let catalog = engine.free_models().await.ok()?;
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
        return None;
    }
    store_engine_models(&models);
    let default = catalog.default_or_first()?;
    models
        .iter()
        .find(|m| m.model_ref == default.model_ref)
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
            // The default engine model is whichever model OpenCode's live catalog marks as
            // default; the pinned seed only stands in while that catalog is unreachable.
            if model.is_default
                && let Some(live) = live_engine_default(&engine).await
            {
                if live.model_ref != model.model_ref {
                    let _ = tx.send(WorkshopTurnMsg::EngineDefaultResolved {
                        model: live.clone(),
                    });
                }
                model = live;
            }
            engine_progress(tx, format!("Waiting for {}…", model.name));
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
                    first_event_at = None;
                    // An abort we asked for (Ctrl-C, or the silence timeout above) is already
                    // reported; the backend's own "run cancelled" would only repeat it.
                    if !(aborted_by_us && message == "run cancelled") {
                        let _ = tx.send(WorkshopTurnMsg::Error(message));
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
        .unwrap_or_else(|| vendor.binary_names().first().copied().unwrap_or("").to_owned());
    let mut argv = vec![bin];
    argv.extend(
        workshop_detect::login_argv(vendor)
            .iter()
            .map(|s| s.to_string()),
    );
    argv
}
