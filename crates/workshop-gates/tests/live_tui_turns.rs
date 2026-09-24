//! Live end-to-end turns through the real TUI (milestones C/D/E user journeys), opt-in because
//! they need the network and/or an installed `opencode`:
//!
//! * `WORKSHOP_LIVE_KILO=1`   — P2a: cold start with an `opencode` that cannot start → the first
//!   message is answered silently through the live keyless pool (the fallback the user never sees
//!   named) → a file-write tool call → the file exists on disk; the composer names the answering
//!   model only.
//! * `WORKSHOP_LIVE_OPENCODE=1` — P2b: cold start lands on "Big Pickle" (type-and-go, no picker;
//!   the launch installs/starts `opencode serve` in the background) → the first message → a tool
//!   call → the file exists on disk.
//!
//! Both need `WORKSHOP_BIN` (the built `workshop` binary) and `--include-ignored`. When `strace` is
//! on `PATH` the TUI runs under `strace -f -e trace=network`, so the evidence directory also holds
//! the exact DNS names / connect() targets of the whole journey (`scripts/no-egress/summarize.py`
//! renders it). Tool calls are auto-approved (`--yolo`) so the run is unattended; the approval UI is
//! upstream's and untouched.
//!
//! Evidence (text + HTML screenshots, strace) lands in `WORKSHOP_PTY_EVIDENCE_DIR`
//! (default `target/pty-evidence/live-<journey>`).

mod pty_common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use pty_common::install_fake_opencode_into;
use xai_grok_pager_pty_harness::PtyHarness;

const FILE_CONTENT: &str = "hello from workshop";

fn evidence_dir(journey: &str) -> PathBuf {
    let dir = std::env::var_os("WORKSHOP_PTY_EVIDENCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/pty-evidence"))
        .join(format!("live-{journey}"));
    std::fs::create_dir_all(&dir).expect("create evidence dir");
    dir
}

fn snapshot(h: &PtyHarness, dir: &Path, name: &str) {
    std::fs::write(dir.join(format!("{name}.txt")), h.screen_contents()).expect("write txt");
    std::fs::write(dir.join(format!("{name}.html")), h.screen_html()).expect("write html");
}

fn wait_gone(h: &mut PtyHarness, text: &str, secs: u64) {
    if let Err(e) = h.wait_for_text_absent(text, Duration::from_secs(secs)) {
        panic!(
            "timed out waiting for {text:?} to disappear: {e}\nscreen:\n{}",
            h.screen_contents()
        );
    }
}

fn wait_for(h: &mut PtyHarness, text: &str, secs: u64) {
    if let Err(e) = h.wait_for_text(text, Duration::from_secs(secs)) {
        panic!(
            "timed out waiting for {text:?}: {e}\nscreen:\n{}",
            h.screen_contents()
        );
    }
}

struct Journey {
    h: PtyHarness,
    dir: PathBuf,
    cwd: tempfile::TempDir,
    _home: tempfile::TempDir,
}

/// Spawn the TUI on a fresh HOME in a fresh git repo, under strace when available. `extra_path`
/// is prepended to `PATH` (rails proof: a `bin/` of fake vendor CLIs).
fn spawn(
    journey: &str,
    bin: &Path,
    extra_env: &[(&str, &str)],
    extra_path: Option<&Path>,
) -> Journey {
    let home = tempfile::tempdir().expect("tempdir");
    let cwd = tempfile::tempdir().expect("tempdir");
    std::process::Command::new("git")
        .args(["init", "-q", "."])
        .current_dir(cwd.path())
        .status()
        .expect("git init");
    let dir = evidence_dir(journey);
    let workshop_home = home.path().join(".workshop");
    let home_s = home.path().to_string_lossy().to_string();
    let wh_s = workshop_home.to_string_lossy().to_string();
    let inherited_path = std::env::var("PATH").unwrap_or_default();
    // Prepend a fake-CLI `bin/` (rails proof) so detection finds `claude`/`codex`/`cursor-agent`.
    let path_s = match extra_path {
        Some(p) => format!("{}:{inherited_path}", p.display()),
        None => inherited_path,
    };
    let mut env: Vec<(&str, &str)> = vec![
        ("HOME", home_s.as_str()),
        ("WORKSHOP_HOME", wh_s.as_str()),
        ("PATH", path_s.as_str()),
        ("TERM", "xterm-256color"),
        ("NO_COLOR", "1"),
        ("GROK_DISABLE_AUTOUPDATER", "1"),
    ];
    env.extend_from_slice(extra_env);

    let bin_s = bin.to_string_lossy().to_string();
    let strace_log = dir.join("strace.log").to_string_lossy().to_string();
    let strace = which("strace");
    let (program, args): (PathBuf, Vec<&str>) = match &strace {
        Some(s) => (
            s.clone(),
            vec![
                "-f",
                "-e",
                "trace=network",
                "-s",
                "512",
                "-o",
                strace_log.as_str(),
                "--",
                bin_s.as_str(),
                "--yolo",
            ],
        ),
        None => (bin.to_path_buf(), vec!["--yolo"]),
    };
    let mut h = PtyHarness::new_inherited_env(&program, 45, 140, &args, &env, Some(cwd.path()))
        .expect("spawn workshop in pty");
    h.set_respond_to_queries(true);
    Journey {
        h,
        dir,
        cwd,
        _home: home,
    }
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|p| {
        std::env::split_paths(&p)
            .map(|d| d.join(name))
            .find(|c| c.is_file())
    })
}

/// The type-and-go first run: the composer is up with the OpenCode default active, named by the
/// model only.
const FIRST_RUN_LABEL: &str = "Big Pickle";
/// The one picker overlay is on screen (its search line starts with this glyph).
const SUBSCRIPTIONS_OVERLAY: &str = "\u{2315}";

/// Type a slash command into the composer and submit it.
fn slash(h: &mut PtyHarness, cmd: &str) {
    h.inject_keys(cmd.as_bytes()).unwrap();
    h.update(Duration::from_millis(400));
    h.inject_keys(b"\r").unwrap();
}

/// Type a prompt that needs a write tool call and wait for the file to land on disk.
fn run_write_turn(j: &mut Journey, secs: u64) -> String {
    let prompt = format!(
        "Create a file named hello.txt in the current directory containing exactly this text: \
         {FILE_CONTENT}. Use your file-writing tool. When it exists, reply with the single word DONE."
    );
    j.h.inject_keys(prompt.as_bytes()).unwrap();
    j.h.update(Duration::from_millis(300));
    snapshot(&j.h, &j.dir, "03-prompt-typed");
    j.h.inject_keys(b"\r").unwrap();

    let target = j.cwd.path().join("hello.txt");
    let started = Instant::now();
    let mut tick = 0;
    while started.elapsed() < Duration::from_secs(secs) {
        j.h.update(Duration::from_millis(500));
        tick += 1;
        if tick % 10 == 0 {
            snapshot(
                &j.h,
                &j.dir,
                &format!("04-turn-{:03}s", started.elapsed().as_secs()),
            );
        }
        if let Ok(text) = std::fs::read_to_string(&target)
            && text.contains(FILE_CONTENT)
        {
            // Let the model's final message render.
            let _ = j.h.wait_for_text("DONE", Duration::from_secs(45));
            j.h.update(Duration::from_millis(800));
            snapshot(&j.h, &j.dir, "05-turn-done");
            return text;
        }
    }
    snapshot(&j.h, &j.dir, "05-turn-timeout");
    panic!(
        "hello.txt was not written within {secs}s\nscreen:\n{}",
        j.h.screen_contents()
    );
}

/// Start a turn, wait for it to begin streaming, then cancel it with Ctrl+C (the upstream cancel
/// gesture; mid-turn Esc is swallowed by the app). Assert the TUI reports the cancellation and the
/// composer becomes usable again (no hang).
fn run_cancel_turn(j: &mut Journey) {
    let prompt = "Write a long, detailed, multi-paragraph essay (at least 600 words) about the \
                  history of command-line text editors. Take your time.";
    j.h.inject_keys(prompt.as_bytes()).unwrap();
    j.h.update(Duration::from_millis(300));
    j.h.inject_keys(b"\r").unwrap();
    // Wait until the turn is visibly underway (a running indicator or streamed text).
    let started = Instant::now();
    let mut streaming = false;
    while started.elapsed() < Duration::from_secs(60) {
        j.h.update(Duration::from_millis(300));
        let screen = j.h.screen_contents();
        if screen.contains("history")
            || screen.contains("editor")
            || screen.contains("Esc to interrupt")
        {
            streaming = true;
            break;
        }
    }
    assert!(
        streaming,
        "turn never started streaming:\n{}",
        j.h.screen_contents()
    );
    snapshot(&j.h, &j.dir, "06-cancel-midstream");
    // Ctrl+C twice (the two-step cancel gesture on an empty prompt with a running turn).
    j.h.inject_keys(b"\x03").unwrap();
    j.h.update(Duration::from_millis(300));
    j.h.inject_keys(b"\x03").unwrap();
    if let Err(e) = j.h.wait_for_text("cancelled", Duration::from_secs(30)) {
        panic!(
            "turn was not cancelled: {e}\nscreen:\n{}",
            j.h.screen_contents()
        );
    }
    j.h.update(Duration::from_millis(500));
    snapshot(&j.h, &j.dir, "07-cancelled");
}

/// A follow-up turn on the same session that must recall the earlier tool call (resume/memory).
fn run_memory_turn(j: &mut Journey, secs: u64) -> String {
    let prompt = "Without using any tools, what exact text did you put inside hello.txt earlier? \
                  Reply with only that text.";
    j.h.inject_keys(prompt.as_bytes()).unwrap();
    j.h.update(Duration::from_millis(300));
    j.h.inject_keys(b"\r").unwrap();
    if let Err(e) = j.h.wait_for_text(FILE_CONTENT, Duration::from_secs(secs)) {
        snapshot(&j.h, &j.dir, "08-memory-timeout");
        panic!(
            "second turn did not recall the file contents: {e}\nscreen:\n{}",
            j.h.screen_contents()
        );
    }
    j.h.update(Duration::from_millis(500));
    snapshot(&j.h, &j.dir, "08-memory-recalled");
    j.h.screen_contents()
}

/// The whole journey's strace must show no connect()/DNS to an xAI / Grok / analytics host. Returns
/// the DNS query-name fragments seen, so the caller can assert the *expected* host was contacted.
fn assert_no_forbidden_egress(dir: &Path) -> String {
    let log = dir.join("strace.log");
    let Ok(text) = std::fs::read_to_string(&log) else {
        // No strace on PATH: the network gate is covered by scripts/no-egress-smoke.sh instead.
        eprintln!(
            "no strace.log ({}); skipping egress assertion",
            log.display()
        );
        return String::new();
    };
    let forbidden = ["x.ai", "grok.com", "mixpanel", "googleapis", "sentry"];
    let mut offenders = Vec::new();
    for line in text.lines() {
        if (line.contains("connect(") || line.contains("sendto(") || line.contains("sendmsg("))
            && let Some(bad) = forbidden.iter().find(|h| line.contains(**h))
        {
            offenders.push(format!("{bad}: {}", line.trim()));
        }
    }
    assert!(
        offenders.is_empty(),
        "forbidden host contacted during the journey:\n  {}",
        offenders.join("\n  ")
    );
    text
}

fn finish(mut j: Journey, hosts_note: &str) {
    // Ctrl-C twice exits the TUI (first press asks to confirm).
    j.h.inject_keys(b"\x03").unwrap();
    j.h.update(Duration::from_millis(400));
    j.h.inject_keys(b"\x03").unwrap();
    j.h.update(Duration::from_millis(800));
    std::fs::write(j.dir.join("README.txt"), hosts_note).expect("write note");
    eprintln!("evidence: {}", j.dir.display());
}

fn bin_from_env() -> Option<PathBuf> {
    let b = std::env::var_os("WORKSHOP_BIN").map(PathBuf::from);
    if b.is_none() {
        eprintln!("WORKSHOP_BIN not set; skipping");
    }
    b
}

/// P2a: `opencode` cannot start → the live keyless pool answers silently, tool call included.
#[test]
#[ignore = "live: needs network + WORKSHOP_BIN; run with WORKSHOP_LIVE_KILO=1 --include-ignored"]
fn opencode_start_failure_falls_back_to_the_live_free_pool_with_tool_call() {
    if std::env::var_os("WORKSHOP_LIVE_KILO").is_none() {
        eprintln!("WORKSHOP_LIVE_KILO not set; skipping");
        return;
    }
    let Some(bin) = bin_from_env() else { return };
    // An `opencode` that identifies itself and then dies on `serve`: the OpenCode model cannot
    // start, so the first message goes through the fallback without a word about it.
    let fakes = tempfile::tempdir().expect("fakes dir");
    let fake_bin = fakes.path().join("bin");
    std::fs::create_dir_all(&fake_bin).unwrap();
    let fake = fake_bin.join("opencode");
    std::fs::write(
        &fake,
        "#!/bin/sh\ncase \"$1\" in\n  --version) echo 1.18.31; exit 0 ;;\n  --help) echo 'opencode run [message..]  run opencode with a message' >&2; exit 0 ;;\nesac\necho 'dyld: Library not loaded: @rpath/libfake.dylib (fault injected)' >&2\nexit 1\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut j = spawn("kilo", &bin, &[], Some(&fake_bin));
    wait_for(&mut j.h, FIRST_RUN_LABEL, 45);
    snapshot(&j.h, &j.dir, "00-first-run-composer");
    let text = run_write_turn(&mut j, 180);
    assert!(text.contains(FILE_CONTENT), "file content: {text:?}");
    let screen = j.h.screen_contents();
    for word in [
        "Kilo",
        "fallback",
        "engine",
        "Couldn't reach",
        "auth.x.ai",
        "grok.com",
    ] {
        assert!(
            !screen.contains(word),
            "{word:?} must never be shown; the answer just arrives:\n{screen}"
        );
    }
    assert!(
        !screen.contains(FIRST_RUN_LABEL),
        "the composer now names the model that answered, not the one that could not start:\n{screen}"
    );
    let strace = assert_no_forbidden_egress(&j.dir);
    if !strace.is_empty() {
        assert!(
            strace.contains("kilo"),
            "expected a DNS query for api.kilo.ai in the strace"
        );
    }
    finish(
        j,
        "OpenCode start forced to fail → silent fallback to the live keyless pool via Workshop's own \
         agent loop. Expected hosts in strace.log: api.kilo.ai only (plus loopback local-server \
         probes and the OS keyring socket).\n",
    );
}

/// P2b: Big Pickle through the OpenCode engine (`opencode serve`) → real tool call.
#[test]
#[ignore = "live: needs opencode + WORKSHOP_BIN; run with WORKSHOP_LIVE_OPENCODE=1 --include-ignored"]
fn opencode_big_pickle_turn_with_tool_call() {
    if std::env::var_os("WORKSHOP_LIVE_OPENCODE").is_none() {
        eprintln!("WORKSHOP_LIVE_OPENCODE not set; skipping");
        return;
    }
    let Some(bin) = bin_from_env() else { return };
    let mut j = spawn("opencode", &bin, &[], None);
    // Type-and-go: the first run lands in the composer with the engine default already active;
    // nothing is picked, and `opencode` is being installed in the background while the first
    // message below is typed (the message waits for it if it is not up yet).
    wait_for(&mut j.h, FIRST_RUN_LABEL, 45);
    j.h.update(Duration::from_millis(1000));
    snapshot(&j.h, &j.dir, "02-home-connected");
    // 1. A real tool-calling turn writes a file (streamed into the scrollback).
    let text = run_write_turn(&mut j, 240);
    assert!(text.contains(FILE_CONTENT), "file content: {text:?}");
    // 2. Cancel a fresh turn mid-stream (Ctrl+C → engine abort).
    run_cancel_turn(&mut j);
    // 3. A follow-up turn on the same session recalls the earlier write (resume/memory intact).
    let recalled = run_memory_turn(&mut j, 120);
    assert!(
        recalled.contains(FILE_CONTENT),
        "memory turn recalled: {recalled:?}"
    );
    assert_no_forbidden_egress(&j.dir);
    finish(
        j,
        "OpenCode Big Pickle via the OpenCode engine (official `opencode serve` on loopback): turn +\n\
         tool call, cancel mid-stream, and same-session memory. Expected hosts in strace.log:\n\
         127.0.0.1:<engine port> from workshop; opencode's own upstream from the child.\n",
    );
}

// ---------------------------------------------------------------------------------------------
// P3: subscription rails through the vendor CLI adapters (RunHandle), driven by fake CLIs.
//
// Hermetic: fake `claude` / `codex` / `cursor-agent` shell scripts (identity, login status, and a
// turn that replays the adapter crate's own success fixture) stand in for the real CLIs, so no
// network and no real account are needed. Proves: a vendor row reads `✓` when the fake reports
// logged in and opens into the CLI's own model list; selecting a model routes a turn through the
// adapter and renders it with the model's name as the composer label (model only, no vendor
// prefix); cancel works; and a logged-out fake reads `sign in`, where Enter launches the vendor's
// documented login command in the terminal.

struct FakeVendor {
    binary: &'static str,
    version_line: &'static str,
    help_text: &'static str,
    help_fd: u8,
    status_logged_in: &'static str,
    status_logged_in_exit: i32,
    status_logged_out: &'static str,
    status_logged_out_exit: i32,
    status_fd: u8,
    login_args: &'static str,
    fixture: &'static str,
    /// Shell `case "$*"` arm answering the vendor's model-list probe (workshop-detect `models`),
    /// in the documented wire shape.
    models_arm: &'static str,
}

const FAKE_CLAUDE: FakeVendor = FakeVendor {
    binary: "claude",
    version_line: "2.1.278 (Claude Code)",
    help_text: "Usage: claude [options] [command] [prompt]\n\nClaude Code - starts an interactive session by default, use -p/--print for non-interactive output",
    help_fd: 1,
    status_logged_in: r#"{"loggedIn": true, "authMethod": "claude.ai", "apiProvider": "firstParty", "email": "user@example.com", "subscriptionType": "max"}"#,
    status_logged_in_exit: 0,
    status_logged_out: r#"{"loggedIn": false, "authMethod": "none", "apiProvider": "firstParty"}"#,
    status_logged_out_exit: 1,
    status_fd: 1,
    login_args: "auth login",
    fixture: "claude_success.jsonl",
    // The model-list probe only (a turn also passes `--input-format`, its control channel).
    models_arm: r#"*--strict-mcp-config*)
    IFS= read -r _req
    printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"workshop-models","response":{"models":[{"value":"default","displayName":"Default (recommended)"},{"value":"opus[1m]","displayName":"Opus (1M context)"},{"value":"sonnet","displayName":"Sonnet"}],"account":{"email":"user@example.com","subscriptionType":"max"}}}}'
    while IFS= read -r _; do :; done; exit 0 ;;"#,
};

const FAKE_CODEX: FakeVendor = FakeVendor {
    binary: "codex",
    version_line: "codex-cli 0.155.1",
    help_text: "Codex CLI\n\nUsage: codex [OPTIONS] [PROMPT]\n       codex [OPTIONS] <COMMAND>",
    help_fd: 1,
    status_logged_in: "Logged in using ChatGPT",
    status_logged_in_exit: 0,
    status_logged_out: "Not logged in",
    status_logged_out_exit: 1,
    status_fd: 2,
    login_args: "login",
    fixture: "codex_success.jsonl",
    models_arm: r#"app-server)
    while IFS= read -r req; do
      case "$req" in
        *'"method":"initialize"'*) printf '%s\n' '{"id":1,"result":{}}' ;;
        *'"method":"account/read"'*) printf '%s\n' '{"id":2,"result":{"account":{"type":"chatgpt","email":"user@example.com","planType":"pro"},"requiresOpenaiAuth":true}}' ;;
        *'"method":"model/list"'*) printf '%s\n' '{"id":3,"result":{"data":[{"id":"gpt-6-astra","model":"gpt-6-astra","displayName":"GPT-6-Astra","hidden":false,"isDefault":true},{"id":"gpt-5.5","model":"gpt-5.5","displayName":"GPT-5.5","hidden":false,"isDefault":false}],"nextCursor":null}}' ;;
      esac
    done; exit 0 ;;"#,
};

const FAKE_CURSOR: FakeVendor = FakeVendor {
    binary: "cursor-agent",
    version_line: "2026.09.18-9a7762b",
    help_text: "Usage: agent [options] [command] [prompt...]\n\nStart the Cursor Agent\n\nArguments:\n  prompt                       Initial prompt for the agent",
    help_fd: 1,
    status_logged_in: r#"{"status":"authenticated","isAuthenticated":true,"hasAccessToken":true,"hasRefreshToken":true,"message":"Logged in"}"#,
    status_logged_in_exit: 0,
    status_logged_out: r#"{"status":"unauthenticated","isAuthenticated":false,"hasAccessToken":false,"hasRefreshToken":false,"message":"Not logged in"}"#,
    status_logged_out_exit: 0,
    status_fd: 1,
    login_args: "login",
    fixture: "cursor_success.jsonl",
    // `<state>/models_fail`: the listing fails like an offline CLI; every call is counted.
    models_arm: r#"models)
    echo models >> "$state/models_calls"
    if [ -f "$state/models_fail" ]; then echo 'Failed to load models: network down' >&2; exit 1; fi
    printf 'Available models\n\nauto - Auto (current)\ncomposer-2.5 - Composer 2.5 (default)\n\nTip: use --model <id> to switch.\n'; exit 0 ;;"#,
};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../workshop-adapters/tests/fixtures")
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Write a fake vendor CLI into `bin_dir` and its per-binary state into `state_dir`. `logged_in`
/// seeds the status; a turn (`-p …` / `exec …`) replays the adapter fixture; the login command
/// touches `<state>/login_ran` so the Connect proof is deterministic.
fn install_fake(bin_dir: &Path, state_dir: &Path, v: &FakeVendor, logged_in: bool) {
    let state = state_dir.join(v.binary);
    std::fs::create_dir_all(&state).expect("fake state dir");
    std::fs::copy(fixtures_dir().join(v.fixture), state.join("fixture.jsonl"))
        .expect("copy fixture");
    if logged_in {
        std::fs::write(state.join("logged_in"), "1").unwrap();
    }
    let status_redirect = if v.status_fd == 2 { " >&2" } else { "" };
    let help_redirect = if v.help_fd == 2 { " >&2" } else { "" };
    // Match any `*status*` invocation (workshop-detect probes `auth status`, workshop-adapters
    // probes `auth status --json`; codex uses `login status`, cursor `status --format json`), then
    // the exact login command, then anything else is a turn.
    let script = format!(
        r#"#!/bin/sh
state={state}
case "$1" in
  --version) printf '%s\n' {version}; exit 0 ;;
  --help) printf '%s\n' {help}{help_redirect}; exit 0 ;;
esac
case "$*" in
  {models_arm}
  *status*)
    if [ -f "$state/logged_in" ]; then printf '%s\n' {status_in}{status_redirect}; exit {in_exit}
    else printf '%s\n' {status_out}{status_redirect}; exit {out_exit}; fi ;;
  {login_args})
    touch "$state/login_ran"; printf 'Opening browser to sign in...\n'
    # `<state>/login_hang`: wait like a real OAuth flow does, until the terminal's Ctrl+C.
    if [ -f "$state/login_hang" ]; then sleep 60; fi
    exit 0 ;;
esac
# Otherwise this is a turn: record its flags, then replay the adapter fixture line by line. A
# per-line sleep keeps the turn on-screen long enough to be cancelled mid-stream (SIGINT from the
# supervisor stops it). On a control channel (`--input-format`, Claude Code) the prompt arrives as
# message lines on stdin, and a replayed `control_request` blocks — as the real CLI does — until
# its `control_response` arrives there; the fixture is read on fd 3 so stdin stays the channel.
trap 'exit 130' INT TERM
printf '%s\n' "$*" >> "$state/turn_argv.txt"
case " $* " in
  *" --input-format "*)
    while IFS= read -r line; do
      printf '%s\n' "$line" >> "$state/stdin.txt"
      case "$line" in *'"type":"user"'*) break ;; esac
    done ;;
esac
while IFS= read -r line <&3 || [ -n "$line" ]; do
  printf '%s\n' "$line"
  case "$line" in
    *'"type":"control_request"'*)
      IFS= read -r reply && printf '%s\n' "$reply" >> "$state/replies.txt" ;;
    *) sleep 0.3 ;;
  esac
done 3< "$state/fixture.jsonl"
exit 0
"#,
        state = sh_quote(&state.to_string_lossy()),
        version = sh_quote(v.version_line),
        help = sh_quote(v.help_text),
        help_redirect = help_redirect,
        status_in = sh_quote(v.status_logged_in),
        status_redirect = status_redirect,
        in_exit = v.status_logged_in_exit,
        status_out = sh_quote(v.status_logged_out),
        out_exit = v.status_logged_out_exit,
        login_args = sh_quote(v.login_args),
        models_arm = v.models_arm,
    );
    let path = bin_dir.join(v.binary);
    std::fs::write(&path, script).expect("write fake");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

struct Fakes {
    _dir: tempfile::TempDir,
    bin: PathBuf,
    state: PathBuf,
}

fn install_fakes(logged_in: bool) -> Fakes {
    let dir = tempfile::tempdir().expect("fakes dir");
    let bin = dir.path().join("bin");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&bin).unwrap();
    for v in [&FAKE_CLAUDE, &FAKE_CODEX, &FAKE_CURSOR] {
        install_fake(&bin, &state, v, logged_in);
    }
    // The engine is brought up at launch; without a fake `opencode` on PATH these hermetic rails
    // gates would run the real vendor installer in the background.
    install_fake_opencode_into(&bin, "crash");
    Fakes {
        _dir: dir,
        bin,
        state,
    }
}

/// From the first-run composer, `/auth` opens the picker on its Subscriptions section; wait for
/// the three vendor rows.
fn open_subscriptions(j: &mut Journey) {
    wait_for(&mut j.h, FIRST_RUN_LABEL, 30);
    slash(&mut j.h, "/auth");
    wait_for(&mut j.h, SUBSCRIPTIONS_OVERLAY, 15);
    wait_for(&mut j.h, "Claude", 10);
    wait_for(&mut j.h, "Codex", 5);
    wait_for(&mut j.h, "Cursor", 5);
    // The rows carry a real state once the CLI probe has finished.
    wait_gone(&mut j.h, "detecting", 20);
    assert!(
        selected_line(&j.h).is_some_and(|l| l.contains("Claude")),
        "/auth lands on the Claude row:\n{}",
        j.h.screen_contents()
    );
}

/// P3 (logged in): the vendor row reads `✓ Max`; it opens into the CLI's own list; selecting a
/// Claude model routes a turn through the adapter and renders it; the composer names the model
/// (no vendor prefix); Ctrl+C cancels a turn.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake CLIs, no network); run with --include-ignored"]
fn rails_ready_adapter_turn_renders_and_cancels() {
    let Some(bin) = bin_from_env() else { return };
    let fakes = install_fakes(true);
    let mut j = spawn("rails-ready", &bin, &[], Some(&fakes.bin));
    open_subscriptions(&mut j);
    // The Claude row (first) reports logged in → `✓ Max ▸` once its CLI has listed its models
    // (the fake's `initialize` answer; the plan from its account), never a placeholder list.
    wait_for(&mut j.h, "\u{2713} Max", 20);
    wait_for(&mut j.h, "3 models", 20);
    snapshot(&j.h, &j.dir, "01-rails-ready");
    let screen = j.h.screen_contents();
    assert!(
        !screen.contains("Claude Opus") && !screen.contains("Codex default model"),
        "placeholder model rows are gone:\n{screen}"
    );
    assert!(
        !screen.contains("Opus (1M context)"),
        "a vendor's models live in its sub-menu, not inline:\n{screen}"
    );
    for pill in [
        "[Ready]",
        "[Sign in]",
        "[Install]",
        "Tab: Models",
        "Tab: Subscriptions",
    ] {
        assert!(
            !screen.contains(pill),
            "no pill, no tab ({pill}):\n{screen}"
        );
    }
    // Enter opens the Claude sub-menu (the CLI's list, default first), Enter again selects the
    // first model: the CLI's default.
    j.h.inject_keys(b"\r").unwrap();
    wait_for(&mut j.h, "Models \u{203a} Claude", 10);
    wait_for(&mut j.h, "Opus (1M context)", 10);
    assert!(
        selected_line(&j.h).is_some_and(|l| l.contains("Default (recommended)")),
        "the sub-menu opens on the CLI's default:\n{}",
        j.h.screen_contents()
    );
    snapshot(&j.h, &j.dir, "02-claude-rail-detail");
    j.h.inject_keys(b"\r").unwrap();
    // The anonymous session activates (async); the overlay closes and the agent composer names
    // the selected model only (no `Claude ·` prefix, no runtime).
    if let Err(e) =
        j.h.wait_for_text_absent(SUBSCRIPTIONS_OVERLAY, Duration::from_secs(120))
    {
        panic!(
            "overlay never closed after selecting a Claude model: {e}\n{}",
            j.h.screen_contents()
        );
    }
    // The composer border names the picked model only — the CLI's own label for its default —
    // never a `Claude ·` prefix or a runtime name.
    let started = Instant::now();
    loop {
        let footer =
            j.h.screen_contents()
                .lines()
                .rev()
                .find(|l| l.contains('\u{256f}'))
                .map(str::to_owned)
                .unwrap_or_default();
        if footer.contains("Default (recommended)") {
            assert!(
                !footer.contains("Claude \u{00b7}"),
                "the composer label is the model only: {footer}"
            );
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the composer never named the picked Claude model:\n{}",
            j.h.screen_contents()
        );
        j.h.update(Duration::from_millis(200));
    }
    snapshot(&j.h, &j.dir, "03-connected-claude");

    // A turn routes through the adapter (RunHandle spawns the fake `claude`), which replays the
    // success fixture; its assistant text renders in the scrollback.
    j.h.inject_keys(b"Summarize the README.").unwrap();
    j.h.update(Duration::from_millis(300));
    j.h.inject_keys(b"\r").unwrap();
    wait_for(&mut j.h, "The README says Demo.", 30);
    snapshot(&j.h, &j.dir, "04-adapter-turn-rendered");

    // Cancel a fresh turn mid-stream (Ctrl+C twice; Esc is swallowed upstream). The fake sleeps
    // between fixture lines, so the turn is still streaming when the cancel lands.
    j.h.inject_keys(b"Read the file again, slowly.").unwrap();
    j.h.update(Duration::from_millis(300));
    j.h.inject_keys(b"\r").unwrap();
    wait_for(&mut j.h, "Let me look at", 20); // first streamed delta of the new turn
    j.h.inject_keys(b"\x03").unwrap();
    j.h.update(Duration::from_millis(300));
    j.h.inject_keys(b"\x03").unwrap();
    if let Err(e) = j.h.wait_for_text("Turn cancelled", Duration::from_secs(20)) {
        panic!(
            "adapter turn was not cancelled: {e}\n{}",
            j.h.screen_contents()
        );
    }
    assert!(
        j.h.is_running().unwrap_or(false),
        "TUI must stay alive after cancel:\n{}",
        j.h.screen_contents()
    );
    snapshot(&j.h, &j.dir, "05-after-cancel");
    finish(
        j,
        "P3 rails (logged-in fakes): Claude rail Ready → model select → adapter turn renders \
         (fixture replay) → cancel. No network (fake CLIs).\n",
    );
}

/// From `/auth`, connect the Claude rail's default model; the composer then names it.
fn connect_claude_default(j: &mut Journey) {
    open_subscriptions(j);
    wait_for(&mut j.h, "[Ready]", 15);
    wait_for(&mut j.h, "3 models", 20);
    j.h.inject_keys(b"\r").unwrap();
    wait_for(&mut j.h, "Opus (1M context)", 10);
    j.h.inject_keys(b"\r").unwrap();
    if let Err(e) =
        j.h.wait_for_text_absent(SUBSCRIPTIONS_OVERLAY, Duration::from_secs(120))
    {
        panic!(
            "overlay never closed after selecting a Claude model: {e}\n{}",
            j.h.screen_contents()
        );
    }
    wait_for(&mut j.h, "Default (recommended)", 30);
}

/// The composer's bottom border, which names the model and, except in Normal mode, the mode.
fn composer_border(screen: &str) -> String {
    screen
        .lines()
        .rev()
        .find(|l| l.contains('\u{256f}'))
        .map(str::to_owned)
        .unwrap_or_default()
}

/// Shift+Tab until the composer reads `mode` (`plan`, `auto`, `always-approve`; Normal shows no
/// label) next to the connected model.
fn set_mode(j: &mut Journey, mode: &str) {
    for _ in 0..6 {
        let border = composer_border(&j.h.screen_contents());
        let current_is = |m: &str| border.contains(&format!("\u{b7} {m}"));
        let at_target = match mode {
            "normal" => !["plan", "auto", "always-approve"]
                .iter()
                .any(|m| current_is(m)),
            m => current_is(m),
        };
        if at_target {
            return;
        }
        j.h.inject_keys(b"\x1b[Z").unwrap();
        j.h.update(Duration::from_millis(400));
    }
    panic!(
        "could not reach mode {mode}\nscreen:\n{}",
        j.h.screen_contents()
    );
}

fn send_prompt(j: &mut Journey, text: &str) {
    j.h.inject_keys(text.as_bytes()).unwrap();
    j.h.update(Duration::from_millis(300));
    j.h.inject_keys(b"\r").unwrap();
}

fn lines_of(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Wait for the turn's `Worked for` line; the fake exits when its fixture is replayed.
fn wait_turn_done(j: &mut Journey, secs: u64) {
    wait_for(&mut j.h, "Worked for", secs);
    j.h.update(Duration::from_millis(500));
}

/// P3 modes (logged-in fakes, Claude rail): Normal is the CLI's write-capable mode that asks —
/// `acceptEdits` on its stdio prompt tool: the CLI's question opens Workshop's question view and
/// the answer goes back on the channel; its `can_use_tool` for a command opens Workshop's approval
/// card, "Yes, run it" goes back as `allow` and the turn completes. Plan is the one read-only
/// mode (`--permission-mode plan`); always-approve is the CLI's run-everything mode
/// (`bypassPermissions`), and neither Normal nor Auto ever runs a plan mode.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake CLIs, no network); run with --include-ignored"]
fn rails_modes_normal_asks_with_the_approval_card_plan_is_read_only() {
    let Some(bin) = bin_from_env() else { return };
    let fakes = install_fakes(true);
    let claude_state = fakes.state.join("claude");
    let mut j = spawn("rails-modes", &bin, &[], Some(&fakes.bin));
    connect_claude_default(&mut j);
    snapshot(&j.h, &j.dir, "01-connected-claude");

    // 1. Normal mode: the fake replays the ask fixture (a question, then a `cp` the permission
    //    mode prompts for).
    std::fs::copy(
        fixtures_dir().join("claude_ask.jsonl"),
        claude_state.join("fixture.jsonl"),
    )
    .unwrap();
    set_mode(&mut j, "normal");
    snapshot(&j.h, &j.dir, "02-normal-mode");
    send_prompt(&mut j, "make the page use my wallpaper");
    // The CLI's question is Workshop's question view: pick the second option (Down, Enter).
    wait_for(&mut j.h, "Which wallpaper should the page use?", 30);
    wait_for(&mut j.h, "Ocean", 5);
    snapshot(&j.h, &j.dir, "03-question-view");
    j.h.inject_keys(b"\x1b[B").unwrap();
    j.h.update(Duration::from_millis(300));
    j.h.inject_keys(b"\r").unwrap();
    // The command the mode prompts for is Workshop's approval card, with the command in full.
    wait_for(&mut j.h, "Run this command?", 30);
    wait_for(&mut j.h, "cp ~/Pictures/ocean.png assets/", 5);
    wait_for(&mut j.h, "Yes, run it", 5);
    j.h.update(Duration::from_millis(400));
    snapshot(&j.h, &j.dir, "04-approval-card");
    assert!(
        lines_of(&claude_state.join("replies.txt")).len() == 1,
        "the command waits for the answer: {:?}",
        lines_of(&claude_state.join("replies.txt"))
    );
    j.h.inject_keys(b"1").unwrap(); // Yes, run it
    wait_for(&mut j.h, "The ocean wallpaper is in assets/.", 30);
    wait_turn_done(&mut j, 30);
    snapshot(&j.h, &j.dir, "05-normal-turn-done");
    let replies: Vec<serde_json::Value> = lines_of(&claude_state.join("replies.txt"))
        .iter()
        .map(|l| serde_json::from_str(l).expect("control responses are JSON"))
        .collect();
    assert_eq!(replies.len(), 2, "{replies:?}");
    assert_eq!(replies[0]["response"]["request_id"], "req_q");
    assert_eq!(
        replies[0]["response"]["response"]["updatedInput"]["answers"]["Which wallpaper should the page use?"],
        "Ocean"
    );
    assert_eq!(replies[1]["response"]["request_id"], "req_cp");
    assert_eq!(replies[1]["response"]["response"]["behavior"], "allow");
    let screen = j.h.screen_contents();
    for plumbing in [
        "AskUserQuestion",
        "can_use_tool",
        "toolu_",
        "control_request",
    ] {
        assert!(
            !screen.contains(plumbing),
            "{plumbing} is plumbing, never on screen:\n{screen}"
        );
    }
    let argv = lines_of(&claude_state.join("turn_argv.txt"));
    assert_eq!(argv.len(), 1, "{argv:?}");
    assert!(
        argv[0].contains("--permission-mode acceptEdits")
            && argv[0].contains("--permission-prompt-tool stdio"),
        "Normal is the write-capable mode with the prompt tool: {argv:?}"
    );

    // 2. Plan mode is the read-only mode; always-approve the run-everything one. The success
    //    fixture asks nothing, so each turn completes on its own.
    std::fs::copy(
        fixtures_dir().join("claude_success.jsonl"),
        claude_state.join("fixture.jsonl"),
    )
    .unwrap();
    set_mode(&mut j, "plan");
    send_prompt(&mut j, "summarize the README");
    wait_turn_done(&mut j, 60);
    set_mode(&mut j, "always-approve");
    send_prompt(&mut j, "summarize the README again");
    wait_turn_done(&mut j, 60);
    snapshot(&j.h, &j.dir, "06-plan-and-always-approve-turns");
    let argv = lines_of(&claude_state.join("turn_argv.txt"));
    assert_eq!(argv.len(), 3, "{argv:?}");
    assert!(
        argv[1].contains("--permission-mode plan"),
        "Plan is the read-only mode: {argv:?}"
    );
    assert!(
        argv[2].contains("--permission-mode bypassPermissions"),
        "always-approve is the run-everything mode: {argv:?}"
    );
    assert!(
        argv.iter()
            .filter(|a| a.contains("--permission-mode plan"))
            .count()
            == 1,
        "only Plan runs the plan mode: {argv:?}"
    );
    finish(
        j,
        "P3 modes (logged-in fakes, Claude rail): Normal → acceptEdits + stdio prompt tool; the \
         CLI's question opened the question view and the answer went back on the channel; its \
         can_use_tool for `cp` opened the approval card and `Yes, run it` went back as allow; the \
         turn completed. Plan → --permission-mode plan; always-approve → bypassPermissions. \
         No network (fake CLIs).\n",
    );
}

/// P3 (logged out): the vendor rows read `sign in`; Enter on the Claude row launches
/// `claude auth login`.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake CLIs, no network); run with --include-ignored"]
fn rails_signin_connect_launches_login() {
    let Some(bin) = bin_from_env() else { return };
    let fakes = install_fakes(false);
    let mut j = spawn("rails-signin", &bin, &[], Some(&fakes.bin));
    open_subscriptions(&mut j);
    wait_for(&mut j.h, "sign in", 15);
    let screen = j.h.screen_contents();
    assert!(
        selected_line(&j.h).is_some_and(|l| l.contains("Claude") && l.contains("sign in")),
        "the signed-out Claude row reads `sign in`:\n{screen}"
    );
    assert!(
        screen.contains("in your terminal:  claude auth login"),
        "the detail names the login command:\n{screen}"
    );
    snapshot(&j.h, &j.dir, "01-rails-signin");
    // Enter on the signed-out Claude row is Connect → suspends the TUI and runs the vendor's
    // documented login command (`claude auth login`) attached to the terminal. (A second Enter
    // would reach the login child's stdin, or start a second login.)
    j.h.inject_keys(b"\r").unwrap();
    // The fake `claude auth login` touches a marker and exits; the TUI resumes.
    let marker = fakes.state.join("claude").join("login_ran");
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(30) {
        j.h.update(Duration::from_millis(300));
        if marker.exists() {
            break;
        }
    }
    snapshot(&j.h, &j.dir, "02-after-connect");
    assert!(
        marker.exists(),
        "Connect must launch `claude auth login` (marker missing):\n{}",
        j.h.screen_contents()
    );
    // The TUI is back (alternate screen, overlay frame) and the rails are re-probed.
    wait_for(&mut j.h, SUBSCRIPTIONS_OVERLAY, 15);
    assert!(
        j.h.terminal_modes().alt_screen,
        "TUI must re-enter the alternate screen after the login child exits"
    );
    // Focus is back on the list: ↓ moves from Claude to Codex.
    wait_gone(&mut j.h, "re-probing", 15);
    j.h.inject_keys(b"\x1b[B").unwrap();
    j.h.update(Duration::from_millis(400));
    assert!(
        selected_line(&j.h).is_some_and(|l| l.contains("Codex")),
        "Down must move to the Codex row right after the login returns:\n{}",
        j.h.screen_contents()
    );
    snapshot(&j.h, &j.dir, "03-down-moves-to-codex");
    finish(
        j,
        "P3 rails (logged-out fakes): Claude rail Sign in → Connect launched `claude auth login` \
         in the terminal (marker written); after it exited the TUI re-entered the alternate \
         screen and ↓ moved to the next rail. No network (fake CLIs).\n",
    );
}

/// P3 (logged in, a CLI that cannot list its models): the rail says "Couldn't load models — press
/// Enter to retry", never placeholder rows, and Enter on it asks that CLI again (and nothing else:
/// no hosted list is fetched); once the CLI answers, the rail lists its models.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake CLIs, no network); run with --include-ignored"]
fn rails_failed_models_retry_on_enter() {
    use workshop_detect::copy::MODELS_FAILED;
    let Some(bin) = bin_from_env() else { return };
    let fakes = install_fakes(true);
    let cursor = fakes.state.join("cursor-agent");
    std::fs::write(cursor.join("models_fail"), "1").unwrap();
    let calls = || {
        std::fs::read_to_string(cursor.join("models_calls"))
            .unwrap_or_default()
            .lines()
            .count()
    };
    let mut j = spawn("rails-models-retry", &bin, &[], Some(&fakes.bin));
    open_subscriptions(&mut j);
    wait_for(&mut j.h, MODELS_FAILED, 20);
    snapshot(&j.h, &j.dir, "01-cursor-models-failed");
    let before = calls();
    // Cursor is the third vendor row.
    for _ in 0..2 {
        j.h.inject_keys(b"\x1b[B").unwrap();
        j.h.update(Duration::from_millis(300));
    }
    assert!(
        selected_line(&j.h).is_some_and(|l| l.contains("Cursor") && l.contains(MODELS_FAILED)),
        "the Cursor row is selected and carries the failure as its state:\n{}",
        j.h.screen_contents()
    );
    std::fs::remove_file(cursor.join("models_fail")).unwrap();
    j.h.inject_keys(b"\r").unwrap();
    if let Err(e) =
        j.h.wait_for_text_absent(MODELS_FAILED, Duration::from_secs(30))
    {
        panic!(
            "Enter did not retry the model list: {e}\n{}",
            j.h.screen_contents()
        );
    }
    assert!(calls() > before, "Enter must ask cursor-agent again");
    wait_for(&mut j.h, "2 models", 10);
    assert!(
        selected_line(&j.h).is_some_and(|l| l.contains("Cursor")
            && l.contains("\u{2713}")
            && l.contains("\u{25b8}")),
        "the Cursor row is signed in and opens into the models its CLI reported:\n{}",
        j.h.screen_contents()
    );
    snapshot(&j.h, &j.dir, "02-cursor-models-after-retry");
    finish(
        j,
        "P3 rails (logged-in fakes, cursor-agent models failing): Cursor rail showed \
         \"Couldn't load models — press Enter to retry\"; Enter re-asked cursor-agent, which then \
         answered, and the rail listed its 2 models. No network (fake CLIs).\n",
    );
}

/// The `›`-marked line of the screen (the selected row / rail).
fn selected_line(h: &PtyHarness) -> Option<String> {
    h.screen_contents()
        .lines()
        .find(|l| l.contains('\u{203a}') && !l.contains("Models \u{203a}"))
        .map(str::to_owned)
}

/// P3 (logged out, hanging login): while the vendor login owns the terminal the TUI has left the
/// alternate screen (its prompt is not drawn over the picker frame); Ctrl+C ends the login child
/// only, and Workshop returns to the picker with a "sign-in cancelled" status and working ↑/↓.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake CLIs, no network); run with --include-ignored"]
fn rails_signin_ctrl_c_cancels_only_the_vendor_login() {
    let Some(bin) = bin_from_env() else { return };
    let fakes = install_fakes(false);
    let claude_state = fakes.state.join("claude");
    std::fs::write(claude_state.join("login_hang"), "1").unwrap();
    let mut j = spawn("rails-signin-ctrl-c", &bin, &[], Some(&fakes.bin));
    open_subscriptions(&mut j);
    wait_for(&mut j.h, "sign in", 15);
    assert!(
        j.h.terminal_modes().alt_screen,
        "the TUI runs on the alternate screen"
    );
    // Enter on a signed-out vendor row is Connect: the vendor login owns the terminal from here.
    j.h.inject_keys(b"\r").unwrap();
    let marker = claude_state.join("login_ran");
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(30) && !marker.exists() {
        j.h.update(Duration::from_millis(200));
    }
    assert!(
        marker.exists(),
        "Connect must launch the fake login:\n{}",
        j.h.screen_contents()
    );
    j.h.update(Duration::from_millis(800));
    // The login has the shell's screen to itself: no picker frame under its output.
    let screen = j.h.screen_contents();
    assert!(
        !j.h.terminal_modes().alt_screen,
        "TUI must leave the alternate screen while the vendor login runs:\n{screen}"
    );
    assert!(
        screen.contains("Workshop: running") && screen.contains("Opening browser to sign in"),
        "banner and the login's own output must be visible:\n{screen}"
    );
    assert!(
        !screen.contains(SUBSCRIPTIONS_OVERLAY),
        "the overlay frame must not be visible under the login output:\n{screen}"
    );
    snapshot(&j.h, &j.dir, "01-login-owns-the-terminal");

    // Ctrl+C in the (cooked-mode) terminal: SIGINT to the foreground process group.
    j.h.inject_keys(b"\x03").unwrap();
    wait_for(&mut j.h, "sign-in cancelled", 20);
    wait_for(&mut j.h, SUBSCRIPTIONS_OVERLAY, 5);
    assert!(
        j.h.is_running().unwrap_or(false),
        "Workshop must survive the Ctrl+C that ended the login:\n{}",
        j.h.screen_contents()
    );
    assert!(
        j.h.terminal_modes().alt_screen,
        "TUI must be back on the alternate screen"
    );
    let screen = j.h.screen_contents();
    assert!(
        selected_line(&j.h).is_some_and(|l| l.contains("Claude") && l.contains("sign in"))
            && !screen.contains("re-probing"),
        "a cancelled login leaves the vendor rows as they were:\n{screen}"
    );
    snapshot(&j.h, &j.dir, "02-cancelled-back-in-picker");
    // The login child (and its `sleep`) is gone.
    let still_running = std::process::Command::new("pgrep")
        .args([
            "-f",
            &format!("{} auth login", fakes.bin.join("claude").display()),
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(!still_running, "the fake login must not outlive Ctrl+C");
    // Focus is back on the list.
    j.h.inject_keys(b"\x1b[B").unwrap();
    j.h.update(Duration::from_millis(400));
    assert!(
        selected_line(&j.h).is_some_and(|l| l.contains("Codex")),
        "Down must move to the Codex row after the cancelled login:\n{}",
        j.h.screen_contents()
    );
    snapshot(&j.h, &j.dir, "03-down-moves-to-codex");
    j.h.write_cast(&j.dir.join("rails-signin-ctrl-c.cast"))
        .expect("write asciinema cast");
    finish(
        j,
        "P3 rails (logged-out fakes, hanging login): Connect left the alternate screen for the \
         vendor login; Ctrl+C ended only the login child; Workshop returned to the /auth overlay \
         with a sign-in cancelled status, rails unchanged, ↓ working. No network (fake CLIs).\n",
    );
}
