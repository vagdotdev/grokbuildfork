//! PTY smoke of the built `workshop` binary: a first run lands in the composer with the OpenCode
//! default model active (no picker, no `opencode` install until the first message), `/model` opens
//! the compact Models overlay, `/auth` the Subscriptions overlay with the Claude / Codex / Cursor
//! rails — and nothing ever shows a `grok.com` login or an `auth.x.ai` URL. The terminal title is
//! `Workshop`, never `grok`. Also captures evidence (asciinema cast, text and HTML screenshots) into
//! `WORKSHOP_PTY_EVIDENCE_DIR` (default `target/pty-evidence`).
//!
//! Opt-in: set `WORKSHOP_BIN` to the built binary and run with `--include-ignored`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use xai_grok_pager_pty_harness::PtyHarness;

fn evidence_dir() -> PathBuf {
    let dir = std::env::var_os("WORKSHOP_PTY_EVIDENCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/pty-evidence"));
    std::fs::create_dir_all(&dir).expect("create evidence dir");
    dir
}

fn snapshot(h: &PtyHarness, dir: &Path, name: &str) {
    std::fs::write(dir.join(format!("{name}.txt")), h.screen_contents()).expect("write txt");
    std::fs::write(dir.join(format!("{name}.html")), h.screen_html()).expect("write html");
}

fn assert_no_xai(h: &PtyHarness, step: &str) {
    let screen = h.screen_contents();
    for bad in [
        "Login with grok.com",
        "Login with Grok",
        "auth.x.ai/.well-known",
        "accounts.x.ai",
    ] {
        assert!(
            !screen.contains(bad),
            "{step}: screen shows {bad:?}\n{screen}"
        );
    }
}

/// The `›`-marked (highlighted) line of the screen.
fn selected_line(h: &PtyHarness) -> Option<String> {
    h.screen_contents()
        .lines()
        .find(|l| l.contains('\u{203a}'))
        .map(str::to_owned)
}

/// Press Down until the highlighted line contains `needle`. Lists are built from the live
/// provider catalog, so row indices are data, not constants.
fn move_selection_to(h: &mut PtyHarness, needle: &str) {
    for _ in 0..40 {
        if selected_line(h).is_some_and(|l| l.contains(needle)) {
            return;
        }
        h.inject_keys(b"\x1b[B").unwrap();
        h.update(Duration::from_millis(80));
    }
    panic!(
        "never reached a highlighted line containing {needle:?}\nscreen:\n{}",
        h.screen_contents()
    );
}

fn wait_for(h: &mut PtyHarness, text: &str, secs: u64) {
    if let Err(e) = h.wait_for_text(text, Duration::from_secs(secs)) {
        panic!(
            "timed out waiting for {text:?}: {e}\nscreen:\n{}",
            h.screen_contents()
        );
    }
}

fn wait_gone(h: &mut PtyHarness, text: &str, secs: u64) {
    if let Err(e) = h.wait_for_text_absent(text, Duration::from_secs(secs)) {
        panic!(
            "timed out waiting for {text:?} to disappear: {e}\nscreen:\n{}",
            h.screen_contents()
        );
    }
}

/// Type a slash command into the composer and submit it.
fn slash(h: &mut PtyHarness, cmd: &str) {
    h.inject_keys(cmd.as_bytes()).unwrap();
    h.update(Duration::from_millis(400));
    h.inject_keys(b"\r").unwrap();
}

/// Every OSC 0/2 title the binary set (`\x1b]0;<title>\x07`).
fn titles_set(h: &PtyHarness) -> Vec<String> {
    let raw = String::from_utf8_lossy(h.raw_output());
    let mut out = Vec::new();
    for prefix in ["\u{1b}]0;", "\u{1b}]2;"] {
        for (i, _) in raw.match_indices(prefix) {
            let rest = &raw[i + prefix.len()..];
            let end = rest.find(['\u{7}', '\u{1b}']).unwrap_or(rest.len());
            out.push(rest[..end].to_owned());
        }
    }
    out
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); run with --include-ignored"]
fn first_run_types_and_goes_model_and_auth_are_the_only_doors() {
    let Some(bin) = std::env::var_os("WORKSHOP_BIN") else {
        eprintln!("WORKSHOP_BIN not set; skipping");
        return;
    };
    let bin = PathBuf::from(bin);
    let home = tempfile::tempdir().expect("tempdir");
    let cwd = tempfile::tempdir().expect("tempdir");
    std::process::Command::new("git")
        .args(["init", "-q", "."])
        .current_dir(cwd.path())
        .status()
        .expect("git init");
    let workshop_home = home.path().join(".workshop");
    let home_s = home.path().to_string_lossy().to_string();
    let wh_s = workshop_home.to_string_lossy().to_string();
    let env: Vec<(&str, &str)> = vec![
        ("HOME", home_s.as_str()),
        ("WORKSHOP_HOME", wh_s.as_str()),
        ("TERM", "xterm-256color"),
        ("NO_COLOR", "1"),
        ("GROK_DISABLE_AUTOUPDATER", "1"),
    ];
    let mut h = PtyHarness::new_inherited_env(&bin, 40, 120, &[], &env, Some(cwd.path()))
        .expect("spawn workshop in pty");
    h.set_respond_to_queries(true);
    let dir = evidence_dir();

    // 1. Cold start: the composer, with the OpenCode default active and the two doors as the hint.
    //    No picker, no explanation paragraph.
    wait_for(&mut h, "OpenCode \u{b7} Big Pickle", 30);
    wait_for(&mut h, "/model to switch", 5);
    wait_for(&mut h, "/auth to connect subscriptions", 5);
    let screen = h.screen_contents();
    assert!(
        !screen.contains("connect a model") && !screen.contains("Connection classes"),
        "first run must not show the picker:\n{screen}"
    );
    assert_no_xai(&h, "cold start");
    snapshot(&h, &dir, "01-first-run-composer");
    let conn = std::fs::read_to_string(workshop_home.join("active-connection.json"))
        .expect("first run persists the active connection");
    assert!(
        conn.contains("\"engine\"") && conn.contains("opencode/big-pickle"),
        "engine default is the active connection: {conn}"
    );
    assert!(
        !workshop_home.join("tools").exists(),
        "opencode must not be installed before the first message"
    );

    // 2. `/model`: the compact Models overlay, active row highlighted, Esc closes.
    slash(&mut h, "/model");
    wait_for(&mut h, "Tab: Subscriptions", 10);
    wait_for(&mut h, "Kilo", 15);
    let screen = h.screen_contents();
    assert!(
        selected_line(&h).is_some_and(|l| l.contains("Big Pickle") && l.contains("active")),
        "the active engine model is highlighted:\n{screen}"
    );
    assert!(
        !screen.contains("never signs you in") && !screen.contains("Connection classes"),
        "no explanatory paragraph or footer sentence:\n{screen}"
    );
    assert!(
        !screen.contains("xAI (optional)"),
        "the Models view lists models only:\n{screen}"
    );
    assert_no_xai(&h, "/model overlay");
    snapshot(&h, &dir, "02-model-overlay");
    h.inject_keys(b"\x1b").unwrap();
    wait_gone(&mut h, "Tab: Subscriptions", 5);
    // Esc lands in the session the command was typed into; its composer names the connection.
    wait_for(&mut h, "OpenCode \u{b7} Big Pickle", 5);

    // 3. `/auth`: the Subscriptions overlay — rails Claude / Codex / Cursor with pills, the API-key
    //    providers below, the optional xAI card last.
    slash(&mut h, "/auth");
    wait_for(&mut h, "Tab: Models", 10);
    wait_for(&mut h, "Claude", 5);
    let screen = h.screen_contents();
    let (c, x, u) = (
        screen.find("Claude").unwrap(),
        screen.find("Codex").unwrap(),
        screen.find("Cursor").unwrap(),
    );
    assert!(
        c < x && x < u,
        "rail order Claude, Codex, Cursor:\n{screen}"
    );
    assert!(
        screen.contains("[Sign in]")
            || screen.contains("[Detecting]")
            || screen.contains("[Ready]"),
        "a pill is shown:\n{screen}"
    );
    assert!(
        screen.contains("xAI (optional)"),
        "the optional xAI card is on the Subscriptions view:\n{screen}"
    );
    assert_no_xai(&h, "/auth overlay");
    snapshot(&h, &dir, "03-auth-overlay");

    // 4. Enter on the OpenAI row opens a key-entry prompt (never a login, never an echo of the key).
    move_selection_to(&mut h, "OpenAI API key");
    h.inject_keys(b"\r").unwrap();
    wait_for(&mut h, "paste or type your key", 5);
    assert_no_xai(&h, "openai key entry");
    snapshot(&h, &dir, "04-auth-openai-key-entry");
    h.inject_keys(b"\x1b").unwrap(); // cancel key entry
    h.update(Duration::from_millis(300));

    // 5. The last row (xAI optional): first Enter only shows the labeled copy; Esc disarms.
    move_selection_to(&mut h, "xAI (optional)");
    h.inject_keys(b"\r").unwrap();
    wait_for(&mut h, "Not required.", 5);
    wait_for(&mut h, "Press Enter again", 5);
    snapshot(&h, &dir, "05-auth-xai-optional-armed");
    h.inject_keys(b"\x1b").unwrap();
    wait_gone(&mut h, "Press Enter again", 5);
    assert_no_xai(&h, "after esc on xai card");

    // 6. Tab switches between the two views; Esc closes back to the composer.
    h.inject_keys(b"\t").unwrap();
    wait_for(&mut h, "Tab: Subscriptions", 5);
    h.inject_keys(b"\t").unwrap();
    wait_for(&mut h, "Tab: Models", 5);
    h.inject_keys(b"\x1b").unwrap();
    wait_gone(&mut h, "Tab: Models", 5);
    wait_for(&mut h, "OpenCode \u{b7} Big Pickle", 5);
    assert_no_xai(&h, "overlay closed");
    snapshot(&h, &dir, "06-composer-after-overlays");

    // 7. Terminal title: Workshop, never grok.
    let titles = titles_set(&h);
    assert!(
        titles.iter().any(|t| t.contains("Workshop")),
        "terminal title must be set to Workshop, got {titles:?}"
    );
    assert!(
        !titles
            .iter()
            .any(|t| t.to_ascii_lowercase().contains("grok")),
        "terminal title must never say grok, got {titles:?}"
    );
    assert!(
        !workshop_home.join("tools").exists(),
        "opencode is still not installed: nothing contacted the network"
    );

    h.write_cast(&dir.join("workshop-first-run.cast"))
        .expect("write asciinema cast");
    h.inject_keys(b"\x03").unwrap();
    h.update(Duration::from_millis(500));
    h.inject_keys(b"\x03").unwrap();
    let _ = h.wait_exit_code(Duration::from_secs(5));
    let _ = h.quit();
    eprintln!("evidence written to {}", dir.display());
}
