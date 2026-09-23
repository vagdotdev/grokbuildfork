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
    private_dir(&run_dir);
    let socket = run_dir.join(format!("askpass-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    let listener = match tokio::net::UnixListener::bind(&socket) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = %e, path = %socket.display(), "askpass: cannot bind the socket");
            return None;
        }
    };
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

#[cfg(unix)]
fn private_dir(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
}
#[cfg(not(unix))]
fn private_dir(_dir: &Path) {}

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
    let (reader, mut writer) = stream.into_split();
    let mut line = String::new();
    tokio::io::BufReader::new(reader)
        .take(4096)
        .read_line(&mut line)
        .await?;
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
