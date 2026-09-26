//! Spawn a verified vendor CLI, stream its JSONL into [`AdapterEvent`]s, and
//! tear it down on cancel.
//!
//! * The child is the leader of its own process group; cancel sends `SIGINT`
//!   to the group, waits [`SupervisorOptions::cancel_grace`], then `SIGKILL`s
//!   the group so descendants die with it.
//! * stdout lines are bounded ([`SupervisorOptions::max_line_bytes`]); stderr
//!   is kept only as a bounded tail for diagnostics.
//! * The run fails closed: a non-JSON line, an unknown shape, or a stream that
//!   ends without the vendor's terminal event is a failure, never a success.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, watch};

use crate::adapter::{Adapter, AskReply, PinStatus, PromptDelivery, RunRequest, Terminal};
use crate::event::AdapterEvent;
use workshop_detect::Identity;

type ReplyMsg = (AskReply, oneshot::Sender<bool>);

/// Answers asks on a live run's control channel ([`PromptDelivery::Channel`]). Cloneable, so a
/// host can answer from wherever its user's decision lands.
#[derive(Clone)]
pub struct Replier(mpsc::UnboundedSender<ReplyMsg>);

impl Replier {
    /// Send `reply` to the CLI. `false` when the run cannot carry it: the vendor has no control
    /// channel, the ask was not one of this run's, or the run is over — the host then falls
    /// back (a question's answers resume the session as the next prompt).
    pub async fn reply(&self, reply: AskReply) -> bool {
        let (done_tx, done_rx) = oneshot::channel();
        if self.0.send((reply, done_tx)).is_err() {
            return false;
        }
        done_rx.await.unwrap_or(false)
    }
}

#[derive(Clone, Debug)]
pub struct SupervisorOptions {
    /// Child environment; `None` builds the minimal env from this process.
    pub env: Option<BTreeMap<OsString, OsString>>,
    /// Extra non-secret variables (rejected if secret-shaped).
    pub extra_env: Vec<(String, String)>,
    pub max_line_bytes: usize,
    pub stderr_tail_bytes: usize,
    /// Kill the run if stdout is silent this long. `None` disables.
    pub idle_timeout: Option<Duration>,
    /// Time between `SIGINT` and `SIGKILL` on cancel.
    pub cancel_grace: Duration,
    /// After the leader exits, how long to wait for stragglers to close
    /// stdout before the group is killed.
    pub drain_timeout: Duration,
    /// Run CLIs newer than the tested pin. Older-than-supported is always
    /// refused.
    pub allow_untested_versions: bool,
}

impl Default for SupervisorOptions {
    fn default() -> Self {
        Self {
            env: None,
            extra_env: Vec::new(),
            max_line_bytes: 4 * 1024 * 1024,
            stderr_tail_bytes: 16 * 1024,
            idle_timeout: None,
            cancel_grace: Duration::from_secs(5),
            drain_timeout: Duration::from_secs(2),
            allow_untested_versions: true,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("adapter {expected} cannot run a {actual} binary")]
    AdapterMismatch {
        expected: crate::AdapterId,
        actual: crate::AdapterId,
    },
    #[error("{adapter} {version} is older than the supported {min}; update the CLI")]
    UnsupportedVersion {
        adapter: crate::AdapterId,
        version: String,
        min: &'static str,
    },
    #[error(
        "{adapter} {version} is newer than the tested {max} and untested versions are disabled"
    )]
    UntestedVersion {
        adapter: crate::AdapterId,
        version: String,
        max: &'static str,
    },
    #[error(transparent)]
    Env(#[from] crate::env::DeniedEnvVar),
    #[error("failed to spawn `{program}`: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
}

/// How a run ended. Available from [`RunHandle::wait`] once events are drained.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunOutcome {
    Completed,
    Failed {
        reason: String,
        /// Bounded tail of the child's stderr. Diagnostic only; may contain
        /// paths, treat as sensitive when logging.
        stderr_tail: String,
    },
    Cancelled,
}

/// A live or finished delegated run.
pub struct RunHandle {
    events: mpsc::Receiver<AdapterEvent>,
    cancel: watch::Sender<bool>,
    session: watch::Receiver<Option<String>>,
    outcome: tokio::task::JoinHandle<RunOutcome>,
    pid: u32,
    replier: Replier,
}

impl RunHandle {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Answer the run's asks ([`AdapterEvent::PermissionAsk`], [`AdapterEvent::Question`]).
    pub fn replier(&self) -> Replier {
        self.replier.clone()
    }

    /// Next normalized event; `None` once the stream is finished.
    pub async fn next_event(&mut self) -> Option<AdapterEvent> {
        self.events.recv().await
    }

    /// Vendor session id as soon as the stream reveals it.
    pub fn session_id(&self) -> Option<String> {
        self.session.borrow().clone()
    }

    /// Wait until the stream reveals the vendor session id. `None` if the run
    /// ends first.
    pub async fn wait_session_id(&mut self) -> Option<String> {
        match self.session.wait_for(|s| s.is_some()).await {
            Ok(guard) => guard.clone(),
            Err(_) => None,
        }
    }

    /// Request cancellation. Idempotent and non-blocking; the stream still
    /// ends with a final `Error` event and [`RunOutcome::Cancelled`].
    pub fn cancel(&self) {
        let _ = self.cancel.send(true);
    }

    /// Drain remaining events and return how the run ended.
    pub async fn wait(mut self) -> RunOutcome {
        while self.events.recv().await.is_some() {}
        self.outcome.await.unwrap_or_else(|e| RunOutcome::Failed {
            reason: format!("supervisor task panicked: {e}"),
            stderr_tail: String::new(),
        })
    }
}

/// Spawn one whole-task run of `adapter` using the verified `cli` (from [`crate::detect`]).
pub async fn spawn(
    adapter: &dyn Adapter,
    cli: &Identity,
    req: RunRequest,
    opts: &SupervisorOptions,
) -> Result<RunHandle, SpawnError> {
    if cli.vendor != adapter.id() {
        return Err(SpawnError::AdapterMismatch {
            expected: adapter.id(),
            actual: cli.vendor,
        });
    }
    let pin = adapter.version_pin();
    match pin.classify(&cli.version) {
        PinStatus::OlderThanSupported => {
            return Err(SpawnError::UnsupportedVersion {
                adapter: cli.vendor,
                version: cli.version.clone(),
                min: pin.min_supported,
            });
        }
        PinStatus::NewerThanTested if !opts.allow_untested_versions => {
            return Err(SpawnError::UntestedVersion {
                adapter: cli.vendor,
                version: cli.version.clone(),
                max: pin.max_tested,
            });
        }
        _ => {}
    }

    let env = match &opts.env {
        Some(env) => crate::env::minimal_env(env.clone(), &opts.extra_env)?,
        None => crate::env::minimal_env_from_process(&opts.extra_env)?,
    };

    let mut args = adapter.run_args(&req);
    let delivery = adapter.prompt_delivery();
    if delivery == PromptDelivery::Argument {
        args.push(req.prompt.clone());
    }

    let mut cmd = tokio::process::Command::new(&cli.path);
    cmd.args(&args)
        .current_dir(&req.cwd)
        .stdin(match delivery {
            PromptDelivery::Stdin | PromptDelivery::Channel => Stdio::piped(),
            PromptDelivery::Argument => Stdio::null(),
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    crate::env::apply(&mut cmd, &env);

    tracing::info!(
        adapter = %adapter.id(),
        program = %cli.path.display(),
        version = %cli.version,
        args = ?redact_prompt(&args, delivery, &req.prompt),
        cwd = %req.cwd.display(),
        "spawning delegated cli"
    );

    // The child leads its own process group and is enrolled in the global
    // scope so session teardown reaps it; `group` is owned by the driver.
    let (mut child, group) =
        crate::spawn::spawn_enrolled(cmd)
            .await
            .map_err(|source| SpawnError::Spawn {
                program: cli.path.display().to_string(),
                source,
            })?;
    let pid = child.id().unwrap_or(0);

    let mut channel_stdin = None;
    match delivery {
        PromptDelivery::Stdin => {
            if let Some(mut stdin) = child.stdin.take() {
                let prompt = req.prompt.clone();
                tokio::spawn(async move {
                    let _ = stdin.write_all(prompt.as_bytes()).await;
                    let _ = stdin.shutdown().await;
                });
            }
        }
        PromptDelivery::Channel => {
            // The prompt goes first; stdin stays open for the run's asks and closes at the end.
            if let Some(mut stdin) = child.stdin.take() {
                let mut framed = String::new();
                for line in adapter.prompt_lines(&req.prompt) {
                    framed.push_str(&line);
                    framed.push('\n');
                }
                if let Err(e) = stdin.write_all(framed.as_bytes()).await {
                    tracing::warn!(error = %e, "could not write the prompt to the cli");
                }
                channel_stdin = Some(stdin);
            }
        }
        PromptDelivery::Argument => {}
    }

    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let (events_tx, events_rx) = mpsc::channel(256);
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let (session_tx, session_rx) = watch::channel(None);
    let (lines_tx, lines_rx) = mpsc::channel::<Result<String, LineError>>(64);
    let (reply_tx, reply_rx) = mpsc::unbounded_channel::<ReplyMsg>();

    let max_line = opts.max_line_bytes;
    tokio::spawn(read_lines(stdout, max_line, lines_tx));
    let stderr_task = tokio::spawn(collect_tail(stderr, opts.stderr_tail_bytes));

    let normalizer = adapter.normalizer();
    let driver = Driver {
        child,
        group,
        pid,
        normalizer,
        events_tx,
        session_tx,
        cancel_rx,
        lines_rx,
        reply_rx,
        stdin: channel_stdin,
        stderr_task,
        idle_timeout: opts.idle_timeout,
        cancel_grace: opts.cancel_grace,
        drain_timeout: opts.drain_timeout,
    };
    let outcome = tokio::spawn(driver.run());

    Ok(RunHandle {
        events: events_rx,
        cancel: cancel_tx,
        session: session_rx,
        outcome,
        pid,
        replier: Replier(reply_tx),
    })
}

fn redact_prompt(args: &[String], delivery: PromptDelivery, prompt: &str) -> Vec<String> {
    args.iter()
        .map(|a| {
            if delivery == PromptDelivery::Argument && a == prompt {
                "<prompt>".to_string()
            } else {
                a.clone()
            }
        })
        .collect()
}

#[derive(Debug)]
enum LineError {
    TooLong(usize),
    Io(String),
}

/// Read newline-delimited lines with a hard per-line byte bound.
async fn read_lines(
    stdout: tokio::process::ChildStdout,
    max_line: usize,
    tx: mpsc::Sender<Result<String, LineError>>,
) {
    let mut reader = BufReader::with_capacity(64 * 1024, stdout);
    let mut line = Vec::new();
    loop {
        let available = match reader.fill_buf().await {
            Ok(buf) => buf,
            Err(e) => {
                let _ = tx.send(Err(LineError::Io(e.to_string()))).await;
                return;
            }
        };
        if available.is_empty() {
            if !line.is_empty() {
                let text = String::from_utf8_lossy(&line).into_owned();
                let _ = tx.send(Ok(text)).await;
            }
            return;
        }
        let (chunk, found_newline) = match available.iter().position(|b| *b == b'\n') {
            Some(idx) => (&available[..idx], true),
            None => (available, false),
        };
        if line.len() + chunk.len() > max_line {
            let _ = tx.send(Err(LineError::TooLong(max_line))).await;
            return;
        }
        line.extend_from_slice(chunk);
        let consumed = chunk.len() + usize::from(found_newline);
        reader.consume(consumed);
        if found_newline {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let text = String::from_utf8_lossy(&line).into_owned();
            line.clear();
            if !text.trim().is_empty() && tx.send(Ok(text)).await.is_err() {
                return;
            }
        }
    }
}

/// Keep only the last `limit` bytes of stderr.
async fn collect_tail(mut stderr: tokio::process::ChildStderr, limit: usize) -> String {
    let mut tail: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 8192];
    loop {
        match stderr.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
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

struct Driver {
    child: tokio::process::Child,
    /// Keeps the process-group enrollment alive until the child is reaped.
    group: std::sync::Arc<xai_tty_utils::ProcessGroup>,
    pid: u32,
    normalizer: Box<dyn crate::adapter::Normalizer>,
    events_tx: mpsc::Sender<AdapterEvent>,
    session_tx: watch::Sender<Option<String>>,
    cancel_rx: watch::Receiver<bool>,
    lines_rx: mpsc::Receiver<Result<String, LineError>>,
    reply_rx: mpsc::UnboundedReceiver<ReplyMsg>,
    /// The control channel of a [`PromptDelivery::Channel`] run; `None` otherwise, and once
    /// closed after the terminal event.
    stdin: Option<tokio::process::ChildStdin>,
    stderr_task: tokio::task::JoinHandle<String>,
    idle_timeout: Option<Duration>,
    cancel_grace: Duration,
    drain_timeout: Duration,
}

enum Stop {
    Eof,
    Cancelled,
    Fatal(String),
}

impl Driver {
    async fn run(mut self) -> RunOutcome {
        let stop = self.stream().await;

        let exit_code = match stop {
            Stop::Eof => self.child.wait().await.ok().and_then(|s| s.code()),
            Stop::Cancelled | Stop::Fatal(_) => self.terminate().await,
        };
        // Whatever is still alive in the group must not outlive the run.
        self.kill_group();

        let outcome = match stop {
            Stop::Cancelled => {
                self.emit(AdapterEvent::Error {
                    message: "run cancelled".to_string(),
                })
                .await;
                RunOutcome::Cancelled
            }
            Stop::Fatal(reason) => {
                self.emit(AdapterEvent::Error {
                    message: reason.clone(),
                })
                .await;
                RunOutcome::Failed {
                    reason,
                    stderr_tail: self.stderr_tail().await,
                }
            }
            Stop::Eof => {
                for ev in self.normalizer.on_eof(exit_code) {
                    self.emit(ev).await;
                }
                match self.normalizer.terminal().cloned() {
                    Some(Terminal::Completed) => RunOutcome::Completed,
                    Some(Terminal::Failed(reason)) => RunOutcome::Failed {
                        reason,
                        stderr_tail: self.stderr_tail().await,
                    },
                    None => {
                        let reason = "stream ended without a terminal event".to_string();
                        self.emit(AdapterEvent::Error {
                            message: reason.clone(),
                        })
                        .await;
                        RunOutcome::Failed {
                            reason,
                            stderr_tail: self.stderr_tail().await,
                        }
                    }
                }
            }
        };
        tracing::info!(pid = self.pid, ?outcome, "delegated cli finished");
        outcome
    }

    async fn stream(&mut self) -> Stop {
        const NEVER: Duration = Duration::from_secs(86_400 * 365);
        let idle = self.idle_timeout.unwrap_or(NEVER);
        // Once the leader has exited, stragglers get `drain_timeout` to close
        // stdout before the group is killed.
        let mut leader_exited = false;
        loop {
            let drain = if leader_exited {
                self.drain_timeout
            } else {
                NEVER
            };
            tokio::select! {
                biased;
                changed = self.cancel_rx.changed() => {
                    if changed.is_err() || *self.cancel_rx.borrow() {
                        return Stop::Cancelled;
                    }
                }
                line = self.lines_rx.recv() => match line {
                    None => return Stop::Eof,
                    Some(Err(LineError::TooLong(max))) => {
                        return Stop::Fatal(format!("stdout line exceeded {max} bytes"));
                    }
                    Some(Err(LineError::Io(e))) => {
                        return Stop::Fatal(format!("stdout read error: {e}"));
                    }
                    Some(Ok(line)) => match self.normalizer.on_line(&line) {
                        Ok(events) => {
                            if let Some(sid) = self.normalizer.session_id()
                                && self.session_tx.borrow().is_none()
                            {
                                let _ = self.session_tx.send(Some(sid.to_string()));
                            }
                            for ev in events {
                                self.emit(ev).await;
                            }
                            self.flush_stdin().await;
                            // The CLI's turn is over: closing its control channel ends it.
                            if self.normalizer.terminal().is_some() {
                                self.close_stdin().await;
                            }
                        }
                        Err(e) => return Stop::Fatal(e.to_string()),
                    },
                },
                Some((reply, done)) = self.reply_rx.recv() => {
                    let carried = self.stdin.is_some() && self.normalizer.reply(&reply);
                    self.flush_stdin().await;
                    let _ = done.send(carried);
                }
                _ = self.child.wait(), if !leader_exited => {
                    leader_exited = true;
                }
                _ = tokio::time::sleep(drain), if leader_exited => {
                    tracing::warn!(pid = self.pid, "stdout still open after leader exit; killing stragglers");
                    return Stop::Eof;
                }
                _ = tokio::time::sleep(idle), if self.idle_timeout.is_some() => {
                    return Stop::Fatal(format!("no output for {idle:?}"));
                }
            }
        }
    }

    async fn emit(&mut self, ev: AdapterEvent) {
        let _ = self.events_tx.send(ev).await;
    }

    /// Write what the normalizer queued for the CLI's stdin (answers to its asks).
    async fn flush_stdin(&mut self) {
        let lines = self.normalizer.take_stdin_lines();
        if lines.is_empty() {
            return;
        }
        let Some(stdin) = self.stdin.as_mut() else {
            return;
        };
        let mut framed = String::new();
        for line in lines {
            framed.push_str(&line);
            framed.push('\n');
        }
        if let Err(e) = stdin.write_all(framed.as_bytes()).await {
            tracing::warn!(pid = self.pid, error = %e, "could not write to the cli's stdin");
            self.stdin = None;
        }
    }

    async fn close_stdin(&mut self) {
        if let Some(mut stdin) = self.stdin.take() {
            let _ = stdin.shutdown().await;
        }
    }

    async fn stderr_tail(&mut self) -> String {
        match tokio::time::timeout(self.drain_timeout, &mut self.stderr_task).await {
            Ok(Ok(tail)) => tail,
            _ => String::new(),
        }
    }

    /// SIGINT the group, wait for the grace period, then SIGKILL.
    async fn terminate(&mut self) -> Option<i32> {
        self.interrupt_group();
        if let Ok(Ok(status)) = tokio::time::timeout(self.cancel_grace, self.child.wait()).await {
            return status.code();
        }
        tracing::warn!(pid = self.pid, grace = ?self.cancel_grace, "cli ignored SIGINT; killing process group");
        self.kill_group();
        self.child.wait().await.ok().and_then(|s| s.code())
    }

    /// `killpg(SIGKILL)` on Unix, `TerminateJobObject` on Windows.
    fn kill_group(&mut self) {
        let _ = self.group.kill();
        let _ = self.child.start_kill();
    }

    #[cfg(unix)]
    fn interrupt_group(&self) {
        if self.pid <= 1 || self.pid > i32::MAX as u32 {
            return;
        }
        // The child leads its own group (ProcessScope::spawn), so its pid is
        // the pgid and this never reaches our own group.
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(self.pid as i32),
            nix::sys::signal::Signal::SIGINT,
        );
    }

    #[cfg(not(unix))]
    fn interrupt_group(&self) {
        // No SIGINT equivalent for a job object; `terminate` falls through to
        // the kill after the grace period.
    }
}
