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
    let mut h = PtyHarness::new_inherited_env(bin, 45, 140, &[], &env, Some(cwd.path()))
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

/// First run (type and go): the composer is up with the OpenCode engine active; no picker.
pub fn connect_big_pickle(j: &mut Journey) {
    wait_for(&mut j.h, "\u{276f}", 45);
    wait_for(&mut j.h, "OpenCode", 30);
    j.h.update(Duration::from_millis(1200));
    let screen = j.h.screen_contents();
    assert!(
        !screen.contains("connect a model"),
        "a fresh HOME lands in the composer, not a picker:\n{screen}"
    );
}

/// Type `text` into the composer and press Enter.
pub fn send_prompt(j: &mut Journey, text: &str) {
    j.h.inject_keys(text.as_bytes()).unwrap();
    j.h.update(Duration::from_millis(300));
    j.h.inject_keys(b"\r").unwrap();
}
