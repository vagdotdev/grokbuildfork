//! `/models` (and bare `/model`) open Workshop's connection view: the OpenCode engine row is
//! there, no bundled Grok model is, and the view is the same one `/auth` opens. Fresh HOME.
//!
//! Opt-in: set `WORKSHOP_BIN` to the built binary and run with `--include-ignored`.

mod pty_common;

use std::time::Duration;

use pty_common::*;

fn assert_workshop_rows_no_grok(screen: &str, step: &str) {
    assert!(
        screen.contains("Big Pickle"),
        "{step}: OpenCode's default model is listed\n{screen}"
    );
    assert!(
        !screen.contains("Kilo"),
        "{step}: Kilo Gateway is never listed\n{screen}"
    );
    for grok in [
        "Grok 4.6",
        "Grok 4.5",
        "grok-4.6",
        "grok-4.5",
        "Grok 4 Fast",
    ] {
        assert!(
            !screen.contains(grok),
            "{step}: bundled Grok model {grok:?} leaks into the model list\n{screen}"
        );
    }
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); run with --include-ignored"]
fn models_and_model_open_the_connection_view_without_grok_rows() {
    let Some(bin) = bin_from_env() else { return };
    let fake = fake_opencode("crash");
    let mut j = spawn("models-view", &bin, &[], Some(fake.path()));
    connect_big_pickle(&mut j);

    // The `/model` autocomplete dropdown: whatever the shell lists, no Grok row — and never the
    // session placeholder as a lonely `(current)` row or the `No connection configured` hint.
    j.h.inject_keys(b"/model ").unwrap();
    j.h.update(Duration::from_millis(600));
    let dropdown = j.h.screen_contents();
    snapshot(&j.h, &j.dir, "01-model-dropdown");
    for grok in ["Grok 4.6", "Grok 4.5", "grok-4.6", "grok-4.5"] {
        assert!(
            !dropdown.contains(grok),
            "/model dropdown lists bundled Grok model {grok:?}\n{dropdown}"
        );
    }
    for stand_in in [
        "No connection configured",
        "(current)",
        "has no model connection",
    ] {
        assert!(
            !dropdown.contains(stand_in),
            "/model dropdown shows the stand-in {stand_in:?}\n{dropdown}"
        );
    }
    // Clear the composer.
    for _ in 0..8 {
        j.h.inject_keys(b"\x7f").unwrap();
    }
    j.h.update(Duration::from_millis(300));

    send_prompt(&mut j, "/models");
    wait_for(&mut j.h, "Big Pickle", 15);
    j.h.update(Duration::from_millis(500));
    let screen = j.h.screen_contents();
    snapshot(&j.h, &j.dir, "02-models-view");
    assert_workshop_rows_no_grok(&screen, "/models");
    // Esc closes the view and lands back on the composer.
    j.h.inject_keys(b"\x1b").unwrap();
    wait_for(&mut j.h, "\u{276f}", 10);

    send_prompt(&mut j, "/model");
    wait_for(&mut j.h, "Big Pickle", 15);
    j.h.update(Duration::from_millis(500));
    let screen = j.h.screen_contents();
    snapshot(&j.h, &j.dir, "03-model-view");
    assert_workshop_rows_no_grok(&screen, "/model");
    j.h.inject_keys(b"\x1b").unwrap();
    wait_for(&mut j.h, "\u{276f}", 10);
}
