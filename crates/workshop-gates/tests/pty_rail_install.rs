//! Hands-off subscriptions: a vendor whose official CLI is not installed offers one action, its
//! row reads `install`. Enter runs the vendor's official installer (here: a fake, via the
//! `WORKSHOP_RAIL_INSTALLER` hook) with one status line that follows its output, then flows
//! straight into the vendor's own sign-in — no manual step, nothing started on its own, and the
//! row ends up `✓` with the CLI's own models behind it. Workshop never reads another app's
//! credential files (the no-theft audit covers that; here the fake CLI's state lives in its own
//! directory).
//!
//! `a_cancelled_chained_sign_in_still_shows_the_installed_cli`: Ctrl+C at the chained vendor
//! sign-in leaves the CLI installed, so the rail re-detects at once and reads `[Sign in]` — no
//! Ctrl+R — and its detail names the whole command (`claude auth login`).
//!
//! Hermetic (fake installer + fake `claude`). Opt-in via `WORKSHOP_BIN`, `--include-ignored`.

mod pty_common;

use std::path::Path;
use std::time::{Duration, Instant};

use pty_common::*;

fn executable(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// The fake installer: prints two progress lines with pauses (so the status line is observable),
/// then writes a fake `claude` — signed out until its own `auth login` runs — into `bin`.
fn fake_installer(dir: &Path, bin: &Path, state: &Path) -> std::path::PathBuf {
    let claude = format!(
        r#"#!/bin/sh
state='{state}'
case "$1" in
  --version) echo '2.1.280 (Claude Code)'; exit 0 ;;
  --help) echo 'Usage: claude [options] [command] [prompt]'; echo; echo 'Claude Code - starts an interactive session by default'; exit 0 ;;
esac
case "$*" in
  *--input-format*)
    IFS= read -r _req
    printf '%s\n' '{{"type":"control_response","response":{{"subtype":"success","request_id":"workshop-models","response":{{"models":[{{"value":"default","displayName":"Default (recommended)"}},{{"value":"opus[1m]","displayName":"Opus (1M context)"}},{{"value":"sonnet","displayName":"Sonnet"}}],"account":{{"email":"user@example.com","subscriptionType":"max"}}}}}}}}'
    while IFS= read -r _; do :; done; exit 0 ;;
  "auth status"*)
    if [ -f "$state/logged_in" ]; then echo '{{"loggedIn": true, "authMethod": "claude.ai", "apiProvider": "firstParty", "email": "user@example.com", "subscriptionType": "max"}}'; exit 0
    else echo '{{"loggedIn": false, "authMethod": "none", "apiProvider": "firstParty"}}'; exit 1; fi ;;
  "auth login")
    touch "$state/login_ran"
    # `<state>/login_hang`: wait like a real OAuth flow does, until the terminal's Ctrl+C.
    if [ -f "$state/login_hang" ]; then echo 'Opening browser to sign in...'; sleep 60; fi
    touch "$state/logged_in"; echo 'Logged in as user@example.com'; exit 0 ;;
esac
echo "fake claude: unexpected $*" >&2
exit 2
"#,
        state = state.display()
    );
    std::fs::write(dir.join("claude.template"), claude).unwrap();
    let installer = dir.join("fake-official-installer.sh");
    executable(
        &installer,
        &format!(
            r#"#!/bin/sh
# $1 is the vendor id Workshop asked for.
echo "installer-called $1" >> '{state}/installer_calls'
[ "$1" = claude ] || {{ echo "unexpected vendor $1" >&2; exit 9; }}
echo 'Downloading Claude Code 2.1.280...'
sleep 1.5
echo 'Installing to ~/.local/bin...'
sleep 1
cp '{template}' '{bin}/claude'
chmod 755 '{bin}/claude'
echo 'Claude Code 2.1.280 installed.'
exit 0
"#,
            state = state.display(),
            template = dir.join("claude.template").display(),
            bin = bin.display(),
        ),
    );
    installer
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake installer + fake claude); run with --include-ignored"]
fn a_missing_cli_installs_and_signs_in_on_one_keypress() {
    let Some(bin) = bin_from_env() else { return };
    let fakes = tempfile::tempdir().expect("tempdir");
    let bin_dir = fakes.path().join("bin");
    let state = fakes.path().join("state");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    let installer = fake_installer(fakes.path(), &bin_dir, &state);
    let installer_s = installer.to_string_lossy().to_string();
    // `bin_dir` is on PATH from the start without a `claude`: Claude Code is "not installed". It
    // does hold the fake `opencode`, so the launch-time engine warm-up stays hermetic.
    install_fake_opencode_into(&bin_dir, "silent");
    let mut j = spawn(
        "rail-install",
        &bin,
        &[("WORKSHOP_RAIL_INSTALLER", installer_s.as_str())],
        Some(&bin_dir),
    );
    connect_big_pickle(&mut j);
    assert!(
        !state.join("installer_calls").exists(),
        "nothing installs on its own before the user acts"
    );

    // 1. `/auth`: the Claude row offers one action, `install`, and its detail says what Enter does.
    send_prompt(&mut j, "/auth");
    wait_for(&mut j.h, PICKER_OPEN, 15);
    wait_gone(&mut j.h, "detecting", 15);
    let screen = j.h.screen_contents();
    assert!(
        selected_line(&j.h).is_some_and(|l| l.contains("Claude") && l.contains("install")),
        "the Claude row reads `install`:\n{screen}"
    );
    assert!(
        screen.contains("Enter installs Claude Code, then signs you in"),
        "the detail is the action, not manual steps:\n{screen}"
    );
    assert!(
        screen.contains("curl -fsSL https://claude.ai/install.sh | bash"),
        "the detail names the vendor's official installer:\n{screen}"
    );
    assert!(
        !screen.contains("Install Claude Code, then sign in"),
        "no manual instruction left:\n{screen}"
    );
    snapshot(&j.h, &j.dir, "01-auth-install-action");
    assert!(
        !state.join("installer_calls").exists(),
        "opening /auth installs nothing"
    );

    // 2. Enter: one status line follows the installer's output.
    j.h.inject_keys(b"\r").unwrap();
    wait_for(&mut j.h, "Installing Claude Code", 10);
    wait_for(&mut j.h, "Downloading Claude Code 2.1.280", 10);
    snapshot(&j.h, &j.dir, "02-installing-status-line");
    assert!(
        std::fs::read_to_string(state.join("installer_calls"))
            .unwrap_or_default()
            .contains("installer-called claude"),
        "the official installer was asked for exactly this vendor"
    );

    // 3. Done: straight into the vendor's own sign-in (no keypress), then the row is signed in.
    let started = Instant::now();
    while !state.join("login_ran").exists() {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the vendor sign-in never ran after the install:\n{}",
            j.h.screen_contents()
        );
        j.h.update(Duration::from_millis(200));
    }
    wait_for(&mut j.h, "\u{2713} Max", 30);
    // …and the row holds the CLI's own models (its default first), not a placeholder.
    wait_for(&mut j.h, "3 models", 30);
    j.h.update(Duration::from_millis(800));
    let screen = j.h.screen_contents();
    assert!(
        !screen.contains("Claude Opus") && !screen.contains("Loading models"),
        "the signed-in row carries the CLI's list, no placeholder and no loading copy:\n{screen}"
    );
    let claude_line = selected_line(&j.h).unwrap_or_default();
    assert!(
        claude_line.contains("Claude")
            && claude_line.contains("\u{2713} Max")
            && claude_line.contains("\u{25b8}")
            && !claude_line.contains("install"),
        "the Claude row no longer offers install: {claude_line}\n{screen}"
    );
    let row_state = |name: &str| {
        screen
            .lines()
            .find(|l| l.contains(name) && l.contains("install"))
            .map(str::to_owned)
    };
    assert!(
        row_state("Codex").is_some() && row_state("Cursor").is_some(),
        "the CLIs that are still missing keep their one install action:\n{screen}"
    );
    for pill in [
        "[Ready]",
        "[Install]",
        "[Sign in]",
        "Tab: Models",
        "Tab: Subscriptions",
    ] {
        assert!(
            !screen.contains(pill),
            "no pill, no tab ({pill}):\n{screen}"
        );
    }
    assert!(
        !screen.contains("Couldn't install"),
        "no failure line on the happy path:\n{screen}"
    );
    snapshot(&j.h, &j.dir, "03-signed-in-ready");
    let log = j.workshop_home().join("logs").join("install-claude.log");
    let log_text = std::fs::read_to_string(&log).expect("installer output is kept in the log");
    assert!(
        log_text.contains("Downloading Claude Code 2.1.280")
            && log_text.contains("[workshop] Installed"),
        "{log_text}"
    );
    assert_eq!(
        std::fs::read_to_string(state.join("installer_calls"))
            .unwrap()
            .lines()
            .count(),
        1,
        "the installer ran exactly once"
    );
}

/// Ctrl+C at the sign-in that follows the one-keypress install: the CLI is installed and signed
/// out, and the rail says so at once (`[Sign in]`, its detail naming `claude auth login`) with no
/// Ctrl+R. The install is not undone and nobody is signed in.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake installer + fake claude); run with --include-ignored"]
fn a_cancelled_chained_sign_in_still_shows_the_installed_cli() {
    let Some(bin) = bin_from_env() else { return };
    let fakes = tempfile::tempdir().expect("tempdir");
    let bin_dir = fakes.path().join("bin");
    let state = fakes.path().join("state");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    // The fake `claude auth login` waits like a real OAuth flow, until the terminal's Ctrl+C.
    std::fs::write(state.join("login_hang"), "1").unwrap();
    let installer = fake_installer(fakes.path(), &bin_dir, &state);
    let installer_s = installer.to_string_lossy().to_string();
    install_fake_opencode_into(&bin_dir, "silent");
    let mut j = spawn(
        "rail-install-cancelled-sign-in",
        &bin,
        &[("WORKSHOP_RAIL_INSTALLER", installer_s.as_str())],
        Some(&bin_dir),
    );
    connect_big_pickle(&mut j);
    send_prompt(&mut j, "/auth");
    wait_for(&mut j.h, "[Install]", 15);

    // Enter: the installer, then straight into the vendor's sign-in, which owns the terminal.
    j.h.inject_keys(b"\r").unwrap();
    let started = Instant::now();
    while !state.join("login_ran").exists() {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the vendor sign-in never ran after the install:\n{}",
            j.h.screen_contents()
        );
        j.h.update(Duration::from_millis(200));
    }
    wait_for(&mut j.h, "Opening browser to sign in", 10);
    snapshot(&j.h, &j.dir, "01-chained-sign-in-owns-the-terminal");

    // Ctrl+C in the (cooked-mode) terminal ends the vendor login only. Back in the picker, the
    // rail must say what is true now — installed, signed out — without a Ctrl+R.
    j.h.inject_keys(b"\x03").unwrap();
    wait_for(&mut j.h, "[Sign in]", 20);
    j.h.update(Duration::from_millis(800));
    assert!(
        j.h.is_running().unwrap_or(false),
        "Workshop must survive the Ctrl+C that ended the login:\n{}",
        j.h.screen_contents()
    );
    let screen = j.h.screen_contents();
    let claude_line = screen
        .lines()
        .find(|l| l.contains("Claude") && l.contains('['))
        .unwrap_or_default();
    assert!(
        claude_line.contains("[Sign in]") && !claude_line.contains("[Install]"),
        "the installed CLI's rail re-detects to Sign in on its own: {claude_line}\n{screen}"
    );
    assert!(
        screen.contains("in your terminal:  claude auth login"),
        "the detail names the whole login command, binary included:\n{screen}"
    );
    assert!(
        screen.contains("Codex   [Install]") && screen.contains("Cursor  [Install]"),
        "the CLIs that are still missing keep their one Install action:\n{screen}"
    );
    assert!(
        !state.join("logged_in").exists(),
        "a cancelled sign-in signs nobody in"
    );
    assert!(
        bin_dir.join("claude").exists(),
        "the install stays; only the sign-in was cancelled"
    );
    snapshot(&j.h, &j.dir, "02-sign-in-after-cancel-no-ctrl-r");
}
