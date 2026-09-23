//! An engine turn is shown the way Grok Build shows its own: the pager's turn-status row runs for
//! the whole turn, names the command that is running, and the turn ends with the pager's marker.
//!
//! * The row (`⠧ <activity> <phase timer>   <turn timer> [stop]`) sits between the transcript and
//!   the composer from the first moment until the turn really ends — never a frozen screen.
//! * While a tool call runs it names the step — the model's description of a command (`Install
//!   Ghostty…`, as upstream prefers over the raw command), `Writing hello.txt…` for a file tool —
//!   and counts that call's own seconds, so a long `apt` is visibly under way (the acceptance
//!   suite's T1 sat 108 s on a still screen). The tool row itself carries the running accent.
//! * Between calls it is the wait for the model again (`Waiting for response…`), counting from
//!   that moment; while text streams it reads `Responding…`.
//! * The end is the pager's `Worked for <turn time>` marker under the last thing the model did,
//!   the row is gone, and the composer's `Ask anything…` placeholder is back. Mid-turn the
//!   composer is blank.
//!
//! Hermetic: the answering fake `opencode` on loopback replays a two-tool turn, holding each call
//! "running" for a few seconds. Opt-in via `WORKSHOP_BIN`, `--include-ignored`.

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

/// The one turn-status row on screen: the row ending in `[stop]`.
fn status_row(screen: &str) -> &str {
    let rows: Vec<&str> = screen
        .lines()
        .filter(|l| l.trim_end().ends_with("[stop]"))
        .collect();
    assert_eq!(rows.len(), 1, "exactly one turn-status row:\n{screen}");
    rows[0]
}

/// Seconds figures on a row (`3.4s`, `12s`, `1m5s` counts as its minutes*60+seconds).
fn timers(row: &str) -> Vec<f64> {
    row.split_whitespace()
        .filter_map(|w| {
            let w = w.strip_suffix('s')?;
            if let Some((m, s)) = w.split_once('m') {
                return Some(m.parse::<f64>().ok()? * 60.0 + s.parse::<f64>().ok()?);
            }
            w.parse().ok()
        })
        .collect()
}

/// Poll until the status row names `activity` with a phase timer of at least `min_secs`.
fn wait_for_row(j: &mut Journey, activity: &str, min_secs: f64, secs: u64) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let screen = j.h.screen_contents();
        if let Some(row) = screen
            .lines()
            .find(|l| l.contains(activity) && l.trim_end().ends_with("[stop]"))
            && timers(row).first().is_some_and(|t| *t >= min_secs)
        {
            return row.to_owned();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the status row never read {activity:?} at {min_secs}s:\n{screen}"
        );
        j.h.update(Duration::from_millis(100));
    }
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode serve on loopback); run with --include-ignored"]
fn status_row_names_the_running_command_and_the_turn_ends_with_the_pagers_marker() {
    let Some(bin) = bin_from_env() else { return };
    let recorder = tempfile::tempdir().expect("tempdir");
    let record = recorder.path().join("prompts.jsonl");
    let turn =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/opencode_serve_two_tools.jsonl");
    let fake = fake_opencode_answering_with(&record, &turn, 0.5);
    let mut j = spawn("turn-progress", &bin, &[], Some(fake.path()));
    connect_big_pickle(&mut j);

    send_prompt(&mut j, "write hello.txt then show it");

    // 1. The wait for the model, then the first call (held running 3 s): the row names the file
    //    being written with the call's timer, above a blank composer.
    wait_for(&mut j.h, WAITING_ROW, 90);
    wait_for(&mut j.h, "Creating /work/hello.txt", 90);
    let row = wait_for_row(&mut j, "Writing /work/hello.txt\u{2026}", 0.0, 5);
    let screen = j.h.screen_contents();
    let tool1 = row_of(&screen, "Creating /work/hello.txt");
    let status = row_of(&screen, "Writing /work/hello.txt");
    assert!(
        status > tool1,
        "the status row is below the transcript (tool {tool1}, row {status}):\n{screen}"
    );
    assert_eq!(status_row(&screen), row.as_str());
    assert!(
        !screen.contains("Ask anything"),
        "the composer is blank while the turn runs:\n{screen}"
    );
    assert_no_plumbing(&j.h, "first tool call");
    snapshot(&j.h, &j.dir, "01-run-write-status-row");

    // 2. A paragraph and a second call (held 6 s): the command's description, `Show the file…`,
    //    with the call's own seconds — a long command is visibly under way — and the turn timer
    //    beside `[stop]`. The row for the running command pulses in the transcript.
    wait_for(&mut j.h, "Show the file", 30);
    let row = wait_for_row(&mut j, "Show the file\u{2026}", 3.0, 10);
    let secs = timers(&row);
    assert!(
        secs.len() >= 2 && secs[0] >= 3.0 && secs[0] <= 6.5 && secs[1] >= secs[0],
        "phase timer counts from the command's start, turn timer from the message: {row:?}"
    );
    let screen = j.h.screen_contents();
    let paragraph = row_of(&screen, "Now checking the file.");
    // The transcript row: `◆ Run cat /work/hello.txt` while it runs, `◆ Run Show the file` (the
    // description as its title) once it finished.
    let tool2 = screen
        .lines()
        .position(|l| {
            !l.trim_end().ends_with("[stop]")
                && (l.contains("cat /work/hello.txt") || l.contains("Show the file"))
        })
        .unwrap_or_else(|| panic!("the tool row is on screen:\n{screen}"));
    assert!(
        tool1 < paragraph && paragraph < tool2,
        "transcript order is tool, paragraph, tool ({tool1}, {paragraph}, {tool2}):\n{screen}"
    );
    assert!(
        !screen.contains("Worked for"),
        "no marker while the turn runs:\n{screen}"
    );
    assert_no_plumbing(&j.h, "second tool call");
    snapshot(&j.h, &j.dir, "02-run-command-with-seconds");

    // 3. The command finished, the model is at work again: the wait for the model, its clock
    //    restarted.
    let row = wait_for_row(&mut j, WAITING_ROW, 0.0, 15);
    assert!(
        timers(&row).first().is_some_and(|t| *t < 3.0),
        "the phase timer restarts with the new phase: {row:?}"
    );
    assert!(
        !status_row(&j.h.screen_contents()).contains("Show the file"),
        "a finished command is no longer the activity:\n{}",
        j.h.screen_contents()
    );
    snapshot(&j.h, &j.dir, "03-waiting-between-tools");

    // 4. The end: the pager's `Worked for …` marker under the last paragraph, the row gone, and
    //    the composer placeholder back.
    wait_for(&mut j.h, "Worked for", 60);
    j.h.update(Duration::from_millis(800));
    let screen = j.h.screen_contents();
    assert!(
        !screen.contains("[stop]") && !screen.contains(WAITING_ROW),
        "the status row leaves with the turn:\n{screen}"
    );
    let last = row_of(&screen, "Both steps are finished.");
    let marker = row_of(&screen, "Worked for");
    assert!(
        marker > last,
        "the marker is under the last paragraph (paragraph {last}, marker {marker}):\n{screen}"
    );
    let marker_line = screen.lines().nth(marker).unwrap_or_default().trim();
    let total = timers(marker_line);
    assert!(
        total.first().is_some_and(|t| *t >= 13.0),
        "the marker counts the whole turn (two held calls and a pause): {marker_line:?}"
    );
    assert!(
        screen.contains("Ask anything"),
        "the composer placeholder returns once the turn is over:\n{screen}"
    );
    assert_no_plumbing(&j.h, "after the turn");
    snapshot(&j.h, &j.dir, "04-worked-for-marker");
}
