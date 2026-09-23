//! A running turn is never a still screen: after an approved command starts, the waiting line
//! comes back naming it (spinner, seconds, cancel hint) until its result lands, and the terminal
//! title stops saying "Action Required" once the prompt is answered.
//!
//! Opt-in: set `WORKSHOP_BIN` to the built binary and run with `--include-ignored`.

mod pty_common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use pty_common::*;

/// The scripted engine (`tests/fixtures/fake-engine-serve.py`) as `opencode` in `bin`.
fn install_fake_engine(bin: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(bin).unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let serve_py = bin.join("fake-engine-serve.py");
    std::fs::copy(fixtures.join("fake-engine-serve.py"), &serve_py).unwrap();
    let providers = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../workshop-adapters/tests/fixtures/opencode_serve_providers.json");
    let log = bin.join("engine-requests.jsonl");
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in\n  --version) echo '1.18.31'; exit 0 ;;\n  --help) printf 'opencode run [message..]\\n' >&2; exit 0 ;;\nesac\n\
         if [ \"$1\" = \"serve\" ]; then exec python3 '{}' --port \"$5\" --providers '{}' --log '{}'; fi\nexit 2\n",
        serve_py.display(),
        providers.display(),
        log.display()
    );
    std::fs::write(bin.join("opencode"), script).unwrap();
    std::fs::set_permissions(bin.join("opencode"), std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The last terminal title the app set (OSC 0/2).
fn last_title(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let mut title = String::new();
    for chunk in text.split("\x1b]").skip(1) {
        if let Some(rest) = chunk.strip_prefix("0;").or_else(|| chunk.strip_prefix("2;")) {
            let end = rest.find(['\x07', '\x1b']).unwrap_or(rest.len());
            title = rest[..end].to_owned();
        }
    }
    title
}

fn live_line(screen: &str) -> Option<String> {
    screen
        .lines()
        .find(|l| l.contains("Running `sudo apt-get install -y thing`") && l.contains("Ctrl+C to cancel"))
        .map(|l| l.trim().to_owned())
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode serve); run with --include-ignored"]
fn a_running_command_keeps_a_live_line_and_the_title_clears_after_the_answer() {
    let Some(bin) = bin_from_env() else { return };
    let fakes = tempfile::tempdir().unwrap();
    let fake_bin: PathBuf = fakes.path().join("bin");
    install_fake_engine(&fake_bin);
    let offline = [
        ("HTTP_PROXY", "http://127.0.0.1:9"),
        ("HTTPS_PROXY", "http://127.0.0.1:9"),
        ("ALL_PROXY", "http://127.0.0.1:9"),
    ];
    let mut j = spawn("turn-liveness", &bin, &offline, Some(&fake_bin));
    connect_big_pickle(&mut j);
    send_prompt(&mut j, "install the thing");
    wait_for(&mut j.h, "Yes, run it", 60);
    snapshot(&j.h, &j.dir, "01-prompt");
    j.h.inject_keys(b"1").unwrap();

    // The command runs for ~7 s after the answer: the line must be back within ~3 s and tick.
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut first = None;
    while Instant::now() < deadline && first.is_none() {
        j.h.update(Duration::from_millis(150));
        first = live_line(&j.h.screen_contents());
    }
    let first = first.unwrap_or_else(|| panic!("no live line while the command runs:\n{}", j.h.screen_contents()));
    snapshot(&j.h, &j.dir, "02-running");
    let title = last_title(j.h.raw_output());
    assert!(
        !title.contains("Action Required"),
        "the prompt was answered; the title still says {title:?}"
    );
    j.h.update(Duration::from_millis(2200));
    let later = live_line(&j.h.screen_contents()).unwrap_or_default();
    assert_ne!(first, later, "the live line ticks (spinner, seconds)");

    wait_for(&mut j.h, "Installed the thing.", 30);
    j.h.update(Duration::from_millis(600));
    snapshot(&j.h, &j.dir, "03-done");
    assert!(
        live_line(&j.h.screen_contents()).is_none() && !j.h.screen_contents().contains("Working\u{2026}"),
        "the line goes with the turn:\n{}",
        j.h.screen_contents()
    );
}
