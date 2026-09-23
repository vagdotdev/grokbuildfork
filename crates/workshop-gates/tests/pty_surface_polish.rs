//! Surface polish gates on the built `workshop` binary (v0.2.2): the welcome card is one product
//! name and an invitation to type; `/model` is an overlay with type-to-filter whose keys never
//! leak into the composer; the waiting line animates, counts seconds and names the cancel key;
//! the terminal title follows the session topic and is restored on exit.
//!
//! Opt-in: set `WORKSHOP_BIN` to the built binary and run with `--include-ignored`. Hermetic: the
//! engine is the fake `opencode` fixture (`silent` serve on loopback), no network.

mod pty_common;

use std::time::Duration;

use pty_common::*;

/// OSC 0/2 title payloads in the order the binary emitted them.
fn titles(raw: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(raw);
    let mut out = Vec::new();
    let mut rest = text.as_ref();
    while let Some(start) = rest.find("\u{1b}]") {
        let after = &rest[start + 2..];
        let Some(semi) = after.find(';') else { break };
        let code = &after[..semi];
        let body = &after[semi + 1..];
        let end = body.find(['\u{7}', '\u{1b}']).unwrap_or(body.len());
        if code == "0" || code == "2" {
            out.push(body[..end].to_owned());
        }
        rest = &body[end..];
    }
    out
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); run with --include-ignored"]
fn welcome_is_one_name_and_an_invitation_to_type() {
    let Some(bin) = bin_from_env() else { return };
    let fake = fake_opencode("silent");
    let mut j = spawn("surface-welcome", &bin, &[], Some(fake.path()));
    connect_big_pickle(&mut j);
    let screen = j.h.screen_contents();
    snapshot(&j.h, &j.dir, "01-welcome");
    assert!(
        screen.contains("Vagdev's Workshop"),
        "one product name:\n{screen}"
    );
    assert!(
        !screen.contains("Workshop by Vagdev"),
        "the alternate name is gone:\n{screen}"
    );
    assert!(
        screen.contains("Ask anything") && screen.contains("add a test for multiply"),
        "the composer invites typing with an example:\n{screen}"
    );
    assert!(
        !screen.contains("Resume session"),
        "a fresh directory has nothing to resume, so the row is not offered:\n{screen}"
    );
    assert!(
        screen.contains("Release notes") && !screen.contains("Changelog"),
        "the notes row is named for what it opens:\n{screen}"
    );
    assert!(
        screen.contains("/model to switch") && screen.contains("/auth to connect subscriptions"),
        "the footer hint line stays:\n{screen}"
    );
    // The title bar: our name while running, saved first so it can be given back on exit.
    j.h.update(Duration::from_millis(300));
    let raw = j.h.raw_output().to_vec();
    assert!(
        String::from_utf8_lossy(&raw).contains("\u{1b}[22;0t"),
        "the terminal's own title is saved (XTWINOPS 22) before ours is set"
    );
    assert!(
        titles(&raw).iter().any(|t| t == "Workshop"),
        "the title reads Workshop while idle: {:?}",
        titles(&raw)
    );
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); run with --include-ignored"]
fn model_picker_filters_as_you_type_and_swallows_stray_keys() {
    let Some(bin) = bin_from_env() else { return };
    let fake = fake_opencode("silent");
    let mut j = spawn("surface-picker", &bin, &[], Some(fake.path()));
    connect_big_pickle(&mut j);

    send_prompt(&mut j, "/model");
    wait_for(&mut j.h, "Tab: Subscriptions", 15);
    wait_for(&mut j.h, "OpenCode", 15);
    j.h.update(Duration::from_millis(500));
    let screen = j.h.screen_contents();
    snapshot(&j.h, &j.dir, "02-model-overlay");
    assert!(
        screen.contains("OpenCode") && screen.contains("Big Pickle"),
        "OpenCode's models lead the list under a quiet group label:\n{screen}"
    );
    assert!(
        !screen.contains("Kilo") && !screen.contains("engine"),
        "Kilo Gateway is never listed and nothing names the engine:\n{screen}"
    );
    assert!(
        screen.contains("type to filter"),
        "the search line invites typing:\n{screen}"
    );
    assert!(
        screen.contains('\u{276f}'),
        "the overlay leaves the composer visible behind it:\n{screen}"
    );

    // Bug 7: typing used to close the picker on the first letter and leave the rest in the prompt.
    j.h.inject_keys(b"pickle").unwrap();
    j.h.update(Duration::from_millis(600));
    let screen = j.h.screen_contents();
    snapshot(&j.h, &j.dir, "03-model-filter-pickle");
    assert!(
        screen.contains("Tab: Subscriptions"),
        "typing filters instead of closing:\n{screen}"
    );
    assert!(
        screen.contains("pickle"),
        "the filter text is shown:\n{screen}"
    );
    // Model rows sit inside the box (they end with its border); the composer footer does not.
    let rows: Vec<&str> = screen
        .lines()
        .filter(|l| l.trim_end().ends_with('\u{2502}') && l.contains("OpenCode "))
        .collect();
    assert!(
        !rows.is_empty()
            && rows
                .iter()
                .all(|r| r.to_ascii_lowercase().contains("pickle")),
        "only matching rows remain: {rows:?}"
    );
    assert!(
        !screen
            .lines()
            .any(|l| l.contains("\u{276f} ickle") || l.contains("\u{276f} pickle")),
        "nothing leaked into the composer:\n{screen}"
    );

    // Esc clears the filter first, then closes; the composer is still empty.
    j.h.inject_keys(b"\x1b").unwrap();
    j.h.update(Duration::from_millis(400));
    let screen = j.h.screen_contents();
    assert!(
        screen.contains("Tab: Subscriptions") && screen.contains("type to filter"),
        "the first Esc clears the filter:\n{screen}"
    );
    j.h.inject_keys(b"\x1b").unwrap();
    if let Err(e) =
        j.h.wait_for_text_absent("Tab: Subscriptions", Duration::from_secs(5))
    {
        panic!(
            "the second Esc closes the picker: {e}\n{}",
            j.h.screen_contents()
        );
    }
    j.h.update(Duration::from_millis(300));
    let screen = j.h.screen_contents();
    snapshot(&j.h, &j.dir, "04-after-esc");
    assert!(
        screen
            .lines()
            .any(|l| l.trim_start().starts_with("\u{2502} \u{276f}") && !l.contains("ickle")),
        "the composer is empty after the picker closes:\n{screen}"
    );
    assert!(
        screen.contains("Big Pickle") && !screen.contains("OpenCode \u{b7} Big Pickle"),
        "the session behind the overlay is still there, labeled with the model only:\n{screen}"
    );
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); run with --include-ignored"]
fn thinking_line_animates_counts_seconds_and_names_the_cancel_key() {
    let Some(bin) = bin_from_env() else { return };
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("python3 not available; skipping the silent-engine scenario");
        return;
    }
    let fake = fake_opencode("silent");
    let mut j = spawn("surface-waiting", &bin, &[], Some(fake.path()));
    connect_big_pickle(&mut j);
    send_prompt(&mut j, "add a test for multiply");
    // One neutral line for every phase behind the first answer; never a runtime name.
    wait_for(&mut j.h, "Thinking", 40);
    wait_for(&mut j.h, "Ctrl+C to cancel", 5);
    assert_no_plumbing(&j.h, "thinking line");
    let waiting_line = |h: &xai_grok_pager_pty_harness::PtyHarness| {
        h.screen_contents()
            .lines()
            .find(|l| l.contains("Thinking"))
            .map(|l| l.trim().to_owned())
    };
    let mut marks = std::collections::BTreeSet::new();
    for _ in 0..12 {
        if let Some(line) = waiting_line(&j.h)
            && let Some(mark) = line.chars().next()
        {
            marks.insert(mark);
        }
        j.h.update(Duration::from_millis(150));
    }
    assert!(marks.len() >= 2, "the spinner animates, saw {marks:?}");
    // Elapsed seconds appear after 3 s.
    j.h.update(Duration::from_millis(3200));
    let line = waiting_line(&j.h).expect("waiting line");
    snapshot(&j.h, &j.dir, "05-waiting-elapsed");
    assert!(
        line.contains("s \u{b7} Ctrl+C to cancel") || line.contains("s · Ctrl+C to cancel"),
        "elapsed seconds precede the cancel hint: {line}"
    );
    // Ten seconds in it is still the same calm line — no phase names, no runtime words.
    j.h.update(Duration::from_millis(7000));
    let line = waiting_line(&j.h).expect("waiting line");
    assert!(
        line.starts_with(|c: char| !c.is_ascii()) && line.contains("Thinking"),
        "{line}"
    );
    assert_no_plumbing(&j.h, "ten seconds in");
    snapshot(&j.h, &j.dir, "06-still-thinking");
    // The title follows the topic (the first prompt) while the turn runs.
    let seen = titles(j.h.raw_output());
    assert!(
        seen.iter().any(|t| t.contains("add a test for multiply")),
        "the terminal title names the session topic: {seen:?}"
    );
    // Ctrl+C twice cancels; the waiting line goes away.
    j.h.inject_keys(b"\x03").unwrap();
    j.h.update(Duration::from_millis(300));
    j.h.inject_keys(b"\x03").unwrap();
    wait_for(&mut j.h, "Turn cancelled", 10);
    assert!(
        !j.h.screen_contents().contains("Ctrl+C to cancel"),
        "the waiting line is removed once the turn ends:\n{}",
        j.h.screen_contents()
    );

    // Quit: the title is cleared and the saved one popped back.
    j.h.inject_keys(b"\x03").unwrap();
    j.h.update(Duration::from_millis(300));
    j.h.inject_keys(b"\x03").unwrap();
    let _ = j.h.wait_exit_code(Duration::from_secs(10));
    j.h.update(Duration::from_millis(300));
    let raw = j.h.raw_output().to_vec();
    let text = String::from_utf8_lossy(&raw);
    let restore = text
        .rfind("\u{1b}[23;0t")
        .expect("the saved title is popped on exit");
    let last_title = text.rfind("\u{1b}]0;").expect("a title escape");
    assert!(
        last_title < restore,
        "the pop comes after our last title write"
    );
    assert_eq!(
        titles(&raw).last().map(String::as_str),
        Some(""),
        "the last title we set is empty: {:?}",
        titles(&raw)
    );
}
