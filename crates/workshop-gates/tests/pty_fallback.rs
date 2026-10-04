//! The silent fallback (owner decision, v0.2.2): when OpenCode's model cannot start, Workshop
//! answers through the keyless community pool without a word about it — the reply just arrives
//! and the composer names the model that answered (`Nemotron 3 Super`), model name only. No
//! "Kilo", "fallback" or "engine" ever reaches the screen.
//!
//! Hermetic: `opencode serve` is the crashing fixture, and the pool is the pty harness's mock
//! inference server on loopback (`WORKSHOP_KILO_BASE_URL`).
//!
//! Opt-in: set `WORKSHOP_BIN` to the built binary and run with `--include-ignored`.

mod pty_common;

use std::time::Duration;

use pty_common::*;
use xai_grok_pager_pty_harness::ContentController;

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode + mock inference on loopback); run with --include-ignored"]
fn a_failed_opencode_start_falls_back_silently_and_the_answer_arrives() {
    let Some(bin) = bin_from_env() else { return };
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let content = rt
        .block_on(ContentController::start())
        .expect("mock inference server");
    content.set_response("Two plus two is four.");
    let url = content.url();

    let fake = fake_opencode("crash");
    let mut j = spawn(
        "fallback-silent",
        &bin,
        &[(KILO_BASE_URL_ENV, url.as_str())],
        Some(fake.path()),
    );
    connect_big_pickle(&mut j);
    snapshot(&j.h, &j.dir, "01-first-run");

    send_prompt(&mut j, "what is two plus two?");
    expect_thinking_line(&mut j, "Two plus two is four.", 30);

    // The answer arrives — through the fallback, with nothing said about it.
    wait_for(&mut j.h, "Two plus two is four.", 60);
    j.h.update(Duration::from_millis(800));
    snapshot(&j.h, &j.dir, "02-answer-through-fallback");
    assert_no_plumbing(&j.h, "answer");
    let raw = String::from_utf8_lossy(j.h.raw_output()).to_string();
    for word in PLUMBING_WORDS {
        assert!(
            !raw.contains(word),
            "{word:?} was drawn at some point during the turn"
        );
    }
    let screen = j.h.screen_contents();
    assert!(
        !screen.contains("Couldn't reach"),
        "no failure line when the fallback answered:\n{screen}"
    );
    assert!(
        content.has_chat_completion(),
        "the reply came from the pool endpoint, not from thin air"
    );
    // The composer names the model that answered — model only, no provider, no explanation.
    assert!(
        screen.contains("Nemotron 3 Super"),
        "the footer names the answering model:\n{screen}"
    );
    assert!(
        !screen.contains("NVIDIA:") && !screen.contains("(free)"),
        "the name is plain — no vendor prefix, no `(free)`:\n{screen}"
    );
    for word in ["Kilo", "fallback", "engine", "instead", "unavailable"] {
        assert!(
            !screen.contains(word),
            "{word:?} must never be shown:\n{screen}"
        );
    }
    // The user's choice is untouched: the next launch tries OpenCode again.
    let conn = std::fs::read_to_string(j.workshop_home().join("active-connection.json"))
        .expect("active connection");
    assert!(
        conn.contains("\"engine\"") && conn.contains("big-pickle"),
        "the saved connection is still OpenCode's model: {conn}"
    );
    let cause = std::fs::read_to_string(j.workshop_home().join("logs").join("opencode-engine.log"))
        .expect("the cause went to the log");
    assert!(
        cause.contains("libfake.dylib"),
        "the technical cause is in the log for doctor: {cause}"
    );

    // A second message keeps going through the fallback, still silently.
    content.set_response("Still here.");
    send_prompt(&mut j, "and three plus three?");
    wait_for(&mut j.h, "Still here.", 60);
    assert_no_plumbing(&j.h, "second answer");
    snapshot(&j.h, &j.dir, "03-second-answer");
    drop(j);
    drop(content);
}

/// Prototype (`WORKSHOP_FREE_MODELS=direct`): a first run lands on the pool model as the shell's
/// own model and answers through the shell's agent loop. No `opencode` is ever run — not the
/// installer, not `--version`, not `serve` — and nothing engine-shaped is written to the home.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (mock inference on loopback, a recording fake opencode); run with --include-ignored"]
fn the_direct_default_answers_with_no_engine_process() {
    let Some(bin) = bin_from_env() else { return };
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let content = rt
        .block_on(ContentController::start())
        .expect("mock inference server");
    content.set_response("Two plus two is four.");
    let url = content.url();

    // An `opencode` on PATH that records every invocation: the proof is that it never runs.
    let fake = tempfile::tempdir().expect("tempdir");
    let calls = fake.path().join("calls");
    std::fs::write(
        fake.path().join("opencode"),
        format!("#!/bin/sh\necho \"$@\" >> '{}'\nexit 2\n", calls.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            fake.path().join("opencode"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }

    let mut j = spawn(
        "direct-default",
        &bin,
        &[
            (KILO_BASE_URL_ENV, url.as_str()),
            ("WORKSHOP_FREE_MODELS", "direct"),
        ],
        Some(fake.path()),
    );
    wait_for(&mut j.h, "\u{276f}", 45);
    wait_for(&mut j.h, "Nemotron 3 Super", 30);
    j.h.update(Duration::from_millis(1200));
    let screen = j.h.screen_contents();
    assert!(
        !screen.contains("Big Pickle") && !screen.contains("connect a model"),
        "the direct first run lands in the composer on the pool model, no engine model, no picker:\n{screen}"
    );
    assert!(
        screen.contains("/model to switch"),
        "the first-launch hint shows for the direct default too:\n{screen}"
    );
    assert_no_plumbing(&j.h, "first run");
    snapshot(&j.h, &j.dir, "01-first-run-direct");

    send_prompt(&mut j, "what is two plus two?");
    expect_thinking_line(&mut j, "Two plus two is four.", 30);
    wait_for(&mut j.h, "Two plus two is four.", 60);
    j.h.update(Duration::from_millis(800));
    snapshot(&j.h, &j.dir, "02-answer-direct");
    assert_no_plumbing(&j.h, "answer");
    assert!(
        content.has_chat_completion(),
        "the reply came from the pool endpoint through the shell's own sampler"
    );
    let screen = j.h.screen_contents();
    assert!(
        screen.contains("Nemotron 3 Super") && !screen.contains("Couldn't reach"),
        "the composer still names the pool model and nothing failed:\n{screen}"
    );

    // Engine-less, provably: opencode never ran, and the home has no engine state or log.
    assert!(
        !calls.exists(),
        "opencode was run: {}",
        std::fs::read_to_string(&calls).unwrap_or_default()
    );
    let home = j.workshop_home();
    for rel in ["engine", "logs/opencode-engine.log", "tools"] {
        assert!(
            !home.join(rel).exists(),
            "{rel} exists in the home although no engine was wanted"
        );
    }
    let conn =
        std::fs::read_to_string(home.join("active-connection.json")).expect("active connection");
    assert!(
        conn.contains("\"shell\""),
        "the saved connection is the shell's own model: {conn}"
    );
    let config = std::fs::read_to_string(home.join("config.toml")).expect("config.toml");
    assert!(
        config.contains("name = \"Nemotron 3 Super\"") && config.contains("max_retries = 2"),
        "the pool model is written with a plain name and the fallback's retry cap:\n{config}"
    );
    drop(j);
    drop(content);
}
