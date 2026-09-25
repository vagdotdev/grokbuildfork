//! Fake vendor CLIs: tiny `sh` scripts that answer `--version` / `--help`,
//! the vendor status command, and replay a captured JSONL fixture for a run.
//! State (logged in?, fixture, mode, exit code) lives in files under the
//! sandbox so no environment variables are needed — the supervisor strips
//! everything but an allowlist anyway.

#![allow(dead_code)]

pub mod fake_serve;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tempfile::TempDir;
use workshop_adapters::{AdapterId, DetectOptions, SupervisorOptions};

pub struct FakeVendor {
    pub id: AdapterId,
    pub binary: &'static str,
    pub version_line: &'static str,
    pub help_text: &'static str,
    /// Exact `$*` of the status command.
    pub status_args: &'static str,
    pub status_logged_in: &'static str,
    pub status_logged_in_exit: i32,
    pub status_logged_out: &'static str,
    pub status_logged_out_exit: i32,
    /// 1 = stdout, 2 = stderr (Codex prints status on stderr).
    pub status_fd: u8,
    /// 1 = stdout, 2 = stderr (OpenCode's yargs prints `--help` on stderr).
    pub help_fd: u8,
}

pub const CLAUDE: FakeVendor = FakeVendor {
    id: AdapterId::Claude,
    binary: "claude",
    version_line: "2.1.278 (Claude Code)",
    help_text: "Usage: claude [options] [command] [prompt]\n\nClaude Code - starts an interactive session by default, use -p/--print for non-interactive output",
    status_args: "auth status --json",
    status_logged_in: r#"{"loggedIn": true, "authMethod": "claude.ai", "apiProvider": "firstParty", "email": "user@example.com", "subscriptionType": "max"}"#,
    status_logged_in_exit: 0,
    status_logged_out: r#"{"loggedIn": false, "authMethod": "none", "apiProvider": "firstParty", "analyticsDisabled": false, "projectsDirectory": "/home/u/.claude/projects", "configDirectory": "/home/u/.claude"}"#,
    status_logged_out_exit: 1,
    status_fd: 1,
    help_fd: 1,
};

pub const CODEX: FakeVendor = FakeVendor {
    id: AdapterId::Codex,
    binary: "codex",
    version_line: "codex-cli 0.155.1",
    help_text: "Codex CLI\n\nUsage: codex [OPTIONS] [PROMPT]\n       codex [OPTIONS] <COMMAND>",
    status_args: "login status",
    status_logged_in: "Logged in using ChatGPT",
    status_logged_in_exit: 0,
    status_logged_out: "Not logged in",
    status_logged_out_exit: 1,
    status_fd: 2,
    help_fd: 1,
};

pub const CURSOR: FakeVendor = FakeVendor {
    id: AdapterId::Cursor,
    binary: "cursor-agent",
    version_line: "2026.09.18-9a7762b",
    help_text: "Usage: agent [options] [command] [prompt...]\n\nStart the Cursor Agent\n\nArguments:\n  prompt                       Initial prompt for the agent",
    status_args: "status --format json",
    status_logged_in: r#"{"status":"authenticated","isAuthenticated":true,"hasAccessToken":true,"hasRefreshToken":true,"message":"Logged in"}"#,
    status_logged_in_exit: 0,
    status_logged_out: r#"{"status":"unauthenticated","isAuthenticated":false,"hasAccessToken":false,"hasRefreshToken":false,"message":"Not logged in"}"#,
    status_logged_out_exit: 0,
    status_fd: 1,
    help_fd: 1,
};

pub const OPENCODE: FakeVendor = FakeVendor {
    id: AdapterId::OpenCode,
    binary: "opencode",
    version_line: "1.18.31",
    help_text: "Commands:\n  opencode run [message..]     run opencode with a message\n  opencode providers           manage AI providers and credentials [aliases: auth]",
    status_args: "auth list",
    status_logged_in: "┌  Credentials \u{1b}[90m~/.local/share/opencode/auth.json\u{1b}[0m\n│\n│  Anthropic \u{1b}[90moauth\u{1b}[0m\n│\n└  1 credentials",
    status_logged_in_exit: 0,
    status_logged_out: "┌  Credentials \u{1b}[90m~/.local/share/opencode/auth.json\u{1b}[0m\n│\n└  0 credentials",
    status_logged_out_exit: 0,
    status_fd: 1,
    help_fd: 2,
};

pub const ALL: [&FakeVendor; 4] = [&CLAUDE, &CODEX, &CURSOR, &OPENCODE];

/// One isolated sandbox: `bin/` for fakes, `state/` for their files, `home/`.
pub struct Sandbox {
    pub root: TempDir,
}

impl Sandbox {
    pub fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        for d in ["bin", "state", "home", "work"] {
            std::fs::create_dir_all(root.path().join(d)).unwrap();
        }
        Self { root }
    }

    pub fn bin(&self) -> PathBuf {
        self.root.path().join("bin")
    }

    pub fn state(&self) -> PathBuf {
        self.root.path().join("state")
    }

    pub fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    pub fn work(&self) -> PathBuf {
        self.root.path().join("work")
    }

    /// Install a fake under `bin/<vendor.binary>`.
    pub fn install(&self, vendor: &FakeVendor) -> PathBuf {
        self.install_as(&self.bin(), vendor, vendor.binary)
    }

    /// Install a fake under `dir/<name>`.
    pub fn install_as(&self, dir: &Path, vendor: &FakeVendor, name: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        write_script(&path, &script(vendor, &self.state()));
        path
    }

    /// Install an executable named `name` that is not any vendor's CLI.
    pub fn install_impostor(&self, name: &str) -> PathBuf {
        let path = self.bin().join(name);
        let body = "#!/bin/sh\ncase \"$1\" in\n  --version) echo '1.0.0'; exit 0 ;;\n  --help) echo 'Usage: agent'; echo 'A totally different agent'; exit 0 ;;\nesac\necho 'nope' >&2\nexit 2\n";
        write_script(&path, body);
        path
    }

    pub fn set_logged_in(&self, yes: bool) {
        let marker = self.state().join("logged_in");
        if yes {
            std::fs::write(&marker, "1").unwrap();
        } else {
            let _ = std::fs::remove_file(&marker);
        }
    }

    pub fn set_fixture(&self, jsonl: &str) {
        std::fs::write(self.state().join("fixture.jsonl"), jsonl).unwrap();
    }

    /// `stream` (default), `hang` (SIGINT-aware), or `hang-ignore-int`.
    pub fn set_mode(&self, mode: &str) {
        std::fs::write(self.state().join("mode"), mode).unwrap();
    }

    pub fn set_exit_code(&self, code: i32) {
        std::fs::write(self.state().join("exit_code"), code.to_string()).unwrap();
    }

    /// Port the fake `opencode serve` claims to listen on (a real in-test
    /// server should be bound there).
    pub fn set_serve_port(&self, port: u16) {
        std::fs::write(self.state().join("serve_port"), port.to_string()).unwrap();
    }

    pub fn serve_argv(&self) -> Vec<String> {
        std::fs::read_to_string(self.state().join("serve_argv.txt"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    pub fn serve_env(&self) -> Vec<String> {
        std::fs::read_to_string(self.state().join("serve_env.txt"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    pub fn argv(&self) -> Vec<String> {
        std::fs::read_to_string(self.state().join("argv.txt"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    pub fn stdin(&self) -> String {
        std::fs::read_to_string(self.state().join("stdin.txt")).unwrap_or_default()
    }

    /// The `control_response` lines the fake read on its control channel, one per
    /// `control_request` it replayed.
    pub fn replies(&self) -> Vec<String> {
        std::fs::read_to_string(self.state().join("replies.txt"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    pub fn child_env(&self) -> Vec<String> {
        std::fs::read_to_string(self.state().join("env.txt"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    pub fn child_cwd(&self) -> String {
        std::fs::read_to_string(self.state().join("cwd.txt"))
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    pub fn got_sigint(&self) -> bool {
        self.state().join("sigint").exists()
    }

    pub fn grandchild_pid(&self) -> Option<i32> {
        std::fs::read_to_string(self.state().join("grandchild.pid"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }

    /// Environment handed to fakes: the real `PATH` (so `sh`, `cat`, `sleep`
    /// resolve) and the sandbox `HOME`.
    pub fn probe_env(&self) -> BTreeMap<OsString, OsString> {
        let mut env = BTreeMap::new();
        env.insert(
            OsString::from("PATH"),
            std::env::var_os("PATH").unwrap_or_else(|| OsString::from("/usr/bin:/bin")),
        );
        env.insert(OsString::from("HOME"), self.home().into_os_string());
        env
    }

    pub fn detect_options(&self) -> DetectOptions {
        DetectOptions {
            path_env: Some(self.bin().into_os_string()),
            home: Some(self.home()),
            known_dirs: Some(Vec::new()),
            probe_env: Some(self.probe_env()),
            probe_timeout: Duration::from_secs(10),
        }
    }

    pub fn supervisor_options(&self) -> SupervisorOptions {
        SupervisorOptions {
            env: Some(self.probe_env()),
            cancel_grace: Duration::from_millis(500),
            drain_timeout: Duration::from_millis(500),
            ..SupervisorOptions::default()
        }
    }
}

/// Write an executable script so it can be exec'd right away: the bytes go to a sibling temp
/// file that is fsynced and closed before it is made executable and renamed into place. Nothing
/// ever sees a half-written or still-open script — the `ETXTBSY` ("Text file busy") a concurrent
/// test's fork can otherwise provoke by inheriting the write handle is left only to the retry in
/// the adapter's spawn path.
fn write_script(path: &Path, body: &str) {
    use std::io::Write;
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    {
        let mut file = std::fs::File::create(&tmp).unwrap();
        file.write_all(body.as_bytes()).unwrap();
        file.sync_all().unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::rename(&tmp, path).unwrap();
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn script(v: &FakeVendor, state: &Path) -> String {
    let state = sh_quote(&state.to_string_lossy());
    let redirect = if v.status_fd == 2 { " >&2" } else { "" };
    let help_redirect = if v.help_fd == 2 { " >&2" } else { "" };
    format!(
        r#"#!/bin/sh
state={state}
case "$1" in
  --version) printf '%s\n' {version}; exit 0 ;;
  --help) printf '%s\n' {help}{help_redirect}; exit 0 ;;
esac
if [ "$*" = {status_args} ]; then
  if [ -f "$state/logged_in" ]; then
    printf '%s\n' {status_in}{redirect}; exit {in_exit}
  else
    printf '%s\n' {status_out}{redirect}; exit {out_exit}
  fi
fi
if [ "$1" = "serve" ]; then
  printf '%s\n' "$@" > "$state/serve_argv.txt"
  env > "$state/serve_env.txt"
  port=$(cat "$state/serve_port")
  echo "opencode server listening on http://127.0.0.1:$port"
  trap 'exit 0' TERM
  while :; do sleep 1; done
fi
printf '%s\n' "$@" > "$state/argv.txt"
case " $* " in
  *" --input-format "*)
    # Control channel (Claude Code's stream-json input): the prompt arrives as message lines
    # and stdin stays open for control responses; read up to the user message.
    : > "$state/stdin.txt"
    while IFS= read -r line; do
      printf '%s\n' "$line" >> "$state/stdin.txt"
      case "$line" in *'"type":"user"'*) break ;; esac
    done ;;
  *) cat > "$state/stdin.txt" ;;
esac
env > "$state/env.txt"
pwd > "$state/cwd.txt"
mode=$(cat "$state/mode" 2>/dev/null || echo stream)
case "$mode" in
  hang)
    trap 'echo sigint > "$state/sigint"; exit 130' INT
    head -n 1 "$state/fixture.jsonl"
    sleep 300 &
    echo $! > "$state/grandchild.pid"
    wait
    exit 0 ;;
  hang-ignore-int)
    trap '' INT
    head -n 1 "$state/fixture.jsonl"
    sleep 300 &
    echo $! > "$state/grandchild.pid"
    wait
    exit 0 ;;
  *)
    # A replayed `control_request` blocks, as the real CLI does, until its `control_response`
    # arrives on stdin; the fixture is read on fd 3 so stdin stays the channel.
    while IFS= read -r line <&3 || [ -n "$line" ]; do
      printf '%s\n' "$line"
      case "$line" in
        *'"type":"control_request"'*)
          IFS= read -r reply && printf '%s\n' "$reply" >> "$state/replies.txt" ;;
      esac
    done 3< "$state/fixture.jsonl"
    exit "$(cat "$state/exit_code" 2>/dev/null || echo 0)" ;;
esac
"#,
        state = state,
        version = sh_quote(v.version_line),
        help = sh_quote(v.help_text),
        status_args = sh_quote(v.status_args),
        status_in = sh_quote(v.status_logged_in),
        status_out = sh_quote(v.status_logged_out),
        in_exit = v.status_logged_in_exit,
        out_exit = v.status_logged_out_exit,
        redirect = redirect,
        help_redirect = help_redirect,
    )
}

/// True when `pid` no longer exists (or is an unreaped zombie).
#[cfg(unix)]
pub fn process_gone(pid: i32) -> bool {
    // kill(pid, 0) succeeds while the process exists, zombies included, so a
    // zombie state in /proc also counts as gone.
    if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err() {
        return true;
    }
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map(|s| {
            s.rsplit(") ")
                .next()
                .is_some_and(|rest| rest.starts_with('Z'))
        })
        .unwrap_or(false)
}

/// Poll [`process_gone`] for up to `timeout`.
#[cfg(unix)]
pub async fn wait_process_gone(pid: i32, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if process_gone(pid) {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
