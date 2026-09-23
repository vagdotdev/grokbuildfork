//! The macOS "nothing works" hang, reproduced on Linux with fault injection and pinned — under the
//! owner's bar for a first-time user: whatever breaks behind the free model, the screen shows one
//! calm `Thinking…` line while Workshop works, then either the answer (through the silent
//! fallback) or one plain failure line — `Couldn't reach Big Pickle — Enter to retry · /model to
//! switch` — within seconds. No runtime or fallback words ever. The technical cause goes to
//! `$WORKSHOP_HOME/engine/state.json` and the log for `workshop doctor`.
//!
//! The fallback provider here is a loopback endpoint that refuses every request (HTTP 400), so
//! the double failure is hermetic and fast.
//!
//! Opt-in: set `WORKSHOP_BIN` to the built binary and run with `--include-ignored`.

mod pty_common;

use std::time::{Duration, Instant};

use pty_common::*;

const FAILURE_LINE: &str = "Couldn't reach Big Pickle";

/// Wait until the scrollback carries the plain failure line; return it and how long it took.
fn wait_for_failure_line(j: &mut Journey, secs: u64) -> (String, Duration) {
    let start = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let screen = j.h.screen_contents();
        if screen.contains(FAILURE_LINE) {
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
        .skip_while(|l| !l.contains(FAILURE_LINE))
        .take_while(|l| !l.trim().is_empty())
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ");
    (line, start.elapsed())
}

/// The plain line and nothing else: the two ways out, no cause, no plumbing.
fn assert_plain_failure(j: &Journey, line: &str) {
    assert!(
        line.contains("Enter to retry") && line.contains("/model to switch"),
        "the failure line names the ways out: {line}"
    );
    assert!(
        !line.contains("log:") && !line.contains(".log"),
        "no log paths on screen: {line}"
    );
    let screen = j.h.screen_contents();
    for wire in [
        "Bad request",
        "(400)",
        "fault injected",
        "invalid_request_error",
        "HTTP ",
    ] {
        assert!(
            !screen.contains(wire),
            "the wire error stays in the log, not on screen ({wire:?}):\n{screen}"
        );
    }
    assert_eq!(
        screen.matches("Couldn't reach").count(),
        1,
        "exactly one failure line:\n{screen}"
    );
    // The composer names the user's own model again, not the stand-in that was tried.
    assert!(
        screen
            .lines()
            .any(|l| l.contains('\u{256f}') && l.contains("Big Pickle"))
            && !screen.contains("Nemotron"),
        "the footer is back to the user's model after the fallback failed:\n{screen}"
    );
    assert_no_plumbing(&j.h, "failure line");
}

/// The cause is recorded for `workshop doctor`, never shown.
fn assert_cause_recorded(j: &Journey, needles: &[&str]) {
    let state = j.workshop_home().join("engine").join("state.json");
    let state = std::fs::read_to_string(&state).expect("engine state written for doctor");
    let log = std::fs::read_to_string(j.workshop_home().join("logs").join("opencode-engine.log"))
        .expect("engine log exists");
    for needle in needles {
        assert!(
            state.contains(needle) || log.contains(needle),
            "cause {needle:?} recorded in state.json or the log:\n{state}\n{log}"
        );
        assert!(
            !j.h.screen_contents().contains(needle),
            "cause {needle:?} stays off the screen:\n{}",
            j.h.screen_contents()
        );
    }
}

fn spawn_with_refusing_fallback(
    journey: &str,
    bin: &std::path::Path,
    fake: &std::path::Path,
) -> (Journey, KillOnDrop) {
    let (api, url) = refusing_api();
    let j = spawn(
        journey,
        bin,
        &[(crate::pty_common::KILO_BASE_URL_ENV, url.as_str())],
        Some(fake),
    );
    (j, api)
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); run with --include-ignored"]
fn serve_that_exits_at_once_ends_in_one_plain_line_within_seconds() {
    let Some(bin) = bin_from_env() else { return };
    let fake = fake_opencode("crash");
    let (mut j, _api) = spawn_with_refusing_fallback("engine-crash", &bin, fake.path());
    connect_big_pickle(&mut j);
    send_prompt(&mut j, "hello");
    expect_thinking_line(&mut j, FAILURE_LINE, 30);
    let (line, took) = wait_for_failure_line(&mut j, 30);
    snapshot(&j.h, &j.dir, "failure-line");
    assert_plain_failure(&j, &line);
    assert!(took < Duration::from_secs(20), "reported in {took:?}");
    assert_cause_recorded(&j, &["exited during startup", "libfake.dylib"]);
    assert!(
        !j.h.screen_contents().contains(WAITING_ROW),
        "the waiting line is gone once the turn ends:\n{}",
        j.h.screen_contents()
    );
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); run with --include-ignored"]
fn serve_that_never_binds_hits_the_30s_ceiling_behind_the_status_row() {
    let Some(bin) = bin_from_env() else { return };
    let fake = fake_opencode("nobind");
    let (mut j, _api) = spawn_with_refusing_fallback("engine-nobind", &bin, fake.path());
    connect_big_pickle(&mut j);
    send_prompt(&mut j, "hello");
    // One calm line while the server "starts"; no phase names.
    wait_for(&mut j.h, WAITING_ROW, 10);
    assert_no_plumbing(&j.h, "status line");
    snapshot(&j.h, &j.dir, "status-line");
    let (line, _) = wait_for_failure_line(&mut j, 60);
    snapshot(&j.h, &j.dir, "failure-line");
    assert_plain_failure(&j, &line);
    assert_cause_recorded(&j, &["did not become ready within 30s"]);
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); run with --include-ignored"]
fn ctrl_c_cancels_while_the_engine_is_still_starting() {
    let Some(bin) = bin_from_env() else { return };
    let fake = fake_opencode("nobind");
    let mut j = spawn("engine-cancel", &bin, &[], Some(fake.path()));
    connect_big_pickle(&mut j);
    send_prompt(&mut j, "hello");
    wait_for(&mut j.h, WAITING_ROW, 10);
    // Ctrl+C is a two-step gesture from the composer: arm, then cancel.
    j.h.inject_keys(b"\x03").unwrap();
    j.h.update(Duration::from_millis(300));
    j.h.inject_keys(b"\x03").unwrap();
    wait_for(&mut j.h, "Turn cancelled", 5);
    snapshot(&j.h, &j.dir, "cancelled");
    // The lock is released: a new message starts a new attempt instead of "already running".
    send_prompt(&mut j, "again");
    wait_for(&mut j.h, WAITING_ROW, 10);
    assert!(
        !j.h.screen_contents()
            .contains("Still working on your last message")
    );
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); run with --include-ignored"]
fn offline_installer_failure_is_recorded_and_the_screen_stays_plain() {
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
    let (mut j, _api) = spawn_with_refusing_fallback("engine-installfail", &bin, dir.path());
    connect_big_pickle(&mut j);
    send_prompt(&mut j, "hello");
    let (line, took) = wait_for_failure_line(&mut j, 40);
    snapshot(&j.h, &j.dir, "failure-line");
    assert_plain_failure(&j, &line);
    assert!(took < Duration::from_secs(25), "reported in {took:?}");
    assert_cause_recorded(&j, &["install failed", "Could not resolve host"]);
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
    let (mut j, _api) = spawn_with_refusing_fallback("engine-silent", &bin, fake.path());
    connect_big_pickle(&mut j);
    send_prompt(&mut j, "hello");
    wait_for(&mut j.h, WAITING_ROW, 40);
    assert_no_plumbing(&j.h, "waiting line");
    snapshot(&j.h, &j.dir, "waiting-line");
    // The model was up and silent for the whole ceiling: it cannot answer, so the fallback is
    // tried (and refused here) — then the plain line, still without a word about either.
    let (line, _) = wait_for_failure_line(&mut j, 120);
    snapshot(&j.h, &j.dir, "failure-line");
    assert_plain_failure(&j, &line);
    assert_cause_recorded(&j, &["no answer from Big Pickle after 90 s"]);
}
