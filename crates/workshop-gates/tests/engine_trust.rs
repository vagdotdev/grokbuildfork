//! Engine-trust gates through the real TUI (hermetic, PTY-driven; opt-in via `WORKSHOP_BIN`, run
//! with `--include-ignored`). A scripted `opencode serve` stand-in (`fixtures/fake-engine-serve.py`)
//! speaks the 1.18.31 session API with the event shapes captured live, so every gate below pins
//! Workshop's behaviour, not the model's mood:
//!
//! * `plan_does_not_write` — Plan mode sends the read-only `plan` agent; "create hello.txt" leaves
//!   no file behind.
//! * `normal_prompts_on_bash` — Normal mode: "rm -rf tmp" shows Workshop's approval prompt with the
//!   command; `Yes, proceed` posts `once` and the directory goes; `No` posts `reject` and it stays.
//! * `always_approve_runs_without_a_prompt` — Always-approve: the same command runs at once
//!   (`once` posted, no prompt drawn).
//! * `edit_row_expands_to_diff` — a finished `◆ Edit` row carries the engine's diff and `Enter`
//!   on it opens the diff; a `◆ Run` row opens its output; the collapsed-edit setting is honoured.
//! * `resume_replays_transcript` — the engine conversation appears in `/resume` and the welcome
//!   picker; `workshop --resume <id>` and `-c` replay it; a launch nothing was said in prints no
//!   resume hint; the quit hint names the engine session.
//! * `meter_hidden_when_unknown` — no context meter before the first turn, `8.6K / 200K` after it
//!   (the engine's tokens against the live model's limit), and `/context` says the same.
//! * `queued_prompts_are_separate` — Enter during a turn queues; each queued prompt becomes its own
//!   turn with its own bubble and answer.
//! * `reasoning_hidden_by_default` — no reasoning text anywhere by default (inline, glued, as a
//!   block, after a tool call); `reasoning_shown_when_turned_on_in_settings` — `/settings` → "Show
//!   thinking blocks" brings it back as its own block, never glued to the answer.
//! * `engine_answers_as_workshop` — "what are you?" / "who made you?" answer as Workshop's
//!   assistant, never as "opencode", in Normal and Plan mode (the `build` and `plan` agents open
//!   their system prompt with Workshop's identity; the instructions file follows).
//! * `picked_model_survives_the_next_warm_up` — a model picked in `/model` is still the one the
//!   next launch's turns run on after the engine warm-up reads OpenCode's live default.
//! * `announced_action_is_carried_out` / `auto_continue_is_bounded` — a turn that ends on an
//!   announced action with no tool call ("I'll run the installer:") is continued without a word
//!   on screen and logged; a finished answer never is, and a model that keeps announcing is
//!   continued at most twice.
//!
//! Evidence (text + HTML screenshots) lands in `WORKSHOP_PTY_EVIDENCE_DIR/engine-trust/*`.

#![cfg(unix)]

mod pty_common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use pty_common::{Journey, bin_from_env, send_prompt, snapshot, wait_for};

const FIRST_RUN_LABEL: &str = "OpenCode \u{b7} Big Pickle";

fn adapter_fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../workshop-adapters/tests/fixtures")
}

/// Install the scripted engine as `opencode` in `bin`; prompts and permission replies are logged
/// to the returned path.
fn install_fake_engine(bin: &Path) -> PathBuf {
    std::fs::create_dir_all(bin).unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let serve_py = bin.join("fake-engine-serve.py");
    std::fs::copy(fixtures.join("fake-engine-serve.py"), &serve_py).unwrap();
    let providers = adapter_fixtures().join("opencode_serve_providers.json");
    let log = bin.join("engine-requests.jsonl");
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
  [ -f '{delay}' ] && sleep "$(cat '{delay}')"
  exec python3 '{serve}' --port "$5" --providers '{providers}' --log '{log}'
fi
echo "fake opencode: unexpected $*" >&2
exit 2
"#,
        serve = serve_py.display(),
        providers = providers.display(),
        log = log.display(),
        delay = bin.join("start-delay").display(),
    );
    let path = bin.join("opencode");
    std::fs::write(&path, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    log
}

/// What the fake engine was asked (prompt_async bodies) and told (permission replies).
fn engine_log(log: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn agents_sent(log: &Path) -> Vec<String> {
    engine_log(log)
        .iter()
        .filter_map(|v| v.get("agent").and_then(|a| a.as_str()).map(str::to_owned))
        .collect()
}

fn permission_replies(log: &Path) -> Vec<String> {
    engine_log(log)
        .iter()
        .filter_map(|v| {
            v.get("response")
                .and_then(|a| a.as_str())
                .map(str::to_owned)
        })
        .collect()
}

/// The hosted catalog refresh must fail fast and offline: a closed proxy port.
const OFFLINE: &[(&str, &str)] = &[
    ("HTTP_PROXY", "http://127.0.0.1:9"),
    ("HTTPS_PROXY", "http://127.0.0.1:9"),
    ("ALL_PROXY", "http://127.0.0.1:9"),
];

struct Fixture {
    _fakes: tempfile::TempDir,
    bin: PathBuf,
    log: PathBuf,
}

fn fixture() -> Fixture {
    let fakes = tempfile::tempdir().expect("fakes dir");
    let bin = fakes.path().join("bin");
    let log = install_fake_engine(&bin);
    Fixture {
        _fakes: fakes,
        bin,
        log,
    }
}

fn launch(journey: &str, bin: &Path, fx: &Fixture) -> Journey {
    let mut j = pty_common::spawn(journey, bin, OFFLINE, Some(&fx.bin));
    pty_common::connect_big_pickle(&mut j);
    j
}

/// Press Up, then Down, until the selected scrollback entry (drawn inside the `│ … │` selection
/// frame) contains `needle`; the selection starts on the newest entry, so earlier rows are
/// reached with Up.
fn select_row(j: &mut Journey, needle: &str) {
    let selected = |screen: &str| {
        screen.lines().any(|l| {
            let t = l.trim_start();
            t.starts_with('\u{2502}') && t.contains(needle)
        })
    };
    for key in [b"\x1b[A", b"\x1b[B"] {
        for _ in 0..40 {
            if selected(&j.h.screen_contents()) {
                return;
            }
            j.h.inject_keys(key).unwrap();
            j.h.update(Duration::from_millis(120));
        }
    }
    panic!(
        "never reached a selected row containing {needle:?}\nscreen:\n{}",
        j.h.screen_contents()
    );
}

fn wait_gone(j: &mut Journey, text: &str, secs: u64) {
    if let Err(e) = j.h.wait_for_text_absent(text, Duration::from_secs(secs)) {
        panic!(
            "timed out waiting for {text:?} to disappear: {e}\nscreen:\n{}",
            j.h.screen_contents()
        );
    }
}

/// Shift+Tab until the composer's mode label reads `mode` (`plan`, `auto`, `always-approve`; Normal
/// shows no label).
fn set_mode(j: &mut Journey, mode: &str) {
    for _ in 0..5 {
        let screen = j.h.screen_contents();
        let label_line = screen
            .lines()
            .find(|l| l.contains(FIRST_RUN_LABEL))
            .unwrap_or_default()
            .to_owned();
        let current_is = |m: &str| label_line.contains(&format!("Big Pickle \u{b7} {m}"));
        let at_target = match mode {
            "normal" => !["plan", "auto", "always-approve"]
                .iter()
                .any(|m| current_is(m)),
            m => current_is(m),
        };
        if at_target {
            return;
        }
        j.h.inject_keys(b"\x1b[Z").unwrap();
        j.h.update(Duration::from_millis(400));
    }
    panic!(
        "could not reach mode {mode}\nscreen:\n{}",
        j.h.screen_contents()
    );
}

/// Ctrl+C twice ends the session; returns the terminal text after exit (the quit summary).
fn quit(j: &mut Journey) -> String {
    j.h.inject_keys(b"\x03").unwrap();
    j.h.update(Duration::from_millis(400));
    j.h.inject_keys(b"\x03").unwrap();
    let _ = j.h.wait_exit_code(Duration::from_secs(10));
    j.h.update(Duration::from_millis(300));
    j.h.screen_contents()
}

/// Plan mode → the engine's read-only `plan` agent; nothing is written.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode serve); run with --include-ignored"]
fn plan_does_not_write() {
    let Some(bin) = bin_from_env() else { return };
    let fx = fixture();
    let mut j = launch("engine-trust/plan-does-not-write", &bin, &fx);
    set_mode(&mut j, "plan");
    snapshot(&j.h, &j.dir, "01-plan-mode");
    send_prompt(&mut j, "create hello.txt containing hi");
    wait_for(&mut j.h, "plan mode is read-only", 60);
    j.h.update(Duration::from_millis(500));
    snapshot(&j.h, &j.dir, "02-plan-answer-no-file");
    assert!(
        !j.cwd.path().join("hello.txt").exists(),
        "plan mode must not create files"
    );
    assert_eq!(
        agents_sent(&fx.log),
        vec!["plan"],
        "the prompt went to OpenCode's read-only plan agent"
    );
    assert!(
        permission_replies(&fx.log).is_empty(),
        "the plan agent never asks; nothing to approve"
    );
    let screen = j.h.screen_contents();
    assert!(
        !screen.contains("Allow"),
        "no approval prompt in plan mode:\n{screen}"
    );
    quit(&mut j);
}

/// Normal mode → the engine asks before `rm -rf tmp`; Workshop's approval prompt decides.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode serve); run with --include-ignored"]
fn normal_prompts_on_bash() {
    let Some(bin) = bin_from_env() else { return };
    let fx = fixture();
    let mut j = launch("engine-trust/normal-prompts-on-bash", &bin, &fx);
    std::fs::create_dir_all(j.cwd.path().join("tmp")).unwrap();
    std::fs::write(j.cwd.path().join("tmp/junk"), "x").unwrap();
    set_mode(&mut j, "normal");

    // 1. Reject: the prompt shows the command; "No" posts reject; tmp stays.
    send_prompt(&mut j, "rm -rf tmp");
    wait_for(&mut j.h, "Allow Execute?", 60);
    wait_for(&mut j.h, "rm -rf tmp", 5);
    wait_for(&mut j.h, "Yes, proceed", 5);
    j.h.update(Duration::from_millis(400));
    snapshot(&j.h, &j.dir, "01-approval-prompt");
    assert!(
        j.cwd.path().join("tmp").exists(),
        "nothing runs before the answer"
    );
    let screen = j.h.screen_contents();
    assert!(
        screen.contains("don't ask again for `rm *`"),
        "the always option names the engine's pattern:\n{screen}"
    );
    j.h.inject_keys(b"3").unwrap(); // No, reject
    wait_for(&mut j.h, "tmp was left alone", 30);
    snapshot(&j.h, &j.dir, "02-rejected");
    assert!(
        j.cwd.path().join("tmp").exists(),
        "a rejected command must not run"
    );
    assert_eq!(permission_replies(&fx.log), vec!["reject"]);

    // 2. Approve: "Yes, proceed" posts once; the directory goes.
    send_prompt(&mut j, "please rm -rf tmp now");
    wait_for(&mut j.h, "Allow Execute?", 60);
    j.h.update(Duration::from_millis(300));
    j.h.inject_keys(b"1").unwrap();
    wait_for(&mut j.h, "Removed tmp.", 30);
    j.h.update(Duration::from_millis(400));
    snapshot(&j.h, &j.dir, "03-approved-and-ran");
    assert!(
        !j.cwd.path().join("tmp").exists(),
        "an approved command runs"
    );
    assert_eq!(permission_replies(&fx.log), vec!["reject", "once"]);
    assert_eq!(agents_sent(&fx.log), vec!["build", "build"]);
    quit(&mut j);
}

/// Always-approve → the same command runs with no prompt drawn.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode serve); run with --include-ignored"]
fn always_approve_runs_without_a_prompt() {
    let Some(bin) = bin_from_env() else { return };
    let fx = fixture();
    let mut j = launch("engine-trust/always-approve-runs", &bin, &fx);
    std::fs::create_dir_all(j.cwd.path().join("tmp")).unwrap();
    set_mode(&mut j, "always-approve");
    send_prompt(&mut j, "rm -rf tmp");
    wait_for(&mut j.h, "Removed tmp.", 60);
    j.h.update(Duration::from_millis(400));
    snapshot(&j.h, &j.dir, "01-ran-without-prompt");
    assert!(!j.cwd.path().join("tmp").exists());
    assert_eq!(permission_replies(&fx.log), vec!["once"]);
    let screen = j.h.screen_contents();
    assert!(
        !screen.contains("Allow Execute?"),
        "always-approve draws no prompt:\n{screen}"
    );
    quit(&mut j);
}

/// Tool rows carry the work: the edit's diff, the command's output; `Enter` opens them.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode serve); run with --include-ignored"]
fn edit_row_expands_to_diff() {
    let Some(bin) = bin_from_env() else { return };
    let fx = fixture();
    let mut j = launch("engine-trust/edit-row-expands-to-diff", &bin, &fx);
    std::fs::write(j.cwd.path().join("hello.txt"), "hi").unwrap();
    std::fs::write(j.cwd.path().join("second.txt"), "2").unwrap();
    set_mode(&mut j, "always-approve");

    send_prompt(&mut j, "edit hello.txt so it says hello");
    wait_for(&mut j.h, "Changed hi to hello", 60);
    j.h.update(Duration::from_millis(500));
    let screen = j.h.screen_contents();
    assert!(
        screen.contains("Edit") && screen.contains("hello.txt"),
        "an Edit row:\n{screen}"
    );
    // Expanded by default (the collapsed-edit-blocks setting is off): the diff is inline, with
    // the engine's removed and added lines.
    // Rendered as `1  hi` (removed) / `1  hello` (added); colours carry the sign, NO_COLOR here.
    let numbered = |want: &str| {
        screen.lines().any(|l| {
            let words: Vec<&str> = l.split_whitespace().collect();
            words == ["1", want]
        })
    };
    assert!(
        numbered("hi") && numbered("hello"),
        "the diff shows under the Edit row:\n{screen}"
    );
    snapshot(&j.h, &j.dir, "01-edit-row-with-diff");

    send_prompt(&mut j, "list files");
    wait_for(&mut j.h, "Here is the listing.", 60);
    j.h.update(Duration::from_millis(400));
    snapshot(&j.h, &j.dir, "02-run-row");

    // Tab into the scrollback; `Enter` on the Edit row opens the diff, on the Run row its output.
    j.h.inject_keys(b"\t").unwrap();
    j.h.update(Duration::from_millis(300));
    select_row(&mut j, "Edit hello.txt");
    wait_for(&mut j.h, "Enter:open", 5);
    j.h.inject_keys(b"\r").unwrap();
    wait_for(&mut j.h, "[x]", 10);
    j.h.update(Duration::from_millis(300));
    snapshot(&j.h, &j.dir, "03-edit-row-opened");
    let screen = j.h.screen_contents();
    assert!(
        screen.contains("hello.txt") && screen.contains("hello"),
        "the opened Edit row shows the diff:\n{screen}"
    );
    j.h.inject_keys(b"\x1b").unwrap();
    wait_gone(&mut j, "[x]", 5);
    select_row(&mut j, "Run ls -1");
    j.h.inject_keys(b"\r").unwrap();
    wait_for(&mut j.h, "second.txt", 10);
    snapshot(&j.h, &j.dir, "04-run-row-opened");
    let screen = j.h.screen_contents();
    assert!(
        screen.contains("hello.txt") && screen.contains("second.txt"),
        "the opened Run row shows the command output:\n{screen}"
    );
    j.h.inject_keys(b"\x1b").unwrap();
    j.h.update(Duration::from_millis(300));
    quit(&mut j);
}

/// The context meter is the live model's: hidden until the engine reports usage, then honest.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode serve); run with --include-ignored"]
fn meter_hidden_when_unknown() {
    let Some(bin) = bin_from_env() else { return };
    let fx = fixture();
    let mut j = launch("engine-trust/meter-hidden-when-unknown", &bin, &fx);
    // A fresh launch: no meter (nothing is known yet), and no made-up 8.2K window anywhere.
    send_prompt(&mut j, "/context");
    wait_for(&mut j.h, "Context usage", 15);
    wait_for(&mut j.h, "No usage yet", 15);
    j.h.update(Duration::from_millis(300));
    snapshot(&j.h, &j.dir, "01-context-before-first-turn");
    let screen = j.h.screen_contents();
    assert!(
        !screen.contains("8.2K") && !screen.contains("8192") && !screen.contains("/ 200K"),
        "no meter before the engine reports usage:\n{screen}"
    );
    assert!(screen.contains("200K token window"), "{screen}");
    j.h.inject_keys(b"\x1b").unwrap();
    wait_gone(&mut j, "Context usage", 5);

    send_prompt(&mut j, "hello there");
    wait_for(&mut j.h, "Echo: hello there", 60);
    wait_for(&mut j.h, "8.6K / 200K", 10);
    snapshot(&j.h, &j.dir, "02-meter-after-turn");
    send_prompt(&mut j, "/context");
    wait_for(&mut j.h, "8.6K / 200K tokens", 15);
    j.h.update(Duration::from_millis(300));
    snapshot(&j.h, &j.dir, "03-context-after-turn");
    let screen = j.h.screen_contents();
    assert!(
        !screen.contains("Tool definitions") && !screen.contains("workshop-connection"),
        "/context shows the engine's numbers, not the placeholder's:\n{screen}"
    );
    j.h.inject_keys(b"\x1b").unwrap();
    j.h.update(Duration::from_millis(300));
    quit(&mut j);
}

/// Prompts sent during a turn wait, then go out one turn each.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode serve); run with --include-ignored"]
fn queued_prompts_are_separate() {
    let Some(bin) = bin_from_env() else { return };
    let fx = fixture();
    let mut j = launch("engine-trust/queued-prompts-are-separate", &bin, &fx);
    send_prompt(&mut j, "slow one");
    // The turn is under way (the engine took the prompt) before the next one is typed; a burst
    // of keys with newlines inside would read as a paste, which is not what a user does.
    wait_for(&mut j.h, "Waiting for Big Pickle", 30);
    send_prompt(&mut j, "two");
    wait_for(&mut j.h, "Queued (1)", 10);
    snapshot(&j.h, &j.dir, "01-queued-toast");
    j.h.update(Duration::from_millis(300));
    send_prompt(&mut j, "three");
    wait_for(&mut j.h, "Queued (2)", 10);
    // The queued prompts go out one turn each, in order; the last one ends the sequence.
    wait_for(&mut j.h, "Echo: three", 90);
    j.h.update(Duration::from_millis(400));
    snapshot(&j.h, &j.dir, "02-last-queued-turn");
    let prompts: Vec<String> = engine_log(&fx.log)
        .iter()
        .filter_map(|v| v.get("text").and_then(|t| t.as_str()).map(str::to_owned))
        .collect();
    assert_eq!(
        prompts,
        vec!["slow one", "two", "three"],
        "three prompts, three turns, nothing joined"
    );
    // Each turn pinned its own prompt at the top on send; page back up to see them all.
    j.h.inject_keys(b"\t").unwrap();
    j.h.update(Duration::from_millis(300));
    let mut seen = String::new();
    for _ in 0..6 {
        seen.push_str(&j.h.screen_contents());
        if seen.contains("Echo: slow one") {
            break;
        }
        j.h.inject_keys(b"\x1b[5~").unwrap(); // PageUp
        j.h.update(Duration::from_millis(300));
    }
    snapshot(&j.h, &j.dir, "03-earlier-turns");
    for needle in [
        "\u{276f} slow one",
        "Echo: slow one",
        "\u{276f} two",
        "Echo: two",
        "\u{276f} three",
        "Echo: three",
    ] {
        assert!(
            seen.contains(needle),
            "{needle} missing from the transcript:\n{seen}"
        );
    }
    assert!(
        !seen.contains("twothree") && !seen.contains("slow onetwo") && !seen.contains("onetwo"),
        "queued prompts are never concatenated:\n{seen}"
    );
    j.h.inject_keys(b"\x1b").unwrap();
    quit(&mut j);
}

/// Text of the model's reasoning in the fake engine's script (`fixtures/fake-engine-serve.py`).
const REASONING: [&str; 2] = ["Keep it brief", "Summarize it"];

fn assert_no_thinking(screen: &str) {
    for text in REASONING.iter().chain(&["Thought", "Thinking"]) {
        assert!(
            !screen.contains(text),
            "no thinking by default ({text:?} on screen):\n{screen}"
        );
    }
}

/// The model's thinking is not shown by default — not inline, not glued to the answer, not as a
/// block, not after a tool call; the answer and the tool row are.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode serve); run with --include-ignored"]
fn reasoning_hidden_by_default() {
    let Some(bin) = bin_from_env() else { return };
    let fx = fixture();
    let mut j = launch("engine-trust/reasoning-hidden-by-default", &bin, &fx);
    send_prompt(&mut j, "think about it");
    wait_for(&mut j.h, "Echo: think about it", 60);
    j.h.update(Duration::from_millis(500));
    snapshot(&j.h, &j.dir, "01-answer-only");
    assert_no_thinking(&j.h.screen_contents());

    send_prompt(&mut j, "think, then list files");
    wait_for(&mut j.h, "Here is the listing.", 60);
    j.h.update(Duration::from_millis(500));
    snapshot(&j.h, &j.dir, "02-after-tool-call");
    let screen = j.h.screen_contents();
    assert!(screen.contains("ls -1"), "the tool row is shown:\n{screen}");
    assert_no_thinking(&screen);
    assert!(
        !screen.lines().any(is_bare_timestamp),
        "the whitespace-only text part after the thinking opens no empty reply row:\n{screen}"
    );
    quit(&mut j);
}

/// A transcript row holding nothing but its timestamp ("9:23 AM"): an empty reply.
fn is_bare_timestamp(line: &str) -> bool {
    let t = line.trim().trim_end_matches('\u{2588}').trim();
    let Some((clock, half)) = t.split_once(' ') else {
        return false;
    };
    (half == "AM" || half == "PM")
        && clock.split_once(':').is_some_and(|(h, m)| {
            (1..=2).contains(&h.len())
                && m.len() == 2
                && h.chars().chain(m.chars()).all(|c| c.is_ascii_digit())
        })
}

/// The prompts the fake engine received, in order.
fn prompts_sent(log: &Path) -> Vec<String> {
    engine_log(log)
        .iter()
        .filter_map(|v| v.get("text").and_then(|t| t.as_str()).map(str::to_owned))
        .collect()
}

/// A turn that ends right after announcing an action ("I'll run the installer:") with no tool
/// call is continued without a word on screen: the action runs, the answer lands in the same
/// turn, and the engine log records the continuation.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode serve); run with --include-ignored"]
fn announced_action_is_carried_out() {
    let Some(bin) = bin_from_env() else { return };
    let fx = fixture();
    let mut j = launch("engine-trust/announced-action-is-carried-out", &bin, &fx);
    send_prompt(&mut j, "download it and install the tool");
    wait_for(&mut j.h, "Installed the tool.", 60);
    j.h.update(Duration::from_millis(500));
    snapshot(&j.h, &j.dir, "01-carried-out");
    let screen = j.h.screen_contents();
    assert!(
        screen.contains("echo installed"),
        "the announced command ran:\n{screen}"
    );
    assert!(
        !screen.contains("Continue"),
        "the continuation is never shown:\n{screen}"
    );
    let prompts = prompts_sent(&fx.log);
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(prompts[1].starts_with("Continue:"), "{prompts:?}");
    let log = std::fs::read_to_string(j.workshop_home().join("logs/opencode-engine.log"))
        .unwrap_or_default();
    assert!(
        log.contains("auto-continue 1/2"),
        "the continuation is logged for workshop doctor:\n{log}"
    );
    quit(&mut j);
}

/// A finished answer is never continued, and a model that keeps announcing is continued at most
/// twice before the turn ends.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode serve); run with --include-ignored"]
fn auto_continue_is_bounded() {
    let Some(bin) = bin_from_env() else { return };
    let fx = fixture();
    let mut j = launch("engine-trust/auto-continue-is-bounded", &bin, &fx);
    send_prompt(&mut j, "hello");
    wait_for(&mut j.h, "Echo: hello", 60);
    send_prompt(&mut j, "keep announcing");
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while prompts_sent(&fx.log).len() < 4 && std::time::Instant::now() < deadline {
        j.h.update(Duration::from_millis(300));
    }
    j.h.update(Duration::from_secs(3));
    snapshot(&j.h, &j.dir, "01-bounded");
    let prompts = prompts_sent(&fx.log);
    assert_eq!(
        prompts.len(),
        4,
        "hello once, keep announcing once plus two continuations: {prompts:?}"
    );
    assert_eq!(prompts[0], "hello");
    assert!(prompts[2].starts_with("Continue:") && prompts[3].starts_with("Continue:"));
    quit(&mut j);
}

/// `/settings` → "Show thinking blocks" brings the thinking back, as its own block that is never
/// glued to the answer.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode serve); run with --include-ignored"]
fn reasoning_shown_when_turned_on_in_settings() {
    let Some(bin) = bin_from_env() else { return };
    let fx = fixture();
    let mut j = launch("engine-trust/reasoning-shown-when-turned-on", &bin, &fx);
    let row = |screen: &str| {
        screen
            .lines()
            .find(|l| l.contains("Show thinking blocks"))
            .unwrap_or_default()
            .to_owned()
    };
    send_prompt(&mut j, "/settings");
    wait_for(&mut j.h, "Space", 20);
    j.h.inject_keys(b"/").unwrap();
    j.h.update(Duration::from_millis(300));
    j.h.inject_keys(b"thinking blocks").unwrap();
    wait_for(&mut j.h, "search: thinking blocks", 20);
    j.h.inject_keys(b"\r").unwrap();
    j.h.update(Duration::from_millis(400));
    snapshot(&j.h, &j.dir, "01-settings-row-off");
    let screen = j.h.screen_contents();
    assert!(row(&screen).contains(" off "), "off by default:\n{screen}");
    j.h.inject_keys(b" ").unwrap();
    j.h.update(Duration::from_millis(600));
    snapshot(&j.h, &j.dir, "02-settings-row-on");
    let screen = j.h.screen_contents();
    assert!(
        row(&screen).contains(" on "),
        "Space turns it on:\n{screen}"
    );
    j.h.inject_keys(b"\x1b").unwrap();
    j.h.update(Duration::from_millis(600));
    let config = std::fs::read_to_string(j.workshop_home().join("config.toml")).unwrap();
    assert!(config.contains("show_thinking_blocks = true"), "{config}");

    send_prompt(&mut j, "think about it");
    wait_for(&mut j.h, "Echo: think about it", 60);
    j.h.update(Duration::from_millis(500));
    snapshot(&j.h, &j.dir, "03-thinking-block");
    let screen = j.h.screen_contents();
    assert!(
        screen.contains("Thought"),
        "the setting shows thinking as a block:\n{screen}"
    );
    assert!(
        !screen.contains("brief.Echo"),
        "thinking is never glued to the answer:\n{screen}"
    );
    quit(&mut j);
}

/// The answer lines containing `needle` (the composer label names the engine, the answers must not).
fn answer_lines<'a>(screen: &'a str, needle: &str) -> Vec<&'a str> {
    screen
        .lines()
        .filter(|l| l.contains(needle) && !l.contains('\u{276f}'))
        .collect()
}

fn assert_answers_as_workshop(j: &mut Journey, question: &str) {
    const ANSWER: &str = "Workshop's coding assistant";
    let answered = |screen: &str| {
        let mut lines = screen.lines();
        lines.any(|l| l.contains(&format!("\u{276f} {question}")))
            && lines.any(|l| l.contains(ANSWER))
    };
    send_prompt(j, question);
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while !answered(&j.h.screen_contents()) {
        assert!(
            std::time::Instant::now() < deadline,
            "{question:?} got no answer as Workshop's assistant:\n{}",
            j.h.screen_contents()
        );
        j.h.update(Duration::from_millis(200));
    }
    j.h.update(Duration::from_millis(400));
    let screen = j.h.screen_contents();
    let lines = answer_lines(&screen, ANSWER);
    assert!(!lines.is_empty(), "{screen}");
    for line in lines {
        let line = line.to_lowercase();
        for other in ["opencode", "anomaly", "grok"] {
            assert!(
                !line.contains(other),
                "{question:?} must not name {other}:\n{screen}"
            );
        }
    }
}

/// "what are you?" and "who made you?" answer as Workshop's assistant, never as "opencode", in
/// Normal and Plan mode: the agent each turn runs on (`build`, `plan`) opens its system prompt
/// with Workshop's identity instead of the model family's, and the instructions file follows.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode serve); run with --include-ignored"]
fn engine_answers_as_workshop() {
    let Some(bin) = bin_from_env() else { return };
    let fx = fixture();
    let mut j = launch("engine-trust/engine-answers-as-workshop", &bin, &fx);
    assert_answers_as_workshop(&mut j, "what are you?");
    snapshot(&j.h, &j.dir, "01-what-are-you");
    assert_answers_as_workshop(&mut j, "who made you?");
    snapshot(&j.h, &j.dir, "02-who-made-you");
    set_mode(&mut j, "plan");
    assert_answers_as_workshop(&mut j, "what are you? (plan)");
    snapshot(&j.h, &j.dir, "03-plan-what-are-you");

    let heads: Vec<(String, String)> = engine_log(&fx.log)
        .iter()
        .filter_map(|v| {
            Some((
                v.get("agent")?.as_str()?.to_owned(),
                v.get("system_head")?.as_str()?.to_owned(),
            ))
        })
        .collect();
    assert!(
        heads.iter().any(|(a, _)| a == "build") && heads.iter().any(|(a, _)| a == "plan"),
        "{heads:?}"
    );
    for (agent, head) in &heads {
        assert!(
            head.starts_with("You are Workshop's coding assistant"),
            "{agent}: {head}"
        );
    }
    let instructions = j.workshop_home().join("engine").join("instructions.md");
    let text = std::fs::read_to_string(&instructions).expect("instructions file under the home");
    assert!(text.contains("Workshop's coding assistant"), "{text}");
    assert!(
        !j.cwd.path().join("AGENTS.md").exists(),
        "nothing is written into the user's project"
    );
    quit(&mut j);
}

/// The same promises against the real `opencode` (keyless Big Pickle, network): the proof run
/// behind the v0.2.2 evidence. Needs a genuine `opencode` on `PATH` and `WORKSHOP_LIVE_OPENCODE=1`;
/// never runs in CI. Screens land in `WORKSHOP_PTY_EVIDENCE_DIR/engine-trust/live-*`.
#[test]
#[ignore = "needs WORKSHOP_BIN, a real opencode on PATH and network; run with WORKSHOP_LIVE_OPENCODE=1 --include-ignored"]
fn live_engine_trust_journey() {
    let Some(bin) = bin_from_env() else { return };
    if std::env::var_os("WORKSHOP_LIVE_OPENCODE").is_none() {
        eprintln!("WORKSHOP_LIVE_OPENCODE not set; skipping");
        return;
    }
    let mut j = pty_common::spawn("engine-trust/live-safety-modes", &bin, &[], None);
    pty_common::connect_big_pickle(&mut j);
    std::fs::create_dir_all(j.cwd.path().join("tmp")).unwrap();
    std::fs::write(j.cwd.path().join("tmp/junk"), "x").unwrap();

    // Plan: a file request is planned, not done.
    set_mode(&mut j, "plan");
    send_prompt(
        &mut j,
        "Create a file named hello.txt containing the word hi.",
    );
    wait_for(&mut j.h, "Waiting for Big Pickle", 120);
    wait_gone(&mut j, "Waiting for Big Pickle", 180);
    j.h.update(Duration::from_millis(1500));
    snapshot(&j.h, &j.dir, "01-plan-mode-answer");
    assert!(
        !j.cwd.path().join("hello.txt").exists(),
        "plan mode must not create files"
    );

    // Normal: the engine asks before `rm -rf tmp`; approve; it runs.
    set_mode(&mut j, "normal");
    send_prompt(
        &mut j,
        "Run exactly this shell command and nothing else: rm -rf tmp",
    );
    wait_for(&mut j.h, "Allow Execute?", 180);
    j.h.update(Duration::from_millis(500));
    snapshot(&j.h, &j.dir, "02-normal-mode-prompt");
    assert!(
        j.cwd.path().join("tmp").exists(),
        "nothing runs before the answer"
    );
    let screen = j.h.screen_contents();
    assert!(screen.contains("rm -rf tmp"), "{screen}");
    j.h.inject_keys(b"1").unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    while j.cwd.path().join("tmp").exists() && std::time::Instant::now() < deadline {
        j.h.update(Duration::from_millis(500));
    }
    assert!(
        !j.cwd.path().join("tmp").exists(),
        "the approved command ran"
    );
    wait_gone(&mut j, "Allow Execute?", 60);
    j.h.update(Duration::from_millis(3000));
    snapshot(&j.h, &j.dir, "03-normal-mode-approved");

    // Always-approve: the same kind of command runs with no prompt.
    std::fs::create_dir_all(j.cwd.path().join("tmp2")).unwrap();
    set_mode(&mut j, "always-approve");
    send_prompt(
        &mut j,
        "Run exactly this shell command and nothing else: rm -rf tmp2",
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    while j.cwd.path().join("tmp2").exists() && std::time::Instant::now() < deadline {
        j.h.update(Duration::from_millis(500));
        assert!(
            !j.h.screen_contents().contains("Allow Execute?"),
            "always-approve draws no prompt"
        );
    }
    assert!(
        !j.cwd.path().join("tmp2").exists(),
        "always-approve ran the command"
    );
    j.h.update(Duration::from_millis(3000));
    snapshot(&j.h, &j.dir, "04-always-approve-ran");

    // Identity and reasoning on the live model.
    send_prompt(&mut j, "What are you? Answer in one sentence.");
    wait_for(&mut j.h, "Workshop", 180);
    j.h.update(Duration::from_millis(4000));
    snapshot(&j.h, &j.dir, "05-identity-and-context-meter");
    let screen = j.h.screen_contents();
    assert!(!screen.contains("I'm opencode"), "{screen}");
    assert!(
        screen.contains("/ 200K"),
        "live meter against the model's window:\n{screen}"
    );
    let after = quit(&mut j);
    std::fs::write(j.dir.join("06-quit-hint.txt"), &after).unwrap();
    assert!(after.contains("workshop --resume ses_"), "{after}");
    eprintln!("evidence: {}", j.dir.display());
}

/// A model picked in `/model` stays picked: the next launch's engine warm-up, which reads
/// OpenCode's live default, does not put the connection back on Big Pickle.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode serve); run with --include-ignored"]
fn picked_model_survives_the_next_warm_up() {
    const LING: &str = "OpenCode \u{b7} Ling 3.0 Flash Fin Free";
    let Some(bin) = bin_from_env() else { return };
    let fx = fixture();
    let mut j = launch("engine-trust/picked-model-survives-warm-up", &bin, &fx);
    send_prompt(&mut j, "hello");
    wait_for(&mut j.h, "Echo: hello", 60);
    send_prompt(&mut j, "/model");
    wait_for(&mut j.h, "Tab: Subscriptions", 15);
    j.h.inject_keys(b"fin free").unwrap();
    wait_for(&mut j.h, "Ling 3.0 Flash Fin Free", 15);
    j.h.update(Duration::from_millis(400));
    j.h.inject_keys(b"\r").unwrap();
    wait_for(&mut j.h, LING, 15);
    quit(&mut j);

    // A real `opencode serve` takes seconds to come up, so the warm-up the first keystroke starts
    // finishes while that first turn is already waiting on the engine.
    std::fs::write(fx.bin.join("start-delay"), "2").unwrap();
    let mut j = pty_common::spawn_in(
        "engine-trust/picked-model-survives-warm-up",
        &bin,
        OFFLINE,
        Some(&fx.bin),
        j.home,
    );
    wait_for(&mut j.h, LING, 45);
    send_prompt(&mut j, "first after relaunch");
    wait_for(&mut j.h, "Echo: first after relaunch", 60);
    send_prompt(&mut j, "second after relaunch");
    wait_for(&mut j.h, "Echo: second after relaunch", 60);
    j.h.update(Duration::from_millis(500));
    snapshot(&j.h, &j.dir, "01-relaunch-keeps-ling");
    let screen = j.h.screen_contents();
    assert!(screen.contains(LING), "the label keeps the pick:\n{screen}");
    let models: Vec<String> = engine_log(&fx.log)
        .iter()
        .filter(|v| {
            v.get("text")
                .and_then(|t| t.as_str())
                .is_some_and(|t| t.contains("after relaunch"))
        })
        .map(|v| v["model"].to_string())
        .collect();
    assert_eq!(models.len(), 2, "{models:?}");
    assert!(
        models.iter().all(|m| m.contains("ling-3.0-flash-fin-free")),
        "both turns ran on the picked model: {models:?}"
    );
    quit(&mut j);
}

/// The engine conversation is what resume finds, replays and continues.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode serve); run with --include-ignored"]
fn resume_replays_transcript() {
    let Some(bin) = bin_from_env() else { return };
    let fx = fixture();
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    std::process::Command::new("git")
        .args(["init", "-q", "."])
        .current_dir(cwd.path())
        .status()
        .unwrap();
    let dir = pty_common::evidence_dir("engine-trust/resume-replays-transcript");
    let spawn = |args: &[&str]| {
        let inherited = std::env::var("PATH").unwrap_or_default();
        let path_s = format!("{}:{inherited}", fx.bin.display());
        let home_s = home.path().to_string_lossy().to_string();
        let wh_s = home.path().join(".workshop").to_string_lossy().to_string();
        let mut env: Vec<(&str, &str)> = vec![
            ("HOME", home_s.as_str()),
            ("WORKSHOP_HOME", wh_s.as_str()),
            ("PATH", path_s.as_str()),
            ("TERM", "xterm-256color"),
            ("NO_COLOR", "1"),
            ("GROK_DISABLE_AUTOUPDATER", "1"),
        ];
        env.extend_from_slice(OFFLINE);
        let mut h = xai_grok_pager_pty_harness::PtyHarness::new_inherited_env(
            &bin,
            45,
            140,
            args,
            &env,
            Some(cwd.path()),
        )
        .expect("spawn workshop in pty");
        h.set_respond_to_queries(true);
        h
    };

    // 1. A launch nothing was said in: no session hint on quit.
    let mut h = spawn(&[]);
    wait_for(&mut h, FIRST_RUN_LABEL, 45);
    h.update(Duration::from_millis(1200));
    h.inject_keys(b"\x03").unwrap();
    h.update(Duration::from_millis(400));
    h.inject_keys(b"\x03").unwrap();
    let _ = h.wait_exit_code(Duration::from_secs(10));
    h.update(Duration::from_millis(300));
    let after_idle_quit = h.screen_contents();
    std::fs::write(dir.join("01-idle-quit.txt"), &after_idle_quit).unwrap();
    assert!(
        !after_idle_quit.contains("Resume this session"),
        "an idle launch leaves nothing to resume:\n{after_idle_quit}"
    );
    assert!(
        !home.path().join(".workshop/engine/sessions").exists(),
        "no engine session is recorded for an idle launch"
    );

    // 2. A conversation; the quit hint names the engine session.
    let mut h = spawn(&[]);
    wait_for(&mut h, FIRST_RUN_LABEL, 45);
    h.update(Duration::from_millis(800));
    h.inject_keys(b"remember the word pelican").unwrap();
    h.update(Duration::from_millis(300));
    h.inject_keys(b"\r").unwrap();
    wait_for(&mut h, "Echo: remember the word pelican", 60);
    h.update(Duration::from_millis(500));
    std::fs::write(dir.join("02-first-conversation.txt"), h.screen_contents()).unwrap();
    std::fs::write(dir.join("02-first-conversation.html"), h.screen_html()).unwrap();
    h.inject_keys(b"\x03").unwrap();
    h.update(Duration::from_millis(400));
    h.inject_keys(b"\x03").unwrap();
    let _ = h.wait_exit_code(Duration::from_secs(10));
    h.update(Duration::from_millis(300));
    let after_quit = h.screen_contents();
    std::fs::write(dir.join("03-quit-hint.txt"), &after_quit).unwrap();
    let hint_line = after_quit
        .lines()
        .find(|l| l.contains("workshop --resume ses_"))
        .unwrap_or_else(|| panic!("quit hint names the engine session:\n{after_quit}"))
        .to_owned();
    let session_id = hint_line.split_whitespace().last().unwrap().to_owned();
    let sessions_dir = home.path().join(".workshop/engine/sessions");
    assert!(
        sessions_dir.join(format!("{session_id}.json")).exists(),
        "{session_id}"
    );

    // 3. `--resume <id>`: the transcript is back, the same engine session continues.
    let mut h = spawn(&["--resume", &session_id]);
    wait_for(&mut h, "remember the word pelican", 45);
    wait_for(&mut h, "Echo: remember the word pelican", 10);
    wait_for(&mut h, "Resumed", 10);
    h.update(Duration::from_millis(500));
    std::fs::write(dir.join("04-resumed-by-id.txt"), h.screen_contents()).unwrap();
    std::fs::write(dir.join("04-resumed-by-id.html"), h.screen_html()).unwrap();
    let screen = h.screen_contents();
    assert!(
        !screen.contains("Workshop connection"),
        "the real model label, never the placeholder's:\n{screen}"
    );
    h.inject_keys(b"and now?").unwrap();
    h.update(Duration::from_millis(300));
    h.inject_keys(b"\r").unwrap();
    wait_for(&mut h, "Echo: and now?", 60);
    let sessions: Vec<String> = engine_log(&fx.log)
        .iter()
        .filter_map(|v| v.get("session").and_then(|s| s.as_str()).map(str::to_owned))
        .collect();
    assert_eq!(
        sessions,
        vec![session_id.clone(), session_id.clone()],
        "the resumed turn continues the same OpenCode session"
    );
    h.inject_keys(b"\x03").unwrap();
    h.update(Duration::from_millis(400));
    h.inject_keys(b"\x03").unwrap();
    let _ = h.wait_exit_code(Duration::from_secs(10));

    // 4. `-c`: the most recent engine conversation; `/resume` and the welcome picker list it.
    let mut h = spawn(&["-c"]);
    wait_for(&mut h, "Echo: and now?", 45);
    h.update(Duration::from_millis(400));
    std::fs::write(dir.join("05-resumed-by-c.txt"), h.screen_contents()).unwrap();
    h.inject_keys(b"/resume").unwrap();
    h.update(Duration::from_millis(400));
    h.inject_keys(b"\r").unwrap();
    wait_for(&mut h, "Resume session", 15);
    wait_for(&mut h, "remember the word pelican", 15);
    h.update(Duration::from_millis(400));
    std::fs::write(dir.join("06-resume-picker.txt"), h.screen_contents()).unwrap();
    std::fs::write(dir.join("06-resume-picker.html"), h.screen_html()).unwrap();
    let screen = h.screen_contents();
    assert!(
        !screen.contains("No sessions found") && !screen.contains("No matches"),
        "the picker lists the engine conversation:\n{screen}"
    );
    h.inject_keys(b"\x1b").unwrap();
    h.update(Duration::from_millis(300));
    h.inject_keys(b"\x03").unwrap();
    h.update(Duration::from_millis(400));
    h.inject_keys(b"\x03").unwrap();
    let _ = h.wait_exit_code(Duration::from_secs(10));

    // 5. A plain launch is a new conversation; ctrl+r on the welcome finds the old one and opens it.
    let mut h = spawn(&[]);
    wait_for(&mut h, FIRST_RUN_LABEL, 45);
    h.update(Duration::from_millis(800));
    h.inject_keys(b"\x12").unwrap(); // ctrl+r
    wait_for(&mut h, "remember the word pelican", 15);
    h.update(Duration::from_millis(300));
    std::fs::write(dir.join("07-welcome-picker.txt"), h.screen_contents()).unwrap();
    pty_common::move_selection_to(&mut h, "remember the word pelican");
    h.inject_keys(b"\r").unwrap();
    wait_for(&mut h, "Echo: and now?", 15);
    wait_for(&mut h, "Resumed", 10);
    h.update(Duration::from_millis(400));
    std::fs::write(dir.join("08-picked-from-welcome.txt"), h.screen_contents()).unwrap();
    std::fs::write(dir.join("08-picked-from-welcome.html"), h.screen_html()).unwrap();
    h.inject_keys(b"\x03").unwrap();
    h.update(Duration::from_millis(400));
    h.inject_keys(b"\x03").unwrap();
    let _ = h.wait_exit_code(Duration::from_secs(10));
    eprintln!("evidence: {}", dir.display());
}
