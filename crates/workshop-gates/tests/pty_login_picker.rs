//! PTY smoke of the built `workshop` binary: first run and Login open the connection picker, never
//! a `grok.com` login or an `auth.x.ai` URL. Also captures evidence (asciinema cast, text and HTML
//! screenshots) into `WORKSHOP_PTY_EVIDENCE_DIR` (default `target/pty-evidence`).
//!
//! Opt-in: set `WORKSHOP_BIN` to the built binary and run with `--include-ignored`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use xai_grok_pager_pty_harness::PtyHarness;

fn evidence_dir() -> PathBuf {
    let dir = std::env::var_os("WORKSHOP_PTY_EVIDENCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/pty-evidence")
        });
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

fn wait_for(h: &mut PtyHarness, text: &str, secs: u64) {
    if let Err(e) = h.wait_for_text(text, Duration::from_secs(secs)) {
        panic!(
            "timed out waiting for {text:?}: {e}\nscreen:\n{}",
            h.screen_contents()
        );
    }
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); run with --include-ignored"]
fn first_run_and_login_open_the_picker_not_grok_com() {
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

    // 1. Cold start: the picker is the login screen.
    wait_for(&mut h, "connect a model", 30);
    wait_for(&mut h, "xAI (optional)", 10);
    assert_no_xai(&h, "cold start");
    snapshot(&h, &dir, "01-first-run-picker-models");

    // 2. Subscriptions tab: rails Claude / Codex / Cursor with pills.
    h.inject_keys(b"\t").unwrap();
    wait_for(&mut h, "Claude", 5);
    let screen = h.screen_contents();
    let (c, x, u) = (
        screen.find("Claude").unwrap(),
        screen.find("Codex").unwrap(),
        screen.find("Cursor").unwrap(),
    );
    assert!(c < x && x < u, "rail order Claude, Codex, Cursor:\n{screen}");
    assert!(
        screen.contains("[Sign in]") || screen.contains("[Detecting]") || screen.contains("[Ready]"),
        "a pill is shown:\n{screen}"
    );
    snapshot(&h, &dir, "02-picker-subscriptions");

    // 3. Back to Models; open the OpenAI card detail (never a login).
    h.inject_keys(b"\t").unwrap();
    h.update(Duration::from_millis(300));
    h.inject_keys(b"\x1b[B").unwrap(); // Down → OpenAI API
    h.update(Duration::from_millis(200));
    h.inject_keys(b"\r").unwrap();
    wait_for(&mut h, "OPENAI_API_KEY", 5);
    assert_no_xai(&h, "openai detail");
    snapshot(&h, &dir, "03-picker-openai-detail");
    h.inject_keys(b"\x1b").unwrap(); // close detail
    h.update(Duration::from_millis(300));

    // 4. Move to the last card (xAI optional): first Enter only shows the labeled copy.
    for _ in 0..8 {
        h.inject_keys(b"\x1b[B").unwrap();
        h.update(Duration::from_millis(60));
    }
    h.inject_keys(b"\r").unwrap();
    wait_for(&mut h, "Not required.", 5);
    wait_for(&mut h, "Press Enter again", 5);
    snapshot(&h, &dir, "04-picker-xai-optional-armed");
    // Esc disarms instead of continuing.
    h.inject_keys(b"\x1b").unwrap();
    h.update(Duration::from_millis(300));
    assert_no_xai(&h, "after esc on xai card");

    // 5. Esc closes the picker → auth-pending home with "Connect a model", not "Login with grok.com".
    h.inject_keys(b"\x1b").unwrap();
    wait_for(&mut h, "Connect a model", 5);
    assert_no_xai(&h, "picker closed");
    snapshot(&h, &dir, "05-home-after-picker-closed");

    // 6. `l` reopens the picker (the Login key).
    h.inject_keys(b"l").unwrap();
    wait_for(&mut h, "Workshop \u{2014} connect a model", 5);
    assert_no_xai(&h, "l reopens picker");
    snapshot(&h, &dir, "06-l-reopens-picker");

    h.write_cast(&dir.join("workshop-login-picker.cast"))
        .expect("write asciinema cast");
    h.inject_keys(b"\x03").unwrap();
    h.update(Duration::from_millis(500));
    h.inject_keys(b"\x03").unwrap();
    let _ = h.wait_exit_code(Duration::from_secs(5));
    let _ = h.quit();
    eprintln!("evidence written to {}", dir.display());
}
