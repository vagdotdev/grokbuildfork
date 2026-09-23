//! Engine-trust gates through the real TUI (hermetic, PTY-driven; opt-in via `WORKSHOP_BIN`, run
//! with `--include-ignored`). A scripted `opencode serve` stand-in (`fixtures/fake-engine-serve.py`)
//! speaks the 1.18.31 session API with the event shapes captured live, so every gate below pins
//! Workshop's behaviour, not the model's mood:
//!
//! * `plan_does_not_write` — Plan mode sends the read-only `plan` agent; "create hello.txt" leaves
//!   no file behind.
//! * `normal_prompts_on_bash` — Normal mode: "rm -rf tmp" shows Workshop's approval prompt with the
//!   command; `Yes, run it` posts `once` and the directory goes; `No` posts `reject` and it stays.
//! * `read_only_commands_run_without_asking` — Normal mode runs `ls -1` with no prompt (the
//!   engine's ask is answered `once`); `rm -rf tmp` still asks.
//! * `out_of_folder_command_asks_once_in_plain_words` — a command touching `~/Desktop` asks the
//!   engine twice (`external_directory`, `bash`) but the user sees one prompt naming the folder,
//!   with the whole command underneath; the second ask follows the first answer.
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
//! * `reasoning_is_a_separate_block` — the model's reasoning is a collapsed thinking block, never
//!   glued to the answer text.
//! * `engine_answers_as_workshop` — "what are you?" answers as Workshop's assistant, never as
//!   "opencode" (the server received Workshop's instructions file).
//! * `engine_starts_at_launch_not_on_enter` — the engine is installed (fresh home, stub installer)
//!   and started the moment the composer opens, before a key is pressed and with nothing on
//!   screen; a returning home starts it at launch too; Enter then reuses that server.
//! * `first_run_starts_in_always_approve_and_a_pick_persists` — with no mode chosen anywhere the
//!   composer opens in always-approve (`Big Pickle · always-approve`, a command runs unprompted);
//!   Shift+Tab reaches the asking mode, the pick is written to the config and the next launch
//!   opens in it.
//! * `new_starts_a_fresh_engine_conversation` — `/new` opens a fresh OpenCode session for the next
//!   prompt (new id, nothing of the old conversation resent, no meter carried over); `-c` still
//!   resumes the most recent conversation.
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
  exec python3 '{serve}' --port "$5" --providers '{providers}' --log '{log}'
fi
echo "fake opencode: unexpected $*" >&2
exit 2
"#,
        serve = serve_py.display(),
        providers = providers.display(),
        log = log.display(),
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
    wait_for(&mut j.h, "Run this command?", 60);
    wait_for(&mut j.h, "rm -rf tmp", 5);
    wait_for(&mut j.h, "Yes, run it", 5);
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

    // 2. Approve: "Yes, run it" posts once; the directory goes.
    send_prompt(&mut j, "please rm -rf tmp now");
    wait_for(&mut j.h, "Run this command?", 60);
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

/// Wait for `needle` while asserting `forbidden` never shows up on the way.
fn wait_for_without(j: &mut Journey, needle: &str, forbidden: &[&str], secs: u64) {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let screen = j.h.screen_contents();
        for f in forbidden {
            assert!(
                !screen.contains(f),
                "{f:?} must not appear while waiting for {needle:?}:\n{screen}"
            );
        }
        if screen.contains(needle) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {needle:?}:\n{screen}"
        );
        j.h.update(Duration::from_millis(150));
    }
}

/// Normal mode: a read-only command (`ls -1`) runs without a prompt; the engine still asked and
/// Workshop answered `once` for it, and the row shows what ran.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode serve); run with --include-ignored"]
fn read_only_commands_run_without_asking() {
    let Some(bin) = bin_from_env() else { return };
    let fx = fixture();
    let mut j = launch("engine-trust/read-only-runs-without-asking", &bin, &fx);
    std::fs::write(j.cwd.path().join("notes.txt"), "n").unwrap();
    set_mode(&mut j, "normal");
    send_prompt(&mut j, "list files");
    wait_for_without(
        &mut j,
        "Here is the listing.",
        &["Run this command?", "Allow"],
        60,
    );
    j.h.update(Duration::from_millis(400));
    snapshot(&j.h, &j.dir, "01-ls-ran-without-prompt");
    let screen = j.h.screen_contents();
    assert!(
        screen.contains("Run ls -1"),
        "the row shows what ran:\n{screen}"
    );
    assert_eq!(
        permission_replies(&fx.log),
        vec!["once"],
        "the engine's ask for the read-only command was answered without a prompt"
    );
    // A command that writes still asks.
    send_prompt(&mut j, "please rm -rf tmp now");
    wait_for(&mut j.h, "Run this command?", 60);
    wait_for(&mut j.h, "rm -rf tmp", 5);
    snapshot(&j.h, &j.dir, "02-rm-still-asks");
    j.h.inject_keys(b"3").unwrap();
    wait_for(&mut j.h, "tmp was left alone", 30);
    quit(&mut j);
}

/// An out-of-folder command asks the engine twice (`external_directory`, then `bash`); the user
/// sees ONE prompt, in plain words, naming the folder, with the whole command underneath.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode serve); run with --include-ignored"]
fn out_of_folder_command_asks_once_in_plain_words() {
    let Some(bin) = bin_from_env() else { return };
    let fx = fixture();
    let mut j = launch("engine-trust/out-of-folder-asks-once", &bin, &fx);
    set_mode(&mut j, "normal");
    send_prompt(&mut j, "make a folder on my desktop with two books");
    wait_for(
        &mut j.h,
        "Run this command? It works outside this folder: ~/Desktop",
        60,
    );
    j.h.update(Duration::from_millis(400));
    snapshot(&j.h, &j.dir, "01-one-prompt-plain-words");
    let screen = j.h.screen_contents();
    assert!(
        !screen.contains("external_directory"),
        "no raw permission name:\n{screen}"
    );
    assert!(
        screen.contains("mkdir -p") && screen.contains("frankenstein.epub"),
        "the whole command is shown (wrapped), not cut off:\n{screen}"
    );
    assert!(
        screen.contains("Yes, run it") && screen.contains("don't ask again for `"),
        "the options:\n{screen}"
    );
    j.h.inject_keys(b"1").unwrap();
    // The engine's second ask (`bash`, same tool call) is answered from the first pick: no
    // second prompt is ever drawn.
    j.h.update(Duration::from_millis(200));
    wait_for_without(
        &mut j,
        "Created the iBooks folder",
        &["Run this command?"],
        60,
    );
    j.h.update(Duration::from_millis(400));
    snapshot(&j.h, &j.dir, "02-ran-after-one-answer");
    assert_eq!(
        permission_replies(&fx.log),
        vec!["once", "once"],
        "both asks answered, one by the user and one from that answer"
    );
    let asked: Vec<(String, String)> = engine_log(&fx.log)
        .iter()
        .filter_map(|v| {
            Some((
                v.get("permission")?.as_str()?.to_owned(),
                v.get("callID")?.as_str()?.to_owned(),
            ))
        })
        .collect();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert_eq!(asked[0].0, "external_directory");
    assert_eq!(asked[1].0, "bash");
    assert_eq!(asked[0].1, asked[1].1, "same tool call");
    assert!(
        j.home.path().join("Desktop/iBooks").exists(),
        "the approved command ran"
    );
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
        !screen.contains("Run this command?"),
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
    // The block's accent rail (`┃`, flashed for 400 ms after a tool finishes) may still stand.
    let numbered = |want: &str| {
        screen.lines().any(|l| {
            let words: Vec<&str> = l.split_whitespace().filter(|w| *w != "\u{2503}").collect();
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

/// Reasoning is a collapsed thinking block, never part of the answer.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode serve); run with --include-ignored"]
fn reasoning_is_a_separate_block() {
    let Some(bin) = bin_from_env() else { return };
    let fx = fixture();
    let mut j = launch("engine-trust/reasoning-is-a-separate-block", &bin, &fx);
    send_prompt(&mut j, "think about it");
    wait_for(&mut j.h, "Echo: think about it", 60);
    j.h.update(Duration::from_millis(500));
    snapshot(&j.h, &j.dir, "01-thinking-block");
    let screen = j.h.screen_contents();
    assert!(
        screen.contains("Thought"),
        "reasoning shows as a thinking block:\n{screen}"
    );
    assert!(
        !screen.contains("Keep it brief.Echo") && !screen.contains("brief.Echo"),
        "reasoning is never glued to the answer:\n{screen}"
    );
    assert!(
        !screen.contains("Keep it brief."),
        "the thinking block is collapsed by default:\n{screen}"
    );
    quit(&mut j);
}

/// "what are you?" answers as Workshop's assistant: the server got Workshop's instructions file.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode serve); run with --include-ignored"]
fn engine_answers_as_workshop() {
    let Some(bin) = bin_from_env() else { return };
    let fx = fixture();
    let mut j = launch("engine-trust/engine-answers-as-workshop", &bin, &fx);
    send_prompt(&mut j, "what are you?");
    wait_for(&mut j.h, "Workshop's assistant", 60);
    j.h.update(Duration::from_millis(400));
    snapshot(&j.h, &j.dir, "01-identity");
    let screen = j.h.screen_contents();
    assert!(
        !screen.contains("I'm opencode"),
        "the engine's model must not introduce itself as opencode:\n{screen}"
    );
    let instructions = j.workshop_home().join("engine").join("instructions.md");
    let text = std::fs::read_to_string(&instructions).expect("instructions file under the home");
    assert!(text.contains("Workshop's coding assistant"), "{text}");
    assert!(
        !j.cwd.path().join("AGENTS.md").exists(),
        "nothing is written into the user's project"
    );
    quit(&mut j);
}

/// Unix seconds at which each `opencode serve` the fake ran came up (its first log line).
fn engine_starts(log: &Path) -> Vec<f64> {
    engine_log(log)
        .iter()
        .filter(|v| v.get("started").is_some())
        .filter_map(|v| v.get("time").and_then(|t| t.as_f64()))
        .collect()
}

/// Poll until the fake engine has started `n` times (without touching the keyboard); returns the
/// `n`th start's unix seconds.
fn wait_for_engine_start(
    h: &mut xai_grok_pager_pty_harness::PtyHarness,
    log: &Path,
    n: usize,
    secs: u64,
) -> f64 {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let starts = engine_starts(log);
        if let Some(at) = n.checked_sub(1).and_then(|i| starts.get(i)) {
            return *at;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the engine did not start (start #{n}) within {secs}s of launch; starts so far: {starts:?}\nscreen:\n{}",
            h.screen_contents()
        );
        h.update(Duration::from_millis(200));
    }
}

fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// The engine is brought up at launch, silently — never on Enter. A fresh home installs it (the
/// vendor installer stubbed by a `curl` that "downloads" a script placing the fake engine where
/// the real one lands) and starts `opencode serve` before a single key is pressed; a returning
/// home starts it before a single key is pressed and installs nothing; in both cases the first
/// answer arrives within seconds of Enter, with no bring-up line first.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode serve, stub installer); run with --include-ignored"]
fn engine_starts_at_launch_not_on_enter() {
    let Some(bin) = bin_from_env() else { return };
    let fakes = tempfile::tempdir().expect("fakes dir");
    // The fake engine lives *off* PATH so a fresh home has to "install" it.
    let engine_dir = fakes.path().join("engine");
    let log = install_fake_engine(&engine_dir);
    let path_dir = fakes.path().join("bin");
    std::fs::create_dir_all(&path_dir).unwrap();
    let curl_log = fakes.path().join("curl-calls.log");
    let curl = format!(
        r#"#!/bin/sh
# The vendor installer, stubbed: record the call, then print the script `bash -s` runs. It puts
# the fake engine exactly where the real installer would (`$HOME/.opencode/bin/opencode`, HOME
# being Workshop's tools tree for the installer process).
python3 -c 'import time; print(time.time())' >> '{curl_log}'
case "$*" in
  *opencode.ai/install*) ;;
  *) echo "stub curl: unexpected $*" >&2; exit 2 ;;
esac
cat <<'EOS'
mkdir -p "$HOME/.opencode/bin" && cp '{engine}' "$HOME/.opencode/bin/opencode" && chmod 755 "$HOME/.opencode/bin/opencode"
EOS
"#,
        curl_log = curl_log.display(),
        engine = engine_dir.join("opencode").display(),
    );
    std::fs::write(path_dir.join("curl"), curl).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            path_dir.join("curl"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let curl_calls = |log: &Path| -> Vec<f64> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.trim().parse::<f64>().ok())
            .collect()
    };

    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    std::process::Command::new("git")
        .args(["init", "-q", "."])
        .current_dir(cwd.path())
        .status()
        .unwrap();
    let dir = pty_common::evidence_dir("engine-trust/engine-starts-at-launch");
    let spawn = || {
        let inherited = std::env::var("PATH").unwrap_or_default();
        let path_s = format!("{}:{inherited}", path_dir.display());
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
            &[],
            &env,
            Some(cwd.path()),
        )
        .expect("spawn workshop in pty");
        h.set_respond_to_queries(true);
        h
    };
    let no_plumbing = |screen: &str, when: &str| {
        for text in [
            "Installing",
            "Starting the OpenCode engine",
            "Thinking",
            "opencode serve",
            "Waiting for",
        ] {
            assert!(
                !screen.contains(text),
                "{when}: the engine bring-up must not show on screen ({text:?}):\n{screen}"
            );
        }
    };
    let installed = home
        .path()
        .join(".workshop/tools/opencode/.opencode/bin/opencode");

    // 1. Fresh home: the installer runs and the server starts before anything is typed.
    let launched = unix_now();
    let mut h = spawn();
    wait_for(&mut h, FIRST_RUN_LABEL, 45);
    let started = wait_for_engine_start(&mut h, &log, 1, 60);
    assert!(
        started >= launched,
        "the start belongs to this launch ({started} < {launched})"
    );
    let calls = curl_calls(&curl_log);
    assert_eq!(
        calls.len(),
        1,
        "the vendor installer ran exactly once, at launch: {calls:?}"
    );
    assert!(installed.is_file(), "installed into Workshop's tools tree");
    h.update(Duration::from_millis(600));
    let screen = h.screen_contents();
    no_plumbing(&screen, "fresh launch, engine up, nothing typed");
    assert!(
        screen.contains(FIRST_RUN_LABEL) && !screen.contains("connect a model"),
        "the composer was live throughout:\n{screen}"
    );
    snapshot(&h, &dir, "01-fresh-launch-engine-up-silently");
    // The first message: the answer, within seconds of Enter, from the server that started at
    // launch (still exactly one start).
    h.inject_keys(b"ping").unwrap();
    h.update(Duration::from_millis(300));
    let enter = std::time::Instant::now();
    h.inject_keys(b"\r").unwrap();
    wait_for(&mut h, "Echo: ping", 30);
    let first_answer = enter.elapsed();
    h.update(Duration::from_millis(400));
    snapshot(&h, &dir, "02-fresh-launch-first-answer");
    assert_eq!(
        engine_starts(&log).len(),
        1,
        "Enter reused the server started at launch, it started none"
    );
    assert!(
        first_answer < Duration::from_secs(10),
        "the first answer waited on no bring-up: {first_answer:?}"
    );
    let (fresh_up, fresh_answer) = (started - launched, first_answer.as_secs_f64());
    eprintln!(
        "fresh home: launch\u{2192}engine up {fresh_up:.2}s (install + start), Enter\u{2192}answer {fresh_answer:.2}s"
    );
    h.inject_keys(b"\x03").unwrap();
    h.update(Duration::from_millis(400));
    h.inject_keys(b"\x03").unwrap();
    let _ = h.wait_exit_code(Duration::from_secs(10));

    // 2. Returning home: the server starts at launch again, nothing is installed, and the first
    //    message is answered at once.
    let launched = unix_now();
    let mut h = spawn();
    wait_for(&mut h, FIRST_RUN_LABEL, 45);
    let started = wait_for_engine_start(&mut h, &log, 2, 60);
    assert!(
        started >= launched,
        "the second start belongs to the second launch"
    );
    assert_eq!(
        curl_calls(&curl_log).len(),
        1,
        "a returning launch installs nothing"
    );
    h.update(Duration::from_millis(600));
    no_plumbing(
        &h.screen_contents(),
        "returning launch, engine up, nothing typed",
    );
    snapshot(&h, &dir, "03-returning-launch-engine-up-silently");
    h.inject_keys(b"ping").unwrap();
    h.update(Duration::from_millis(300));
    let enter = std::time::Instant::now();
    h.inject_keys(b"\r").unwrap();
    wait_for(&mut h, "Echo: ping", 30);
    let first_answer = enter.elapsed();
    h.update(Duration::from_millis(400));
    snapshot(&h, &dir, "04-returning-launch-first-answer");
    assert_eq!(engine_starts(&log).len(), 2, "one server per launch");
    assert!(
        first_answer < Duration::from_secs(10),
        "the first answer waited on no bring-up: {first_answer:?}"
    );
    let (again_up, again_answer) = (started - launched, first_answer.as_secs_f64());
    eprintln!(
        "returning home: launch\u{2192}engine up {again_up:.2}s, Enter\u{2192}answer {again_answer:.2}s"
    );
    std::fs::write(
        dir.join("timings.txt"),
        format!(
            "fresh home: engine up {fresh_up:.2}s after launch (stub install + start); first answer {fresh_answer:.2}s after Enter\nreturning home: engine up {again_up:.2}s after launch; first answer {again_answer:.2}s after Enter\n"
        ),
    )
    .unwrap();
    h.inject_keys(b"\x03").unwrap();
    h.update(Duration::from_millis(400));
    h.inject_keys(b"\x03").unwrap();
    let _ = h.wait_exit_code(Duration::from_secs(10));
    eprintln!("evidence: {}", dir.display());
}

/// Workshop starts in always-approve when nothing chose a mode: a fresh home's composer reads
/// `Big Pickle · always-approve` and a command runs with no prompt; one Shift+Tab reaches the
/// asking mode, the pick lands in the home's config, and the next launch opens in it (a command
/// prompts again). A home whose config already names a mode keeps it.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode serve); run with --include-ignored"]
fn first_run_starts_in_always_approve_and_a_pick_persists() {
    let Some(bin) = bin_from_env() else { return };
    let fx = fixture();
    let mut j = launch("engine-trust/always-approve-default", &bin, &fx);
    wait_for(&mut j.h, "Big Pickle \u{b7} always-approve", 10);
    j.h.update(Duration::from_millis(400));
    snapshot(&j.h, &j.dir, "01-fresh-home-always-approve");
    std::fs::create_dir_all(j.cwd.path().join("tmp")).unwrap();
    std::fs::write(j.cwd.path().join("tmp/junk"), "x").unwrap();
    send_prompt(&mut j, "rm -rf tmp");
    wait_for_without(&mut j, "Removed tmp.", &["Run this command?"], 60);
    j.h.update(Duration::from_millis(400));
    snapshot(&j.h, &j.dir, "02-default-runs-without-prompt");
    assert!(
        !j.cwd.path().join("tmp").exists(),
        "always-approve is in force: the command ran"
    );
    assert_eq!(
        permission_replies(&fx.log),
        vec!["once"],
        "the engine's ask was answered for the user"
    );

    // One Shift+Tab: the asking mode, and the pick is written to the home's config.
    set_mode(&mut j, "normal");
    let config_path = j.workshop_home().join("config.toml");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline
        && !std::fs::read_to_string(&config_path)
            .unwrap_or_default()
            .contains("permission_mode = \"ask\"")
    {
        j.h.update(Duration::from_millis(200));
    }
    let config = std::fs::read_to_string(&config_path).unwrap_or_default();
    assert!(
        config.contains("permission_mode = \"ask\""),
        "the mode pick persists as an explicit choice:\n{config}"
    );
    snapshot(&j.h, &j.dir, "03-shift-tab-to-asking-mode");
    quit(&mut j);

    // Next launch on the same home: the asking mode — the same command now prompts.
    let mut j = pty_common::spawn_in(
        "engine-trust/always-approve-default",
        &bin,
        OFFLINE,
        Some(&fx.bin),
        j.home,
    );
    pty_common::connect_big_pickle(&mut j);
    let screen = j.h.screen_contents();
    assert!(
        screen.contains(FIRST_RUN_LABEL) && !screen.contains("always-approve"),
        "the picked (asking) mode is what the next launch opens in:\n{screen}"
    );
    std::fs::create_dir_all(j.cwd.path().join("tmp")).unwrap();
    std::fs::write(j.cwd.path().join("tmp/junk"), "x").unwrap();
    send_prompt(&mut j, "rm -rf tmp");
    wait_for(&mut j.h, "Run this command?", 60);
    j.h.update(Duration::from_millis(400));
    snapshot(&j.h, &j.dir, "04-relaunch-in-asking-mode-prompts");
    j.h.inject_keys(b"3").unwrap(); // No
    wait_for(&mut j.h, "tmp was left alone", 30);
    assert!(j.cwd.path().join("tmp").exists());
    assert_eq!(permission_replies(&fx.log), vec!["once", "reject"]);
    quit(&mut j);
}

/// Sessions the fake engine was asked to open (`POST /session`), in order.
fn sessions_created(log: &Path) -> Vec<String> {
    engine_log(log)
        .iter()
        .filter_map(|v| v.get("created").and_then(|s| s.as_str()).map(str::to_owned))
        .collect()
}

/// `(session, prompt text)` for every prompt the fake engine received, in order.
fn prompts_by_session(log: &Path) -> Vec<(String, String)> {
    engine_log(log)
        .iter()
        .filter_map(|v| {
            Some((
                v.get("session")?.as_str()?.to_owned(),
                v.get("text")?.as_str()?.to_owned(),
            ))
        })
        .collect()
}

/// `/new` is a new conversation on the engine: the next prompt opens a fresh OpenCode session
/// (a new id; the old session is never written to again, and nothing of it is sent along), while
/// `workshop -c` still resumes the most recent conversation.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode serve); run with --include-ignored"]
fn new_starts_a_fresh_engine_conversation() {
    let Some(bin) = bin_from_env() else { return };
    let fx = fixture();
    let mut j = launch("engine-trust/new-starts-fresh-conversation", &bin, &fx);
    send_prompt(&mut j, "remember the word pelican");
    wait_for(&mut j.h, "Echo: remember the word pelican", 60);
    j.h.update(Duration::from_millis(400));
    let first = prompts_by_session(&fx.log);
    assert_eq!(first.len(), 1, "{first:?}");
    let old = first.first().map(|(s, _)| s.clone()).unwrap_or_default();
    assert_eq!(sessions_created(&fx.log), vec![old.clone()]);
    // The record store keys conversations by second; keep the two turns apart.
    j.h.update(Duration::from_millis(1200));

    // `/new`: an empty conversation — the transcript is gone, the meter is gone.
    send_prompt(&mut j, "/new");
    wait_gone(&mut j, "Echo: remember the word pelican", 15);
    j.h.update(Duration::from_millis(600));
    let screen = j.h.screen_contents();
    assert!(
        !screen.contains("8.6K / 200K"),
        "the context meter starts over with the new conversation:\n{screen}"
    );
    snapshot(&j.h, &j.dir, "01-after-new");

    // The next prompt goes to a fresh session: a new id, and the old one receives nothing more.
    send_prompt(&mut j, "and now?");
    wait_for(&mut j.h, "Echo: and now?", 60);
    j.h.update(Duration::from_millis(400));
    snapshot(&j.h, &j.dir, "02-first-turn-of-new-conversation");
    let created = sessions_created(&fx.log);
    assert_eq!(
        created.len(),
        2,
        "a second OpenCode session was opened: {created:?}"
    );
    let new = created.get(1).cloned().unwrap_or_default();
    assert_ne!(new, old, "the new conversation has its own session id");
    assert_eq!(
        prompts_by_session(&fx.log),
        vec![
            (old.clone(), "remember the word pelican".to_owned()),
            (new.clone(), "and now?".to_owned()),
        ],
        "each prompt went to its own session; nothing of the old conversation was resent"
    );
    let screen = j.h.screen_contents();
    assert!(
        !screen.contains("pelican"),
        "the old conversation is not shown in the new one:\n{screen}"
    );
    let after = quit(&mut j);
    assert!(
        after.contains(&format!("workshop --resume {new}")),
        "the quit hint names the new conversation:\n{after}"
    );

    // `-c` on the same folder resumes the most recent conversation — the new one.
    let mut h = {
        let inherited = std::env::var("PATH").unwrap_or_default();
        let path_s = format!("{}:{inherited}", fx.bin.display());
        let home_s = j.home.path().to_string_lossy().to_string();
        let wh_s = j.workshop_home().to_string_lossy().to_string();
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
            &["-c"],
            &env,
            Some(j.cwd.path()),
        )
        .expect("spawn workshop -c in pty");
        h.set_respond_to_queries(true);
        h
    };
    wait_for(&mut h, "Echo: and now?", 45);
    wait_for(&mut h, "Resumed", 10);
    h.update(Duration::from_millis(400));
    let screen = h.screen_contents();
    std::fs::write(
        j.dir.join("03-continue-resumes-new-conversation.txt"),
        &screen,
    )
    .unwrap();
    std::fs::write(
        j.dir.join("03-continue-resumes-new-conversation.html"),
        h.screen_html(),
    )
    .unwrap();
    assert!(
        !screen.contains("pelican"),
        "-c resumes the newest conversation, not the one before /new:\n{screen}"
    );
    // A turn on the resumed conversation never touches the session from before `/new`. (This
    // launch's fake server has no memory of the earlier process's sessions, so Workshop opens a
    // replacement session for it — with the real engine, which persists sessions, it is `new`.)
    h.inject_keys(b"still here?").unwrap();
    h.update(Duration::from_millis(300));
    h.inject_keys(b"\r").unwrap();
    wait_for(&mut h, "Echo: still here?", 60);
    let last = prompts_by_session(&fx.log).pop().unwrap_or_default();
    assert_eq!(last.1, "still here?");
    assert_ne!(
        last.0, old,
        "the resumed turn must not continue the conversation from before /new"
    );
    h.inject_keys(b"\x03").unwrap();
    h.update(Duration::from_millis(400));
    h.inject_keys(b"\x03").unwrap();
    let _ = h.wait_exit_code(Duration::from_secs(10));
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
    wait_for(&mut j.h, "Run this command?", 180);
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
    wait_gone(&mut j, "Run this command?", 60);
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
            !j.h.screen_contents().contains("Run this command?"),
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
