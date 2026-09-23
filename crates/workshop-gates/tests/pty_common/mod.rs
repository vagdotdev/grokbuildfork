//! Shared PTY helpers for the Workshop product gates that drive the built `workshop` binary on a
//! fresh HOME (opt-in via `WORKSHOP_BIN`, run with `--include-ignored`).

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use xai_grok_pager_pty_harness::PtyHarness;

pub fn bin_from_env() -> Option<PathBuf> {
    let bin = std::env::var_os("WORKSHOP_BIN")?;
    let bin = PathBuf::from(bin);
    if !bin.is_file() {
        eprintln!("WORKSHOP_BIN={} is not a file; skipping", bin.display());
        return None;
    }
    Some(bin)
}

pub fn evidence_dir(journey: &str) -> PathBuf {
    let root = std::env::var_os("WORKSHOP_PTY_EVIDENCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/pty-evidence"));
    let dir = root.join(journey);
    std::fs::create_dir_all(&dir).expect("create evidence dir");
    dir
}

pub fn snapshot(h: &PtyHarness, dir: &Path, name: &str) {
    std::fs::write(dir.join(format!("{name}.txt")), h.screen_contents()).expect("write txt");
    std::fs::write(dir.join(format!("{name}.html")), h.screen_html()).expect("write html");
}

pub fn wait_for(h: &mut PtyHarness, text: &str, secs: u64) {
    if let Err(e) = h.wait_for_text(text, Duration::from_secs(secs)) {
        panic!(
            "timed out waiting for {text:?}: {e}\nscreen:\n{}",
            h.screen_contents()
        );
    }
}

/// Press Down until the `›`-marked row contains `needle`.
pub fn move_selection_to(h: &mut PtyHarness, needle: &str) {
    for _ in 0..40 {
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

pub struct Journey {
    pub h: PtyHarness,
    pub dir: PathBuf,
    pub home: tempfile::TempDir,
    pub cwd: tempfile::TempDir,
}

impl Journey {
    pub fn workshop_home(&self) -> PathBuf {
        self.home.path().join(".workshop")
    }
}

/// Spawn the TUI on a fresh HOME in a fresh git repo. `extra_path` is prepended to `PATH`;
/// `extra_env` is appended after the fixed isolation variables (so it can add `GROK_HOME`).
pub fn spawn(
    journey: &str,
    bin: &Path,
    extra_env: &[(&str, &str)],
    extra_path: Option<&Path>,
) -> Journey {
    spawn_in(
        journey,
        bin,
        extra_env,
        extra_path,
        tempfile::tempdir().expect("tempdir"),
    )
}

/// [`spawn`] on a HOME the test has prepared (foreign config to be ignored, hooks to run, …).
pub fn spawn_in(
    journey: &str,
    bin: &Path,
    extra_env: &[(&str, &str)],
    extra_path: Option<&Path>,
    home: tempfile::TempDir,
) -> Journey {
    spawn_in_with_args(journey, bin, &[], extra_env, extra_path, home)
}

/// [`spawn`] with command-line arguments for the binary (`--yolo`).
pub fn spawn_with_args(
    journey: &str,
    bin: &Path,
    args: &[&str],
    extra_env: &[(&str, &str)],
    extra_path: Option<&Path>,
) -> Journey {
    spawn_in_with_args(
        journey,
        bin,
        args,
        extra_env,
        extra_path,
        tempfile::tempdir().expect("tempdir"),
    )
}

pub fn spawn_in_with_args(
    journey: &str,
    bin: &Path,
    args: &[&str],
    extra_env: &[(&str, &str)],
    extra_path: Option<&Path>,
    home: tempfile::TempDir,
) -> Journey {
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
    let mut h = PtyHarness::new_inherited_env(bin, 45, 140, args, &env, Some(cwd.path()))
        .expect("spawn workshop in pty");
    h.set_respond_to_queries(true);
    Journey { h, dir, home, cwd }
}

/// Install the fixture fake `opencode` (see `tests/fixtures/fake-opencode.sh`) in a fresh `bin/`
/// with the given failure `mode`; returns the directory to prepend to `PATH`.
pub fn fake_opencode(mode: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    install_fake_opencode_into(dir.path(), mode);
    dir
}

/// Write the fixture fake `opencode` (and its `serve` stand-in + `mode` file) into an existing
/// `bin/`. A gate that spawns a fresh HOME without one would have the launch-time engine warm-up
/// run the real vendor installer; the fake keeps it hermetic.
pub fn install_fake_opencode_into(bin: &Path, mode: &str) {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for (src, dst) in [
        ("fake-opencode.sh", "opencode"),
        ("fake-opencode-serve.py", "fake-opencode-serve.py"),
    ] {
        std::fs::copy(fixtures.join(src), bin.join(dst)).expect("copy fixture");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(bin.join("opencode"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    std::fs::write(bin.join("mode"), mode).unwrap();
}

/// A fake `opencode` whose `serve` answers: the shared stand-in
/// `tests/fixtures/fake-opencode-serve-turn.py` serves the adapter crate's captured
/// `/config/providers` (eight free models, some with effort variants) and replays its captured
/// turn; every `prompt_async` body is appended to `record` as one JSON line. Returns the
/// directory to prepend to `PATH`.
pub fn fake_opencode_answering(record: &Path) -> tempfile::TempDir {
    let turn = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../workshop-adapters/tests/fixtures/opencode_serve_turn.jsonl");
    fake_opencode_answering_with(record, &turn, 0.01)
}

/// [`fake_opencode_answering`] replaying the given turn (JSON lines of `opencode serve` events)
/// with `pace` seconds between events, so a gate can watch the transcript mid-turn.
pub fn fake_opencode_answering_with(record: &Path, turn: &Path, pace: f64) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let adapter_fixtures =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../workshop-adapters/tests/fixtures");
    let script = format!(
        r#"#!/bin/sh
case "$1" in
  --version) echo '1.18.31'; exit 0 ;;
  --help) printf 'Commands:\n  opencode run [message..]     run opencode with a message\n' >&2; exit 0 ;;
esac
if [ "$*" = "auth list" ]; then
  printf '%s\n' '┌  Credentials ~/.local/share/opencode/auth.json' '│' '└  0 credentials'; exit 0
fi
if [ "$1" = "serve" ]; then
  exec python3 '{serve}' --port "$5" --providers '{providers}' --turn '{turn}' --record '{record}' --pace {pace}
fi
echo "fake opencode: unexpected $*" >&2
exit 2
"#,
        serve = fixtures.join("fake-opencode-serve-turn.py").display(),
        providers = adapter_fixtures
            .join("opencode_serve_providers.json")
            .display(),
        turn = turn.display(),
        record = record.display(),
    );
    let path = dir.path().join("opencode");
    std::fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir
}

/// First run (type and go): the composer is up with OpenCode's default model, `Big Pickle`, named
/// in its footer (the model only, no provider); no picker.
pub fn connect_big_pickle(j: &mut Journey) {
    wait_for(&mut j.h, "\u{276f}", 45);
    wait_for(&mut j.h, "Big Pickle", 30);
    j.h.update(Duration::from_millis(1200));
    let screen = j.h.screen_contents();
    assert!(
        !screen.contains("connect a model"),
        "a fresh HOME lands in the composer, not a picker:\n{screen}"
    );
    assert!(
        !screen.contains("OpenCode \u{b7} Big Pickle"),
        "the composer names the model only, not `OpenCode · Big Pickle`:\n{screen}"
    );
}

/// Test hook read by the binary: the silent fallback's base URL (see `workshop::KILO_BASE_URL_ENV`).
pub const KILO_BASE_URL_ENV: &str = "WORKSHOP_KILO_BASE_URL";

/// Words a first-time user must never read on screen: runtime and fallback plumbing.
pub const PLUMBING_WORDS: [&str; 6] = [
    "engine",
    "Kilo",
    "fallback",
    "OpenCode unavailable",
    "Starting the",
    "Installing the",
];

/// The pager's own turn-status row while nothing has come back yet: `⠧ Waiting for response… 3s …
/// 5s [stop]` — the same row a shell turn shows, with no plumbing beside it.
pub const WAITING_ROW: &str = "Waiting for response";

/// While the turn waits, the turn-status row reads [`WAITING_ROW`] with no plumbing beside it.
/// Polls until that row shows or `outcome` (the answer, the failure line) has already landed — a
/// failure faster than a frame may skip the waiting row altogether.
pub fn expect_thinking_line(j: &mut Journey, outcome: &str, secs: u64) {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let screen = j.h.screen_contents();
        if screen.contains(WAITING_ROW) {
            assert_no_plumbing(&j.h, "while waiting for the model");
            return;
        }
        if screen.contains(outcome) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "neither the waiting line nor {outcome:?} after {secs}s:\n{screen}"
        );
        j.h.update(Duration::from_millis(50));
    }
}

/// Fail when the screen shows any [`PLUMBING_WORDS`] entry.
pub fn assert_no_plumbing(h: &PtyHarness, step: &str) {
    let screen = h.screen_contents();
    for word in PLUMBING_WORDS {
        assert!(
            !screen.contains(word),
            "{step}: {word:?} is plumbing a user must never read:\n{screen}"
        );
    }
}

/// A loopback OpenAI-compatible endpoint that refuses every request (HTTP 400): the silent
/// fallback's provider for the failure gates. Returns the child (killed on drop) and its base URL
/// for `WORKSHOP_KILO_BASE_URL`.
#[allow(clippy::disallowed_methods)] // short-lived loopback fixture, killed by KillOnDrop below
pub fn refusing_api() -> (KillOnDrop, String) {
    use std::io::BufRead;
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-refusing-api.py");
    let mut child = std::process::Command::new("python3")
        .arg(script)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn fake-refusing-api.py");
    let mut url = String::new();
    std::io::BufReader::new(child.stdout.take().expect("stdout"))
        .read_line(&mut url)
        .expect("fake api prints its url");
    (KillOnDrop(child), url.trim().to_owned())
}

pub struct KillOnDrop(pub std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Type `text` into the composer and press Enter.
pub fn send_prompt(j: &mut Journey, text: &str) {
    j.h.inject_keys(text.as_bytes()).unwrap();
    j.h.update(Duration::from_millis(300));
    j.h.inject_keys(b"\r").unwrap();
}
