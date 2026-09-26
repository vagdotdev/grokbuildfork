//! Every state of the one picker, captured as evidence (text + HTML screenshots under
//! `WORKSHOP_PTY_EVIDENCE_DIR/picker-states/`) and checked: nothing signed in, `/auth` landing on
//! the Subscriptions section, a vendor signed in (`✓ Max ▸`), that vendor expanded into its real
//! models, a model's effort sub-menu, the typed filter over the whole tree, and the `API keys`
//! sub-menu. Hermetic: the answering fake `opencode serve` on loopback (eight free models, some
//! with effort levels) and a fake `claude` that is signed out on the first launch and signed in on
//! the second. Opt-in via `WORKSHOP_BIN`, `--include-ignored`.

mod pty_common;

use std::path::Path;
use std::time::Duration;

use pty_common::*;

/// A fake `claude` (Claude Code 2.1.278's answers) whose login state is the `<state>/logged_in`
/// marker and whose model list is the real CLI's `initialize` shape.
fn fake_claude(bin: &Path, state: &Path) {
    std::fs::create_dir_all(state).unwrap();
    let script = format!(
        r#"#!/bin/sh
state='{state}'
case "$1" in
  --version) echo '2.1.278 (Claude Code)'; exit 0 ;;
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
esac
echo "fake claude: unexpected $*" >&2
exit 2
"#,
        state = state.display()
    );
    let path = bin.join("claude");
    std::fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn quit(mut j: Journey) -> tempfile::TempDir {
    j.h.inject_keys(b"\x1b").unwrap();
    j.h.update(Duration::from_millis(300));
    j.h.inject_keys(b"\x1b").unwrap();
    j.h.update(Duration::from_millis(300));
    j.h.inject_keys(b"\x03").unwrap();
    j.h.update(Duration::from_millis(400));
    j.h.inject_keys(b"\x03").unwrap();
    let _ = j.h.wait_exit_code(Duration::from_secs(10));
    let _ = j.h.quit();
    j.home
}

/// The row line for `name` inside the overlay (its `›` marker stripped).
fn row_line(j: &Journey, name: &str) -> String {
    j.h.screen_contents()
        .lines()
        .find(|l| l.contains(name) && l.contains('\u{2502}'))
        .map(|l| l.replace('\u{203a}', " "))
        .unwrap_or_default()
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake claude + fake opencode); run with --include-ignored"]
fn every_picker_state_is_captured() {
    let Some(bin) = bin_from_env() else { return };
    let recorder = tempfile::tempdir().expect("tempdir");
    let record = recorder.path().join("prompts.jsonl");
    let fakes = fake_opencode_answering(&record);
    let state = recorder.path().join("claude-state");
    fake_claude(fakes.path(), &state);

    // ---- Launch 1: Claude installed and signed out, Codex and Cursor not installed.
    let mut j = spawn_with_args("picker-states", &bin, &["--yolo"], &[], Some(fakes.path()));
    connect_big_pickle(&mut j);
    // The live free list is cached by the launch warm-up before `/model` is even opened.
    let cache = j
        .workshop_home()
        .join("catalog-cache")
        .join("opencode-engine.json");
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while !cache.is_file() && std::time::Instant::now() < deadline {
        j.h.update(Duration::from_millis(200));
    }

    // 1. Nothing signed in.
    send_prompt(&mut j, "/model");
    wait_for(&mut j.h, PICKER_OPEN, 15);
    wait_for(&mut j.h, "Ling 3.0 Flash Fin Free", 20);
    wait_gone(&mut j.h, "detecting", 20);
    wait_gone(&mut j.h, "refreshing lists", 15);
    j.h.update(Duration::from_millis(500));
    let screen = j.h.screen_contents();
    assert!(
        row_line(&j, "Claude").contains("sign in"),
        "Claude installed, signed out → `sign in`:\n{screen}"
    );
    assert!(
        row_line(&j, "Codex").contains("install") && row_line(&j, "Cursor").contains("install"),
        "missing CLIs → `install`:\n{screen}"
    );
    assert!(
        selected_line(&j.h).is_some_and(|l| l.contains("Big Pickle") && l.contains("active")),
        "the active model is highlighted:\n{screen}"
    );
    for pill in [
        "[Install]",
        "[Sign in]",
        "[Detecting]",
        "[Ready]",
        "Tab: Models",
        "Tab: Subscriptions",
        "engine",
    ] {
        assert!(!screen.contains(pill), "{pill:?} must not show:\n{screen}");
    }
    snapshot(&j.h, &j.dir, "01-model-nothing-signed-in");
    j.h.inject_keys(b"\x1b").unwrap();
    wait_picker_closed(&mut j.h, 5);

    // 2. `/auth`: the same list, landing on Claude.
    send_prompt(&mut j, "/auth");
    wait_for(&mut j.h, PICKER_OPEN, 15);
    wait_gone(&mut j.h, "detecting", 20);
    j.h.update(Duration::from_millis(500));
    assert!(
        selected_line(&j.h).is_some_and(|l| l.contains("Claude")),
        "/auth lands on the first vendor row:\n{}",
        j.h.screen_contents()
    );
    assert!(
        j.h.screen_contents()
            .contains("in your terminal:  claude auth login"),
        "the detail names the login command:\n{}",
        j.h.screen_contents()
    );
    snapshot(&j.h, &j.dir, "02-auth-lands-on-subscriptions");

    // 3. The `API keys` sub-menu.
    move_selection_to(&mut j.h, "API keys");
    j.h.inject_keys(b"\r").unwrap();
    wait_for(&mut j.h, "Models \u{203a} API keys", 5);
    wait_for(&mut j.h, "OpenAI \u{2014} API key", 5);
    j.h.update(Duration::from_millis(300));
    snapshot(&j.h, &j.dir, "07-api-keys-sub-menu");
    let home = quit(j);

    // ---- Launch 2: signed in to Claude.
    std::fs::write(state.join("logged_in"), "1").unwrap();
    let mut j = spawn_in_with_args(
        "picker-states",
        &bin,
        &["--yolo"],
        &[],
        Some(fakes.path()),
        home,
    );
    wait_for(&mut j.h, "\u{276f}", 45);

    // 4. Vendor signed in: `✓ Max ▸`, detail with the account.
    send_prompt(&mut j, "/model");
    wait_for(&mut j.h, PICKER_OPEN, 15);
    wait_for(&mut j.h, "\u{2713} Max", 30);
    wait_gone(&mut j.h, "refreshing lists", 15);
    move_selection_to(&mut j.h, "Claude");
    wait_for(&mut j.h, "3 models", 10);
    j.h.update(Duration::from_millis(300));
    let screen = j.h.screen_contents();
    assert!(
        row_line(&j, "Claude").contains("\u{2713} Max")
            && row_line(&j, "Claude").contains(OPENS_SUBMENU),
        "signed in → `✓ Max ▸`:\n{screen}"
    );
    assert!(
        screen.contains("Signed in as user@example.com \u{b7} Max \u{b7} 3 models"),
        "the detail names the account:\n{screen}"
    );
    snapshot(&j.h, &j.dir, "03-model-claude-signed-in");

    // 5. Vendor expanded: the CLI's real list, default first.
    j.h.inject_keys(b"\r").unwrap();
    wait_for(&mut j.h, "Models \u{203a} Claude", 10);
    wait_for(&mut j.h, "Opus (1M context)", 5);
    j.h.update(Duration::from_millis(300));
    let screen = j.h.screen_contents();
    assert!(
        selected_line(&j.h).is_some_and(|l| l.contains("Default (recommended)")),
        "the sub-menu opens on the CLI's default:\n{screen}"
    );
    assert!(
        screen.contains("Your Claude subscription \u{b7} signed in as user@example.com (Max)"),
        "{screen}"
    );
    snapshot(&j.h, &j.dir, "04-claude-expanded");
    j.h.inject_keys(b"\x1b[D").unwrap(); // ← back
    wait_gone(&mut j.h, "Models \u{203a} Claude", 5);

    // 6. Effort sub-menu of a model with levels.
    move_selection_to(&mut j.h, "Ling 3.0 Flash Fin Free");
    j.h.inject_keys(b"\r").unwrap();
    wait_for(&mut j.h, "Models \u{203a} Ling 3.0 Flash Fin Free", 10);
    wait_for(&mut j.h, "medium", 5);
    j.h.update(Duration::from_millis(300));
    let screen = j.h.screen_contents();
    assert!(
        screen.contains("at its default effort"),
        "the detail explains the highlighted level:\n{screen}"
    );
    snapshot(&j.h, &j.dir, "05-effort-sub-menu");
    j.h.inject_keys(b"\x1b").unwrap();
    wait_gone(&mut j.h, "Models \u{203a} Ling", 5);

    // 7. The filter: a vendor's model found from the top level, with its provider.
    j.h.inject_keys(b"son").unwrap();
    j.h.update(Duration::from_millis(600));
    let screen = j.h.screen_contents();
    assert!(
        selected_line(&j.h).is_some_and(|l| l.contains("Sonnet") && l.contains("Claude")),
        "typing finds a vendor's model with its provider:\n{screen}"
    );
    snapshot(&j.h, &j.dir, "06-filter");
    j.h.inject_keys(b"\x1b").unwrap();
    wait_for(&mut j.h, "type to filter", 5);

    // 8. Pick Sonnet: the composer names it; the row is marked on the way back.
    j.h.inject_keys(b"sonnet\r").unwrap();
    wait_picker_closed(&mut j.h, 30);
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let footer =
            j.h.screen_contents()
                .lines()
                .rev()
                .find(|l| l.contains('\u{256f}'))
                .map(str::to_owned)
                .unwrap_or_default();
        if footer.contains("Sonnet \u{b7} always-approve") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the composer never named Sonnet:\n{}",
            j.h.screen_contents()
        );
        j.h.update(Duration::from_millis(200));
    }
    send_prompt(&mut j, "/model");
    wait_for(&mut j.h, PICKER_OPEN, 15);
    wait_for(&mut j.h, "\u{2713} Max", 30);
    j.h.update(Duration::from_millis(500));
    assert!(
        row_line(&j, "Claude").contains("active"),
        "the vendor holding the active model is marked:\n{}",
        j.h.screen_contents()
    );
    snapshot(&j.h, &j.dir, "08-vendor-model-active");
    let _ = quit(j);
}
