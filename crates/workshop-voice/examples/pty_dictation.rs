//! Drive the real `workshop` TUI in a PTY through a dictation: `/voice`, a recorded clip from a
//! fake system recorder, Esc (text stays in the prompt), then `/voice` again and Enter. Writes
//! screen snapshots to `$WORKSHOP_PTY_EVIDENCE_DIR` (voice-spec §8.2–8.5, §9.2).
//!
//! ```bash
//! WORKSHOP_BIN=target/debug/workshop WORKSHOP_HOME=/tmp/cleanhome/.workshop \
//! FAKE_MIC_BIN_DIR=/tmp/fakemic FAKE_MIC_WAV=crates/workshop-voice/tests/fixtures/jfk.wav \
//! WORKSHOP_PTY_EVIDENCE_DIR=/tmp/pty-voice cargo run -p workshop-voice --example pty_dictation
//! ```
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use xai_grok_pager_pty_harness::PtyHarness;

fn snapshot(h: &PtyHarness, dir: &Path, name: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let screen = h.screen_contents();
    std::fs::write(dir.join(format!("{name}.txt")), &screen).unwrap();
    println!("--- {name} ---\n{}", screen.trim_end());
}

fn wait_for(h: &mut PtyHarness, text: &str, secs: u64) -> Duration {
    let t = Instant::now();
    if let Err(e) = h.wait_for_text(text, Duration::from_secs(secs)) {
        panic!(
            "timed out waiting for {text:?}: {e}\nscreen:\n{}",
            h.screen_contents()
        );
    }
    t.elapsed()
}

fn move_selection_to(h: &mut PtyHarness, needle: &str) {
    for _ in 0..40 {
        let screen = h.screen_contents();
        if screen
            .lines()
            .any(|l| l.contains('\u{203a}') && l.contains(needle))
        {
            return;
        }
        h.inject_keys(b"\x1b[B").unwrap();
        h.update(Duration::from_millis(80));
    }
    panic!(
        "never reached a selected row containing {needle:?}\nscreen:\n{}",
        h.screen_contents()
    );
}

fn main() {
    let bin = PathBuf::from(std::env::var_os("WORKSHOP_BIN").expect("WORKSHOP_BIN"));
    let workshop_home = std::env::var("WORKSHOP_HOME").expect("WORKSHOP_HOME (an installed home)");
    let fake_dir = std::env::var("FAKE_MIC_BIN_DIR").expect("FAKE_MIC_BIN_DIR");
    let wav = std::env::var("FAKE_MIC_WAV").expect("FAKE_MIC_WAV");
    let evidence = PathBuf::from(
        std::env::var_os("WORKSHOP_PTY_EVIDENCE_DIR").unwrap_or_else(|| "target/pty-voice".into()),
    );
    let cwd = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init", "-q", "."])
        .current_dir(cwd.path())
        .status()
        .unwrap();
    let home = Path::new(&workshop_home)
        .parent()
        .unwrap()
        .to_string_lossy()
        .to_string();
    // Only the fake recorder is on PATH: upstream's Linux capture resolves `arecord` from it.
    let path = format!("{fake_dir}:/usr/bin:/bin");
    let env: Vec<(&str, &str)> = vec![
        ("HOME", home.as_str()),
        ("WORKSHOP_HOME", workshop_home.as_str()),
        ("PATH", path.as_str()),
        ("FAKE_MIC_WAV", wav.as_str()),
        ("TERM", "xterm-256color"),
        ("NO_COLOR", "1"),
        ("GROK_DISABLE_AUTOUPDATER", "1"),
    ];
    let mut h = PtyHarness::new_inherited_env(&bin, 40, 120, &[], &env, Some(cwd.path()))
        .expect("spawn workshop in pty");
    h.set_respond_to_queries(true);

    // Cold start opens the connection picker. On this branch a session needs a configured
    // connection before the prompt accepts input, so pick the keyless free pool (it writes a
    // `[model.…]` entry to config.toml; nothing is contacted until a prompt is sent).
    wait_for(&mut h, "\u{276f}", 30); // prompt marker, with or without the picker on top
    if h.screen_contents().contains("connect a model") {
        move_selection_to(&mut h, "Auto Free (rotates free models)");
        h.inject_keys(b"\r").unwrap();
        h.update(Duration::from_millis(1500));
        if h.screen_contents().contains("connect a model") {
            h.inject_keys(b"\x1b").unwrap();
            h.update(Duration::from_millis(500));
        }
    }
    snapshot(&h, &evidence, "01-home");

    // /voice on the welcome screen creates a session and starts recording.
    let t_press = Instant::now();
    h.inject_keys(b"/voice\r").unwrap();
    let to_banner = wait_for(&mut h, "Recording", 60);
    println!(
        "recording banner {:.2}s after /voice",
        to_banner.as_secs_f32()
    );
    snapshot(&h, &evidence, "02-recording-banner");

    // Watch for the interim overlay while the clip plays (the fake mic leads with 0.5 s silence).
    let mut first_interim: Option<Duration> = None;
    let deadline = Instant::now() + Duration::from_secs(40);
    while Instant::now() < deadline {
        h.update(Duration::from_millis(300));
        let s = h.screen_contents();
        if s.contains("fellow") || s.contains("country") {
            first_interim.get_or_insert(t_press.elapsed());
            if s.contains("your country") {
                break;
            }
        }
    }
    snapshot(&h, &evidence, "03-interim-overlay");
    println!(
        "first interim visible {:.2}s after /voice",
        first_interim.map(|d| d.as_secs_f32()).unwrap_or(f32::NAN)
    );
    // Let the clip finish, then Esc: stop, keep the text, do not send.
    std::thread::sleep(Duration::from_secs(3));
    let t_stop = Instant::now();
    h.inject_keys(b"\x1b").unwrap();
    let to_final = wait_for(&mut h, "ask what you can do for your country", 60);
    h.update(Duration::from_millis(500));
    println!(
        "committed text visible {:.2}s after Esc",
        to_final.as_secs_f32()
    );
    let _ = t_stop;
    snapshot(&h, &evidence, "04-after-esc-text-in-prompt");
    let screen = h.screen_contents();
    assert!(
        !screen.contains("Recording"),
        "Esc must stop the banner:\n{screen}"
    );

    // Second dictation via the Ctrl+Space chord (the prompt already holds text, so `/voice` would
    // not be a command) reuses the warm helper: no reload, no dead gap. Esc again keeps the text;
    // sending a turn to the connected free pool is deliberately not part of this run.
    let t2 = Instant::now();
    h.inject_keys(b"\x00").unwrap();
    let to_banner2 = wait_for(&mut h, "Recording", 30);
    println!(
        "second recording banner {:.2}s after Ctrl+Space (warm helper)",
        to_banner2.as_secs_f32()
    );
    let mut second_interim: Option<Duration> = None;
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        h.update(Duration::from_millis(300));
        if h.screen_contents().matches("fellow").count() >= 2 {
            second_interim.get_or_insert(t2.elapsed());
            break;
        }
    }
    println!(
        "second dictation: first interim visible {:.2}s after the chord",
        second_interim.map(|d| d.as_secs_f32()).unwrap_or(f32::NAN)
    );
    std::thread::sleep(Duration::from_secs(8));
    h.inject_keys(b"\x1b").unwrap();
    h.update(Duration::from_secs(4));
    snapshot(&h, &evidence, "05-second-dictation-appended");
    h.inject_keys(b"\x1b").unwrap();
    h.update(Duration::from_millis(300));
    println!("evidence in {}", evidence.display());
}
