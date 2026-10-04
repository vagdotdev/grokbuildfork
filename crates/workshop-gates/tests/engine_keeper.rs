//! Engine keeper gates: the engine (`opencode serve`) is kept warm between sessions by a detached
//! keeper process, with a clean lifecycle. Against the built binary (`WORKSHOP_BIN`, run with
//! `--include-ignored`), hermetic: the fake `opencode` whose `serve` is a loopback stand-in.
//!
//!   * `a_relaunch_attaches_to_the_warm_engine` — launch 1 starts a keeper; quitting leaves the
//!     keeper and its server running; launch 2 on the same home attaches (same keeper, same
//!     server pid, no second server), is ready at once, and a turn answers through it.
//!   * `a_crashed_tui_frees_the_keeper_which_stops_after_the_keep_warm_time` — SIGKILL on the TUI
//!     is seen by the keeper (its connection closes); with a 2 s keep-warm the keeper stops the
//!     server, removes `serve.json` and exits on its own. Nothing leaks.
//!   * `a_changed_configuration_replaces_the_keeper` — a launch whose engine environment differs
//!     ends the old keeper and server and starts fresh ones.

#![cfg(unix)]

use std::path::Path;
use std::time::{Duration, Instant};

mod pty_common;
use pty_common::*;

const RECORD: &str = "prompts.jsonl";

fn pid_alive(pid: u32) -> bool {
    // SAFETY: kill(2) with signal 0 only checks that the process exists.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn serve_info(home: &Path) -> Option<serde_json::Value> {
    let raw = std::fs::read_to_string(home.join("engine").join("serve.json")).ok()?;
    serde_json::from_str(&raw).ok()
}

fn pid(info: &serde_json::Value, key: &str) -> u32 {
    info.get(key)
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| panic!("serve.json lacks {key}: {info}")) as u32
}

fn wait_until(what: &str, secs: u64, j: &mut Journey, mut done: impl FnMut() -> bool) -> Duration {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        if done() {
            return start.elapsed();
        }
        j.h.update(Duration::from_millis(100));
    }
    panic!(
        "{what} did not happen within {secs}s\n{}",
        j.h.screen_contents()
    );
}

fn engine_ready(home: &Path) -> bool {
    std::fs::read_to_string(home.join("engine").join("state.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .is_some_and(|v| v.get("last_phase").and_then(|p| p.as_str()) == Some("ready"))
}

fn engine_log(home: &Path) -> String {
    std::fs::read_to_string(home.join("logs").join("opencode-engine.log")).unwrap_or_default()
}

fn quit(j: &mut Journey) {
    j.h.inject_keys(b"/exit").unwrap();
    j.h.update(Duration::from_millis(300));
    j.h.inject_keys(b"\r").unwrap();
    let _ = j.h.wait_exit_code(Duration::from_secs(10));
}

/// Launch on `home` with the fake engine on PATH and `keep_warm` seconds of keep-warm.
fn launch(
    name: &str,
    bin: &Path,
    fake: &Path,
    home: tempfile::TempDir,
    keep_warm: &str,
    extra: &[(&str, &str)],
) -> Journey {
    let mut env = vec![("WORKSHOP_OPENCODE_KEEP_WARM_SECS", keep_warm)];
    env.extend_from_slice(extra);
    spawn_in(name, bin, &env, Some(fake), home)
}

fn end_keeper(home: &Path) {
    if let Some(info) = serve_info(home) {
        for key in ["keeper_pid", "serve_pid"] {
            let p = pid(&info, key);
            // SAFETY: ending the test's own keeper/server pids.
            unsafe {
                libc::kill(p as libc::pid_t, libc::SIGKILL);
            }
        }
    }
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode); run with --include-ignored"]
fn a_relaunch_attaches_to_the_warm_engine() {
    let Some(bin) = bin_from_env() else { return };
    let recorder = tempfile::tempdir().expect("tempdir");
    let fake = fake_opencode_answering(&recorder.path().join(RECORD));
    let mut j = launch(
        "keeper-launch-1",
        &bin,
        fake.path(),
        tempfile::tempdir().expect("tempdir"),
        "120",
        &[],
    );
    connect_big_pickle(&mut j);
    let home = j.workshop_home();
    wait_until("engine ready (launch 1)", 60, &mut j, || {
        engine_ready(&home)
    });
    let info1 = serve_info(&home).expect("serve.json after launch 1");
    let (keeper1, serve1) = (pid(&info1, "keeper_pid"), pid(&info1, "serve_pid"));
    assert!(
        pid_alive(keeper1) && pid_alive(serve1),
        "keeper and server run"
    );
    let log = engine_log(&home);
    assert!(
        log.contains("keeper: started"),
        "launch 1 started the keeper:\n{log}"
    );
    snapshot(&j.h, &j.dir, "01-launch-1-ready");

    // The TUI quits; the engine stays warm.
    quit(&mut j);
    std::thread::sleep(Duration::from_secs(2));
    assert!(
        pid_alive(keeper1) && pid_alive(serve1),
        "the keeper and its server outlive the TUI"
    );
    assert_eq!(
        serve_info(&home),
        Some(info1.clone()),
        "serve.json unchanged"
    );

    // Launch 2 on the same home attaches: same keeper, same server, ready at once.
    let Journey {
        h, home: home_dir, ..
    } = j;
    drop(h);
    std::fs::remove_file(home.join("engine").join("state.json")).ok();
    let mut j = launch("keeper-launch-2", &bin, fake.path(), home_dir, "120", &[]);
    let ready_in = wait_until("engine ready (launch 2)", 30, &mut j, || {
        engine_ready(&home)
    });
    let info2 = serve_info(&home).expect("serve.json after launch 2");
    assert_eq!(
        (pid(&info2, "keeper_pid"), pid(&info2, "serve_pid")),
        (keeper1, serve1),
        "launch 2 attached to the same keeper and server"
    );
    let log = engine_log(&home);
    assert!(
        log.contains("keeper: attaching to the warm engine"),
        "launch 2 attached:\n{log}"
    );
    assert_eq!(
        log.matches("keeper: started").count(),
        1,
        "only one keeper was ever started:\n{log}"
    );
    eprintln!("launch 2: engine ready {ready_in:?} after the composer check began");
    connect_big_pickle(&mut j);
    send_prompt(&mut j, "hello");
    wait_for(&mut j.h, "Created hello.txt with the exact line.", 90);
    snapshot(&j.h, &j.dir, "02-launch-2-answered-through-the-warm-engine");
    quit(&mut j);
    end_keeper(&home);
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode); run with --include-ignored"]
fn a_crashed_tui_frees_the_keeper_which_stops_after_the_keep_warm_time() {
    let Some(bin) = bin_from_env() else { return };
    let recorder = tempfile::tempdir().expect("tempdir");
    let fake = fake_opencode_answering(&recorder.path().join(RECORD));
    let mut j = launch(
        "keeper-crash",
        &bin,
        fake.path(),
        tempfile::tempdir().expect("tempdir"),
        "2",
        &[],
    );
    connect_big_pickle(&mut j);
    let home = j.workshop_home();
    wait_until("engine ready", 60, &mut j, || engine_ready(&home));
    let info = serve_info(&home).expect("serve.json");
    let (keeper, serve) = (pid(&info, "keeper_pid"), pid(&info, "serve_pid"));
    let tui = j.h.child_pid().expect("tui pid");
    // SAFETY: the test kills its own TUI to simulate a crash.
    unsafe {
        libc::kill(tui as libc::pid_t, libc::SIGKILL);
    }
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(20) && (pid_alive(keeper) || pid_alive(serve)) {
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        !pid_alive(keeper) && !pid_alive(serve),
        "with a 2 s keep-warm, a crashed TUI's keeper ({keeper}) and server ({serve}) stop on their own"
    );
    assert!(
        serve_info(&home).is_none(),
        "the keeper removed serve.json on its way out"
    );
    assert!(
        !home.join("run").join("engine-keeper.sock").exists(),
        "the keeper removed its socket"
    );
    let log = engine_log(&home);
    assert!(
        log.contains("keeper: stopping (idle)") && log.contains("keeper: stopped"),
        "the keeper logged its idle stop:\n{log}"
    );
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode); run with --include-ignored"]
fn a_changed_configuration_replaces_the_keeper() {
    let Some(bin) = bin_from_env() else { return };
    let recorder = tempfile::tempdir().expect("tempdir");
    let fake = fake_opencode_answering(&recorder.path().join(RECORD));
    let mut j = launch(
        "keeper-replace-1",
        &bin,
        fake.path(),
        tempfile::tempdir().expect("tempdir"),
        "120",
        &[],
    );
    connect_big_pickle(&mut j);
    let home = j.workshop_home();
    wait_until("engine ready (launch 1)", 60, &mut j, || {
        engine_ready(&home)
    });
    let info1 = serve_info(&home).expect("serve.json");
    let (keeper1, serve1) = (pid(&info1, "keeper_pid"), pid(&info1, "serve_pid"));
    quit(&mut j);

    // The engine's environment is part of what the server was configured with: a launch whose
    // environment differs (here one extra variable the engine's commands would see) must not
    // use the old server.
    let Journey {
        h, home: home_dir, ..
    } = j;
    drop(h);
    std::fs::remove_file(home.join("engine").join("state.json")).ok();
    let mut j = launch(
        "keeper-replace-2",
        &bin,
        fake.path(),
        home_dir,
        "120",
        &[("WORKSHOP_GATE_EXTRA_ENV", "changed")],
    );
    wait_until("engine ready (launch 2)", 60, &mut j, || {
        engine_ready(&home)
    });
    let info2 = serve_info(&home).expect("serve.json after launch 2");
    let (keeper2, serve2) = (pid(&info2, "keeper_pid"), pid(&info2, "serve_pid"));
    assert_ne!(keeper2, keeper1, "a new keeper");
    assert_ne!(serve2, serve1, "a new server");
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) && (pid_alive(keeper1) || pid_alive(serve1)) {
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        !pid_alive(keeper1) && !pid_alive(serve1),
        "the old keeper ({keeper1}) and server ({serve1}) were ended"
    );
    let log = engine_log(&home);
    assert!(
        log.contains("different build, engine or configuration"),
        "the log says why:\n{log}"
    );
    quit(&mut j);
    end_keeper(&home);
}
