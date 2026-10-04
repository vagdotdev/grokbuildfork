//! Workshop: the engine kept warm between sessions.
//!
//! `opencode serve` takes about a second to listen and then tens of seconds of CPU to settle,
//! and until now every `workshop` launch started its own and killed it on exit. The keeper is a
//! small detached process (`workshop __engine-keeper`, its own session) that owns one
//! `opencode serve` per Workshop home and outlives the TUI that started it. A launch attaches to
//! it in a few milliseconds instead of starting a server; the keeper stops the server and exits
//! itself once no Workshop has been attached for [`DEFAULT_KEEP_WARM`] (or
//! `WORKSHOP_OPENCODE_KEEP_WARM_SECS`).
//!
//! Lifecycle, in one place:
//!
//! * **One per home.** The keeper holds an exclusive lock on `engine/keeper.lock` for its whole
//!   life; a second keeper exits at once. A launch takes `engine/attach.lock` while it decides to
//!   attach or spawn, so two Workshops starting together end up on the same server.
//! * **What it is serving is written down.** `engine/serve.json` (owner-only) names the keeper
//!   and server pids, the port, the password and an [`EngineIdentity`]: the Workshop build, the
//!   `opencode` binary (path, version, size, mtime) and a hash of everything the server was
//!   configured with (its environment minus the per-launch bits, the agent prompts, the
//!   permission policy, the identity file's content). A launch whose identity differs — an
//!   update, a re-installed engine, a changed model name in the identity file, a different
//!   environment — stops the old keeper and starts a fresh one. Stale config is never served.
//! * **It knows who is attached.** Every Workshop holds a connection to
//!   `run/engine-keeper.sock` for as long as it runs; the kernel closes it when the process ends,
//!   however it ends. With no connection left the idle clock starts; a new connection stops it.
//!   `quit` on the socket stops the keeper at once.
//! * **It never outlives its server, and (almost) never the other way round.** The server runs in
//!   its own process group; the keeper terminates it on exit (idle, `quit`, SIGTERM) and exits
//!   when the server dies on its own. The one gap is a keeper killed with SIGKILL: its server
//!   lingers until the next launch finds a dead keeper in `serve.json` and ends the orphan.
//!
//! The server is started with exactly the environment and arguments
//! [`OpenCodeEngine::start`](workshop_adapters::opencode_engine::OpenCodeEngine::start) would
//! use ([`serve_env`]), so a turn through the keeper's server is the turn it always was.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use workshop_adapters::opencode_engine::{EngineOptions, SERVE_USER, serve_args, serve_env};
use workshop_detect::Identity;

/// How long the keeper keeps the server after the last Workshop detached.
pub const DEFAULT_KEEP_WARM: Duration = Duration::from_secs(600);
/// Override for the keep-warm time, in seconds (`0`: the server stops with the last Workshop).
pub const KEEP_WARM_ENV: &str = "WORKSHOP_OPENCODE_KEEP_WARM_SECS";
/// Slack a launch gives a keeper it spawned beyond the server's own startup timeout.
const SPAWN_SLACK: Duration = Duration::from_secs(5);

const SUBCOMMAND: &str = "__engine-keeper";
const QUIT: &str = "quit";

pub fn keep_warm() -> Duration {
    keep_warm_from(std::env::var(KEEP_WARM_ENV).ok().as_deref())
}

fn keep_warm_from(raw: Option<&str>) -> Duration {
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_KEEP_WARM)
}

// ── Paths ────────────────────────────────────────────────────────────────────────────────────────

pub fn serve_info_path(home: &Path) -> PathBuf {
    home.join("engine").join("serve.json")
}

fn keeper_lock_path(home: &Path) -> PathBuf {
    home.join("engine").join("keeper.lock")
}

fn attach_lock_path(home: &Path) -> PathBuf {
    home.join("engine").join("attach.lock")
}

pub fn socket_path(home: &Path) -> PathBuf {
    home.join("run").join("engine-keeper.sock")
}

/// `engine/keeper-failed.json`: why a keeper gave up before it had a server (the launch that
/// spawned it reports this cause; `workshop doctor` keeps it in `state.json`).
fn failure_path(home: &Path) -> PathBuf {
    home.join("engine").join("keeper-failed.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeeperFailure {
    keeper_pid: u32,
    reason: String,
}

impl KeeperFailure {
    fn write(home: &Path, reason: &str) {
        let f = KeeperFailure {
            keeper_pid: std::process::id(),
            reason: reason.to_owned(),
        };
        if let Ok(json) = serde_json::to_vec_pretty(&f) {
            let _ = workshop_providers::atomic_write_private(&failure_path(home), &json);
        }
    }

    fn take(home: &Path, keeper_pid: u32) -> Option<String> {
        let raw = std::fs::read_to_string(failure_path(home)).ok()?;
        let _ = std::fs::remove_file(failure_path(home));
        let f: KeeperFailure = serde_json::from_str(&raw).ok()?;
        (f.keeper_pid == keeper_pid).then_some(f.reason)
    }
}

// ── What is served ───────────────────────────────────────────────────────────────────────────────

/// Everything that must match for a launch to use a running server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineIdentity {
    /// `xai_grok_version::full_version()` of the Workshop that started it.
    pub workshop_version: String,
    /// Modification time of that Workshop binary (dev builds share a version string).
    pub workshop_exe_mtime_ns: u128,
    pub opencode: PathBuf,
    pub opencode_version: String,
    pub opencode_len: u64,
    pub opencode_mtime_ns: u128,
    /// Hash of the server's configuration: its environment without the per-launch entries (the
    /// password, the askpass socket), plus the identity file's content.
    pub config_hash: u64,
}

/// Environment entries that differ per launch without changing what the server does: the
/// per-launch secrets Workshop sets, and the shell's and terminal's bookkeeping (the directory
/// the user launched from, the window, the multiplexer pane, the terminal size). Everything else
/// (`PATH`, `HOME`, `DISPLAY`, proxies, locale, …) is what the engine's commands run with, so a
/// change there is a different configuration.
const PER_LAUNCH_ENV: &[&str] = &[
    "OPENCODE_SERVER_PASSWORD",
    crate::app::workshop_askpass::SOCKET_ENV,
    "PWD",
    "OLDPWD",
    "SHLVL",
    "_",
    "COLUMNS",
    "LINES",
    "WINDOWID",
    "TERM_SESSION_ID",
    "ITERM_SESSION_ID",
    "TERM_PROGRAM_VERSION",
    "TMUX",
    "TMUX_PANE",
    "STY",
    "WINDOW",
    "KITTY_WINDOW_ID",
    "KITTY_PID",
    "KITTY_PUBLIC_KEY",
    "WEZTERM_PANE",
    "WEZTERM_UNIX_SOCKET",
    "WT_SESSION",
    "SSH_TTY",
    "SSH_CONNECTION",
    "SSH_CLIENT",
    "GPG_TTY",
];

fn mtime_ns(path: &Path) -> u128 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

impl EngineIdentity {
    /// The identity a server started now, by this Workshop, for `cli`, with `env` and the
    /// identity file saying `instructions`, would have.
    pub fn compute(cli: &Identity, env: &BTreeMap<OsString, OsString>, instructions: &str) -> Self {
        let mut hasher = std::hash::DefaultHasher::new();
        for (k, v) in env {
            if PER_LAUNCH_ENV
                .iter()
                .any(|skip| k.as_os_str() == OsStr::new(skip))
            {
                continue;
            }
            k.hash(&mut hasher);
            v.hash(&mut hasher);
        }
        instructions.hash(&mut hasher);
        let exe = std::env::current_exe().unwrap_or_default();
        Self {
            workshop_version: xai_grok_version::full_version().to_owned(),
            workshop_exe_mtime_ns: mtime_ns(&exe),
            opencode: cli.path.clone(),
            opencode_version: cli.version.clone(),
            opencode_len: std::fs::metadata(&cli.path).map(|m| m.len()).unwrap_or(0),
            opencode_mtime_ns: mtime_ns(&cli.path),
            config_hash: hasher.finish(),
        }
    }
}

/// `engine/serve.json`: the keeper's report of what it is serving.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServeInfo {
    pub keeper_pid: u32,
    pub serve_pid: u32,
    pub port: u16,
    pub password: String,
    pub identity: EngineIdentity,
    pub started_unix: u64,
    pub keep_warm_secs: u64,
}

impl ServeInfo {
    pub fn load(home: &Path) -> Option<Self> {
        let raw = std::fs::read_to_string(serve_info_path(home)).ok()?;
        serde_json::from_str(&raw).ok()
    }

    fn save(&self, home: &Path) -> std::io::Result<()> {
        let json = serde_json::to_vec_pretty(self)?;
        workshop_providers::atomic_write_private(&serve_info_path(home), &json)
            .map_err(|e| std::io::Error::other(e.to_string()))
    }

    pub fn addr(&self) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], self.port))
    }
}

/// What the keeper is told to run. Travels over its stdin, never through a file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeeperSpec {
    pub home: PathBuf,
    pub log: PathBuf,
    pub opencode: PathBuf,
    pub cwd: PathBuf,
    /// The complete environment for `opencode serve` ([`serve_env`]), password included.
    pub env: Vec<(String, String)>,
    pub password: String,
    pub identity: EngineIdentity,
    pub keep_warm_secs: u64,
    /// How long the server may take to listen before the keeper gives up (the launch's
    /// `EngineOptions::startup_timeout`).
    pub startup_timeout_secs: u64,
}

// ── Deciding ─────────────────────────────────────────────────────────────────────────────────────

/// What a launch does with what it finds in `serve.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// A live keeper serving exactly this identity.
    Attach(ServeInfo),
    /// Nothing usable: start a keeper (after ending whatever `end` names).
    Spawn { end: Vec<Ending>, why: &'static str },
}

/// A process a launch ends before spawning a fresh keeper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ending {
    /// The keeper, told to quit (then signalled if it does not).
    Keeper(u32),
    /// An orphaned server whose keeper is gone.
    Serve(u32),
}

/// Pure: the decision for `info` given what this launch wants and which pids are alive.
pub fn decide(
    info: Option<ServeInfo>,
    want: &EngineIdentity,
    alive: impl Fn(u32) -> bool,
) -> Decision {
    let Some(info) = info else {
        return Decision::Spawn {
            end: vec![],
            why: "no keeper",
        };
    };
    let keeper_alive = alive(info.keeper_pid);
    let serve_alive = alive(info.serve_pid);
    if !keeper_alive {
        return Decision::Spawn {
            end: if serve_alive {
                vec![Ending::Serve(info.serve_pid)]
            } else {
                vec![]
            },
            why: "keeper gone",
        };
    }
    if !serve_alive {
        return Decision::Spawn {
            end: vec![Ending::Keeper(info.keeper_pid)],
            why: "server gone",
        };
    }
    if info.identity != *want {
        return Decision::Spawn {
            end: vec![Ending::Keeper(info.keeper_pid)],
            why: "different build, engine or configuration",
        };
    }
    Decision::Attach(info)
}

// ── Process helpers (unix) ───────────────────────────────────────────────────────────────────────

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: kill with signal 0 only checks for the process's existence.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    false
}

/// Whether `pid` still runs an `opencode` (a pid can be reused; never signal a stranger).
#[cfg(target_os = "linux")]
fn pid_is_opencode(pid: u32) -> bool {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|c| String::from_utf8_lossy(&c).contains("opencode"))
        .unwrap_or(false)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn pid_is_opencode(pid: u32) -> bool {
    std::process::Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("opencode"))
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn pid_is_opencode(_pid: u32) -> bool {
    false
}

#[cfg(unix)]
fn signal(pid: u32, sig: libc::c_int) {
    // SAFETY: a plain kill(2) on a pid this module recorded itself.
    unsafe {
        libc::kill(pid as libc::pid_t, sig);
    }
}

#[cfg(unix)]
fn signal_group(pid: u32, sig: libc::c_int) {
    // SAFETY: kill(2) on the negated pid addresses the process group the server leads.
    unsafe {
        libc::kill(-(pid as libc::pid_t), sig);
    }
}

async fn wait_gone(pid: u32, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if !pid_alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    !pid_alive(pid)
}

/// An exclusive advisory lock on `path` (created if needed), released on drop.
struct FileLock(std::fs::File);

impl FileLock {
    fn try_exclusive(path: &Path) -> std::io::Result<Option<Self>> {
        let file = Self::open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self(file))),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        }
    }

    fn exclusive(path: &Path) -> std::io::Result<Self> {
        let file = Self::open(path)?;
        file.lock()?;
        Ok(Self(file))
    }

    fn open(path: &Path) -> std::io::Result<std::fs::File> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        opts.open(path)
    }
}

// ── The launch side ──────────────────────────────────────────────────────────────────────────────

/// A connection to the keeper's socket, held for the life of the process: the keeper counts it.
pub struct Lease(#[allow(dead_code)] tokio::net::UnixStream);

/// What a launch ends up with: a server to attach to, and the lease that keeps it.
pub struct Warm {
    pub addr: SocketAddr,
    pub password: String,
    pub lease: Lease,
    /// Whether this launch found a running keeper (true) or had to start one (false).
    pub attached: bool,
}

/// Attach to the home's warm server, or start a keeper and attach to its server. `opts` is what
/// [`OpenCodeEngine::start`](workshop_adapters::opencode_engine::OpenCodeEngine::start) would
/// have been given; `instructions` is the identity file's content; `log` receives one line per
/// step (the engine log).
pub async fn acquire(
    home: &Path,
    cli: &Identity,
    opts: &EngineOptions,
    instructions: &str,
    log: &(dyn Fn(&str) + Sync),
) -> Result<Warm, String> {
    let (env, password) = serve_env(opts).map_err(|e| format!("engine environment: {e}"))?;
    let want = EngineIdentity::compute(cli, &env, instructions);
    let lock_path = attach_lock_path(home);
    let _attach = tokio::task::spawn_blocking(move || FileLock::exclusive(&lock_path))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("attach lock: {e}"))?;

    let decision = decide(ServeInfo::load(home), &want, pid_alive);
    match decision {
        Decision::Attach(info) => {
            log(&format!(
                "keeper: attaching to the warm engine (keeper {}, server {}, port {})",
                info.keeper_pid, info.serve_pid, info.port
            ));
            match lease(home).await {
                Ok(lease) => {
                    return Ok(Warm {
                        addr: info.addr(),
                        password: info.password,
                        lease,
                        attached: true,
                    });
                }
                Err(e) => {
                    log(&format!(
                        "keeper: the warm engine's keeper did not answer ({e}); replacing it"
                    ));
                    end(&[Ending::Keeper(info.keeper_pid)], home, log).await;
                }
            }
        }
        Decision::Spawn { end: ends, why } => {
            if !ends.is_empty() || why != "no keeper" {
                log(&format!("keeper: {why}; starting a fresh engine"));
            }
            end(&ends, home, log).await;
        }
    }
    let _ = std::fs::remove_file(serve_info_path(home));

    let spec = KeeperSpec {
        home: home.to_path_buf(),
        log: crate::app::workshop_engine_state::log_path(home),
        opencode: cli.path.clone(),
        cwd: opts.workspace.clone(),
        env: env
            .iter()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
            .collect(),
        password: password.clone(),
        identity: want.clone(),
        keep_warm_secs: keep_warm().as_secs(),
        startup_timeout_secs: opts.startup_timeout.as_secs().max(1),
    };
    let _ = std::fs::remove_file(failure_path(home));
    let keeper_pid = spawn_keeper(&spec).await?;
    log(&format!(
        "keeper: started (pid {keeper_pid}), waiting for its server"
    ));
    let deadline = Instant::now() + opts.startup_timeout + SPAWN_SLACK;
    loop {
        if let Some(info) = ServeInfo::load(home)
            && info.keeper_pid == keeper_pid
        {
            let lease = lease(home)
                .await
                .map_err(|e| format!("keeper socket: {e}"))?;
            return Ok(Warm {
                addr: info.addr(),
                password: info.password,
                lease,
                attached: false,
            });
        }
        if !pid_alive(keeper_pid) {
            // The keeper says why it gave up (the server exited during startup, never listened):
            // the same causes `OpenCodeEngine::start` reported when the server was a child.
            return Err(KeeperFailure::take(home, keeper_pid).unwrap_or_else(|| {
                format!(
                    "the engine keeper exited before its server was up; log: {}",
                    spec.log.display()
                )
            }));
        }
        if Instant::now() > deadline {
            signal(keeper_pid, libc::SIGTERM);
            return Err(format!(
                "`opencode serve` did not become ready within {:?}: the keeper reported no server; log: {}",
                opts.startup_timeout,
                spec.log.display()
            ));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn lease(home: &Path) -> std::io::Result<Lease> {
    let stream = tokio::time::timeout(
        Duration::from_secs(3),
        tokio::net::UnixStream::connect(socket_path(home)),
    )
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timed out"))??;
    Ok(Lease(stream))
}

/// End the processes a decision named: a keeper is asked to quit over its socket and signalled
/// if it stays; an orphaned server is signalled directly, but only while it still is `opencode`.
async fn end(ends: &[Ending], home: &Path, log: &(dyn Fn(&str) + Sync)) {
    for ending in ends {
        match *ending {
            Ending::Keeper(pid) => {
                if let Ok(Ok(mut stream)) = tokio::time::timeout(
                    Duration::from_secs(2),
                    tokio::net::UnixStream::connect(socket_path(home)),
                )
                .await
                {
                    use tokio::io::AsyncWriteExt;
                    let _ = stream.write_all(format!("{QUIT}\n").as_bytes()).await;
                }
                if !wait_gone(pid, Duration::from_secs(3)).await {
                    signal(pid, libc::SIGTERM);
                    if !wait_gone(pid, Duration::from_secs(3)).await {
                        signal(pid, libc::SIGKILL);
                    }
                }
                log(&format!("keeper: previous keeper {pid} ended"));
            }
            Ending::Serve(pid) => {
                if pid_is_opencode(pid) {
                    signal_group(pid, libc::SIGTERM);
                    signal(pid, libc::SIGTERM);
                    if !wait_gone(pid, Duration::from_secs(3)).await {
                        signal_group(pid, libc::SIGKILL);
                        signal(pid, libc::SIGKILL);
                    }
                    log(&format!("keeper: orphaned server {pid} ended"));
                }
            }
        }
    }
    // Whatever was there, its socket is stale now.
    let _ = std::fs::remove_file(socket_path(home));
}

/// Start `workshop __engine-keeper` in its own session, hand it the spec on stdin, and reap it
/// whenever it exits (it is this process's child until then; it keeps running after this process
/// is gone).
async fn spawn_keeper(spec: &KeeperSpec) -> Result<u32, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&spec.log)
        .map_err(|e| format!("engine log {}: {e}", spec.log.display()))?;
    let log_err = log
        .try_clone()
        .map_err(|e| format!("engine log {}: {e}", spec.log.display()))?;
    let mut cmd = tokio::process::Command::new(exe);
    cmd.arg(SUBCOMMAND)
        .stdin(std::process::Stdio::piped())
        .stdout(log)
        .stderr(log_err)
        .kill_on_drop(false);
    // Its own session: no controlling terminal, no SIGHUP when the window closes, not in the
    // TUI's process group (which is killed on exit).
    #[cfg(unix)]
    {
        // SAFETY: setsid is async-signal-safe and only detaches the child from our session.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    // The one child meant to outlive this session: enrolling it in the process scope would kill
    // it with the TUI, which is the opposite of its job. It is reaped below and its own lifetime
    // is bounded by the keep-warm clock.
    #[allow(clippy::disallowed_methods)]
    let mut child = cmd.spawn().map_err(|e| format!("spawn keeper: {e}"))?;
    let pid = child.id().ok_or("keeper has no pid")?;
    let json = serde_json::to_vec(spec).map_err(|e| e.to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        stdin
            .write_all(&json)
            .await
            .map_err(|e| format!("keeper stdin: {e}"))?;
        stdin.shutdown().await.ok();
    }
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(pid)
}

// ── The keeper process ───────────────────────────────────────────────────────────────────────────

/// `workshop __engine-keeper`: the detached process. Returns the exit code when this process is
/// the keeper, `None` otherwise.
pub fn maybe_run_helper() -> Option<i32> {
    let args: Vec<OsString> = std::env::args_os().collect();
    if args.get(1).map(OsString::as_os_str) != Some(OsStr::new(SUBCOMMAND)) {
        return None;
    }
    let mut raw = Vec::new();
    if std::io::stdin().read_to_end(&mut raw).is_err() {
        return Some(2);
    }
    let spec: KeeperSpec = match serde_json::from_slice(&raw) {
        Ok(spec) => spec,
        Err(e) => {
            eprintln!("engine keeper: bad spec: {e}");
            return Some(2);
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("engine keeper: runtime: {e}");
            return Some(2);
        }
    };
    Some(runtime.block_on(keeper_main(spec)))
}

fn stamp(log: &Path, line: &str) {
    crate::app::workshop_engine_state::append_log(log, &format!("keeper: {line}"));
}

#[cfg(unix)]
async fn keeper_main(spec: KeeperSpec) -> i32 {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    let home = spec.home.clone();
    let log = spec.log.clone();
    let _lock = match FileLock::try_exclusive(&keeper_lock_path(&home)) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            stamp(&log, "another keeper holds the lock; exiting");
            return 3;
        }
        Err(e) => {
            stamp(&log, &format!("cannot take the keeper lock: {e}"));
            return 2;
        }
    };
    // SAFETY: ignoring SIGHUP so a closing terminal that still delivers it does not end us.
    unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }

    let port = match workshop_adapters::opencode_engine::free_loopback_port().await {
        Ok(p) => p,
        Err(e) => {
            stamp(&log, &format!("no free port: {e}"));
            return 2;
        }
    };
    let mut cmd = tokio::process::Command::new(&spec.opencode);
    cmd.args(serve_args(port))
        .current_dir(&spec.cwd)
        .env_clear()
        .envs(spec.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    // The keeper is the server's process scope: it ends the server's group on every exit path
    // below and exits when the server dies, so the TUI's scope (which this process is not part
    // of) is not what should own it.
    #[allow(clippy::disallowed_methods)]
    let spawned = cmd.spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            stamp(&log, &format!("cannot start `opencode serve`: {e}"));
            return 2;
        }
    };
    let serve_pid = child.id().unwrap_or(0);
    stamp(
        &log,
        &format!(
            "start: {} serve --hostname 127.0.0.1 --port {port} (pid {serve_pid}, workspace {}, keep warm {} s)",
            spec.opencode.display(),
            spec.cwd.display(),
            spec.keep_warm_secs
        ),
    );
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let (log_tx, mut log_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let tx = log_tx.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = tx.send(format!("stdout: {line}"));
        }
    });
    let tx = log_tx.clone();
    tokio::spawn(async move {
        let mut buf = String::new();
        let mut reader = BufReader::new(stderr);
        while let Ok(n) = reader.read_line(&mut buf).await {
            if n == 0 {
                break;
            }
            let _ = tx.send(format!("stderr: {}", buf.trim_end()));
            buf.clear();
        }
    });
    drop(log_tx);

    // Wait for the server to listen (its stdout says so), logging everything it prints. A server
    // that exits first, or never listens, is reported the way `OpenCodeEngine::start` reported it
    // when the server was the TUI's own child, so the launch shows the same plain line and
    // `workshop doctor` sees the same cause.
    let startup_timeout = Duration::from_secs(spec.startup_timeout_secs);
    let mut stderr_tail: Vec<String> = Vec::new();
    let listen_deadline = tokio::time::sleep(startup_timeout);
    tokio::pin!(listen_deadline);
    let outcome: Result<(), String> = loop {
        tokio::select! {
            line = log_rx.recv() => {
                let Some(line) = line else {
                    // Both pipes hit EOF: the server is exiting (or has). Report its status.
                    let tail = stderr_tail.join(" ").trim().to_owned();
                    let tail = if tail.is_empty() { "no output".to_owned() } else { tail };
                    break Err(
                        match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
                            Ok(Ok(status)) => format!(
                                "`opencode serve` exited during startup ({status}): {tail}"
                            ),
                            Ok(Err(e)) => format!(
                                "`opencode serve` exited during startup ({e}): {tail}"
                            ),
                            Err(_) => format!(
                                "`opencode serve` closed its output before it listened: {tail}"
                            ),
                        },
                    );
                };
                crate::app::workshop_engine_state::append_log(&log, &line);
                if let Some(rest) = line.strip_prefix("stderr: ") {
                    stderr_tail.push(rest.to_owned());
                    if stderr_tail.len() > 20 {
                        stderr_tail.remove(0);
                    }
                }
                if line.contains("listening on") {
                    break Ok(());
                }
            }
            status = child.wait() => {
                // Drain what the server printed on its way out (the readers close the channel
                // once its pipes hit EOF), then report it.
                while let Ok(Some(line)) =
                    tokio::time::timeout(Duration::from_millis(500), log_rx.recv()).await
                {
                    crate::app::workshop_engine_state::append_log(&log, &line);
                    if let Some(rest) = line.strip_prefix("stderr: ") {
                        stderr_tail.push(rest.to_owned());
                    }
                }
                let status = match status {
                    Ok(s) => s.to_string(),
                    Err(e) => e.to_string(),
                };
                let tail = stderr_tail.join(" ").trim().to_owned();
                break Err(format!(
                    "`opencode serve` exited during startup ({status}): {}",
                    if tail.is_empty() { "no output".to_owned() } else { tail }
                ));
            }
            _ = &mut listen_deadline => {
                break Err(format!(
                    "`opencode serve` did not become ready within {startup_timeout:?}: never printed `listening on`"
                ));
            }
        }
    };
    if let Err(reason) = outcome {
        stamp(&log, &format!("{reason}; stopping"));
        KeeperFailure::write(&home, &reason);
        stop_server(&mut child, serve_pid).await;
        return 2;
    }

    // The socket the attached Workshops hold, then the report every launch reads.
    let sock = socket_path(&home);
    if let Some(dir) = sock.parent() {
        let _ = std::fs::create_dir_all(dir);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    let _ = std::fs::remove_file(&sock);
    let listener = match tokio::net::UnixListener::bind(&sock) {
        Ok(l) => l,
        Err(e) => {
            stamp(
                &log,
                &format!("cannot bind {}: {e}; stopping", sock.display()),
            );
            stop_server(&mut child, serve_pid).await;
            return 2;
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600));
    }
    let info = ServeInfo {
        keeper_pid: std::process::id(),
        serve_pid,
        port,
        password: spec.password.clone(),
        identity: spec.identity.clone(),
        started_unix: crate::app::workshop_engine_state::now_unix(),
        keep_warm_secs: spec.keep_warm_secs,
    };
    if let Err(e) = info.save(&home) {
        stamp(&log, &format!("cannot write serve.json: {e}; stopping"));
        stop_server(&mut child, serve_pid).await;
        let _ = std::fs::remove_file(&sock);
        return 2;
    }
    stamp(
        &log,
        &format!("serving on port {port}; waiting for Workshops"),
    );

    let keep_warm = Duration::from_secs(spec.keep_warm_secs);
    let (quit_tx, mut quit_rx) = tokio::sync::mpsc::unbounded_channel::<&'static str>();
    let (detach_tx, mut detach_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let mut attached: usize = 0;
    // The idle clock runs only after the first Workshop has attached and the last has gone; the
    // launch that started this keeper is about to connect, and a keep-warm of 0 must not stop the
    // server before it does. A keeper nobody attaches to at all stops after the startup timeout.
    let mut ever_attached = false;
    let mut idle_since: Option<Instant> = None;
    let first_attach_deadline = Instant::now() + startup_timeout + Duration::from_secs(10);
    let mut sigterm =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).ok();
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    let why: &str = loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                attached += 1;
                ever_attached = true;
                idle_since = None;
                let quit_tx = quit_tx.clone();
                let detach_tx = detach_tx.clone();
                tokio::spawn(async move {
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    loop {
                        line.clear();
                        match reader.read_line(&mut line).await {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {
                                if line.trim() == QUIT {
                                    let _ = quit_tx.send("asked to quit");
                                }
                            }
                        }
                    }
                    let _ = detach_tx.send(());
                });
            }
            Some(()) = detach_rx.recv() => {
                attached = attached.saturating_sub(1);
                if attached == 0 {
                    idle_since = Some(Instant::now());
                }
            }
            Some(why) = quit_rx.recv() => break why,
            status = child.wait() => {
                stamp(&log, &format!("the server exited on its own ({})", match status { Ok(s) => s.to_string(), Err(e) => e.to_string() }));
                break "server exited";
            }
            _ = async { match sigterm.as_mut() { Some(s) => { s.recv().await; } None => std::future::pending::<()>().await } } => break "SIGTERM",
            _ = async { match sigint.as_mut() { Some(s) => { s.recv().await; } None => std::future::pending::<()>().await } } => break "SIGINT",
            _ = tick.tick() => {
                if attached == 0 {
                    if ever_attached {
                        if idle_since.is_some_and(|since| since.elapsed() >= keep_warm) {
                            break "idle";
                        }
                    } else if Instant::now() > first_attach_deadline {
                        break "no Workshop attached";
                    }
                }
                // Another keeper's report must never be removed by us: only act on our own.
                if ServeInfo::load(&home).is_none_or(|i| i.keeper_pid != std::process::id()) {
                    break "serve.json replaced";
                }
            }
        }
    };
    log_rx.close();
    stamp(&log, &format!("stopping ({why})"));
    stop_server(&mut child, serve_pid).await;
    if ServeInfo::load(&home).is_some_and(|i| i.keeper_pid == std::process::id()) {
        let _ = std::fs::remove_file(serve_info_path(&home));
    }
    let _ = std::fs::remove_file(&sock);
    stamp(&log, "stopped");
    0
}

#[cfg(not(unix))]
async fn keeper_main(_spec: KeeperSpec) -> i32 {
    2
}

/// SIGTERM the server's group, wait, then SIGKILL.
#[cfg(unix)]
async fn stop_server(child: &mut tokio::process::Child, serve_pid: u32) {
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    signal_group(serve_pid, libc::SIGTERM);
    if tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .is_err()
    {
        signal_group(serve_pid, libc::SIGKILL);
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
}

/// The user name of the warm server's basic auth.
pub const USER: &str = SERVE_USER;

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(tag: &str) -> EngineIdentity {
        EngineIdentity {
            workshop_version: format!("0.2.6 ({tag})"),
            workshop_exe_mtime_ns: 1,
            opencode: PathBuf::from("/x/opencode"),
            opencode_version: "1.18.31".into(),
            opencode_len: 10,
            opencode_mtime_ns: 2,
            config_hash: 3,
        }
    }

    fn info(keeper: u32, serve: u32, id: EngineIdentity) -> ServeInfo {
        ServeInfo {
            keeper_pid: keeper,
            serve_pid: serve,
            port: 4242,
            password: "pw".into(),
            identity: id,
            started_unix: 0,
            keep_warm_secs: 600,
        }
    }

    #[test]
    fn a_live_keeper_with_the_same_identity_is_attached_to() {
        let want = identity("a");
        let d = decide(Some(info(10, 11, want.clone())), &want, |_| true);
        assert_eq!(d, Decision::Attach(info(10, 11, want)));
    }

    #[test]
    fn no_report_means_spawn_with_nothing_to_end() {
        assert_eq!(
            decide(None, &identity("a"), |_| true),
            Decision::Spawn {
                end: vec![],
                why: "no keeper"
            }
        );
    }

    #[test]
    fn a_dead_keeper_with_a_live_server_ends_the_orphan() {
        let want = identity("a");
        let d = decide(Some(info(10, 11, want.clone())), &want, |pid| pid == 11);
        assert_eq!(
            d,
            Decision::Spawn {
                end: vec![Ending::Serve(11)],
                why: "keeper gone"
            }
        );
        let d = decide(Some(info(10, 11, want.clone())), &want, |_| false);
        assert!(matches!(d, Decision::Spawn { end, .. } if end.is_empty()));
    }

    #[test]
    fn a_keeper_whose_server_died_is_replaced() {
        let want = identity("a");
        let d = decide(Some(info(10, 11, want.clone())), &want, |pid| pid == 10);
        assert_eq!(
            d,
            Decision::Spawn {
                end: vec![Ending::Keeper(10)],
                why: "server gone"
            }
        );
    }

    #[test]
    fn a_different_build_engine_or_config_replaces_the_keeper() {
        let served = identity("a");
        for want in [
            EngineIdentity {
                workshop_version: "0.2.7 (b)".into(),
                ..served.clone()
            },
            EngineIdentity {
                opencode_version: "1.19.0".into(),
                ..served.clone()
            },
            EngineIdentity {
                opencode_mtime_ns: 99,
                ..served.clone()
            },
            EngineIdentity {
                config_hash: 4,
                ..served.clone()
            },
        ] {
            let d = decide(Some(info(10, 11, served.clone())), &want, |_| true);
            assert_eq!(
                d,
                Decision::Spawn {
                    end: vec![Ending::Keeper(10)],
                    why: "different build, engine or configuration"
                },
                "{want:?}"
            );
        }
    }

    #[test]
    fn the_identity_ignores_the_per_launch_environment_and_sees_the_rest() {
        let cli = Identity {
            vendor: workshop_detect::Vendor::OpenCode,
            path: std::env::current_exe().unwrap(),
            version: "1.18.31".into(),
        };
        let mut env = BTreeMap::new();
        env.insert(OsString::from("PATH"), OsString::from("/usr/bin"));
        env.insert(
            OsString::from("OPENCODE_SERVER_PASSWORD"),
            OsString::from("one"),
        );
        env.insert(
            OsString::from(crate::app::workshop_askpass::SOCKET_ENV),
            OsString::from("/run/askpass-1.sock"),
        );
        let a = EngineIdentity::compute(&cli, &env, "hello");
        env.insert(
            OsString::from("OPENCODE_SERVER_PASSWORD"),
            OsString::from("two"),
        );
        env.insert(
            OsString::from(crate::app::workshop_askpass::SOCKET_ENV),
            OsString::from("/run/askpass-2.sock"),
        );
        assert_eq!(
            EngineIdentity::compute(&cli, &env, "hello"),
            a,
            "the password and the askpass socket change per launch"
        );
        env.insert(OsString::from("PATH"), OsString::from("/opt/bin"));
        assert_ne!(EngineIdentity::compute(&cli, &env, "hello"), a);
        env.insert(OsString::from("PATH"), OsString::from("/usr/bin"));
        assert_ne!(
            EngineIdentity::compute(&cli, &env, "hello, Big Pickle"),
            a,
            "a changed identity file is a different configuration"
        );
    }

    #[test]
    fn serve_info_round_trips_owner_only() {
        let tmp = tempfile::tempdir().unwrap();
        let i = info(1, 2, identity("a"));
        i.save(tmp.path()).unwrap();
        assert_eq!(ServeInfo::load(tmp.path()), Some(i));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(serve_info_path(tmp.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "serve.json is owner-only");
        }
    }

    #[test]
    fn keep_warm_reads_the_override() {
        assert_eq!(keep_warm_from(None), DEFAULT_KEEP_WARM);
        assert_eq!(keep_warm_from(Some("0")), Duration::ZERO);
        assert_eq!(keep_warm_from(Some(" 42 ")), Duration::from_secs(42));
        assert_eq!(keep_warm_from(Some("soon")), DEFAULT_KEEP_WARM);
    }
}
