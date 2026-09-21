//! Process supervisor: spawn a verified vendor CLI, stream normalized events, enforce bounds.
//!
//! Containment rules:
//! * the child is its own process group; cancel and timeouts kill the **group** (SIGTERM, grace,
//!   SIGKILL) so tool subprocesses do not outlive the run;
//! * stdin is closed; stdout is read line-by-line with a hard per-line byte bound; stderr is kept
//!   as a bounded tail;
//! * the environment is the allowlisted minimum plus documented, non-secret vendor settings;
//! * success is only the vendor's own terminal event with a zero exit; anything else fails closed.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use workshop_detect::{DetectConfig, Identity, Vendor, probe_vendor};
use xai_tty_utils::{ProcessGroup, ProcessScope};

use crate::command::{SupportMatrix, build_argv, describe, vendor_env};
use crate::error::{AdapterError, FailureReason};
use crate::event::{AdapterEvent, RunOutcome, RunStatus, Usage};
use crate::normalize::{self, LineResult, Normalizer};
use crate::request::RunRequest;

#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    /// How binaries are located and verified. Login is never checked here; the CLI's own error
    /// stream reports a signed-out state.
    pub detect: DetectConfig,
    pub support: SupportMatrix,
    /// Number of stderr lines kept for diagnostics.
    pub stderr_tail: usize,
    /// Time between SIGTERM and SIGKILL when stopping the process group.
    pub kill_grace: Duration,
    /// Per-line cap for stderr (longer lines are truncated, not fatal).
    pub max_stderr_line_bytes: usize,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            detect: DetectConfig::default(),
            support: SupportMatrix::default(),
            stderr_tail: 50,
            kill_grace: Duration::from_secs(2),
            max_stderr_line_bytes: 64 * 1024,
        }
    }
}

/// Owns adapter children through a [`ProcessScope`], so a host session can reap every run it
/// started even if the supervising task wedges.
#[derive(Clone)]
pub struct Supervisor {
    cfg: SupervisorConfig,
    scope: ProcessScope,
}

impl Default for Supervisor {
    fn default() -> Self {
        Self::new(SupervisorConfig::default())
    }
}

impl std::fmt::Debug for Supervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Supervisor").field("cfg", &self.cfg).finish_non_exhaustive()
    }
}

/// A running adapter: consume `events`, call [`RunHandle::cancel`] to stop, await
/// [`RunHandle::wait`] for the outcome.
pub struct RunHandle {
    pub events: mpsc::Receiver<AdapterEvent>,
    cancel: CancellationToken,
    outcome: tokio::task::JoinHandle<RunOutcome>,
}

impl RunHandle {
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    pub async fn wait(self) -> RunOutcome {
        match self.outcome.await {
            Ok(outcome) => outcome,
            Err(join) => RunOutcome {
                vendor: Vendor::Claude,
                status: RunStatus::Failed {
                    reason: FailureReason::SpawnFailed {
                        detail: format!("supervisor task failed: {join}"),
                    },
                },
                session_id: None,
                final_text: None,
                exit_code: None,
                usage: Usage::default(),
                events_seen: 0,
                unknown_events: 0,
                stderr_tail: Vec::new(),
            },
        }
    }
}

enum OutLine {
    Line(String),
    TooLong,
    Eof,
}

enum ErrLine {
    Line(String),
    Eof,
}

/// Read one `\n`-terminated line with a hard byte cap.
async fn read_line_bounded<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
    buf: &mut Vec<u8>,
    max: usize,
) -> std::io::Result<OutLine> {
    buf.clear();
    let mut limited = (&mut *reader).take(max as u64 + 1);
    let n = limited.read_until(b'\n', buf).await?;
    if n == 0 {
        return Ok(OutLine::Eof);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        return Ok(OutLine::Line(String::from_utf8_lossy(buf).into_owned()));
    }
    if buf.len() > max {
        return Ok(OutLine::TooLong);
    }
    // Final line without a trailing newline.
    Ok(OutLine::Line(String::from_utf8_lossy(buf).into_owned()))
}

struct Bookkeeping {
    vendor: Vendor,
    session_id: Option<String>,
    final_text: Option<String>,
    usage: Usage,
    events_seen: usize,
    unknown_events: usize,
    vendor_failure: Option<FailureReason>,
    stderr_tail: VecDeque<String>,
    stderr_cap: usize,
}

impl Bookkeeping {
    fn note(&mut self, event: &AdapterEvent) {
        self.events_seen += 1;
        match event {
            AdapterEvent::Session { id } => self.session_id = Some(id.clone()),
            AdapterEvent::Text { text } => self.final_text = Some(text.clone()),
            AdapterEvent::Usage(u) => self.usage = *u,
            AdapterEvent::Unknown { .. } => self.unknown_events += 1,
            AdapterEvent::Completed { final_text, session_id } => {
                if final_text.is_some() {
                    self.final_text = final_text.clone();
                }
                if session_id.is_some() {
                    self.session_id = session_id.clone();
                }
            }
            AdapterEvent::Failed { reason } => {
                if self.vendor_failure.is_none() {
                    self.vendor_failure = Some(reason.clone());
                }
            }
            AdapterEvent::Stderr { line } => {
                if self.stderr_tail.len() == self.stderr_cap {
                    self.stderr_tail.pop_front();
                }
                self.stderr_tail.push_back(line.clone());
            }
            _ => {}
        }
    }

    fn outcome(self, status: RunStatus, exit_code: Option<i32>) -> RunOutcome {
        RunOutcome {
            vendor: self.vendor,
            status,
            session_id: self.session_id,
            final_text: self.final_text,
            exit_code,
            usage: self.usage,
            events_seen: self.events_seen,
            unknown_events: self.unknown_events,
            stderr_tail: self.stderr_tail.into_iter().collect(),
        }
    }
}

async fn emit(sink: &mpsc::Sender<AdapterEvent>, book: &mut Bookkeeping, event: AdapterEvent) {
    book.note(&event);
    // A closed receiver means the caller stopped listening; the run still finishes.
    let _ = sink.send(event).await;
}

/// Stop the child and everything in its process group / job: terminate, wait `grace`, then kill.
async fn kill_group(child: &mut Child, group: Option<&Arc<ProcessGroup>>, grace: Duration) -> Option<i32> {
    if let Some(group) = group {
        let _ = group.terminate();
        if let Ok(Ok(status)) = tokio::time::timeout(grace, child.wait()).await {
            return status.code();
        }
        let _ = group.kill();
    }
    let _ = child.start_kill();
    match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(Ok(status)) => status.code(),
        _ => None,
    }
}

impl Supervisor {
    /// A supervisor with its own private [`ProcessScope`].
    pub fn new(cfg: SupervisorConfig) -> Self {
        Self::with_scope(cfg, ProcessScope::new())
    }

    /// A supervisor whose children are enrolled in the host's scope (for example the session's
    /// scope, so `kill_all` on session teardown also reaps adapter runs).
    pub fn with_scope(cfg: SupervisorConfig, scope: ProcessScope) -> Self {
        Self { cfg, scope }
    }

    pub fn scope(&self) -> &ProcessScope {
        &self.scope
    }

    pub fn config(&self) -> &SupervisorConfig {
        &self.cfg
    }

    /// Validate the request and start it on the current Tokio runtime.
    pub fn start(&self, req: RunRequest) -> Result<RunHandle, AdapterError> {
        if req.prompt.trim().is_empty() {
            return Err(AdapterError::EmptyPrompt);
        }
        if !req.workdir.path().is_dir() {
            return Err(AdapterError::MissingWorkdir(req.workdir.path().to_path_buf()));
        }
        // Reject credential-like extras before anything is spawned.
        workshop_detect::env::minimal_env(&req.extra_env)?;

        let (tx, rx) = mpsc::channel(256);
        let cancel = CancellationToken::new();
        let sup = self.clone();
        let token = cancel.clone();
        let outcome = tokio::spawn(async move { sup.run(req, tx, token).await });
        Ok(RunHandle {
            events: rx,
            cancel,
            outcome,
        })
    }

    /// Locate and verify the vendor binary for `vendor` without checking login.
    pub async fn resolve_binary(&self, vendor: Vendor) -> Result<Identity, FailureReason> {
        let mut detect = self.cfg.detect.clone();
        detect.check_login = false;
        let probe = tokio::task::spawn_blocking(move || probe_vendor(vendor, &detect))
            .await
            .map_err(|e| FailureReason::SpawnFailed {
                detail: format!("probe task failed: {e}"),
            })?;
        let identity = probe.binary.ok_or(FailureReason::NotInstalled { vendor })?;
        self.cfg.support.check(&identity)?;
        Ok(identity)
    }

    /// Run to completion, streaming events into `sink`. Always returns an outcome.
    pub async fn run(
        &self,
        req: RunRequest,
        sink: mpsc::Sender<AdapterEvent>,
        cancel: CancellationToken,
    ) -> RunOutcome {
        let mut book = Bookkeeping {
            vendor: req.vendor,
            session_id: None,
            final_text: None,
            usage: Usage::default(),
            events_seen: 0,
            unknown_events: 0,
            vendor_failure: None,
            stderr_tail: VecDeque::new(),
            stderr_cap: self.cfg.stderr_tail,
        };

        let identity = match self.resolve_binary(req.vendor).await {
            Ok(id) => id,
            Err(reason) => {
                emit(&sink, &mut book, AdapterEvent::Failed { reason: reason.clone() }).await;
                return book.outcome(RunStatus::Failed { reason }, None);
            }
        };

        let mut env: Vec<(OsString, OsString)> = match workshop_detect::env::minimal_env(&req.extra_env) {
            Ok(env) => env,
            Err(e) => {
                let reason = FailureReason::SpawnFailed { detail: e.to_string() };
                emit(&sink, &mut book, AdapterEvent::Failed { reason: reason.clone() }).await;
                return book.outcome(RunStatus::Failed { reason }, None);
            }
        };
        env.extend(vendor_env(req.vendor, req.permissions));
        let args = build_argv(&req, &identity);
        tracing::info!(target: "workshop_adapters", command = %describe(&identity.path, &args), "spawning adapter");

        let mut cmd = Command::new(&identity.path);
        cmd.args(&args)
            .env_clear()
            .envs(env.iter().map(|(k, v)| (k.as_os_str(), v.as_os_str())))
            .current_dir(req.workdir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // New process group (Unix) / job object (Windows) so cancel reaches every descendant.
        self.scope.prepare(&mut cmd);

        #[allow(
            clippy::disallowed_methods,
            reason = "enrolled in the supervisor's ProcessScope immediately below; the group is torn down on cancel/timeout and reaped on exit"
        )]
        let spawned = cmd.spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(e) => {
                let reason = FailureReason::SpawnFailed { detail: e.to_string() };
                emit(&sink, &mut book, AdapterEvent::Failed { reason: reason.clone() }).await;
                return book.outcome(RunStatus::Failed { reason }, None);
            }
        };
        let pid = child.id();
        // Keep the Arc alive for the whole run: the scope only holds a Weak.
        let group: Option<Arc<ProcessGroup>> = match self.scope.enroll(&child) {
            Ok(group) => Some(group),
            Err(e) => {
                tracing::warn!(target: "workshop_adapters", error = %e, "could not enroll adapter child; falling back to direct kill");
                None
            }
        };
        emit(&sink, &mut book, AdapterEvent::Started { vendor: req.vendor, pid }).await;

        // Reader tasks keep the select loop cancel-safe.
        let (out_tx, mut out_rx) = mpsc::channel::<OutLine>(64);
        let (err_tx, mut err_rx) = mpsc::channel::<ErrLine>(64);
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");
        let max_line = req.max_line_bytes;
        let max_err = self.cfg.max_stderr_line_bytes;
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut buf = Vec::new();
            loop {
                match read_line_bounded(&mut reader, &mut buf, max_line).await {
                    Ok(OutLine::Line(line)) => {
                        if out_tx.send(OutLine::Line(line)).await.is_err() {
                            break;
                        }
                    }
                    Ok(OutLine::TooLong) => {
                        let _ = out_tx.send(OutLine::TooLong).await;
                        break;
                    }
                    Ok(OutLine::Eof) | Err(_) => {
                        let _ = out_tx.send(OutLine::Eof).await;
                        break;
                    }
                }
            }
        });
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut buf = Vec::new();
            loop {
                match read_line_bounded(&mut reader, &mut buf, max_err).await {
                    Ok(OutLine::Line(line)) => {
                        if err_tx.send(ErrLine::Line(line)).await.is_err() {
                            break;
                        }
                    }
                    Ok(OutLine::TooLong) => {
                        // Truncate rather than fail: stderr is diagnostics, not the protocol.
                        let shown: String = String::from_utf8_lossy(&buf).chars().take(1024).collect();
                        if err_tx.send(ErrLine::Line(format!("{shown}… [truncated]"))).await.is_err() {
                            break;
                        }
                        // Skip the rest of the oversized line.
                        let mut rest = Vec::new();
                        if reader.read_until(b'\n', &mut rest).await.is_err() {
                            break;
                        }
                    }
                    Ok(OutLine::Eof) | Err(_) => {
                        let _ = err_tx.send(ErrLine::Eof).await;
                        break;
                    }
                }
            }
        });

        let mut normalizer: Box<dyn Normalizer> = normalize::for_vendor(req.vendor);
        let started = Instant::now();
        let deadline = tokio::time::sleep(req.timeout);
        tokio::pin!(deadline);
        let idle = tokio::time::sleep(req.idle_timeout);
        tokio::pin!(idle);
        let mut stdout_open = true;
        let mut stderr_open = true;
        let mut fatal: Option<RunStatus> = None;

        while stdout_open || stderr_open {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    fatal = Some(RunStatus::Cancelled);
                    break;
                }
                _ = &mut deadline => {
                    fatal = Some(RunStatus::Failed { reason: FailureReason::Timeout { secs: req.timeout.as_secs() } });
                    break;
                }
                _ = &mut idle => {
                    fatal = Some(RunStatus::Failed { reason: FailureReason::IdleTimeout { secs: req.idle_timeout.as_secs() } });
                    break;
                }
                item = out_rx.recv(), if stdout_open => {
                    idle.as_mut().reset(tokio::time::Instant::now() + req.idle_timeout);
                    match item {
                        Some(OutLine::Line(line)) => match normalizer.line(&line) {
                            LineResult::Events(events) => {
                                for event in events {
                                    emit(&sink, &mut book, event).await;
                                }
                            }
                            LineResult::NotJson(sample) => {
                                fatal = Some(RunStatus::Failed { reason: FailureReason::SchemaDrift {
                                    detail: format!("non-JSON stdout line: {sample}"),
                                } });
                                break;
                            }
                            LineResult::Empty => {}
                        },
                        Some(OutLine::TooLong) => {
                            fatal = Some(RunStatus::Failed { reason: FailureReason::OutputBound { max_line_bytes: req.max_line_bytes } });
                            break;
                        }
                        Some(OutLine::Eof) | None => stdout_open = false,
                    }
                }
                item = err_rx.recv(), if stderr_open => {
                    match item {
                        Some(ErrLine::Line(line)) => {
                            idle.as_mut().reset(tokio::time::Instant::now() + req.idle_timeout);
                            emit(&sink, &mut book, AdapterEvent::Stderr { line }).await;
                        }
                        Some(ErrLine::Eof) | None => stderr_open = false,
                    }
                }
            }
        }

        let exit_code = if let Some(status) = fatal {
            let code = kill_group(&mut child, group.as_ref(), self.cfg.kill_grace).await;
            let event = match &status {
                RunStatus::Cancelled => AdapterEvent::Cancelled,
                RunStatus::Failed { reason } => AdapterEvent::Failed { reason: reason.clone() },
                RunStatus::Completed => unreachable!("fatal path never completes"),
            };
            emit(&sink, &mut book, event).await;
            tracing::info!(target: "workshop_adapters", elapsed = ?started.elapsed(), ?status, "adapter stopped");
            return book.outcome(status, code);
        } else {
            match child.wait().await {
                Ok(status) => status.code(),
                Err(_) => None,
            }
        };

        let status = if let Some(reason) = book.vendor_failure.clone() {
            RunStatus::Failed { reason }
        } else if normalizer.completed() && exit_code == Some(0) {
            RunStatus::Completed
        } else if normalizer.completed() {
            RunStatus::Failed {
                reason: FailureReason::NonZeroExit { code: exit_code },
            }
        } else if exit_code == Some(0) {
            RunStatus::Failed {
                reason: FailureReason::SchemaDrift {
                    detail: format!(
                        "exited 0 without the {} terminal event ({} events seen)",
                        req.vendor.display_name(),
                        book.events_seen
                    ),
                },
            }
        } else {
            RunStatus::Failed {
                reason: FailureReason::NonZeroExit { code: exit_code },
            }
        };
        if let RunStatus::Failed { reason } = &status {
            emit(&sink, &mut book, AdapterEvent::Failed { reason: reason.clone() }).await;
        }
        tracing::info!(target: "workshop_adapters", elapsed = ?started.elapsed(), ?status, "adapter finished");
        book.outcome(status, exit_code)
    }
}
