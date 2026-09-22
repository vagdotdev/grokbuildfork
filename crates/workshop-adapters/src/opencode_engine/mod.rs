//! OpenCode engine: run the genuine `opencode serve` underneath Workshop and
//! drive it over its loopback HTTP/SSE session API.
//!
//! Why `serve` and not `opencode run --format json`:
//!
//! | need | `run --format json` | `serve` (this module) |
//! |---|---|---|
//! | text streaming | whole `text` part at end only | `message.part.delta` per token |
//! | tool events | completed parts only | `pending -> running -> completed/error` |
//! | cancel | kill the process (turn lost) | `POST /session/{id}/abort`, session intact |
//! | multi-turn | respawn per turn with `--session` | same session, same process |
//! | permissions | auto-reject (or `--auto`) | `permission.*` events + reply endpoint |
//! | free catalog | `opencode models` ids, no cost | `GET /config/providers` with cost + default |
//! | health/version | none | `GET /global/health` |
//!
//! Both work keyless for OpenCode's free tier because the requests to
//! `opencode.ai/zen` are made by the real OpenCode binary; Workshop only
//! talks to `127.0.0.1`. Nothing here reads OpenCode's `auth.json` or its
//! database, and `OPENCODE_PERMISSION` is never set (it is not in the env
//! allowlist); the agent (`plan` / `build`) carries the permission policy the
//! way OpenCode expects.

mod catalog;
mod events;
mod http;
mod install;
pub mod state;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use ::http::Method;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::sync::{mpsc, oneshot, watch};

pub use catalog::{FreeCatalog, FreeModel, parse_free_catalog};
pub use events::{PermissionRequest, ServeTurn};
pub use http::{HttpError, ServerClient};
pub use install::{
    InstallError, InstallOptions, InstallTarget, OFFICIAL_INSTALLER_URL, detect_opencode,
    ensure_opencode, install_opencode, is_workshop_managed, workshop_tools_dir,
};
pub use state::{EngineState, quarantine_flag};

use crate::adapter::{Adapter, AdapterId, PermissionPolicy, PinStatus, Terminal};
use crate::detect::InstalledCli;
use crate::event::AdapterEvent;
use crate::supervisor::RunOutcome;
use crate::vendors::OpenCodeAdapter;

/// Answer to a [`PermissionRequest`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionReply {
    Once,
    Always,
    Reject,
}

impl PermissionReply {
    fn as_str(self) -> &'static str {
        match self {
            PermissionReply::Once => "once",
            PermissionReply::Always => "always",
            PermissionReply::Reject => "reject",
        }
    }
}

/// UI hook that answers permission prompts. Without one, every ask is
/// rejected (fail closed) and the agent continues with that answer.
pub type PermissionHandler = Arc<dyn Fn(&PermissionRequest) -> PermissionReply + Send + Sync>;

#[derive(Clone)]
pub struct EngineOptions {
    /// Project directory the server runs in — normally an isolated worktree.
    pub workspace: PathBuf,
    /// Environment for the server; `None` = minimal env from this process.
    pub env: Option<BTreeMap<OsString, OsString>>,
    pub startup_timeout: Duration,
    /// How long a turn may stay silent before it is aborted.
    pub idle_timeout: Option<Duration>,
    /// After `abort`, how long to wait for the server to report idle.
    pub cancel_grace: Duration,
    pub permission_handler: Option<PermissionHandler>,
    pub allow_untested_versions: bool,
    /// Append the server's stdout and stderr lines here (see [`state::log_path`]) so a failed
    /// start on a machine we cannot see still leaves the actual cause on disk.
    pub log_path: Option<PathBuf>,
}

impl EngineOptions {
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
            env: None,
            startup_timeout: Duration::from_secs(60),
            idle_timeout: Some(Duration::from_secs(600)),
            cancel_grace: Duration::from_secs(10),
            permission_handler: None,
            allow_untested_versions: true,
            log_path: None,
        }
    }
}

impl std::fmt::Debug for EngineOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineOptions")
            .field("workspace", &self.workspace)
            .field("startup_timeout", &self.startup_timeout)
            .field("idle_timeout", &self.idle_timeout)
            .field("cancel_grace", &self.cancel_grace)
            .field("permission_handler", &self.permission_handler.is_some())
            .field("allow_untested_versions", &self.allow_untested_versions)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("expected an opencode binary, got {0}")]
    NotOpenCode(AdapterId),
    #[error("opencode {version} is older than the supported {min}; update it")]
    UnsupportedVersion { version: String, min: &'static str },
    #[error("opencode {version} is newer than the tested {max} and untested versions are disabled")]
    UntestedVersion { version: String, max: &'static str },
    #[error(transparent)]
    Env(#[from] crate::env::DeniedEnvVar),
    #[error("failed to start `opencode serve`: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("`opencode serve` did not become ready within {timeout:?}: {detail}")]
    Startup { timeout: Duration, detail: String },
    #[error("`opencode serve` exited during startup ({status}): {stderr}")]
    Exited { status: String, stderr: String },
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error("unexpected response shape from {endpoint}: {detail}")]
    Shape {
        endpoint: &'static str,
        detail: String,
    },
    #[error("model reference `{0}` must look like `opencode/<model>`")]
    BadModelRef(String),
}

/// One request to the engine for a single turn.
#[derive(Clone, Debug)]
pub struct TurnRequest {
    pub text: String,
    /// `opencode/<model>`; `None` lets OpenCode pick its default (free) model.
    pub model: Option<String>,
    pub permission: PermissionPolicy,
}

impl TurnRequest {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            model: None,
            permission: PermissionPolicy::ReadOnly,
        }
    }
}

struct ServerProcess {
    child: tokio::process::Child,
    group: Arc<xai_tty_utils::ProcessGroup>,
    stderr_tail: tokio::task::JoinHandle<String>,
}

/// A running `opencode serve` owned (or attached to) by Workshop.
pub struct OpenCodeEngine {
    client: ServerClient,
    version: String,
    workspace: PathBuf,
    process: Option<ServerProcess>,
    permission_handler: Option<PermissionHandler>,
    idle_timeout: Option<Duration>,
    cancel_grace: Duration,
}

impl std::fmt::Debug for OpenCodeEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenCodeEngine")
            .field("base_url", &self.client.base_url())
            .field("version", &self.version)
            .field("workspace", &self.workspace)
            .field("owns_process", &self.process.is_some())
            .finish()
    }
}

fn random_password() -> String {
    let bytes: [u8; 24] = rand::random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn parse_listening_line(line: &str) -> Option<SocketAddr> {
    let idx = line.find("listening on ")?;
    let url = line[idx + "listening on ".len()..].trim();
    let host_port = url.strip_prefix("http://").unwrap_or(url);
    let host_port = host_port.split(['/', ' ']).next()?;
    host_port.parse().ok().or_else(|| {
        // `localhost:port`
        let (_, port) = host_port.rsplit_once(':')?;
        Some(SocketAddr::from(([127, 0, 0, 1], port.parse().ok()?)))
    })
}

impl OpenCodeEngine {
    /// Spawn `opencode serve` for `opts.workspace` and wait until it is healthy.
    pub async fn start(cli: &InstalledCli, opts: EngineOptions) -> Result<Self, EngineError> {
        if cli.adapter != AdapterId::OpenCode {
            return Err(EngineError::NotOpenCode(cli.adapter));
        }
        let pin = OpenCodeAdapter.version_pin();
        match cli.pin {
            PinStatus::OlderThanSupported => {
                return Err(EngineError::UnsupportedVersion {
                    version: cli.version.clone(),
                    min: pin.min_supported,
                });
            }
            PinStatus::NewerThanTested if !opts.allow_untested_versions => {
                return Err(EngineError::UntestedVersion {
                    version: cli.version.clone(),
                    max: pin.max_tested,
                });
            }
            _ => {}
        }

        let mut env = match &opts.env {
            Some(env) => crate::env::minimal_env(env.clone(), &[])?,
            None => crate::env::minimal_env_from_process(&[])?,
        };
        // Loopback server; still password-protected so another local user
        // cannot drive the session. Generated per launch, never persisted.
        let password = random_password();
        env.insert(
            OsString::from("OPENCODE_SERVER_PASSWORD"),
            OsString::from(&password),
        );
        debug_assert!(!env.contains_key(&OsString::from("OPENCODE_PERMISSION")));

        let port = pick_free_port().await.map_err(EngineError::Spawn)?;
        let mut cmd = tokio::process::Command::new(&cli.path);
        cmd.args([
            "serve",
            "--hostname",
            "127.0.0.1",
            "--port",
            &port.to_string(),
        ])
        .current_dir(&opts.workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
        crate::env::apply(&mut cmd, &env);
        tracing::info!(program = %cli.path.display(), version = %cli.version, port, workspace = %opts.workspace.display(), "starting opencode serve");
        let (mut child, group) = xai_tty_utils::global_process_scope()
            .spawn(cmd)
            .map_err(EngineError::Spawn)?;

        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let (addr_tx, addr_rx) = oneshot::channel();
        if let Some(log) = &opts.log_path {
            state::append_log(
                log,
                &format!(
                    "start: {} serve --hostname 127.0.0.1 --port {port} (version {}, workspace {})",
                    cli.path.display(),
                    cli.version,
                    opts.workspace.display()
                ),
            );
        }
        tokio::spawn(watch_stdout(stdout, addr_tx, opts.log_path.clone()));
        let stderr_tail = tokio::spawn(collect_tail(stderr, 16 * 1024, opts.log_path.clone()));

        let mut process = ServerProcess {
            child,
            group,
            stderr_tail,
        };
        let client = match tokio::time::timeout(opts.startup_timeout, addr_rx).await {
            Ok(Ok(addr)) => ServerClient::new(addr, "opencode", &password),
            // The stdout reader is gone: the process exited before it ever listened.
            Ok(Err(_)) => {
                let (status, tail) = teardown(&mut process).await;
                return Err(EngineError::Exited {
                    status: status
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "unknown".into()),
                    stderr: if tail.is_empty() {
                        "no output".into()
                    } else {
                        tail
                    },
                });
            }
            Err(_) => {
                let (_, tail) = teardown(&mut process).await;
                return Err(EngineError::Startup {
                    timeout: opts.startup_timeout,
                    detail: with_stderr("never printed `listening on`", &tail),
                });
            }
        };
        let version = match tokio::time::timeout(opts.startup_timeout, wait_healthy(&client)).await
        {
            Ok(Ok(version)) => version,
            Ok(Err(e)) => {
                let (_, tail) = teardown(&mut process).await;
                return Err(EngineError::Startup {
                    timeout: opts.startup_timeout,
                    detail: with_stderr(&e.to_string(), &tail),
                });
            }
            Err(_) => {
                let (_, tail) = teardown(&mut process).await;
                return Err(EngineError::Startup {
                    timeout: opts.startup_timeout,
                    detail: with_stderr("health check never passed", &tail),
                });
            }
        };
        if version != cli.version {
            tracing::warn!(binary = %cli.version, server = %version, "opencode serve reports a different version than its binary");
        }
        Ok(Self {
            client,
            version,
            workspace: opts.workspace,
            process: Some(process),
            permission_handler: opts.permission_handler,
            idle_timeout: opts.idle_timeout,
            cancel_grace: opts.cancel_grace,
        })
    }

    /// Use an already running `opencode serve` (e.g. one the user started with
    /// `opencode serve`) instead of spawning one. The engine will not stop it.
    pub async fn attach(
        addr: SocketAddr,
        username: &str,
        password: &str,
        opts: EngineOptions,
    ) -> Result<Self, EngineError> {
        let client = ServerClient::new(addr, username, password);
        let version = tokio::time::timeout(opts.startup_timeout, wait_healthy(&client))
            .await
            .map_err(|_| EngineError::Startup {
                timeout: opts.startup_timeout,
                detail: "health check never passed".to_string(),
            })??;
        Ok(Self {
            client,
            version,
            workspace: opts.workspace,
            process: None,
            permission_handler: opts.permission_handler,
            idle_timeout: opts.idle_timeout,
            cancel_grace: opts.cancel_grace,
        })
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn base_url(&self) -> String {
        self.client.base_url()
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// `?directory=<workspace>`: every request names the project explicitly.
    fn dir_query(&self) -> String {
        format!(
            "?directory={}",
            percent_encode(&self.workspace.to_string_lossy())
        )
    }

    fn path(&self, route: &str) -> String {
        format!("{route}{}", self.dir_query())
    }

    /// The free `opencode/*` models OpenCode currently offers, as OpenCode
    /// itself computes them for a keyless client.
    pub async fn free_models(&self) -> Result<FreeCatalog, EngineError> {
        let providers = self
            .client
            .get_json(&self.path("/config/providers"))
            .await?;
        if providers
            .get("providers")
            .and_then(Value::as_array)
            .is_none()
        {
            return Err(EngineError::Shape {
                endpoint: "/config/providers",
                detail: "missing `providers` array".to_string(),
            });
        }
        Ok(parse_free_catalog(&providers, &self.version))
    }

    /// Create a new session; the returned id is the resume handle.
    pub async fn create_session(&self, title: Option<&str>) -> Result<String, EngineError> {
        let body = match title {
            Some(t) => json!({ "title": t }),
            None => json!({}),
        };
        let v = self.client.post_json(&self.path("/session"), &body).await?;
        v.get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| EngineError::Shape {
                endpoint: "/session",
                detail: "missing `id`".to_string(),
            })
    }

    /// Whether a session id from a previous launch is still known to OpenCode.
    pub async fn session_exists(&self, session_id: &str) -> Result<bool, EngineError> {
        match self
            .client
            .get_json(&self.path(&format!("/session/{session_id}")))
            .await
        {
            Ok(v) => Ok(v.get("id").is_some()),
            Err(HttpError::Status { status, .. }) if status.as_u16() == 404 => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Persisted history of a session (for resume display).
    pub async fn session_messages(&self, session_id: &str) -> Result<Value, EngineError> {
        Ok(self
            .client
            .get_json(&self.path(&format!("/session/{session_id}/message")))
            .await?)
    }

    /// Abort the running turn; the session stays usable.
    pub async fn abort(&self, session_id: &str) -> Result<(), EngineError> {
        self.client
            .call(
                Method::POST,
                &self.path(&format!("/session/{session_id}/abort")),
                None,
            )
            .await?;
        Ok(())
    }

    /// Send one prompt and stream the resulting turn.
    pub async fn prompt(
        &self,
        session_id: &str,
        req: TurnRequest,
    ) -> Result<TurnHandle, EngineError> {
        let model = match &req.model {
            Some(m) => Some(parse_model_ref(m)?),
            None => None,
        };
        let agent = match req.permission {
            PermissionPolicy::ReadOnly => "plan",
            PermissionPolicy::WorkspaceWrite => "build",
        };

        // Subscribe before prompting so no event of this turn is missed; the
        // server greets a new subscriber with `server.connected`.
        let (sse_tx, mut sse_rx) = mpsc::channel::<Value>(1024);
        let client = self.client.clone();
        let event_path = self.path("/event");
        let sse_task = tokio::spawn(async move {
            if let Err(e) = client.subscribe_sse(&event_path, sse_tx).await {
                tracing::debug!(error = %e, "event stream ended");
            }
        });
        let greeted = tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(ev) = sse_rx.recv().await {
                if ev.get("type").and_then(Value::as_str) == Some("server.connected") {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap_or(false);
        if !greeted {
            sse_task.abort();
            return Err(EngineError::Shape {
                endpoint: "/event",
                detail: "no `server.connected` greeting".to_string(),
            });
        }

        let mut body = json!({
            "agent": agent,
            "parts": [{ "type": "text", "text": req.text }],
        });
        if let Some((provider_id, model_id)) = model {
            body["model"] = json!({ "providerID": provider_id, "modelID": model_id });
        }
        if let Err(e) = self
            .client
            .post_json(
                &self.path(&format!("/session/{session_id}/prompt_async")),
                &body,
            )
            .await
        {
            sse_task.abort();
            return Err(e.into());
        }

        let (events_tx, events_rx) = mpsc::channel(256);
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let driver = TurnDriver {
            client: self.client.clone(),
            session_id: session_id.to_string(),
            dir_query: self.dir_query(),
            turn: ServeTurn::new(session_id),
            sse_rx,
            sse_task,
            events_tx,
            cancel_rx,
            permission_handler: self.permission_handler.clone(),
            idle_timeout: self.idle_timeout,
            cancel_grace: self.cancel_grace,
        };
        let outcome = tokio::spawn(driver.run());
        Ok(TurnHandle {
            session_id: session_id.to_string(),
            events: events_rx,
            cancel: cancel_tx,
            outcome,
        })
    }

    /// Stop the server this engine started (no-op when attached).
    pub async fn shutdown(mut self) {
        if let Some(mut process) = self.process.take() {
            let _ = teardown(&mut process).await;
        }
    }
}

/// `opencode/<model>` -> (`opencode`, `<model>`); a bare id defaults to `opencode`.
pub fn parse_model_ref(model: &str) -> Result<(String, String), EngineError> {
    match model.split_once('/') {
        Some((provider, id)) if !provider.is_empty() && !id.is_empty() => {
            Ok((provider.to_string(), id.to_string()))
        }
        None if !model.is_empty() => Ok(("opencode".to_string(), model.to_string())),
        _ => Err(EngineError::BadModelRef(model.to_string())),
    }
}

fn with_stderr(detail: &str, tail: &str) -> String {
    if tail.trim().is_empty() {
        detail.to_owned()
    } else {
        format!("{detail}; stderr: {}", tail.trim())
    }
}

async fn pick_free_port() -> std::io::Result<u16> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    Ok(listener.local_addr()?.port())
}

async fn watch_stdout(
    stdout: tokio::process::ChildStdout,
    addr_tx: oneshot::Sender<SocketAddr>,
    log: Option<PathBuf>,
) {
    let mut lines = BufReader::new(stdout).lines();
    let mut addr_tx = Some(addr_tx);
    while let Ok(Some(line)) = lines.next_line().await {
        tracing::debug!(target: "opencode_serve", "{line}");
        if let Some(log) = &log {
            state::append_log(log, &format!("stdout: {line}"));
        }
        if let Some(tx) = addr_tx.take_if(|_| line.contains("listening on"))
            && let Some(addr) = parse_listening_line(&line)
        {
            let _ = tx.send(addr);
        }
    }
}

async fn collect_tail(
    mut stderr: tokio::process::ChildStderr,
    limit: usize,
    log: Option<PathBuf>,
) -> String {
    let mut tail: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 8192];
    loop {
        match stderr.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if let Some(log) = &log {
                    for line in String::from_utf8_lossy(&buf[..n]).lines() {
                        state::append_log(log, &format!("stderr: {line}"));
                    }
                }
                tail.extend_from_slice(&buf[..n]);
                if tail.len() > limit {
                    let cut = tail.len() - limit;
                    tail.drain(..cut);
                }
            }
        }
    }
    String::from_utf8_lossy(&tail).into_owned()
}

/// Poll `/global/health` until it reports healthy; returns the server version.
async fn wait_healthy(client: &ServerClient) -> Result<String, EngineError> {
    loop {
        match client.get_json("/global/health").await {
            Ok(v) if v.get("healthy").and_then(Value::as_bool) == Some(true) => {
                return Ok(v
                    .get("version")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string());
            }
            Ok(_) | Err(HttpError::Connect { .. }) => {
                tokio::time::sleep(Duration::from_millis(150)).await
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// SIGTERM the server's group, wait briefly, then SIGKILL. Returns the
/// stderr tail for diagnostics.
async fn teardown(process: &mut ServerProcess) -> (Option<std::process::ExitStatus>, String) {
    // A server that already died (Gatekeeper SIGKILL, missing shared library, bad binary) is the
    // interesting case: report how it exited before anything else.
    let early_exit = process.child.try_wait().ok().flatten();
    let _ = process.group.terminate();
    if tokio::time::timeout(Duration::from_secs(5), process.child.wait())
        .await
        .is_err()
    {
        let _ = process.group.kill();
        let _ = process.child.start_kill();
        let _ = process.child.wait().await;
    }
    let _ = process.group.kill();
    let tail = match tokio::time::timeout(Duration::from_secs(2), &mut process.stderr_tail).await {
        Ok(Ok(tail)) => tail.trim().to_owned(),
        _ => String::new(),
    };
    (early_exit, tail)
}

/// A live or finished turn. Same contract as [`crate::RunHandle`].
pub struct TurnHandle {
    session_id: String,
    events: mpsc::Receiver<AdapterEvent>,
    cancel: watch::Sender<bool>,
    outcome: tokio::task::JoinHandle<RunOutcome>,
}

impl TurnHandle {
    /// The OpenCode session id — pass it back to [`OpenCodeEngine::prompt`]
    /// to continue the conversation, now or after a restart.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub async fn next_event(&mut self) -> Option<AdapterEvent> {
        self.events.recv().await
    }

    /// Abort the turn on the server. Idempotent; the stream still ends with a
    /// final `Error` and the outcome is [`RunOutcome::Cancelled`].
    pub fn cancel(&self) {
        let _ = self.cancel.send(true);
    }

    pub async fn wait(mut self) -> RunOutcome {
        while self.events.recv().await.is_some() {}
        self.outcome.await.unwrap_or_else(|e| RunOutcome::Failed {
            reason: format!("turn task panicked: {e}"),
            stderr_tail: String::new(),
        })
    }
}

struct TurnDriver {
    client: ServerClient,
    session_id: String,
    dir_query: String,
    turn: ServeTurn,
    sse_rx: mpsc::Receiver<Value>,
    sse_task: tokio::task::JoinHandle<()>,
    events_tx: mpsc::Sender<AdapterEvent>,
    cancel_rx: watch::Receiver<bool>,
    permission_handler: Option<PermissionHandler>,
    idle_timeout: Option<Duration>,
    cancel_grace: Duration,
}

impl TurnDriver {
    fn abort_path(&self) -> String {
        format!("/session/{}/abort{}", self.session_id, self.dir_query)
    }

    fn permission_path(&self, permission_id: &str) -> String {
        format!(
            "/session/{}/permissions/{permission_id}{}",
            self.session_id, self.dir_query
        )
    }

    async fn run(mut self) -> RunOutcome {
        const NEVER: Duration = Duration::from_secs(86_400 * 365);
        let mut cancel_requested = false;
        let mut abort_deadline: Option<tokio::time::Instant> = None;
        let idle = self.idle_timeout.unwrap_or(NEVER);
        let outcome = loop {
            let grace = match abort_deadline {
                Some(deadline) => deadline.saturating_duration_since(tokio::time::Instant::now()),
                None => NEVER,
            };
            tokio::select! {
                biased;
                changed = self.cancel_rx.changed(), if !cancel_requested => {
                    if changed.is_err() || *self.cancel_rx.borrow() {
                        cancel_requested = true;
                        abort_deadline = Some(tokio::time::Instant::now() + self.cancel_grace);
                        if let Err(e) = self.client.call(Method::POST, &self.abort_path(), None).await {
                            tracing::warn!(error = %e, "abort request failed");
                        }
                    }
                }
                ev = self.sse_rx.recv() => match ev {
                    None => {
                        let reason = "opencode event stream closed before the turn finished".to_string();
                        self.emit(AdapterEvent::Error { message: reason.clone() }).await;
                        break RunOutcome::Failed { reason, stderr_tail: String::new() };
                    }
                    Some(ev) => {
                        for out in self.turn.on_event(&ev) {
                            self.emit(out).await;
                        }
                        for perm in self.turn.take_permissions() {
                            self.answer_permission(&perm).await;
                        }
                        if let Some(terminal) = self.turn.terminal().cloned() {
                            break match terminal {
                                _ if cancel_requested || self.turn.aborted() => {
                                    self.emit(AdapterEvent::Error { message: "run cancelled".to_string() }).await;
                                    RunOutcome::Cancelled
                                }
                                Terminal::Completed => RunOutcome::Completed,
                                Terminal::Failed(reason) => RunOutcome::Failed { reason, stderr_tail: String::new() },
                            };
                        }
                    }
                },
                _ = tokio::time::sleep(grace), if abort_deadline.is_some() => {
                    self.emit(AdapterEvent::Error { message: "run cancelled".to_string() }).await;
                    break RunOutcome::Cancelled;
                }
                _ = tokio::time::sleep(idle), if self.idle_timeout.is_some() && !cancel_requested => {
                    let reason = format!("no output for {idle:?}");
                    let _ = self.client.call(Method::POST, &self.abort_path(), None).await;
                    self.emit(AdapterEvent::Error { message: reason.clone() }).await;
                    break RunOutcome::Failed { reason, stderr_tail: String::new() };
                }
            }
        };
        self.sse_task.abort();
        outcome
    }

    async fn emit(&mut self, ev: AdapterEvent) {
        let _ = self.events_tx.send(ev).await;
    }

    async fn answer_permission(&self, perm: &PermissionRequest) {
        let reply = match &self.permission_handler {
            Some(handler) => handler(perm),
            None => PermissionReply::Reject,
        };
        tracing::info!(kind = %perm.kind, title = %perm.title, ?reply, "answering opencode permission request");
        let path = self.permission_path(&perm.id);
        if let Err(e) = self
            .client
            .post_json(&path, &json!({ "response": reply.as_str() }))
            .await
        {
            tracing::warn!(error = %e, "permission reply failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_listening_line() {
        assert_eq!(
            parse_listening_line("opencode server listening on http://127.0.0.1:4096"),
            Some(SocketAddr::from(([127, 0, 0, 1], 4096)))
        );
        assert_eq!(
            parse_listening_line("opencode server listening on http://localhost:5000/"),
            Some(SocketAddr::from(([127, 0, 0, 1], 5000)))
        );
        assert_eq!(parse_listening_line("something else"), None);
    }

    #[test]
    fn model_refs() {
        assert_eq!(
            parse_model_ref("opencode/big-pickle").unwrap(),
            ("opencode".into(), "big-pickle".into())
        );
        assert_eq!(
            parse_model_ref("big-pickle").unwrap(),
            ("opencode".into(), "big-pickle".into())
        );
        assert!(parse_model_ref("").is_err());
        assert!(parse_model_ref("/x").is_err());
    }

    #[test]
    fn encodes_directory_param() {
        assert_eq!(percent_encode("/tmp/a b/c"), "/tmp/a%20b/c");
    }
}
