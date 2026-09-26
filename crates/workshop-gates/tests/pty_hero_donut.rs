//! The welcome hero is an ASCII donut that spins only where it should: in the hero box of a
//! focused colour terminal, at the slow tick, repainting its own cells and nothing else. It rests
//! on its first frame under `NO_COLOR` and under `[ui] hero_animation = false`, and the first
//! message ends it — the welcome view leaves and the binary goes quiet.
//!
//! Hermetic: the answering fake `opencode` on loopback. Opt-in via `WORKSHOP_BIN`,
//! `--include-ignored`.

mod pty_common;

use std::time::Duration;

use pty_common::*;
use workshop_brand::donut::{self, Size};

/// The hero logo's 7 x 14 cells as text: the rows from the version line down, between the box's
/// left border and the title column.
fn hero_region(screen: &str) -> Vec<String> {
    let lines: Vec<&str> = screen.lines().collect();
    let row = lines
        .iter()
        .position(|l| l.contains("Vagdev's Workshop") && l.contains('\u{2502}'))
        .unwrap_or_else(|| panic!("the hero box's version row is on screen:\n{screen}"));
    let version_line: Vec<char> = lines[row].chars().collect();
    let border = version_line.iter().position(|c| *c == '\u{2502}').unwrap();
    let title = version_line
        .windows(8)
        .position(|w| w.iter().collect::<String>() == "Vagdev's")
        .unwrap();
    let logo_left = border + 3;
    assert!(
        title >= logo_left + Size::Full.cols(),
        "the logo column fits between the border and the title"
    );
    (row..row + Size::Full.rows())
        .map(|r| {
            lines
                .get(r)
                .map(|l| {
                    l.chars()
                        .skip(logo_left)
                        .take(Size::Full.cols())
                        .collect::<String>()
                })
                .unwrap_or_default()
        })
        .collect()
}

fn trimmed(rows: &[String]) -> String {
    rows.iter()
        .map(|r| r.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

fn resting_frame() -> String {
    donut::frame(Size::Full, 0)
        .text()
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Sample the hero region every 120 ms for ~1.5 s; distinct pictures seen.
fn hero_pictures(j: &mut Journey) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for _ in 0..12 {
        let picture = trimmed(&hero_region(&j.h.screen_contents()));
        if !seen.contains(&picture) {
            seen.push(picture);
        }
        j.h.update(Duration::from_millis(120));
    }
    seen
}

/// Cells outside the hero region must not change between samples while the welcome screen idles.
fn screen_without_hero(screen: &str) -> String {
    let lines: Vec<&str> = screen.lines().collect();
    let row = lines
        .iter()
        .position(|l| l.contains("Vagdev's Workshop") && l.contains('\u{2502}'))
        .unwrap();
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if (row..row + Size::Full.rows()).contains(&i) {
                // Keep the right column (title, menu); blank the logo's cells.
                let chars: Vec<char> = l.chars().collect();
                let border = chars.iter().position(|c| *c == '\u{2502}').unwrap_or(0);
                chars
                    .iter()
                    .enumerate()
                    .map(|(x, c)| {
                        if (border + 1..border + 3 + Size::Full.cols() + 3).contains(&x) {
                            ' '
                        } else {
                            *c
                        }
                    })
                    .collect()
            } else {
                (*l).to_owned()
            }
        })
        .collect::<Vec<String>>()
        .join("\n")
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode serve on loopback); run with --include-ignored"]
fn hero_donut_spins_in_the_hero_box_and_stops_at_the_first_prompt() {
    let Some(bin) = bin_from_env() else { return };
    let recorder = tempfile::tempdir().expect("tempdir");
    let fake = fake_opencode_answering(&recorder.path().join("prompts.jsonl"));
    let home = tempfile::tempdir().expect("tempdir");
    let mut j = spawn_in_colored("hero-donut", &bin, &[], Some(fake.path()), home);
    connect_big_pickle(&mut j);
    j.h.update(Duration::from_millis(500));
    snapshot(&j.h, &j.dir, "01-welcome-donut");

    // 1. The donut turns: several distinct frames within 1.5 s, all of them frames of the loop,
    //    and nothing outside the logo's cells moves.
    let before = screen_without_hero(&j.h.screen_contents());
    let pictures = hero_pictures(&mut j);
    assert!(
        pictures.len() >= 4,
        "the hero turns (saw {} distinct pictures):\n{}",
        pictures.len(),
        pictures.join("\n---\n")
    );
    let loop_frames: Vec<String> = (0..donut::FRAMES)
        .map(|i| {
            donut::frame(Size::Full, i)
                .text()
                .lines()
                .map(str::trim_end)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect();
    for picture in &pictures {
        assert!(
            loop_frames.contains(picture),
            "every picture is a frame of the donut's loop:\n{picture}"
        );
    }
    assert_eq!(
        screen_without_hero(&j.h.screen_contents()),
        before,
        "only the hero's cells change while the welcome screen idles"
    );
    assert!(
        pictures.iter().all(|p| !p.contains('\u{2800}')),
        "no braille from the old mark"
    );

    // 2. Repaints reach only the hero: every cursor move the binary emits during a second of
    //    spinning lands inside the logo's rows.
    let screen = j.h.screen_contents();
    let logo_row = screen
        .lines()
        .position(|l| l.contains("Vagdev's Workshop") && l.contains('\u{2502}'))
        .unwrap();
    let mark = j.h.raw_output().len();
    j.h.update(Duration::from_millis(1000));
    let fresh = String::from_utf8_lossy(&j.h.raw_output()[mark..]).to_string();
    let rows_touched = cursor_rows(&fresh);
    assert!(
        !rows_touched.is_empty(),
        "the spin paints something in a second: {fresh:?}"
    );
    for row in &rows_touched {
        assert!(
            (logo_row..logo_row + Size::Full.rows()).contains(&(row - 1)),
            "a repaint left the hero rows ({logo_row}..): row {row} in\n{fresh}"
        );
    }
    let bytes_per_second = fresh.len();
    eprintln!("hero spin: {bytes_per_second} bytes/s, rows {rows_touched:?}");

    // 3. The first message: the welcome view leaves, the donut with it, and once the reply has
    //    landed the binary emits nothing at all.
    send_prompt(&mut j, "hello");
    wait_for(&mut j.h, "Worked for", 60);
    j.h.update(Duration::from_millis(1500));
    let screen = j.h.screen_contents();
    assert!(
        !screen.contains("Vagdev's Workshop  "),
        "the hero box is gone with the welcome view:\n{screen}"
    );
    snapshot(&j.h, &j.dir, "02-after-first-prompt");
    let quiet_from = j.h.raw_output().len();
    j.h.update(Duration::from_millis(2500));
    let after = j.h.raw_output().len();
    assert_eq!(
        after,
        quiet_from,
        "nothing repaints after the first prompt (got {} bytes):\n{}",
        after - quiet_from,
        String::from_utf8_lossy(&j.h.raw_output()[quiet_from..])
    );
}

/// Rows (1-based, as in the escape) of every `CSI row ; col H` cursor move in `out` that text is
/// painted after. A move followed only by other control sequences (the cursor parked back on the
/// composer at the end of a frame) paints nothing and is not counted.
fn cursor_rows(out: &str) -> Vec<usize> {
    let mut rows = Vec::new();
    let mut rest = out;
    while let Some(start) = rest.find("\u{1b}[") {
        let after = &rest[start + 2..];
        let end = after
            .find(|c: char| c.is_ascii_alphabetic())
            .unwrap_or(after.len());
        let (params, cmd) = after.split_at(end);
        rest = &after[end.min(after.len())..];
        if cmd.starts_with('H')
            && let Some((row, _)) = params.split_once(';')
            && let Ok(row) = row.parse::<usize>()
            && paints_text(&rest[1.min(rest.len())..])
        {
            rows.push(row);
        }
    }
    rows.sort_unstable();
    rows.dedup();
    rows
}

/// Whether printable text follows before the next cursor move or end of the frame (styling
/// sequences in between are fine).
fn paints_text(mut rest: &str) -> bool {
    loop {
        let Some(c) = rest.chars().next() else {
            return false;
        };
        if c != '\u{1b}' {
            return !c.is_control();
        }
        let Some(after) = rest.strip_prefix("\u{1b}[") else {
            return false;
        };
        let end = after
            .find(|c: char| c.is_ascii_alphabetic())
            .unwrap_or(after.len());
        let cmd = after[end..].chars().next();
        if cmd != Some('m') {
            return false;
        }
        rest = &after[end + 1..];
    }
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic; run with --include-ignored"]
fn hero_donut_rests_on_frame_zero_under_no_color() {
    let Some(bin) = bin_from_env() else { return };
    let fake = fake_opencode("silent");
    // `spawn` sets NO_COLOR=1.
    let mut j = spawn("hero-donut-no-color", &bin, &[], Some(fake.path()));
    connect_big_pickle(&mut j);
    j.h.update(Duration::from_millis(500));
    snapshot(&j.h, &j.dir, "01-welcome-resting");
    let pictures = hero_pictures(&mut j);
    assert_eq!(
        pictures,
        vec![resting_frame()],
        "one picture, the first frame of the loop"
    );
    // A resting welcome screen paints nothing.
    let mark = j.h.raw_output().len();
    j.h.update(Duration::from_millis(2000));
    assert_eq!(
        j.h.raw_output().len(),
        mark,
        "no repaint while resting:\n{}",
        String::from_utf8_lossy(&j.h.raw_output()[mark..])
    );
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic; run with --include-ignored"]
fn hero_donut_rests_when_the_user_turns_it_off() {
    let Some(bin) = bin_from_env() else { return };
    let fake = fake_opencode("silent");
    let home = tempfile::tempdir().expect("tempdir");
    let workshop_home = home.path().join(".workshop");
    std::fs::create_dir_all(&workshop_home).unwrap();
    std::fs::write(
        workshop_home.join("config.toml"),
        "[ui]\nhero_animation = false\n",
    )
    .unwrap();
    let mut j = spawn_in_colored("hero-donut-off", &bin, &[], Some(fake.path()), home);
    connect_big_pickle(&mut j);
    j.h.update(Duration::from_millis(500));
    snapshot(&j.h, &j.dir, "01-welcome-off");
    let screen = j.h.screen_contents();
    assert!(
        !screen.contains("unrecognized"),
        "`hero_animation` is a known key:\n{screen}"
    );
    let pictures = hero_pictures(&mut j);
    assert_eq!(pictures, vec![resting_frame()]);
}
