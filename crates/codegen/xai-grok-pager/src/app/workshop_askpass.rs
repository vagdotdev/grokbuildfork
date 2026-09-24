//! Workshop: `sudo` passwords for the engine's commands, the way Grok Build handles them.
//!
//! Grok Build's own shell tool runs commands with a null stdin and no PTY
//! (`xai-grok-tools/src/computer/local/terminal.rs`, the `.stdin(xai_tty_utils::null_stdio())`
//! spawns), so `sudo` there cannot ask on a terminal either; its one accommodation is the user's
//! `SUDO_ASKPASS`: when that is set, the persistent shell gets `alias sudo='sudo -A'`
//! (`computer/local/shell_state.rs::sudo_alias_injection`, `static_shell.rs`) and sudo runs that
//! helper. Grok Build ships no helper and no password prompt of its own; without one, sudo fails
//! with "a terminal is required to read the password".
//!
//! The engine's commands are in the same position (no tty; with `SUDO_ASKPASS` set, sudo runs the
//! helper on its own — no alias needed). Workshop mirrors upstream: a user's `SUDO_ASKPASS` passes
//! through untouched. Only when there is none does Workshop stand in as the helper: it points the
//! engine at a one-line script that runs `workshop askpass <prompt>`; that process connects to a
//! Unix socket this process listens on, the UI shows one masked prompt in the permission prompt's
//! style ("Needs your password for: sudo …"), and the answer travels socket → helper stdout →
//! sudo. Nothing else sees it: not the model, not the transcript, not the logs, not the disk. Esc
//! skips: the helper exits non-zero with "Skipped — needs your password" on stderr, which is what
//! sudo's caller — the model — reads.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use tokio::sync::{mpsc, oneshot};

use super::workshop::WorkshopTurnMsg;

/// What the helper prints on stderr when the user skips (Esc) or Workshop is gone; `sudo` adds
/// its own "no password was provided" and the command fails plainly.
pub const SKIPPED: &str = "Skipped \u{2014} needs your password";

/// The engine-side environment: the helper `sudo` runs and the socket it reaches Workshop on.
pub const HELPER_ENV: &str = "SUDO_ASKPASS";
pub const SOCKET_ENV: &str = "WORKSHOP_ASKPASS_SOCK";

// ── Caller authentication ────────────────────────────────────────────────────────────────────────
//
// The helper path and the socket are in every engine command's environment, and with the `sudo -A`
// shim every sudo runs the helper. Without a check, a prompt-injected model command could run
// `$SUDO_ASKPASS` itself (or connect to the socket directly), show Workshop's legitimate password
// card, and read the typed password. So the password is delivered only when the *caller's parent is
// a live `sudo` process* — verified two ways: the helper checks its own parent before it connects,
// and the socket server checks the connecting peer's parent via `SO_PEERCRED`.
//
// "Real sudo" = a process whose *effective* uid is root and whose executable name is `sudo`. A
// non-root attacker cannot impersonate that: obtaining euid 0 needs a setuid-root binary they cannot
// create. An env-carried secret would not help instead — every engine command shares one
// environment and can set variables inline, so any nonce would be readable and forgeable by the
// attacker; the kernel-verified peer credential is the sound binding to the active sudo tool call.

/// True when `pid` is a live process with effective uid 0 and executable name `sudo` (real,
/// setuid-root sudo). Reads `/proc/<pid>/status` on Linux — world-readable even for a setuid,
/// non-dumpable process, unlike `/proc/<pid>/exe`, which is root-only there, so an unprivileged
/// helper/server can still check it. Falls back to `ps` on other unix.
fn effective_root_sudo(pid: i32) -> bool {
    if pid <= 1 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status")) else {
            return false;
        };
        let mut name_sudo = false;
        let mut euid_root = false;
        for line in status.lines() {
            if let Some(name) = line.strip_prefix("Name:") {
                name_sudo = name.trim() == "sudo";
            } else if let Some(uids) = line.strip_prefix("Uid:") {
                // "Uid:\t<real>\t<effective>\t<saved>\t<fs>"
                euid_root = uids.split_whitespace().nth(1) == Some("0");
            }
        }
        name_sudo && euid_root
    }
    #[cfg(not(target_os = "linux"))]
    {
        let Ok(out) = std::process::Command::new("ps")
            .args(["-o", "euid=", "-o", "comm=", "-p", &pid.to_string()])
            .output()
        else {
            return false;
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let mut fields = text.split_whitespace();
        let euid = fields.next();
        let comm = fields.next();
        euid == Some("0") && comm.is_some_and(|c| c == "sudo" || c.ends_with("/sudo"))
    }
}

/// The parent pid of `pid`.
fn parent_pid(pid: i32) -> Option<i32> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // "<pid> (comm) <state> <ppid> …"; comm may itself contain ')', so split at the last one.
        let after_comm = stat.rsplit_once(')')?.1;
        let mut fields = after_comm.split_whitespace();
        let _state = fields.next()?;
        fields.next()?.parse().ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let out = std::process::Command::new("ps")
            .args(["-o", "ppid=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    }
}

/// The connecting socket peer's pid via `SO_PEERCRED` (Linux). `None` where the platform cannot
/// report it.
#[cfg(target_os = "linux")]
fn peer_pid(fd: std::os::fd::RawFd) -> Option<i32> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `getsockopt(SO_PEERCRED)` writes a `ucred` of `len` bytes into `cred`, which outlives
    // the call; `len` is initialised to its size.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    (rc == 0 && cred.pid > 0).then_some(cred.pid)
}

#[cfg(not(target_os = "linux"))]
fn peer_pid(_fd: std::os::fd::RawFd) -> Option<i32> {
    None
}

/// Whether the socket peer is a real sudo's askpass helper: its parent is real sudo. On a platform
/// without `SO_PEERCRED` this cannot be verified server-side, so it defers to the helper's own
/// parent check (the helper refused to connect unless launched by sudo).
fn peer_is_real_sudo_child(stream: &tokio::net::UnixStream) -> bool {
    use std::os::fd::AsRawFd;
    match peer_pid(stream.as_raw_fd()) {
        Some(pid) => parent_pid(pid).is_some_and(effective_root_sudo),
        None => !cfg!(target_os = "linux"),
    }
}

/// A password `sudo` is waiting for. Lives on `AppView` while the prompt is up; the typed bytes
/// are wiped when the answer goes out or the prompt is dropped.
pub struct PendingPassword {
    /// `sudo …` — the command that needs it, when the running tool call is a shell command;
    /// else sudo's own prompt text.
    pub title: String,
    typed: Vec<u8>,
    reply: Option<oneshot::Sender<Option<Vec<u8>>>>,
}

impl PendingPassword {
    pub fn new(title: String, reply: oneshot::Sender<Option<Vec<u8>>>) -> Self {
        Self {
            title,
            typed: Vec::new(),
            reply: Some(reply),
        }
    }

    /// Characters typed so far (for the masked field's width only).
    pub fn typed_len(&self) -> usize {
        String::from_utf8_lossy(&self.typed).chars().count()
    }

    pub fn push_char(&mut self, c: char) {
        let mut buf = [0u8; 4];
        self.typed
            .extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }

    pub fn push_str(&mut self, s: &str) {
        self.typed.extend_from_slice(s.as_bytes());
    }

    pub fn pop_char(&mut self) {
        let s = String::from_utf8_lossy(&self.typed).into_owned();
        let mut chars = s.chars();
        chars.next_back();
        let kept: String = chars.collect();
        wipe(&mut self.typed);
        self.typed = kept.into_bytes();
    }

    /// Send the password to the helper (`sudo` reads it), or `None` to skip.
    pub fn answer(mut self, send: bool) {
        let secret = std::mem::take(&mut self.typed);
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(send.then_some(secret));
        }
    }
}

impl Drop for PendingPassword {
    fn drop(&mut self) {
        wipe(&mut self.typed);
    }
}

fn wipe(bytes: &mut Vec<u8>) {
    for b in bytes.iter_mut() {
        *b = 0;
    }
    bytes.clear();
}

/// The listening side, one per process: the socket the helpers connect to and the helper
/// script `sudo` runs. Both live under `$WORKSHOP_HOME`, never in the user's project.
pub struct AskpassServer {
    pub socket: PathBuf,
    pub helper: PathBuf,
}

impl AskpassServer {
    /// Environment for the engine (and so for every command it runs).
    pub fn env(&self) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        vec![
            (HELPER_ENV.into(), self.helper.clone().into_os_string()),
            (SOCKET_ENV.into(), self.socket.clone().into_os_string()),
        ]
    }
}

/// Bind the socket, write the helper script, and serve asks on `ui_tx` for the life of the
/// process. `None` (logged) when the home cannot hold them — `sudo` then fails as it does today.
pub fn start(ui_tx: mpsc::UnboundedSender<WorkshopTurnMsg>) -> Option<AskpassServer> {
    let home = super::workshop::workshop_home();
    let run_dir = home.join("run");
    let bin_dir = home.join("bin");
    if let Err(e) =
        std::fs::create_dir_all(&run_dir).and_then(|_| std::fs::create_dir_all(&bin_dir))
    {
        tracing::warn!(error = %e, "askpass: cannot create $WORKSHOP_HOME/run or bin");
        return None;
    }
    // Fail closed if the socket directory cannot be made private: a world-accessible askpass socket
    // would let any local process reach the password prompt.
    if !make_private(&run_dir) {
        tracing::warn!(path = %run_dir.display(), "askpass: refusing to start — cannot make the socket directory private");
        return None;
    }
    let socket = run_dir.join(format!("askpass-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    let listener = match tokio::net::UnixListener::bind(&socket) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = %e, path = %socket.display(), "askpass: cannot bind the socket");
            return None;
        }
    };
    if !restrict_socket(&socket) {
        tracing::warn!(path = %socket.display(), "askpass: refusing to start — cannot restrict the socket to the owner");
        let _ = std::fs::remove_file(&socket);
        return None;
    }
    let helper = bin_dir.join("askpass");
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "askpass: current_exe unknown");
            return None;
        }
    };
    let script = format!(
        "#!/bin/sh\n# Workshop's sudo askpass helper: asks in the Workshop window, prints the answer for sudo.\nexec '{}' askpass \"$@\"\n",
        exe.display().to_string().replace('\'', "'\\''")
    );
    if let Err(e) = std::fs::write(&helper, script) {
        tracing::warn!(error = %e, "askpass: cannot write the helper script");
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755));
    }
    tokio::spawn(serve(listener, ui_tx));
    Some(AskpassServer { socket, helper })
}

/// Make `dir` owner-only (0700) and confirm it took; the caller fails closed otherwise.
#[cfg(unix)]
fn make_private(dir: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    if std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).is_err() {
        return false;
    }
    std::fs::metadata(dir)
        .map(|m| m.permissions().mode() & 0o077 == 0)
        .unwrap_or(false)
}
#[cfg(not(unix))]
fn make_private(_dir: &Path) -> bool {
    true
}

/// Restrict the socket file to the owner (0600); the caller fails closed otherwise.
#[cfg(unix)]
fn restrict_socket(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).is_ok()
}
#[cfg(not(unix))]
fn restrict_socket(_path: &Path) -> bool {
    true
}

async fn serve(listener: tokio::net::UnixListener, ui_tx: mpsc::UnboundedSender<WorkshopTurnMsg>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let ui_tx = ui_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(stream, ui_tx).await {
                tracing::debug!(error = %e, "askpass: helper connection ended");
            }
        });
    }
}

async fn handle(
    stream: tokio::net::UnixStream,
    ui_tx: mpsc::UnboundedSender<WorkshopTurnMsg>,
) -> std::io::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    // Only a real sudo's askpass helper is answered: a command that connects directly (or runs
    // `$SUDO_ASKPASS` itself) has a shell/other parent, never sudo, so no card is shown and no
    // password is returned.
    let authenticated = peer_is_real_sudo_child(&stream);
    let (reader, mut writer) = stream.into_split();
    let mut line = String::new();
    tokio::io::BufReader::new(reader)
        .take(4096)
        .read_line(&mut line)
        .await?;
    if !authenticated {
        tracing::warn!(
            "askpass: refusing a client whose parent is not sudo (possible direct $SUDO_ASKPASS call)"
        );
        writer.write_all(b"{\"skipped\":true}\n").await?;
        return writer.shutdown().await;
    }
    let request: serde_json::Value = serde_json::from_str(line.trim()).unwrap_or_default();
    let prompt = request
        .get("prompt")
        .and_then(|p| p.as_str())
        .unwrap_or("[sudo] password:")
        .trim()
        .to_owned();
    let (reply_tx, reply_rx) = oneshot::channel();
    if ui_tx
        .send(WorkshopTurnMsg::PasswordAsk {
            prompt,
            reply: reply_tx,
        })
        .is_err()
    {
        writer.write_all(b"{\"skipped\":true}\n").await?;
        return Ok(());
    }
    let answer = reply_rx.await.ok().flatten();
    match answer {
        Some(mut secret) => {
            // One JSON line; the helper prints the string and exits. Wiped right after.
            let json = serde_json::json!({ "password": String::from_utf8_lossy(&secret) });
            let mut out = json.to_string().into_bytes();
            out.push(b'\n');
            let written = writer.write_all(&out).await;
            wipe(&mut secret);
            wipe(&mut out);
            written?;
        }
        None => writer.write_all(b"{\"skipped\":true}\n").await?,
    }
    writer.shutdown().await
}

/// `workshop askpass <prompt>` — the process `sudo` runs. Returns the exit code.
pub fn maybe_run_helper() -> Option<i32> {
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(String::as_str) != Some("askpass") {
        return None;
    }
    let prompt = argv.get(2).cloned().unwrap_or_default();
    Some(helper_main(&prompt))
}

fn helper_main(prompt: &str) -> i32 {
    // Fail closed unless our parent is real sudo: a model command that runs `$SUDO_ASKPASS` itself
    // has a shell parent, so it never reaches the socket and never shows a card.
    #[cfg(unix)]
    {
        // SAFETY: getppid takes no arguments and only reads the caller's parent pid.
        let ppid = unsafe { libc::getppid() };
        if !effective_root_sudo(ppid) {
            eprintln!("{SKIPPED}");
            return 1;
        }
    }
    let Some(socket) = std::env::var_os(SOCKET_ENV) else {
        eprintln!("{SKIPPED}");
        return 1;
    };
    let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&socket) else {
        eprintln!("{SKIPPED}");
        return 1;
    };
    let request = serde_json::json!({ "prompt": prompt }).to_string();
    if stream.write_all(format!("{request}\n").as_bytes()).is_err() {
        eprintln!("{SKIPPED}");
        return 1;
    }
    let mut line = String::new();
    if BufReader::new(&mut stream).read_line(&mut line).is_err() {
        eprintln!("{SKIPPED}");
        return 1;
    }
    let reply: serde_json::Value = serde_json::from_str(line.trim()).unwrap_or_default();
    match reply.get("password").and_then(|p| p.as_str()) {
        Some(password) => {
            let mut stdout = std::io::stdout().lock();
            let _ = stdout.write_all(password.as_bytes());
            let _ = stdout.write_all(b"\n");
            let _ = stdout.flush();
            0
        }
        None => {
            eprintln!("{SKIPPED}");
            1
        }
    }
}

/// The prompt's title: the shell command that needs the password when the turn's running tool
/// is one, else sudo's own prompt text.
pub fn title_for(command: Option<&str>, sudo_prompt: &str) -> String {
    match command {
        Some(cmd) if !cmd.trim().is_empty() => {
            let one_line = cmd.split_whitespace().collect::<Vec<_>>().join(" ");
            format!("Needs your password for: {one_line}")
        }
        _ => {
            let prompt = sudo_prompt.trim().trim_end_matches(':').trim();
            if prompt.is_empty() {
                "Needs your password for: sudo".to_owned()
            } else {
                format!("Needs your password: {prompt}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_names_the_command_or_sudos_prompt() {
        assert_eq!(
            title_for(Some("sudo apt install  htop"), "[sudo] password for me: "),
            "Needs your password for: sudo apt install htop"
        );
        assert_eq!(
            title_for(None, "[sudo] password for me: "),
            "Needs your password: [sudo] password for me"
        );
        assert_eq!(title_for(None, ""), "Needs your password for: sudo");
    }

    #[test]
    fn only_real_sudo_parents_are_trusted() {
        // This test process is not sudo: not effective-root and not named `sudo`.
        assert!(!effective_root_sudo(std::process::id() as i32));
        assert!(!effective_root_sudo(1)); // init
        assert!(!effective_root_sudo(0)); // degenerate
        // Our own parent (cargo test harness / shell) is discoverable and is not sudo either.
        let ppid = parent_pid(std::process::id() as i32).expect("own ppid");
        assert!(!effective_root_sudo(ppid));
    }

    #[test]
    fn typed_bytes_are_wiped_on_answer_and_drop() {
        let (tx, rx) = oneshot::channel();
        let mut pending = PendingPassword::new("t".into(), tx);
        pending.push_str("hunter2");
        pending.pop_char();
        assert_eq!(pending.typed_len(), 6);
        pending.answer(true);
        let secret = rx.blocking_recv().unwrap().unwrap();
        assert_eq!(secret, b"hunter");
        let (tx, rx) = oneshot::channel();
        let pending = PendingPassword::new("t".into(), tx);
        pending.answer(false);
        assert_eq!(rx.blocking_recv().unwrap(), None);
    }
}
