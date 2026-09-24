//! Bounded, timed execution of a vendor CLI probe command.
//!
//! Every child this crate starts for a vendor goes through that vendor's [`VendorSlot`]: at most
//! one is alive at a time, and a child that outlives its budget keeps the slot until it has
//! actually exited. Claude children are never killed early (see [`teardown`]).

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

use wait_timeout::ChildExt;

use crate::model::Vendor;

/// Maximum bytes kept from each of stdout and stderr. Probe output is tiny; anything larger is
/// truncated rather than buffered without bound.
pub const MAX_CAPTURE_BYTES: usize = 64 * 1024;

/// Longest stdout line a [`Session`] keeps. Claude's `initialize` answer is one line that also
/// lists the user's skills and agents; longer lines are dropped, not buffered without bound.
pub const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

/// How long a [`Session`] child gets to exit by itself after its stdin closes, before a vendor
/// whose kill grace is zero is killed.
pub const SESSION_EXIT_GRACE: Duration = Duration::from_secs(2);

struct Gate {
    busy: Mutex<bool>,
    freed: Condvar,
}

static GATES: [Gate; 4] = [const {
    Gate {
        busy: Mutex::new(false),
        freed: Condvar::new(),
    }
}; 4];

fn gate(vendor: Vendor) -> &'static Gate {
    &GATES[match vendor {
        Vendor::Claude => 0,
        Vendor::Codex => 1,
        Vendor::Cursor => 2,
        Vendor::OpenCode => 3,
    }]
}

/// The right to have one child of `vendor` alive, process-wide.
///
/// Two `claude` children against one credential store can both try to rotate the single-use OAuth
/// refresh token, and the loser signs the user out (Traycer's "2026-08-14 sign-out root cause",
/// `src/domain/providers/auth-probes/claude.ts`, MIT). The same one-at-a-time rule covers every
/// vendor's probes. Dropping the slot releases it.
#[derive(Debug)]
pub struct VendorSlot {
    vendor: Vendor,
}

impl VendorSlot {
    /// Wait until no other child of `vendor` is alive, for at most `wait`. `None` when the wait ran
    /// out first: the caller has given up on its answer, so it must not spawn a child nobody is
    /// waiting for (Traycer's staleness check, same file).
    pub fn acquire(vendor: Vendor, wait: Duration) -> Option<VendorSlot> {
        let g = gate(vendor);
        let deadline = Instant::now() + wait;
        let mut busy = g.busy.lock().unwrap_or_else(|e| e.into_inner());
        while *busy {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            busy = g
                .freed
                .wait_timeout(busy, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        *busy = true;
        Some(VendorSlot { vendor })
    }
}

impl Drop for VendorSlot {
    fn drop(&mut self) {
        let g = gate(self.vendor);
        *g.busy.lock().unwrap_or_else(|e| e.into_inner()) = false;
        g.freed.notify_all();
    }
}

/// Stop waiting on `child` and make sure it ends, keeping `slot` until it has.
///
/// With a zero `grace` the process group is killed now. Otherwise a background thread gives the
/// child `grace` to exit by itself and only then kills the group: a `claude` child can be between
/// "refresh token consumed" and "new pair persisted", and a kill there loses the credential
/// (Traycer, `src/domain/providers/ephemeral-probe.ts`, `OAUTH_REFRESH_SAFE_TEARDOWN_GRACE_MS`).
/// A healthy child exits long before, so the grace is only ever paid by a wedged one.
fn teardown(mut child: Child, slot: Option<VendorSlot>, grace: Duration) {
    if grace.is_zero() {
        kill_group(&mut child);
        let _ = child.wait();
        drop(slot);
        return;
    }
    std::thread::spawn(move || {
        let _slot = slot;
        if !matches!(child.wait_timeout(grace), Ok(Some(_))) {
            kill_group(&mut child);
            let _ = child.wait();
        }
    });
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildOutput {
    /// Exit code; `None` when killed by a signal or by the timeout.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl ChildOutput {
    pub fn success(&self) -> bool {
        self.code == Some(0) && !self.timed_out
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("failed to spawn {0}: {1}")]
    Spawn(String, #[source] std::io::Error),
    #[error("failed while waiting for {0}: {1}")]
    Wait(String, #[source] std::io::Error),
    #[error("another {0} probe is still running")]
    Busy(&'static str),
}

fn drain(mut reader: impl Read + Send + 'static) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if buf.len() < MAX_CAPTURE_BYTES {
                        let take = n.min(MAX_CAPTURE_BYTES - buf.len());
                        buf.extend_from_slice(&chunk[..take]);
                    }
                }
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    })
}

fn command(bin: &Path, args: &[&str], cwd: Option<&Path>, env: &[(OsString, OsString)]) -> Command {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k.as_os_str(), v.as_os_str())))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd
}

/// Attempts before an `ETXTBSY` is reported as the spawn error it is.
const ETXTBSY_ATTEMPTS: u32 = 8;
/// Backoff between attempts, growing linearly (25, 50, … ms).
const ETXTBSY_BACKOFF: Duration = Duration::from_millis(25);

/// `execve` fails with `ETXTBSY` ("Text file busy") while any process holds the program open for
/// writing. A binary that was just written — a vendor CLI the installer put in place, a test's
/// fake — is exactly that for a moment; the window is microseconds to a few milliseconds, so a
/// short retry closes it. Nothing else is retried.
fn spawn(mut cmd: Command, label: &str) -> Result<Child, RunError> {
    let mut attempt = 0u32;
    loop {
        #[allow(
            clippy::disallowed_methods,
            reason = "probe child is its own process group, waited on with a hard timeout, and torn down by `teardown` on expiry"
        )]
        match cmd.spawn() {
            Ok(child) => return Ok(child),
            Err(e)
                if e.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && attempt + 1 < ETXTBSY_ATTEMPTS =>
            {
                attempt += 1;
                std::thread::sleep(ETXTBSY_BACKOFF * attempt);
            }
            Err(e) => return Err(RunError::Spawn(label.to_owned(), e)),
        }
    }
}

/// Run `bin args…` with a cleared environment replaced by `env`, no stdin, and a hard timeout.
///
/// On Unix the child is placed in its own process group so a timeout kills any grandchildren too.
pub fn run(
    bin: &Path,
    args: &[&str],
    cwd: Option<&Path>,
    env: &[(OsString, OsString)],
    timeout: Duration,
) -> Result<ChildOutput, RunError> {
    run_inner(bin, args, cwd, env, timeout, None, Duration::ZERO)
}

/// [`run`] for a vendor CLI: waits (within `timeout`) for the vendor's [`VendorSlot`], and a
/// child that outlives `timeout` is handed to [`teardown`] with `kill_grace` instead of being
/// killed on the spot. The caller gets `timed_out` back at the deadline either way.
pub fn run_vendor(
    vendor: Vendor,
    bin: &Path,
    args: &[&str],
    cwd: Option<&Path>,
    env: &[(OsString, OsString)],
    timeout: Duration,
    kill_grace: Duration,
) -> Result<ChildOutput, RunError> {
    let slot = VendorSlot::acquire(vendor, timeout).ok_or(RunError::Busy(vendor.id()))?;
    run_inner(bin, args, cwd, env, timeout, Some(slot), kill_grace)
}

fn run_inner(
    bin: &Path,
    args: &[&str],
    cwd: Option<&Path>,
    env: &[(OsString, OsString)],
    timeout: Duration,
    slot: Option<VendorSlot>,
    kill_grace: Duration,
) -> Result<ChildOutput, RunError> {
    let label = format!("{} {}", bin.display(), args.join(" "));
    let mut cmd = command(bin, args, cwd, env);
    cmd.stdin(Stdio::null());
    let mut child = spawn(cmd, &label)?;
    let stdout = drain(child.stdout.take().expect("stdout piped"));
    let stderr = drain(child.stderr.take().expect("stderr piped"));

    match child.wait_timeout(timeout) {
        Ok(Some(status)) => Ok(ChildOutput {
            code: status.code(),
            stdout: stdout.join().unwrap_or_default(),
            stderr: stderr.join().unwrap_or_default(),
            timed_out: false,
        }),
        // The drains finish when the child's pipes close; a child still in its grace period
        // keeps them open, so its partial output is not waited for.
        Ok(None) => {
            teardown(child, slot, kill_grace);
            Ok(ChildOutput {
                code: None,
                stdout: String::new(),
                stderr: String::new(),
                timed_out: true,
            })
        }
        Err(e) => {
            teardown(child, slot, kill_grace);
            Err(RunError::Wait(label, e))
        }
    }
}

/// What [`Session::next_line`] saw before its deadline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    Line(String),
    /// stdout closed: the child exited or closed it.
    Eof,
    TimedOut,
}

/// A vendor CLI spoken to in JSON lines over stdin/stdout (Claude's stream-json control
/// protocol, Codex's app-server), holding the vendor's [`VendorSlot`] for its whole life.
///
/// Dropping the session closes stdin, which ends both CLIs cleanly, and hands the child to
/// [`teardown`] with at least [`SESSION_EXIT_GRACE`]; the slot is released once it has exited.
pub struct Session {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
    slot: Option<VendorSlot>,
    kill_grace: Duration,
}

impl Session {
    /// Wait (within `slot_wait`) for the vendor's slot, then start `bin args…` with piped stdin.
    pub fn start(
        vendor: Vendor,
        bin: &Path,
        args: &[&str],
        cwd: Option<&Path>,
        env: &[(OsString, OsString)],
        slot_wait: Duration,
        kill_grace: Duration,
    ) -> Result<Session, RunError> {
        let slot = VendorSlot::acquire(vendor, slot_wait).ok_or(RunError::Busy(vendor.id()))?;
        let label = format!("{} {}", bin.display(), args.join(" "));
        let mut cmd = command(bin, args, cwd, env);
        cmd.stdin(Stdio::piped());
        let mut child = spawn(cmd, &label)?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("stdout piped");
        // stderr is only drained so the child never blocks on a full pipe.
        let _ = drain(child.stderr.take().expect("stderr piped"));
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || read_lines(stdout, tx));
        Ok(Session {
            child: Some(child),
            stdin,
            lines,
            slot: Some(slot),
            kill_grace: kill_grace.max(SESSION_EXIT_GRACE),
        })
    }

    /// Write one line (a newline is appended) and flush.
    pub fn send(&mut self, line: &str) -> std::io::Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::BrokenPipe))?;
        stdin.write_all(line.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()
    }

    /// The next stdout line, waiting until `deadline` at most.
    pub fn next_line(&self, deadline: Instant) -> Next {
        let left = deadline.saturating_duration_since(Instant::now());
        match self.lines.recv_timeout(left) {
            Ok(line) => Next::Line(line),
            Err(mpsc::RecvTimeoutError::Timeout) => Next::TimedOut,
            Err(mpsc::RecvTimeoutError::Disconnected) => Next::Eof,
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        drop(self.stdin.take());
        if let Some(child) = self.child.take() {
            teardown(child, self.slot.take(), self.kill_grace);
        }
    }
}

fn read_lines(stdout: impl Read, tx: mpsc::Sender<String>) {
    let mut reader = BufReader::new(stdout);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let mut oversized = false;
        let mut eof = false;
        loop {
            let limit = (MAX_LINE_BYTES + 1).saturating_sub(buf.len()) as u64;
            match reader.by_ref().take(limit).read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => {
                    eof = true;
                    break;
                }
                Ok(_) if buf.ends_with(b"\n") => break,
                Ok(_) if buf.len() > MAX_LINE_BYTES => {
                    // Past the cap without a newline: drop the line, keep reading to its end.
                    oversized = true;
                    buf.clear();
                }
                Ok(_) => {}
            }
        }
        if !oversized && !buf.is_empty() {
            let line = String::from_utf8_lossy(&buf).trim_end().to_owned();
            if tx.send(line).is_err() {
                return;
            }
        }
        if eof {
            return;
        }
    }
}

/// How an interactive vendor login, run attached to the user's terminal, ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractiveExit {
    /// Exit status 0.
    Success,
    /// Ended by the terminal's Ctrl+C: the caller saw the SIGINT the terminal sent the foreground
    /// process group, or the child was killed by SIGINT / exited 130 (a wrapper's rendering of
    /// it). `codex login` exits 0 on Ctrl+C, so the exit status alone cannot tell.
    Interrupted,
    /// Any other non-zero exit, or the command could not be started.
    Failed,
}

/// Run `argv` attached to the caller's terminal (inherited stdin/stdout, `stderr` when given) and
/// wait for it.
///
/// The child shares the terminal's foreground process group, so a Ctrl+C typed while it runs is
/// delivered to the caller as well. For the duration of the run the caller only records SIGINT
/// instead of acting on it, and the child resets it to the default before `exec`, so the keystroke
/// ends the login and only the login; the caller's previous disposition is restored before
/// returning.
pub fn run_interactive(argv: &[String], stderr: Option<Stdio>) -> InteractiveExit {
    let Some((program, args)) = argv.split_first() else {
        return InteractiveExit::Failed;
    };
    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(stderr) = stderr {
        cmd.stderr(stderr);
    }
    #[cfg(unix)]
    let shield = SigintShield::install(&mut cmd);
    let exit = match cmd.status() {
        Ok(status) => classify_interactive(status),
        Err(_) => return InteractiveExit::Failed,
    };
    #[cfg(unix)]
    if shield.interrupted() {
        return InteractiveExit::Interrupted;
    }
    exit
}

fn classify_interactive(status: std::process::ExitStatus) -> InteractiveExit {
    if status.success() {
        return InteractiveExit::Success;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if status.signal() == Some(libc::SIGINT) {
            return InteractiveExit::Interrupted;
        }
    }
    if status.code() == Some(130) {
        return InteractiveExit::Interrupted;
    }
    InteractiveExit::Failed
}

#[cfg(unix)]
static SIGINT_SEEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn note_sigint(_signal: libc::c_int) {
    SIGINT_SEEN.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Turns this process's SIGINT into a recorded flag while alive; restores the previous
/// disposition on drop.
#[cfg(unix)]
struct SigintShield(Option<libc::sigaction>);

#[cfg(unix)]
impl SigintShield {
    fn install(cmd: &mut Command) -> Self {
        use std::os::unix::process::CommandExt;
        // SAFETY: `signal` is async-signal-safe; the hook runs between fork and exec and does not
        // allocate (pre_exec contract).
        unsafe {
            cmd.pre_exec(|| {
                libc::signal(libc::SIGINT, libc::SIG_DFL);
                Ok(())
            });
        }
        SIGINT_SEEN.store(false, std::sync::atomic::Ordering::SeqCst);
        // SAFETY: plain sigaction calls on zeroed structs; the handler only stores an atomic;
        // `prev` is only read when the call reported success.
        let prev = unsafe {
            let mut record: libc::sigaction = std::mem::zeroed();
            record.sa_sigaction = note_sigint as extern "C" fn(libc::c_int) as usize;
            record.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut record.sa_mask);
            let mut prev: libc::sigaction = std::mem::zeroed();
            (libc::sigaction(libc::SIGINT, &record, &mut prev) == 0).then_some(prev)
        };
        Self(prev)
    }

    fn interrupted(&self) -> bool {
        SIGINT_SEEN.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(unix)]
impl Drop for SigintShield {
    fn drop(&mut self) {
        if let Some(prev) = self.0.take() {
            // SAFETY: restores the disposition captured by `install`.
            unsafe { libc::sigaction(libc::SIGINT, &prev, std::ptr::null_mut()) };
        }
    }
}

fn kill_group(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // The child is its own group leader (process_group(0)), so its pid is the pgid.
        // SAFETY: killpg with a pid we spawned and still own; failure is harmless (ESRCH).
        unsafe {
            libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

/// Strip ANSI escape sequences (CSI and simple two-byte escapes) from CLI output.
pub fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                // CSI: parameter bytes 0x30–0x3F, intermediates 0x20–0x2F, final 0x40–0x7E.
                for next in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                // OSC … BEL or ESC \
                chars.next();
                let mut prev = '\0';
                for next in chars.by_ref() {
                    if next == '\u{7}' || (prev == '\u{1b}' && next == '\\') {
                        break;
                    }
                    prev = next;
                }
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A script still open for writing cannot be exec'd (`ETXTBSY`); the probe waits it out, so a
    /// binary an installer just finished writing (or a fixture another test thread is still
    /// closing) probes fine.
    #[cfg(unix)]
    #[test]
    fn a_briefly_busy_executable_is_retried() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("busy.sh");
        let mut writer = std::fs::File::create(&script).unwrap();
        writer.write_all(b"#!/bin/sh\necho ok\n").unwrap();
        writer.flush().unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            drop(writer);
        });
        let out = run(&script, &[], None, &[], Duration::from_secs(10)).expect("waits out ETXTBSY");
        assert_eq!(out.stdout.trim(), "ok");
        release.join().unwrap();
        // Any other spawn error is reported at once.
        let err = run(
            &dir.path().join("missing"),
            &[],
            None,
            &[],
            Duration::from_secs(1),
        )
        .expect_err("missing program fails");
        assert!(matches!(err, RunError::Spawn(..)), "{err}");
    }

    #[test]
    fn strips_csi_and_osc() {
        assert_eq!(
            strip_ansi("\u{1b}[91m\u{1b}[1mError: \u{1b}[0mSession"),
            "Error: Session"
        );
        assert_eq!(
            strip_ansi("\u{1b}[0m\n┌  Credentials \u{1b}[90m~/x"),
            "\n┌  Credentials ~/x"
        );
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi("\u{1b}]0;title\u{7}rest"), "rest");
    }

    #[cfg(unix)]
    #[test]
    fn timeout_kills_child_and_reports() {
        let out = run(
            Path::new("/bin/sh"),
            &["-c", "sleep 30"],
            None,
            &[],
            Duration::from_millis(200),
        )
        .unwrap();
        assert!(out.timed_out);
        assert_eq!(out.code, None);
    }

    #[cfg(unix)]
    #[test]
    fn captures_output_and_exit_code() {
        let out = run(
            Path::new("/bin/sh"),
            &["-c", "printf out; printf err >&2; exit 3"],
            None,
            &[],
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(out.code, Some(3));
        assert_eq!(out.stdout, "out");
        assert_eq!(out.stderr, "err");
        assert!(!out.timed_out);
    }

    #[cfg(unix)]
    #[test]
    fn environment_is_exactly_what_was_passed() {
        let env = vec![(OsString::from("ONLY_THIS"), OsString::from("1"))];
        let out = run(
            Path::new("/usr/bin/env"),
            &[],
            None,
            &env,
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(out.stdout.trim(), "ONLY_THIS=1");
    }

    /// One test on purpose: the SIGINT shield is process-wide, so parallel runs would race the
    /// save/restore of the disposition.
    #[cfg(unix)]
    #[test]
    fn interactive_run_classifies_exits_and_shields_the_caller_from_sigint() {
        fn sigint_disposition() -> libc::sighandler_t {
            // SAFETY: query only (`act` null); `cur` is fully written on success.
            unsafe {
                let mut cur: libc::sigaction = std::mem::zeroed();
                libc::sigaction(libc::SIGINT, std::ptr::null(), &mut cur);
                cur.sa_sigaction
            }
        }
        let sh = |script: &str| {
            run_interactive(
                &["/bin/sh".into(), "-c".into(), script.into()],
                Some(Stdio::null()),
            )
        };
        let before = sigint_disposition();
        assert_eq!(sh("exit 0"), InteractiveExit::Success);
        assert_eq!(sh("exit 3"), InteractiveExit::Failed);
        assert_eq!(sh("exit 130"), InteractiveExit::Interrupted);
        // The child's SIGINT is the default again after exec, so it dies of the signal it sends
        // itself; the same signal sent to this process first is only recorded (a default
        // disposition would have ended the test binary here). The pause after `kill -INT $PPID`
        // is the terminal's reality — Ctrl+C reaches Workshop and the login together, and the
        // login takes a moment to clean up — and keeps the test off the race where the child is
        // reaped before the parent's handler has run on another thread of the test binary.
        assert_eq!(
            sh("kill -INT $PPID; sleep 0.2; kill -INT $$; exit 7"),
            InteractiveExit::Interrupted
        );
        // A login that handles Ctrl+C itself and exits 0 (`codex login`) is still a cancel: the
        // terminal sent the caller the same SIGINT.
        assert_eq!(
            sh("kill -INT $PPID; sleep 0.2; exit 0"),
            InteractiveExit::Interrupted
        );
        assert_eq!(sh("exit 0"), InteractiveExit::Success, "flag is per run");
        assert_eq!(
            run_interactive(&["/nonexistent/vendor-cli".into()], None),
            InteractiveExit::Failed
        );
        assert_eq!(run_interactive(&[], None), InteractiveExit::Failed);
        assert_eq!(
            sigint_disposition(),
            before,
            "the caller's SIGINT disposition is restored"
        );
    }
}
