//! The macOS "nothing works" hang, reproduced on Linux with fault injection and pinned: whatever
//! breaks in the OpenCode engine path, the user sees one line with the cause, `/model` as the way
//! out and the engine log path — within seconds, never a silent turn lock.
//!
//! Opt-in: set `WORKSHOP_BIN` to the built binary and run with `--include-ignored`.

mod pty_common;

use std::time::{Duration, Instant};

use pty_common::*;

/// Wait until the scrollback carries the failure line (`OpenCode unavailable (…)` when the
/// engine never came up — followed by the Kilo fallback — or `OpenCode engine: …` when it was up
/// but the turn failed); return it and how long it took.
fn wait_for_failure_line(j: &mut Journey, secs: u64) -> (String, Duration) {
    let start = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let screen = j.h.screen_contents();
        if screen.contains("OpenCode unavailable") || screen.contains("OpenCode engine:") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no failure line after {secs}s:\n{screen}"
        );
        j.h.update(Duration::from_millis(200));
    }
    j.h.update(Duration::from_millis(600));
    let screen = j.h.screen_contents();
    let line = screen
        .lines()
        .skip_while(|l| !(l.contains("OpenCode unavailable") || l.contains("OpenCode engine:")))
        .take_while(|l| !l.trim().is_empty())
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ");
    (line, start.elapsed())
}

/// A bring-up failure: the cause, the engine log pointer, and the Kilo fallback as the way out.
fn assert_fallback_way_out(line: &str) {
    assert!(
        line.contains("OpenCode unavailable"),
        "a bring-up failure falls back: {line}"
    );
    assert!(
        line.contains("opencode-engine.log"),
        "failure line names the engine log: {line}"
    );
    assert!(
        line.contains("instead"),
        "the Kilo fallback is announced on the same line: {line}"
    );
    assert!(
        !line.contains("already running"),
        "no turn-lock message stands in for a real cause: {line}"
    );
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); run with --include-ignored"]
fn serve_that_exits_at_once_is_reported_with_its_stderr_in_seconds() {
    let Some(bin) = bin_from_env() else { return };
    let fake = fake_opencode("crash");
    let mut j = spawn("engine-crash", &bin, &[], Some(fake.path()));
    connect_big_pickle(&mut j);
    send_prompt(&mut j, "hello");
    let (line, took) = wait_for_failure_line(&mut j, 20);
    snapshot(&j.h, &j.dir, "failure-line");
    assert!(
        line.contains("exited during startup") && line.contains("libfake.dylib"),
        "the process exit and its stderr are the reported cause: {line}"
    );
    assert_fallback_way_out(&line);
    assert!(took < Duration::from_secs(10), "reported in {took:?}");
    let state = j.workshop_home().join("engine").join("state.json");
    let state = std::fs::read_to_string(&state).expect("engine state written for doctor");
    assert!(state.contains("\"last_error\""), "{state}");
    assert!(
        j.workshop_home()
            .join("logs")
            .join("opencode-engine.log")
            .is_file(),
        "engine log exists"
    );
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); run with --include-ignored"]
fn serve_that_never_binds_hits_the_30s_ceiling_with_a_visible_status_line() {
    let Some(bin) = bin_from_env() else { return };
    let fake = fake_opencode("nobind");
    let mut j = spawn("engine-nobind", &bin, &[], Some(fake.path()));
    connect_big_pickle(&mut j);
    send_prompt(&mut j, "hello");
    // The status line shows at once, while the server "starts".
    wait_for(&mut j.h, "Starting the OpenCode engine", 10);
    snapshot(&j.h, &j.dir, "status-line");
    let (line, _) = wait_for_failure_line(&mut j, 60);
    snapshot(&j.h, &j.dir, "failure-line");
    assert!(
        line.contains("did not become ready within 30s"),
        "the hard startup ceiling is the reported cause: {line}"
    );
    assert_fallback_way_out(&line);
    assert!(
        !j.h.screen_contents()
            .contains("Starting the OpenCode engine"),
        "the status line is removed once the turn ends:\n{}",
        j.h.screen_contents()
    );
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); run with --include-ignored"]
fn ctrl_c_cancels_while_the_engine_is_still_starting() {
    let Some(bin) = bin_from_env() else { return };
    let fake = fake_opencode("nobind");
    let mut j = spawn("engine-cancel", &bin, &[], Some(fake.path()));
    connect_big_pickle(&mut j);
    send_prompt(&mut j, "hello");
    wait_for(&mut j.h, "Starting the OpenCode engine", 10);
    // Ctrl+C is a two-step gesture from the composer: arm, then cancel.
    j.h.inject_keys(b"\x03").unwrap();
    j.h.update(Duration::from_millis(300));
    j.h.inject_keys(b"\x03").unwrap();
    wait_for(&mut j.h, "Turn cancelled", 5);
    snapshot(&j.h, &j.dir, "cancelled");
    // The lock is released: a new message starts a new attempt instead of "already running".
    send_prompt(&mut j, "again");
    wait_for(&mut j.h, "Starting the OpenCode engine", 10);
    assert!(
        !j.h.screen_contents()
            .contains("Still working on your last message")
    );
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); run with --include-ignored"]
fn offline_installer_failure_names_the_curl_error() {
    let Some(bin) = bin_from_env() else { return };
    // No `opencode` anywhere and a `curl` that cannot resolve the host: the vendor installer
    // must fail loudly (pipefail), not "succeed" with nothing installed.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("curl"),
        "#!/bin/sh\necho 'curl: (6) Could not resolve host: opencode.ai' >&2\nexit 6\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            dir.path().join("curl"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let mut j = spawn("engine-installfail", &bin, &[], Some(dir.path()));
    connect_big_pickle(&mut j);
    send_prompt(&mut j, "hello");
    let (line, took) = wait_for_failure_line(&mut j, 30);
    snapshot(&j.h, &j.dir, "failure-line");
    assert!(
        line.contains("install failed") && line.contains("Could not resolve host"),
        "the installer's own error is the reported cause: {line}"
    );
    assert_fallback_way_out(&line);
    assert!(took < Duration::from_secs(15), "reported in {took:?}");
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); run with --include-ignored"]
fn healthy_serve_with_a_silent_model_hits_the_90s_first_event_ceiling() {
    let Some(bin) = bin_from_env() else { return };
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("python3 not available; skipping the silent-model scenario");
        return;
    }
    let fake = fake_opencode("silent");
    let mut j = spawn("engine-silent", &bin, &[], Some(fake.path()));
    connect_big_pickle(&mut j);
    send_prompt(&mut j, "hello");
    wait_for(&mut j.h, "Waiting for Big Pickle", 40);
    snapshot(&j.h, &j.dir, "waiting-line");
    let (line, _) = wait_for_failure_line(&mut j, 120);
    snapshot(&j.h, &j.dir, "failure-line");
    assert!(
        line.contains("no answer from Big Pickle after 90 s"),
        "the first-event ceiling is the reported cause: {line}"
    );
    // The engine is up, so there is nothing to fall back from: `/model` is the way out.
    assert!(
        line.contains("/model") && line.contains("opencode-engine.log"),
        "{line}"
    );
}
