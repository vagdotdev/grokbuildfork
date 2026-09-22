//! Live end-to-end turns through the real TUI (milestones C/D/E user journeys), opt-in because
//! they need the network and/or an installed `opencode`:
//!
//! * `WORKSHOP_LIVE_KILO=1`   — P2a: cold start → picker → Kilo "Free Models Router" (`:free`,
//!   keyless) → a prompt whose answer needs a file-write tool call → the file exists on disk.
//! * `WORKSHOP_LIVE_OPENCODE=1` — P2b: cold start → picker → "Big Pickle (OpenCode default)" →
//!   the OpenCode engine (`opencode serve`) runs the turn → a tool call → the file exists on disk.
//!
//! Both need `WORKSHOP_BIN` (the built `workshop` binary) and `--include-ignored`. When `strace` is
//! on `PATH` the TUI runs under `strace -f -e trace=network`, so the evidence directory also holds
//! the exact DNS names / connect() targets of the whole journey (`scripts/no-egress/summarize.py`
//! renders it). Tool calls are auto-approved (`--yolo`) so the run is unattended; the approval UI is
//! upstream's and untouched.
//!
//! Evidence (text + HTML screenshots, strace) lands in `WORKSHOP_PTY_EVIDENCE_DIR`
//! (default `target/pty-evidence/live-<journey>`).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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

fn wait_for(h: &mut PtyHarness, text: &str, secs: u64) {
    if let Err(e) = h.wait_for_text(text, Duration::from_secs(secs)) {
        panic!(
            "timed out waiting for {text:?}: {e}\nscreen:\n{}",
            h.screen_contents()
        );
    }
}

/// Press Down until the `›`-marked (selected) row contains `needle`.
fn move_selection_to(h: &mut PtyHarness, needle: &str) {
    for _ in 0..60 {
        if h.screen_contents()
            .lines()
            .any(|l| l.contains('\u{203a}') && l.contains(needle))
        {
            return;
        }
        h.inject_keys(b"\x1b[B").unwrap();
        h.update(Duration::from_millis(80));
    }
    panic!(
        "never reached a selected row containing {needle:?}\nscreen:\n{}",
        h.screen_contents()
    );
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

/// Pick `row` in the Models tab and wait for the picker to hand over to the home prompt.
fn pick_model_row(j: &mut Journey, row: &str, ready_text: &str) {
    wait_for(&mut j.h, "connect a model", 45);
    move_selection_to(&mut j.h, row);
    snapshot(&j.h, &j.dir, "01-picker-row-selected");
    j.h.inject_keys(b"\r").unwrap();
    // The picker closes once the shell has the model (Direct API: config.toml written + models
    // reloaded; engine: `opencode serve` up + catalog fetched).
    if let Err(e) =
        j.h.wait_for_text_absent("connect a model", Duration::from_secs(120))
    {
        panic!(
            "picker never closed after selecting {row:?}: {e}\nscreen:\n{}",
            j.h.screen_contents()
        );
    }
    if !ready_text.is_empty() {
        wait_for(&mut j.h, ready_text, 30);
    }
    j.h.update(Duration::from_millis(1500));
    snapshot(&j.h, &j.dir, "02-home-connected");
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

/// P2a: Kilo `:free` (Direct API, keyless) → real tool call through Workshop's own agent loop.
#[test]
#[ignore = "live: needs network + WORKSHOP_BIN; run with WORKSHOP_LIVE_KILO=1 --include-ignored"]
fn kilo_free_router_turn_with_tool_call() {
    if std::env::var_os("WORKSHOP_LIVE_KILO").is_none() {
        eprintln!("WORKSHOP_LIVE_KILO not set; skipping");
        return;
    }
    let Some(bin) = bin_from_env() else { return };
    let mut j = spawn("kilo", &bin, &[], None);
    pick_model_row(&mut j, "Free Models Router", "Free Models Router");
    let screen = j.h.screen_contents();
    assert!(
        !screen.contains("auth.x.ai") && !screen.contains("grok.com"),
        "connected home shows an xAI path:\n{screen}"
    );
    let text = run_write_turn(&mut j, 180);
    assert!(text.contains(FILE_CONTENT), "file content: {text:?}");
    let strace = assert_no_forbidden_egress(&j.dir);
    if !strace.is_empty() {
        assert!(
            strace.contains("kilo"),
            "expected a DNS query for api.kilo.ai in the strace"
        );
    }
    finish(
        j,
        "Kilo Free Models Router (:free) via Workshop's own agent loop. Expected hosts in strace.log: \
         api.kilo.ai only (plus loopback local-server probes and the OS keyring socket).\n",
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
    // After selecting, the app lands in an agent composer whose label names the engine connection.
    pick_model_row(&mut j, "Big Pickle", "Big Pickle \u{00b7} OpenCode");
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
// network and no real account are needed. Proves: rails show Ready when the fake reports logged in;
// selecting a model routes a turn through the adapter and renders it with the `Claude · {model}`
// label; cancel works; and a logged-out fake shows Sign in, where Connect launches the vendor's
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
  *status*)
    if [ -f "$state/logged_in" ]; then printf '%s\n' {status_in}{status_redirect}; exit {in_exit}
    else printf '%s\n' {status_out}{status_redirect}; exit {out_exit}; fi ;;
  {login_args})
    touch "$state/login_ran"; printf 'Opening browser to sign in...\n'; exit 0 ;;
esac
# Otherwise this is a turn: replay the adapter success fixture line by line. A per-line sleep keeps
# the turn on-screen long enough to be cancelled mid-stream (SIGINT from the supervisor stops it).
trap 'exit 130' INT TERM
while IFS= read -r line || [ -n "$line" ]; do printf '%s\n' "$line"; sleep 0.3; done < "$state/fixture.jsonl"
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
    Fakes {
        _dir: dir,
        bin,
        state,
    }
}

/// Open the picker's Subscriptions tab and wait for the three rails.
fn open_subscriptions(j: &mut Journey) {
    wait_for(&mut j.h, "connect a model", 30);
    j.h.inject_keys(b"\t").unwrap(); // Models -> Subscriptions
    wait_for(&mut j.h, "Claude", 10);
    wait_for(&mut j.h, "Codex", 5);
    wait_for(&mut j.h, "Cursor", 5);
}

/// P3 (logged in): rails show Ready; selecting a Claude model routes a turn through the adapter and
/// renders it; the composer is labeled `Claude · …`; Ctrl+C cancels a turn.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake CLIs, no network); run with --include-ignored"]
fn rails_ready_adapter_turn_renders_and_cancels() {
    let Some(bin) = bin_from_env() else { return };
    let fakes = install_fakes(true);
    let mut j = spawn("rails-ready", &bin, &[], Some(&fakes.bin));
    open_subscriptions(&mut j);
    // The Claude rail (first) reports logged in → Ready.
    wait_for(&mut j.h, "Ready", 15);
    snapshot(&j.h, &j.dir, "01-rails-ready");
    // Enter opens the Claude rail detail (its model radios), Enter again selects the first model.
    j.h.inject_keys(b"\r").unwrap();
    j.h.update(Duration::from_millis(400));
    snapshot(&j.h, &j.dir, "02-claude-rail-detail");
    j.h.inject_keys(b"\r").unwrap();
    // The anonymous session activates (async); the picker closes and an agent composer appears,
    // labeled for the adapter connection (`Claude · …`).
    if let Err(e) =
        j.h.wait_for_text_absent("connect a model", Duration::from_secs(120))
    {
        panic!(
            "picker never closed after selecting a Claude model: {e}\n{}",
            j.h.screen_contents()
        );
    }
    wait_for(&mut j.h, "Claude \u{00b7}", 30);
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

/// P3 (logged out): rails show Sign in; Connect on the Claude rail launches `claude auth login`.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake CLIs, no network); run with --include-ignored"]
fn rails_signin_connect_launches_login() {
    let Some(bin) = bin_from_env() else { return };
    let fakes = install_fakes(false);
    let mut j = spawn("rails-signin", &bin, &[], Some(&fakes.bin));
    open_subscriptions(&mut j);
    wait_for(&mut j.h, "Sign in", 15);
    snapshot(&j.h, &j.dir, "01-rails-signin");
    // Enter opens the Claude rail detail; Enter again is Connect → suspends the TUI and runs the
    // vendor's documented login command (`claude auth login`) attached to the terminal.
    j.h.inject_keys(b"\r").unwrap();
    j.h.update(Duration::from_millis(400));
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
    finish(
        j,
        "P3 rails (logged-out fakes): Claude rail Sign in → Connect launched `claude auth login` \
         in the terminal (marker written). No network (fake CLIs).\n",
    );
}
