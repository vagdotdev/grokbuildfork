//! Live proof (network, real `opencode`): a Big Pickle turn edits and runs code by default.
//! The engine runs OpenCode's `build` agent for every turn — the `plan` agent it used to get
//! without always-approve cannot touch files, which made a coding tool that could not code.
//!
//! Opt-in: `WORKSHOP_BIN` + `WORKSHOP_LIVE_OPENCODE=1`, run with `--include-ignored`.

mod pty_common;

use std::time::{Duration, Instant};

use pty_common::*;

#[test]
#[ignore = "needs WORKSHOP_BIN, WORKSHOP_LIVE_OPENCODE=1 and network; run with --include-ignored"]
fn big_pickle_creates_and_runs_a_file_without_always_approve() {
    let Some(bin) = bin_from_env() else { return };
    if std::env::var_os("WORKSHOP_LIVE_OPENCODE").is_none() {
        eprintln!("WORKSHOP_LIVE_OPENCODE not set; skipping the live engine proof");
        return;
    }
    let mut j = spawn("engine-build-agent", &bin, &[], None);
    connect_big_pickle(&mut j);
    let hello = j.cwd.path().join("hello.py");
    send_prompt(
        &mut j,
        "create hello.py that prints today's date and run it with python3",
    );
    let start = Instant::now();
    while !hello.is_file() && start.elapsed() < Duration::from_secs(150) {
        j.h.update(Duration::from_millis(500));
        let screen = j.h.screen_contents();
        assert!(
            !screen.contains("OpenCode engine:"),
            "engine failed instead of editing:\n{screen}"
        );
    }
    assert!(hello.is_file(), "hello.py was never created:\n{}", j.h.screen_contents());
    // The run step shows up as a tool call in the scrollback.
    wait_for(&mut j.h, "python", 60);
    j.h.update(Duration::from_secs(4));
    snapshot(&j.h, &j.dir, "hello-py-created-and-run");
    let screen = j.h.screen_contents();
    assert!(
        !screen.contains("rejected"),
        "no permission ask was auto-rejected:\n{screen}"
    );
    assert!(
        !screen.to_ascii_lowercase().contains("plan mode"),
        "the build agent, not plan, answered:\n{screen}"
    );
}
