//! The waiting line lives for the whole turn, under whatever the model did last, and the turn ends
//! quietly.
//!
//! * `Thinking…` (animated mark, elapsed seconds, `Ctrl+C to cancel`) is on screen from the first
//!   moment until the turn really ends — one line, always below the latest block (a tool row, a
//!   paragraph), never stranded above one.
//! * The end is quiet: the waiting line goes, a dim `Done · Ns` line sits under the last thing the
//!   model did, and the composer's `Ask anything…` placeholder is back. Mid-turn the composer is
//!   blank.
//!
//! Hermetic: the answering fake `opencode` on loopback replays a two-tool turn at one event per
//! second. Opt-in via `WORKSHOP_BIN`, `--include-ignored`.

mod pty_common;

use std::path::Path;
use std::time::Duration;

use pty_common::*;

/// Screen row of the first line containing `needle`.
fn row_of(screen: &str, needle: &str) -> usize {
    screen
        .lines()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} is not on screen:\n{screen}"))
}

/// The one waiting line on screen: `⠹ Thinking… · 5s · Ctrl+C to cancel`.
fn thinking_line(screen: &str) -> &str {
    let lines: Vec<&str> = screen.lines().filter(|l| l.contains("Thinking")).collect();
    assert_eq!(
        lines.len(),
        1,
        "exactly one waiting line, never one per block:\n{screen}"
    );
    lines[0]
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode serve on loopback); run with --include-ignored"]
fn thinking_line_follows_the_turn_and_ends_quietly() {
    let Some(bin) = bin_from_env() else { return };
    let recorder = tempfile::tempdir().expect("tempdir");
    let record = recorder.path().join("prompts.jsonl");
    let turn =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/opencode_serve_two_tools.jsonl");
    let fake = fake_opencode_answering_with(&record, &turn, 1.0);
    let mut j = spawn("turn-progress", &bin, &[], Some(fake.path()));
    connect_big_pickle(&mut j);

    send_prompt(&mut j, "write hello.txt then show it");

    // 1. The first tool row: the waiting line sits right under it and the composer is blank.
    wait_for(&mut j.h, "Creating /work/hello.txt", 90);
    let screen = j.h.screen_contents();
    let tool1 = row_of(&screen, "Creating /work/hello.txt");
    let thinking = row_of(&screen, "Thinking");
    assert!(
        thinking > tool1,
        "the waiting line is below the tool row (tool {tool1}, waiting {thinking}):\n{screen}"
    );
    assert!(
        thinking_line(&screen).contains("Ctrl+C to cancel"),
        "the waiting line names the way out:\n{screen}"
    );
    assert!(
        !screen.contains("Ask anything"),
        "the composer is blank while the turn runs:\n{screen}"
    );
    assert_no_plumbing(&j.h, "under the first tool row");
    snapshot(&j.h, &j.dir, "01-thinking-under-first-tool");

    // 2. A paragraph and a second tool row later: still the one waiting line, now under the
    //    second tool row, counting seconds.
    wait_for(&mut j.h, "cat /work/hello.txt", 30);
    let screen = j.h.screen_contents();
    let paragraph = row_of(&screen, "Now checking the file.");
    let tool2 = row_of(&screen, "cat /work/hello.txt");
    let thinking = row_of(&screen, "Thinking");
    assert!(
        tool1 < paragraph && paragraph < tool2 && tool2 < thinking,
        "transcript order is tool, paragraph, tool, waiting line \
         ({tool1}, {paragraph}, {tool2}, {thinking}):\n{screen}"
    );
    let line = thinking_line(&screen);
    assert!(
        line.contains("Thinking\u{2026} \u{b7}") && line.contains("s \u{b7} Ctrl+C to cancel"),
        "the waiting line counts the seconds since the message was sent: {line:?}"
    );
    assert!(
        !screen.contains("Done \u{b7}"),
        "no done line while the turn runs:\n{screen}"
    );
    assert_no_plumbing(&j.h, "under the second tool row");
    snapshot(&j.h, &j.dir, "02-thinking-under-second-tool");

    // 3. The end: the waiting line is gone, `Done · Ns` sits under the last paragraph, and the
    //    composer placeholder is back.
    wait_for(&mut j.h, "Done \u{b7}", 60);
    j.h.update(Duration::from_millis(800));
    let screen = j.h.screen_contents();
    assert!(
        !screen.contains("Thinking"),
        "the waiting line leaves with the turn:\n{screen}"
    );
    let last = row_of(&screen, "Both steps are finished.");
    let done = row_of(&screen, "Done \u{b7}");
    assert!(
        done > last,
        "the done line is under the last paragraph (paragraph {last}, done {done}):\n{screen}"
    );
    let done_line = screen.lines().nth(done).unwrap_or_default().trim();
    assert!(
        done_line.starts_with("Done \u{b7} ")
            && done_line.ends_with('s')
            && done_line.chars().any(|c| c.is_ascii_digit()),
        "the done line is `Done · <seconds>s`: {done_line:?}"
    );
    assert!(
        screen.contains("Ask anything"),
        "the composer placeholder returns once the turn is over:\n{screen}"
    );
    assert_no_plumbing(&j.h, "after the turn");
    snapshot(&j.h, &j.dir, "03-done-quietly");
}
