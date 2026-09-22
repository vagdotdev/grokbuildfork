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
    EngineOptions, InstallOptions, OpenCodeEngine, PermissionHandler, PermissionReply, TurnHandle,
    TurnRequest, ensure_opencode,
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
#[derive(Debug, Clone, PartialEq)]
pub enum WorkshopConnection {
    /// The shell's own agent loop (Direct API / Local `[model.<key>]`), the default.
    Shell,
    /// A free model behind the OpenCode engine (`opencode serve`).
    Engine { model: EngineModel },
    /// A vendor CLI adapter on a Ready subscription rail.
    Adapter {
        rail: workshop_detect::Rail,
        model: workshop_detect::ModelRef,
    },
}

impl WorkshopConnection {
    /// Composer label: `Claude · {model}` for rails, `OpenCode · {model}` for the engine.
    pub fn composer_label(&self) -> Option<String> {
        match self {
            Self::Shell => None,
            Self::Engine { model } => Some(format!("{} · OpenCode", model.name)),
            Self::Adapter { rail, model } => Some(workshop_detect::composer_label(*rail, model)),
        }
    }
    pub fn is_shell(&self) -> bool {
        matches!(self, Self::Shell)
    }
}

/// `$WORKSHOP_HOME` (created if missing).
pub fn workshop_home() -> PathBuf {
    xai_dirs::grok_home()
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
    /// The OpenCode engine started (lazily, on the first turn); cache it and the session so later
    /// turns reuse the same `opencode serve` and conversation. Engine turns only.
    EngineReady {
        engine: Arc<OpenCodeEngine>,
        session: String,
    },
    Delta(String),
    Tool { name: String, summary: String },
    ToolResult { ok: bool },
    /// A permission ask the backend escalated; `decision` is what Workshop answered (asks are
    /// answered from the pager's permission mode — a synchronous vendor hook, so it is surfaced,
    /// not blocking-interactive).
    Permission { summary: String, decision: &'static str },
    Error(String),
    /// The turn ended; `session_id` is persisted per workspace for resume.
    Done {
        session_id: Option<String>,
        cancelled: bool,
    },
}

/// Which backend a submitted prompt should run on.
pub enum WorkshopTurnKind {
    Engine {
        engine: Option<Arc<OpenCodeEngine>>,
        session: Option<String>,
        model_ref: String,
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
/// permission mode. `PermissionHandler` is synchronous (it cannot await the user), so this is a
/// surfaced auto-decision, not a blocking prompt — full interactive asks are a follow-up.
fn engine_permission_handler(
    tx: mpsc::UnboundedSender<WorkshopTurnMsg>,
    always_approve: bool,
) -> PermissionHandler {
    Arc::new(move |req| {
        let decision = if always_approve {
            PermissionReply::Once
        } else {
            PermissionReply::Reject
        };
        let _ = tx.send(WorkshopTurnMsg::Permission {
            summary: format!("{} ({})", req.title, req.kind),
            decision: if always_approve { "allowed" } else { "rejected" },
        });
        decision
    })
}

async fn start_engine(
    workspace: &Path,
    tx: mpsc::UnboundedSender<WorkshopTurnMsg>,
    always_approve: bool,
) -> Result<OpenCodeEngine, String> {
    // Detect an `opencode` on PATH / known dirs / the Workshop tools tree; install the pinned
    // version via the vendor's own script only if absent (never a bundled binary).
    let cli = ensure_opencode(&DetectOptions::default(), Some(&InstallOptions::default()))
        .await
        .map_err(|e| e.to_string())?;
    let mut opts = EngineOptions::new(workspace);
    opts.permission_handler = Some(engine_permission_handler(tx, always_approve));
    OpenCodeEngine::start(&cli, opts)
        .await
        .map_err(|e| e.to_string())
}

async fn build_stream(
    spec: &WorkshopTurnSpec,
    tx: &mpsc::UnboundedSender<WorkshopTurnMsg>,
    permission: PermissionPolicy,
) -> Result<TurnStream, String> {
    match &spec.kind {
        WorkshopTurnKind::Engine {
            engine,
            session,
            model_ref,
        } => {
            let engine = match engine {
                Some(e) => e.clone(),
                None => Arc::new(start_engine(&spec.cwd, tx.clone(), spec.always_approve).await?),
            };
            let session = match session {
                Some(s) if engine.session_exists(s).await.unwrap_or(false) => s.clone(),
                _ => engine
                    .create_session(Some("Workshop"))
                    .await
                    .map_err(|e| e.to_string())?,
            };
            let _ = tx.send(WorkshopTurnMsg::EngineReady {
                engine: engine.clone(),
                session: session.clone(),
            });
            let mut req = TurnRequest::new(spec.text.clone());
            req.model = Some(model_ref.clone());
            req.permission = permission;
            let turn = engine
                .prompt(&session, req)
                .await
                .map_err(|e| e.to_string())?;
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
                    return Err(format!("{} could not be verified: {reason}", adapter.id()));
                }
                Detection::NotInstalled => {
                    return Err(format!("{} is not installed", adapter.id()));
                }
            };
            let mut req = RunRequest::new(spec.text.clone(), &spec.cwd);
            req.model = model.clone();
            req.resume = resume.clone();
            req.permission = permission;
            let handle = spawn(&*adapter, &cli, req, &SupervisorOptions::default())
                .await
                .map_err(|e| e.to_string())?;
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
    let permission = if spec.always_approve {
        PermissionPolicy::WorkspaceWrite
    } else {
        PermissionPolicy::ReadOnly
    };
    let mut stream = match build_stream(&spec, &tx, permission).await {
        Ok(s) => s,
        Err(error) => {
            let _ = tx.send(WorkshopTurnMsg::Error(error));
            let _ = tx.send(WorkshopTurnMsg::Done {
                session_id: None,
                cancelled: false,
            });
            return;
        }
    };

    let mut cancelled = false;
    loop {
        tokio::select! {
            ev = stream.next_event() => match ev {
                Some(AdapterEvent::TextDelta { text }) => {
                    let _ = tx.send(WorkshopTurnMsg::Delta(text));
                }
                Some(AdapterEvent::ToolCall { name, input, .. }) => {
                    let _ = tx.send(WorkshopTurnMsg::Tool {
                        name,
                        summary: summarize_tool_input(&input),
                    });
                }
                Some(AdapterEvent::ToolResult { is_error, .. }) => {
                    let _ = tx.send(WorkshopTurnMsg::ToolResult { ok: !is_error });
                }
                Some(AdapterEvent::Error { message }) => {
                    let _ = tx.send(WorkshopTurnMsg::Error(message));
                }
                Some(AdapterEvent::Thinking { .. })
                | Some(AdapterEvent::Usage(_))
                | Some(AdapterEvent::Done { .. }) => {}
                None => break,
            },
            _ = cancel_rx.changed() => {
                if *cancel_rx.borrow() && !cancelled {
                    cancelled = true;
                    stream.cancel();
                }
            }
        }
    }

    let session_id = stream.session_id();
    let _ = tx.send(WorkshopTurnMsg::Done {
        session_id,
        cancelled,
    });
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
