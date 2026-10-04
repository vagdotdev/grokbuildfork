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

    // `/model` lists the pool as its own group ahead of OpenCode, with the active row marked and
    // plain names; Esc closes it. OpenCode's rows are still there to pick (that path is unchanged).
    j.h.inject_keys(b"/model").unwrap();
    j.h.update(Duration::from_millis(400));
    j.h.inject_keys(b"\r").unwrap();
    wait_for(&mut j.h, PICKER_OPEN, 15);
    wait_for(&mut j.h, "Community pool", 20);
    j.h.update(Duration::from_millis(600));
    let screen = j.h.screen_contents();
    let pool_at = screen.find("Community pool").expect("pool header");
    let engine_at = screen.find("OpenCode").expect("OpenCode header");
    assert!(
        pool_at < engine_at,
        "the pool group comes before OpenCode in direct mode:\n{screen}"
    );
    let active_row = screen
        .lines()
        .find(|l| l.contains("Nemotron 3 Super") && l.contains("active"))
        .unwrap_or_else(|| panic!("the pool row is marked active:\n{screen}"));
    assert!(
        !active_row.contains("NVIDIA:") && !active_row.contains("(free)"),
        "plain name on the row: {active_row}"
    );
    assert!(
        screen.contains("Big Pickle"),
        "OpenCode's rows stay:\n{screen}"
    );
    assert!(
        !screen.contains("Kilo"),
        "no gateway name anywhere in the picker:\n{screen}"
    );
    snapshot(&j.h, &j.dir, "03-model-picker-direct");
    j.h.inject_keys(b"\x1b").unwrap();
    wait_picker_closed(&mut j.h, 15);

    // Engine-less, provably: `opencode serve` never ran (the picker's CLI detection may ask a
    // binary on PATH for its `--version`, which is identity, not an engine), and the home has no
    // engine state or log.
    let ran = std::fs::read_to_string(&calls).unwrap_or_default();
    assert!(
        ran.lines().all(|l| l == "--version" || l == "--help"),
        "opencode was run beyond an identity probe: {ran}"
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

/// A recording fake `opencode` that must never run (direct mode): returns the PATH dir to prepend
/// and the file that would record a call.
fn never_run_opencode() -> (tempfile::TempDir, std::path::PathBuf) {
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
    (fake, calls)
}

/// The composer's bottom border line (`… Nemotron 3 Super · always-approve ─╯`).
fn composer_border(screen: &str) -> String {
    screen
        .lines()
        .rev()
        .find(|l| l.contains('\u{256f}'))
        .map(str::to_owned)
        .unwrap_or_default()
}

/// Direct mode: the shared pool answers 429 for the active pool model. The turn goes on with the
/// next row of the chain — one plain line, the same prompt resent — and the composer names the
/// model that answered. Nothing about the pool's gateway or a "fallback" reaches the screen.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (mock inference on loopback answers 429 then 200); run with --include-ignored"]
fn a_rate_limited_pool_model_hands_the_turn_to_the_next_row() {
    use xai_grok_pager_pty_harness::{
        InferenceEndpoint, InferenceRequestMatcher, ScriptedResponse,
    };
    let Some(bin) = bin_from_env() else { return };
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let content = rt
        .block_on(ContentController::start())
        .expect("mock inference server");
    content.set_response("Four, says the next model.");
    let url = content.url();
    let (fake, calls) = never_run_opencode();

    let mut j = spawn(
        "direct-rate-limited",
        &bin,
        &[
            (KILO_BASE_URL_ENV, url.as_str()),
            ("WORKSHOP_FREE_MODELS", "direct"),
        ],
        Some(fake.path()),
    );
    wait_for(&mut j.h, "\u{276f}", 45);
    wait_for(&mut j.h, "Nemotron 3 Super", 30);
    j.h.update(Duration::from_millis(800));
    // The pool model's retry cap is two attempts: both agent-turn requests (the ones carrying the
    // tool schema; side queries such as the session title are not matched) get a 429, then the
    // next model's request gets the reply.
    let rate_limited = || {
        ScriptedResponse::json(
            429,
            serde_json::json!({"error": {"message": "Rate limit exceeded for free models", "type": "rate_limit"}}),
        )
    };
    let _first = content.expect_response(
        "429 first attempt",
        InferenceRequestMatcher::foreground(InferenceEndpoint::ChatCompletions),
        rate_limited(),
    );
    let _second = content.expect_response(
        "429 second attempt",
        InferenceRequestMatcher::foreground(InferenceEndpoint::ChatCompletions),
        rate_limited(),
    );

    send_prompt(&mut j, "what is two plus two?");
    wait_for(&mut j.h, "Four, says the next model.", 60);
    j.h.update(Duration::from_millis(1000));
    snapshot(&j.h, &j.dir, "01-answer-after-429");
    let screen = j.h.screen_contents();
    let seen: Vec<String> = content
        .requests()
        .iter()
        .map(|r| {
            format!(
                "{} {} model={:?}",
                r.method,
                r.path,
                r.body
                    .as_ref()
                    .and_then(|b| b.get("model"))
                    .and_then(|m| m.as_str())
            )
        })
        .collect();
    assert!(
        screen.contains("Nemotron 3 Super is busy right now \u{2014} using Nemotron 3 Ultra"),
        "one plain line names the switch (requests: {seen:#?}):\n{screen}"
    );
    assert!(
        composer_border(&screen).contains("Nemotron 3 Ultra"),
        "the composer names the model that answered:\n{screen}"
    );
    assert!(
        !screen.contains("Couldn't reach") && !screen.contains("Turn failed"),
        "no failure line when the next row answered:\n{screen}"
    );
    assert_no_plumbing(&j.h, "answer after 429");
    // The turn's requests: two for the first model (both 429), then the next model's.
    let models: Vec<String> = content
        .request_bodies()
        .iter()
        .filter_map(|b| b.get("model").and_then(|m| m.as_str()).map(str::to_owned))
        .filter(|m| m.starts_with("nvidia/"))
        .collect();
    assert!(
        models.starts_with(&[
            "nvidia/nemotron-3-super-120b-a12b:free".to_owned(),
            "nvidia/nemotron-3-super-120b-a12b:free".to_owned(),
            "nvidia/nemotron-3-ultra-550b-a55b:free".to_owned(),
        ]),
        "{models:?}"
    );
    assert!(!calls.exists(), "opencode was run");
    let config =
        std::fs::read_to_string(j.workshop_home().join("config.toml")).expect("config.toml");
    assert!(
        config.contains("name = \"Nemotron 3 Ultra\"")
            && config.contains("default = \"kilo-nvidia-nemotron-3-ultra-550b-a55b-free\""),
        "the next row is the shell's model now:\n{config}"
    );
    drop(j);
    drop(content);
}

/// Direct mode: a prompt with an image on a pool model that cannot see goes to the pool's vision
/// row, quietly — the request carries the image, the model that sees answers, and the composer
/// names it.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (mock inference on loopback); run with --include-ignored"]
fn an_image_prompt_on_a_text_only_pool_model_goes_to_the_vision_row() {
    let Some(bin) = bin_from_env() else { return };
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let content = rt
        .block_on(ContentController::start())
        .expect("mock inference server");
    content.set_response("A blue rectangle.");
    let url = content.url();
    let (fake, calls) = never_run_opencode();
    let pictures = tempfile::tempdir().expect("tempdir");
    // Big enough for the shell's image filter (at least 512 pixels).
    let img = image::RgbaImage::from_pixel(64, 32, image::Rgba([31, 111, 235, 255]));
    let mut bytes = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut bytes),
        image::ImageFormat::Png,
    )
    .unwrap();
    let png = pictures.path().join("screenshot.png");
    std::fs::write(&png, bytes).unwrap();

    let mut j = spawn(
        "direct-vision",
        &bin,
        &[
            (KILO_BASE_URL_ENV, url.as_str()),
            ("WORKSHOP_FREE_MODELS", "direct"),
        ],
        Some(fake.path()),
    );
    wait_for(&mut j.h, "\u{276f}", 45);
    wait_for(&mut j.h, "Nemotron 3 Super", 30);
    j.h.update(Duration::from_millis(800));

    // Paste the image the way a terminal drops a file: its path in a bracketed paste.
    j.h.inject_keys(format!("\x1b[200~{}\x1b[201~", png.display()).as_bytes())
        .unwrap();
    wait_for(&mut j.h, "[Image #1]", 10);
    j.h.update(Duration::from_millis(300));
    send_prompt(&mut j, " what does it show?");
    wait_for(&mut j.h, "A blue rectangle.", 90);
    j.h.update(Duration::from_millis(1000));
    snapshot(&j.h, &j.dir, "01-vision-answer");
    let screen = j.h.screen_contents();
    assert!(
        composer_border(&screen).contains("Qwen3.8 27B"),
        "the composer names the model that sees:\n{screen}"
    );
    assert!(
        !screen.contains("Couldn't reach") && !screen.contains("cannot see"),
        "no failure, no notice:\n{screen}"
    );
    assert_no_plumbing(&j.h, "vision answer");
    // The turn's request (not the next-prompt suggestion that follows a turn) carries the image
    // and names the vision model.
    let bodies = content.request_bodies();
    let body = bodies
        .iter()
        .find(|b| b.to_string().contains("image_url"))
        .unwrap_or_else(|| panic!("a request carrying the image: {bodies:?}"));
    assert_eq!(
        body.get("model").and_then(|m| m.as_str()),
        Some("qwen/qwen3.8-27b:free"),
        "{body}"
    );
    assert!(!calls.exists(), "opencode was run");
    drop(j);
    drop(content);
}
