//! The composer's bottom border reads like upstream Grok Build's — `<model> (<effort>) · <mode>`:
//! `Big Pickle · always-approve`, `Ling 3.0 Flash Fin Free (high) · always-approve`.
//!
//! * The model name only: no provider, never "OpenCode".
//! * `(effort)` only when the model has effort levels in OpenCode's live catalog and one was
//!   picked — in `/model`'s effort sub-menu (one row per model, the levels a follow-up choice) or
//!   with `/effort <level>`; no level is ever invented. The pick reaches the engine as the
//!   prompt's `variant` — the fake `opencode serve` records every prompt body.
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

    // 3. `/model` after the live catalog: one row per model; a model with effort levels opens
    //    (Enter) into its levels, `Default` first, as a follow-up choice — never a row per level.
    send_prompt(&mut j, "/model");
    wait_for(&mut j.h, PICKER_OPEN, 15);
    wait_for(&mut j.h, "Ling 3.0 Flash Fin Free", 20);
    wait_gone(&mut j.h, "refreshing lists", 15);
    let screen = j.h.screen_contents();
    for level_row in [
        "Ling 3.0 Flash Fin Free (low)",
        "Ling 3.0 Flash Fin Free (medium)",
        "Ling 3.0 Flash Fin Free (high)",
        "Big Pickle (",
    ] {
        assert!(
            !screen.contains(level_row),
            "one row per model, no level rows ({level_row:?}):\n{screen}"
        );
    }
    move_selection_to(&mut j.h, "Ling 3.0 Flash Fin Free");
    let line = selected_line(&j.h).unwrap_or_default();
    assert!(
        line.contains(OPENS_SUBMENU),
        "a model with levels is marked as opening a sub-menu: {line}"
    );
    snapshot(&j.h, &j.dir, "03-picker-one-row-per-model");
    j.h.inject_keys(b"\r").unwrap();
    wait_for(&mut j.h, "Models \u{203a} Ling 3.0 Flash Fin Free", 10);
    let screen = j.h.screen_contents();
    let (d, l, m, h) = (
        screen.find("Default").unwrap(),
        screen.find(" low").unwrap(),
        screen.find(" medium").unwrap(),
        screen.find(" high").unwrap(),
    );
    assert!(
        d < l && l < m && m < h,
        "the levels follow `Default`, lowest first:\n{screen}"
    );
    assert!(
        screen.contains("at its default effort"),
        "the detail explains the highlighted level:\n{screen}"
    );
    move_selection_to(&mut j.h, "high");
    snapshot(&j.h, &j.dir, "03b-picker-effort-sub-menu");
    j.h.inject_keys(b"\r").unwrap();
    wait_picker_closed(&mut j.h, 30);

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

    // 6. `/effort <level>` changes the level without the picker; the border and the saved pick
    //    follow, and an unknown level is refused with the offered ones.
    send_prompt(&mut j, "/effort medium");
    wait_for(
        &mut j.h,
        "Ling 3.0 Flash Fin Free (medium) \u{b7} always-approve",
        15,
    );
    snapshot(&j.h, &j.dir, "06-effort-command");
    let conn = std::fs::read_to_string(j.workshop_home().join("active-connection.json"))
        .expect("active connection saved");
    assert!(
        conn.contains("\"effort\": \"medium\""),
        "/effort persists like a /model pick: {conn}"
    );
    send_prompt(&mut j, "/effort turbo");
    wait_for(&mut j.h, "unknown effort level 'turbo'", 10);
    let screen = j.h.screen_contents();
    assert!(
        screen.contains("default|low|medium|high"),
        "the refusal lists the catalog's levels:\n{screen}"
    );
    send_prompt(&mut j, "/effort default");
    wait_for(
        &mut j.h,
        "Ling 3.0 Flash Fin Free \u{b7} always-approve",
        15,
    );
    assert!(
        !footer(&j).contains('('),
        "the default level shows no `(effort)`: {}",
        footer(&j)
    );
}
