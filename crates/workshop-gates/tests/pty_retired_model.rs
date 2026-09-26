//! The picked model sticks — except when it no longer exists (owner rule, 2026-09-24). Then
//! Workshop routes back to the default by itself, says so in one short plain line, and never
//! shows an error or forces the picker open:
//!
//! * an OpenCode model picked in `/model` that the free catalog no longer lists on the next launch
//!   (`Muse Spark 1.3 Free is no longer offered — using Big Pickle`);
//! * a vendor model picked from a signed-in vendor whose CLI no longer lists it on the next launch
//!   (`Sonnet is no longer offered — using Default (recommended)`, the vendor's default);
//! * that vendor signed out on the next launch (`Claude is signed out — using Big Pickle`).
//!
//! Hermetic: the answering fake `opencode serve` on loopback with a catalog that drops the picked
//! model between launches, and a fake `claude` whose model list and login state change between
//! launches. Opt-in via `WORKSHOP_BIN`, `--include-ignored`.

mod pty_common;

use std::path::Path;
use std::time::Duration;

use pty_common::*;

/// The composer's bottom border line (`╰──… <label> ─╯`).
fn footer(j: &Journey) -> String {
    j.h.screen_contents()
        .lines()
        .rev()
        .find(|l| l.contains('\u{256f}'))
        .map(str::to_owned)
        .unwrap_or_default()
}

fn wait_footer(j: &mut Journey, label: &str, secs: u64) {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    while !footer(j).contains(label) {
        assert!(
            std::time::Instant::now() < deadline,
            "the composer never read {label:?}:\n{}",
            j.h.screen_contents()
        );
        j.h.update(Duration::from_millis(200));
    }
}

/// Two-step Ctrl+C quit; hands the HOME back for the next launch.
fn quit(mut j: Journey) -> tempfile::TempDir {
    j.h.inject_keys(b"\x1b").unwrap();
    j.h.update(Duration::from_millis(300));
    j.h.inject_keys(b"\x03").unwrap();
    j.h.update(Duration::from_millis(400));
    j.h.inject_keys(b"\x03").unwrap();
    let _ = j.h.wait_exit_code(Duration::from_secs(10));
    let _ = j.h.quit();
    j.home
}

fn active_connection(home: &Path) -> String {
    std::fs::read_to_string(home.join(".workshop/active-connection.json")).unwrap_or_default()
}

/// Nothing about the switch reads as a failure, and the picker is closed.
fn assert_plain(j: &Journey, step: &str) {
    let screen = j.h.screen_contents();
    for bad in ["rror", "Couldn't", "failed", "retired"] {
        assert!(
            !screen.contains(bad),
            "{step}: the switch must read as a plain line, not {bad:?}:\n{screen}"
        );
    }
    assert!(
        !screen.contains(PICKER_OPEN),
        "{step}: no picker is forced open:\n{screen}"
    );
    assert_no_plumbing(&j.h, step);
}

/// A picked OpenCode model gone from the free list on the next launch: Big Pickle, one line.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode serve on loopback); run with --include-ignored"]
fn a_retired_opencode_model_routes_back_to_big_pickle_with_one_line() {
    let Some(bin) = bin_from_env() else { return };
    let recorder = tempfile::tempdir().expect("tempdir");
    let record = recorder.path().join("prompts.jsonl");
    let full = fake_opencode_answering(&record);
    let mut j = spawn_with_args("retired-model", &bin, &["--yolo"], &[], Some(full.path()));
    connect_big_pickle(&mut j);

    // 1. Pick Muse Spark 1.3 Free (it has effort levels: Enter opens them, Enter on `Default`
    //    picks the model). The pick is saved for the next launch.
    send_prompt(&mut j, "/model");
    wait_for(&mut j.h, PICKER_OPEN, 15);
    wait_for(&mut j.h, "Muse Spark 1.3 Free", 20);
    j.h.inject_keys(b"spark 1.3").unwrap();
    j.h.update(Duration::from_millis(500));
    j.h.inject_keys(b"\r").unwrap();
    wait_for(&mut j.h, "Models \u{203a} Muse Spark 1.3 Free", 10);
    j.h.inject_keys(b"\r").unwrap();
    wait_picker_closed(&mut j.h, 30);
    wait_footer(&mut j, "Muse Spark 1.3 Free \u{b7} always-approve", 15);
    snapshot(&j.h, &j.dir, "01-muse-picked");
    let home = quit(j);
    assert!(
        active_connection(home.path()).contains("muse-spark-1.3"),
        "the pick is saved"
    );

    // 2. Next launch: OpenCode's catalog no longer lists Muse Spark 1.3. The composer comes up on
    //    Big Pickle with one plain line over it; the saved pick follows; no error, no picker.
    let trimmed = providers_without(&record, &["muse-spark-1.3-contributor-free"]);
    let turn = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../workshop-adapters/tests/fixtures/opencode_serve_turn.jsonl");
    let without_muse = fake_opencode_answering_serving(&record, &turn, 0.01, &trimmed);
    let mut j = spawn_in_with_args(
        "retired-model",
        &bin,
        &["--yolo"],
        &[],
        Some(without_muse.path()),
        home,
    );
    wait_for(&mut j.h, "\u{276f}", 45);
    wait_for(
        &mut j.h,
        "Muse Spark 1.3 Free is no longer offered \u{2014} using Big Pickle",
        60,
    );
    wait_footer(&mut j, "Big Pickle \u{b7} always-approve", 15);
    assert_plain(&j, "retired OpenCode model");
    snapshot(&j.h, &j.dir, "02-muse-gone-big-pickle");
    assert!(
        active_connection(j.home.path()).contains("big-pickle"),
        "the fallback is saved for the next launch: {}",
        active_connection(j.home.path())
    );

    // 3. The first message runs on Big Pickle.
    send_prompt(&mut j, "hello");
    wait_for(&mut j.h, "Created hello.txt with the exact line.", 90);
    let prompts: Vec<serde_json::Value> = std::fs::read_to_string(&record)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("recorded prompt body is JSON"))
        .collect();
    assert_eq!(
        prompts.last().map(|p| p["model"]["modelID"].clone()),
        Some(serde_json::Value::String("big-pickle".into())),
        "the turn went to the default: {prompts:?}"
    );
    snapshot(&j.h, &j.dir, "03-turn-on-big-pickle");
    let _ = quit(j);
}

/// A fake `claude` whose model list is `<state>/models` (a JSON array of `{value, displayName}`)
/// and whose login state is the `<state>/logged_in` marker — both changed between launches.
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
    models=$(cat "$state/models")
    printf '%s\n' "{{\"type\":\"control_response\",\"response\":{{\"subtype\":\"success\",\"request_id\":\"workshop-models\",\"response\":{{\"models\":$models,\"account\":{{\"email\":\"user@example.com\",\"subscriptionType\":\"max\"}}}}}}}}"
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

const THREE_MODELS: &str = r#"[{"value":"default","displayName":"Default (recommended)"},{"value":"opus[1m]","displayName":"Opus (1M context)"},{"value":"sonnet","displayName":"Sonnet"}]"#;
const WITHOUT_SONNET: &str = r#"[{"value":"default","displayName":"Default (recommended)"},{"value":"opus[1m]","displayName":"Opus (1M context)"}]"#;

/// Make the vendor's cached model list old enough for the next launch to ask the CLI again
/// (a list younger than a minute is reused as is).
fn age_vendor_cache(home: &Path) {
    let path = home.join(".workshop/catalog-cache/claude-models.json");
    let text = std::fs::read_to_string(&path).expect("the picked vendor's list is cached");
    let mut doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    doc["fetched_at_secs"] = serde_json::Value::from(1_000_000u64);
    std::fs::write(&path, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();
}

/// A picked vendor model gone from the signed-in vendor's list: the vendor's default, one line;
/// the vendor signed out: Big Pickle, one line.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake claude + fake opencode); run with --include-ignored"]
fn a_retired_vendor_model_routes_to_the_vendors_default_then_big_pickle() {
    let Some(bin) = bin_from_env() else { return };
    let recorder = tempfile::tempdir().expect("tempdir");
    let record = recorder.path().join("prompts.jsonl");
    let fakes = fake_opencode_answering(&record);
    let state = recorder.path().join("claude-state");
    fake_claude(fakes.path(), &state);
    std::fs::write(state.join("models"), THREE_MODELS).unwrap();
    std::fs::write(state.join("logged_in"), "1").unwrap();
    let mut j = spawn_with_args(
        "retired-vendor-model",
        &bin,
        &["--yolo"],
        &[],
        Some(fakes.path()),
    );
    connect_big_pickle(&mut j);

    // 1. `/auth` → Claude (signed in, `✓ Max ▸`) → Sonnet. Saved for the next launch.
    send_prompt(&mut j, "/auth");
    wait_for(&mut j.h, PICKER_OPEN, 15);
    wait_for(&mut j.h, "\u{2713} Max", 30);
    j.h.inject_keys(b"\r").unwrap();
    wait_for(&mut j.h, "Models \u{203a} Claude", 10);
    move_selection_to(&mut j.h, "Sonnet");
    j.h.inject_keys(b"\r").unwrap();
    wait_picker_closed(&mut j.h, 30);
    wait_footer(&mut j, "Sonnet \u{b7} always-approve", 15);
    snapshot(&j.h, &j.dir, "01-sonnet-picked");
    let home = quit(j);
    assert!(active_connection(home.path()).contains("sonnet"));

    // 2. Next launch: the CLI no longer lists Sonnet → Claude's default, one line.
    std::fs::write(state.join("models"), WITHOUT_SONNET).unwrap();
    age_vendor_cache(home.path());
    let mut j = spawn_in_with_args(
        "retired-vendor-model",
        &bin,
        &["--yolo"],
        &[],
        Some(fakes.path()),
        home,
    );
    wait_for(&mut j.h, "\u{276f}", 45);
    wait_for(
        &mut j.h,
        "Sonnet is no longer offered \u{2014} using Default (recommended)",
        60,
    );
    wait_footer(&mut j, "Default (recommended) \u{b7} always-approve", 15);
    assert_plain(&j, "retired vendor model");
    snapshot(&j.h, &j.dir, "02-sonnet-gone-vendor-default");
    assert!(
        active_connection(j.home.path()).contains("\"default\""),
        "the vendor's default is saved: {}",
        active_connection(j.home.path())
    );
    let home = quit(j);

    // 3. Next launch: signed out of Claude → Big Pickle, one line.
    std::fs::remove_file(state.join("logged_in")).unwrap();
    age_vendor_cache(home.path());
    let mut j = spawn_in_with_args(
        "retired-vendor-model",
        &bin,
        &["--yolo"],
        &[],
        Some(fakes.path()),
        home,
    );
    wait_for(&mut j.h, "\u{276f}", 45);
    wait_for(
        &mut j.h,
        "Claude is signed out \u{2014} using Big Pickle",
        60,
    );
    wait_footer(&mut j, "Big Pickle \u{b7} always-approve", 15);
    assert_plain(&j, "vendor signed out");
    snapshot(&j.h, &j.dir, "03-signed-out-big-pickle");
    assert!(active_connection(j.home.path()).contains("big-pickle"));
    let _ = quit(j);
}
