//! PTY smoke of the built `workshop` binary: a first run lands in the composer with the OpenCode
//! default model active (no picker; the engine install starts in the background, off screen —
//! here against a closed proxy, so it fails at once and hermetically), `/model` opens the one
//! compact picker — OpenCode's models, then the Subscriptions section with the Claude / Codex /
//! Cursor rows, `API keys` and the optional xAI row — and `/auth` opens the same picker on its
//! Subscriptions section; nothing ever shows a `grok.com` login or an `auth.x.ai` URL. The
//! terminal title is `Workshop`, never `grok`. Also captures evidence (asciinema cast, text and
//! HTML screenshots) into `WORKSHOP_PTY_EVIDENCE_DIR` (default `target/pty-evidence`).
//!
//! Opt-in: set `WORKSHOP_BIN` to the built binary and run with `--include-ignored`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use xai_grok_pager_pty_harness::PtyHarness;

/// The picker overlay is on screen: its search line starts with this glyph.
const PICKER: &str = "\u{2315}";

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
        .find(|l| l.contains('\u{203a}') && !l.contains("Models \u{203a}"))
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
        // The launch-time engine install (`curl https://opencode.ai/install`) must fail fast and
        // offline here: a closed proxy port.
        ("HTTP_PROXY", "http://127.0.0.1:9"),
        ("HTTPS_PROXY", "http://127.0.0.1:9"),
        ("ALL_PROXY", "http://127.0.0.1:9"),
        ("http_proxy", "http://127.0.0.1:9"),
        ("https_proxy", "http://127.0.0.1:9"),
    ];
    let mut h = PtyHarness::new_inherited_env(&bin, 40, 120, &[], &env, Some(cwd.path()))
        .expect("spawn workshop in pty");
    h.set_respond_to_queries(true);
    let dir = evidence_dir();

    // 1. Cold start: the composer, labelled with the model name only (no provider), the two doors
    //    as the first-run hint. No picker, no explanation paragraph.
    wait_for(&mut h, "Big Pickle", 30);
    wait_for(&mut h, "/model to switch", 5);
    wait_for(&mut h, "/auth to connect subscriptions", 5);
    let screen = h.screen_contents();
    assert!(
        !screen.contains("connect a model") && !screen.contains("Connection classes"),
        "first run must not show the picker:\n{screen}"
    );
    assert!(
        !screen.contains("OpenCode \u{b7} Big Pickle") && !screen.contains("engine"),
        "the composer label is the model name only, no plumbing:\n{screen}"
    );
    assert_no_xai(&h, "cold start");
    snapshot(&h, &dir, "01-first-run-composer");
    let conn = std::fs::read_to_string(workshop_home.join("active-connection.json"))
        .expect("first run persists the active connection");
    assert!(
        conn.contains("\"engine\"") && conn.contains("opencode/big-pickle"),
        "engine default is the active connection: {conn}"
    );
    // The engine is brought up in the background from launch (a first run installs it first);
    // the attempt is on record, and nothing about it is on screen.
    let engine_log = workshop_home.join("logs").join("opencode-engine.log");
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !engine_log.is_file() && std::time::Instant::now() < deadline {
        h.update(Duration::from_millis(200));
    }
    let log = std::fs::read_to_string(&engine_log).expect("the engine bring-up starts at launch");
    assert!(
        log.contains("install: running the official OpenCode installer"),
        "a first run installs the engine at launch:\n{log}"
    );
    let screen = h.screen_contents();
    assert!(
        !screen.contains("Installing") && !screen.contains("Starting the OpenCode engine"),
        "the launch bring-up never shows on the composer:\n{screen}"
    );

    // 2. `/model`: the compact picker, active row highlighted, the Subscriptions section below the
    //    OpenCode models, Esc closes.
    slash(&mut h, "/model");
    wait_for(&mut h, PICKER, 10);
    wait_for(&mut h, "OpenCode", 15);
    wait_for(&mut h, "Subscriptions", 5);
    let screen = h.screen_contents();
    assert!(
        selected_line(&h).is_some_and(|l| l.contains("Big Pickle") && l.contains("active")),
        "the active OpenCode model is highlighted:\n{screen}"
    );
    assert!(
        !screen.contains("Kilo") && !screen.contains("engine"),
        "the picker never names Kilo Gateway or the engine:\n{screen}"
    );
    assert!(
        !screen.contains("never signs you in") && !screen.contains("Connection classes"),
        "no explanatory paragraph or footer sentence:\n{screen}"
    );
    let (o, s) = (
        screen.find("OpenCode").unwrap(),
        screen.find("Subscriptions").unwrap(),
    );
    assert!(
        o < s,
        "OpenCode's models come first, the subscriptions below:\n{screen}"
    );
    for pill in [
        "Tab: Models",
        "Tab: Subscriptions",
        "[Install]",
        "[Sign in]",
        "[Detecting]",
        "[Ready]",
    ] {
        assert!(
            !screen.contains(pill),
            "one picker: no tabs, no pills ({pill}):\n{screen}"
        );
    }
    assert_no_xai(&h, "/model overlay");
    snapshot(&h, &dir, "02-model-overlay");
    h.inject_keys(b"\x1b").unwrap();
    wait_gone(&mut h, PICKER, 5);
    // Esc lands in the session the command was typed into; its composer names the model.
    wait_for(&mut h, "Big Pickle", 5);
    assert!(
        !h.screen_contents().contains("OpenCode \u{b7} Big Pickle"),
        "model name only:\n{}",
        h.screen_contents()
    );

    // 3. `/auth`: the same picker, opened on the Subscriptions section — the vendor rows Claude /
    //    Codex / Cursor with their state as a plain suffix (no CLI here: `install`), `API keys`,
    //    the optional xAI row last.
    slash(&mut h, "/auth");
    wait_for(&mut h, PICKER, 10);
    wait_for(&mut h, "Claude", 5);
    wait_gone(&mut h, "detecting", 15);
    let screen = h.screen_contents();
    let (c, x, u) = (
        screen.find("Claude").unwrap(),
        screen.find("Codex").unwrap(),
        screen.find("Cursor").unwrap(),
    );
    assert!(
        c < x && x < u,
        "vendor order Claude, Codex, Cursor:\n{screen}"
    );
    assert!(
        selected_line(&h).is_some_and(|l| l.contains("Claude")),
        "/auth lands on the first vendor row:\n{screen}"
    );
    assert!(
        screen.contains("OpenCode") && screen.contains("Big Pickle"),
        "the same picker: the models are still listed above:\n{screen}"
    );
    for pill in [
        "[Install]",
        "[Sign in]",
        "[Detecting]",
        "[Ready]",
        "Tab: Models",
        "Tab: Subscriptions",
    ] {
        assert!(
            !screen.contains(pill),
            "no pill, no tab ({pill}):\n{screen}"
        );
    }
    let claude_line = screen
        .lines()
        .find(|l| l.contains("Claude"))
        .unwrap_or_default();
    assert!(
        claude_line.contains("install") || claude_line.contains("sign in"),
        "a vendor row carries its state as a plain suffix: {claude_line}\n{screen}"
    );
    assert!(
        screen.contains("xAI") && screen.contains("optional"),
        "the optional xAI row is last, in the row style:\n{screen}"
    );
    let (k, xai) = (
        screen.find("API keys").unwrap(),
        screen.find("xAI").unwrap(),
    );
    assert!(k < xai, "API keys before xAI, xAI last:\n{screen}");
    assert_no_xai(&h, "/auth overlay");
    snapshot(&h, &dir, "03-auth-overlay");

    // 4. `API keys` opens the providers to connect (one vocabulary: `Provider — API key` |
    //    `Provider — Sign in`); Enter on the OpenAI row opens a key-entry prompt (never a login,
    //    never an echo of the key). Esc backs out of the sub-menu.
    move_selection_to(&mut h, "API keys");
    h.inject_keys(b"\r").unwrap();
    wait_for(&mut h, "Models \u{203a} API keys", 5);
    let screen = h.screen_contents();
    assert!(
        screen.contains("OpenAI \u{2014} API key")
            && screen.contains("OpenRouter \u{2014} Sign in"),
        "connect rows share one vocabulary (Provider — API key | Sign in):\n{screen}"
    );
    assert!(
        !screen.contains("xAI \u{2014} Sign in"),
        "the xAI row is not an API key:\n{screen}"
    );
    snapshot(&h, &dir, "04-auth-api-keys");
    move_selection_to(&mut h, "OpenAI \u{2014} API key");
    h.inject_keys(b"\r").unwrap();
    wait_for(&mut h, "paste or type your key", 5);
    assert_no_xai(&h, "openai key entry");
    snapshot(&h, &dir, "05-auth-openai-key-entry");
    h.inject_keys(b"\x1b").unwrap(); // cancel key entry
    h.update(Duration::from_millis(300));
    h.inject_keys(b"\x1b").unwrap(); // back to the list
    wait_gone(&mut h, "Models \u{203a} API keys", 5);
    assert!(
        selected_line(&h).is_some_and(|l| l.contains("API keys")),
        "Esc returns to the row the sub-menu came from:\n{}",
        h.screen_contents()
    );

    // 5. The last row (xAI optional): first Enter only shows the labeled copy; Esc disarms.
    move_selection_to(&mut h, "xAI");
    h.inject_keys(b"\r").unwrap();
    wait_for(&mut h, "Not required.", 5);
    wait_for(&mut h, "Press Enter again", 5);
    snapshot(&h, &dir, "06-auth-xai-optional-armed");
    h.inject_keys(b"\x1b").unwrap();
    wait_gone(&mut h, "Press Enter again", 5);
    assert_no_xai(&h, "after esc on xai card");

    // 6. Tab is swallowed (one list, no views); Esc closes back to the composer.
    h.inject_keys(b"\t").unwrap();
    h.update(Duration::from_millis(300));
    wait_for(&mut h, PICKER, 5);
    h.inject_keys(b"\x1b").unwrap();
    wait_gone(&mut h, PICKER, 5);
    wait_for(&mut h, "Big Pickle", 5);
    assert_no_xai(&h, "overlay closed");
    snapshot(&h, &dir, "07-composer-after-overlays");

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
        !workshop_home
            .join("tools/opencode/.opencode/bin/opencode")
            .exists(),
        "offline, the launch install could not complete: no binary landed"
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
