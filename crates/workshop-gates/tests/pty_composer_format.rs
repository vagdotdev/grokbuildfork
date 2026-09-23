//! The composer's bottom border reads like upstream Grok Build's — `<model> (<effort>) · <mode>`:
//! `Big Pickle · always-approve`, `Ling 3.0 Flash Fin Free (high) · always-approve`.
//!
//! * The model name only: no provider, never "OpenCode".
//! * `(effort)` only when the model has effort levels in OpenCode's live catalog and one was
//!   picked on `/model` (one row per level); no level is ever invented. The pick reaches the
//!   engine as the prompt's `variant` — the fake `opencode serve` records every prompt body.
//! * The mode comes from the same flags upstream draws (`--yolo` → `always-approve`).
//! * On the first message nothing says "OpenCode", "Starting…" or "Waiting for OpenCode…".
//!
//! Hermetic: the answering fake `opencode` on loopback. Opt-in via `WORKSHOP_BIN`,
//! `--include-ignored`.

mod pty_common;

use std::time::Duration;

use pty_common::*;

/// The composer's bottom border line (`╰──… <label> ─╯`): the last box bottom on screen (the
/// welcome card above it is a box too).
fn footer(j: &Journey) -> String {
    j.h.screen_contents()
        .lines()
        .rev()
        .find(|l| l.contains('\u{256f}'))
        .map(str::to_owned)
        .unwrap_or_default()
}

fn recorded_prompts(record: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(record)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("recorded prompt body is JSON"))
        .collect()
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode serve on loopback); run with --include-ignored"]
fn composer_border_is_model_effort_and_mode_like_upstream() {
    let Some(bin) = bin_from_env() else { return };
    let recorder = tempfile::tempdir().expect("tempdir");
    let record = recorder.path().join("prompts.jsonl");
    let fake = fake_opencode_answering(&record);
    let mut j = spawn_with_args("composer-format", &bin, &["--yolo"], &[], Some(fake.path()));

    // 1. First run under always-approve: `Big Pickle · always-approve` — upstream's exact shape,
    //    no provider, and no `(effort)` because Big Pickle has no levels.
    connect_big_pickle(&mut j);
    let line = footer(&j);
    assert!(
        line.contains("Big Pickle \u{b7} always-approve"),
        "composer border reads `<model> · <mode>`: {line}"
    );
    assert!(
        !line.contains("OpenCode") && !line.contains('('),
        "no provider and no invented effort level: {line}"
    );
    snapshot(&j.h, &j.dir, "01-first-run-always-approve");

    // 2. The first message: the reply arrives; nothing on the way named the runtime.
    send_prompt(&mut j, "hello");
    wait_for(&mut j.h, "Created hello.txt with the exact line.", 90);
    j.h.update(Duration::from_millis(800));
    snapshot(&j.h, &j.dir, "02-after-first-reply");
    let raw = String::from_utf8_lossy(j.h.raw_output()).to_string();
    for word in [
        "OpenCode engine",
        "OpenCode \u{b7}",
        "Starting the",
        "Starting OpenCode",
        "Installing the",
        "Waiting for OpenCode",
        "Waiting for Big Pickle",
    ] {
        assert!(
            !raw.contains(word),
            "{word:?} must never be drawn on the first message"
        );
    }
    assert_no_plumbing(&j.h, "after the first reply");
    let prompts = recorded_prompts(&record);
    assert_eq!(
        prompts.len(),
        1,
        "one prompt reached the engine: {prompts:?}"
    );
    assert_eq!(prompts[0]["model"]["modelID"], "big-pickle");
    assert!(
        prompts[0].get("variant").is_none(),
        "a model without levels sends no variant: {}",
        prompts[0]
    );

    // 3. `/model` after the live catalog: a model with effort levels is one row per level,
    //    titled the way the composer will read.
    send_prompt(&mut j, "/model");
    wait_for(&mut j.h, "Tab: Subscriptions", 15);
    wait_for(&mut j.h, "Ling 3.0 Flash Fin Free (high)", 20);
    let screen = j.h.screen_contents();
    for row in [
        "Ling 3.0 Flash Fin Free",
        "Ling 3.0 Flash Fin Free (low)",
        "Ling 3.0 Flash Fin Free (medium)",
        "Ling 3.0 Flash Fin Free (high)",
    ] {
        assert!(screen.contains(row), "level row {row:?} listed:\n{screen}");
    }
    assert!(
        !screen.contains("Big Pickle ("),
        "a model without levels gets no level rows:\n{screen}"
    );
    move_selection_to(&mut j.h, "Ling 3.0 Flash Fin Free (high)");
    snapshot(&j.h, &j.dir, "03-picker-effort-rows");
    j.h.inject_keys(b"\r").unwrap();
    if let Err(e) =
        j.h.wait_for_text_absent("Tab: Subscriptions", Duration::from_secs(30))
    {
        panic!(
            "picker did not close after picking a level: {e}\n{}",
            j.h.screen_contents()
        );
    }

    // 4. The border now carries the level in upstream's format, mode included.
    wait_for(
        &mut j.h,
        "Ling 3.0 Flash Fin Free (high) \u{b7} always-approve",
        15,
    );
    let line = footer(&j);
    assert!(
        line.contains("Ling 3.0 Flash Fin Free (high) \u{b7} always-approve")
            && !line.contains("OpenCode"),
        "composer border reads `<model> (<effort>) · <mode>`: {line}"
    );
    snapshot(&j.h, &j.dir, "04-composer-model-effort-mode");
    let conn = std::fs::read_to_string(j.workshop_home().join("active-connection.json"))
        .expect("active connection saved");
    assert!(
        conn.contains("\"effort\": \"high\"") && conn.contains("ling-3.0-flash-fin-free"),
        "the pick (model + level) persists for the next launch: {conn}"
    );

    // 5. The pick reaches the engine: the next prompt names the model and carries `variant`.
    send_prompt(&mut j, "again");
    let started = std::time::Instant::now();
    while recorded_prompts(&record).len() < 2 {
        assert!(
            started.elapsed() < Duration::from_secs(90),
            "the second prompt never reached the engine:\n{}",
            j.h.screen_contents()
        );
        j.h.update(Duration::from_millis(200));
    }
    // Let the (second) replayed reply render.
    j.h.update(Duration::from_millis(2500));
    let prompts = recorded_prompts(&record);
    assert_eq!(
        prompts.len(),
        2,
        "two prompts reached the engine: {prompts:?}"
    );
    assert_eq!(prompts[1]["model"]["modelID"], "ling-3.0-flash-fin-free");
    assert_eq!(
        prompts[1]["variant"], "high",
        "the picked level is the prompt's variant: {}",
        prompts[1]
    );
    assert_no_plumbing(&j.h, "after the second reply");
    snapshot(&j.h, &j.dir, "05-second-reply-at-high");
}
