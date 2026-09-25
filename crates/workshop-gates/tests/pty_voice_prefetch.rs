//! Voice gets ready in the background. A default install ships `workshop` alone; the TUI fetches
//! the `voice-engine` helper and this machine's speech model itself, from the release mirror only:
//!
//! * First run: nothing is fetched before the first reply; once it lands, the helper archive is
//!   verified against the release's `SHA256SUMS` and linked as `~/.workshop/bin/voice-engine`, then
//!   the model downloads into `~/.workshop/voice/`. `/voice` meanwhile says `Voice is getting ready
//!   — 62%` instead of failing. The mirror saw exactly those three files.
//! * `WORKSHOP_VOICE_AUTO=0`: nothing is fetched on its own; a `/voice` press still asks for it.
//!
//! Hermetic: a loopback mirror (`fixtures/fake-voice-mirror.py`) with a test lock file pinning a
//! small model; the answering fake `opencode` on loopback. Opt-in via `WORKSHOP_BIN`,
//! `--include-ignored`.

mod pty_common;

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::time::Duration;

use pty_common::*;

const MODEL_FILE: &str = "ggml-tiny-test.bin";
const HELPER_VERSION: &str = "0.0.0-test";

/// The platform string this build's prefetch asks the mirror for.
fn platform() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "macos-aarch64",
        ("macos", "x86_64") => "macos-x86_64",
        ("linux", "aarch64") => "linux-aarch64",
        _ => "linux-x86_64",
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A release mirror's directory: `SHA256SUMS`, the helper archive (a shell script that answers
/// `--version`), a 2 MiB "model" and the lock file that pins it for every tier.
struct Mirror {
    dir: tempfile::TempDir,
    lock: PathBuf,
    log: PathBuf,
    helper_asset: String,
}

fn build_mirror() -> Mirror {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper_asset = format!("voice-engine-{HELPER_VERSION}-{}.tar.gz", platform());
    // The helper: one executable member, packed with the system tar.
    let stage = dir.path().join("stage");
    std::fs::create_dir_all(&stage).unwrap();
    std::fs::write(
        stage.join("voice-engine"),
        format!("#!/bin/sh\ncase \"$1\" in --version) echo 'voice-engine {HELPER_VERSION}'; exit 0;; esac\nexit 2\n"),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            stage.join("voice-engine"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let status = std::process::Command::new("tar")
        .args(["-czf"])
        .arg(dir.path().join(&helper_asset))
        .args(["-C"])
        .arg(&stage)
        .arg("voice-engine")
        .status()
        .expect("tar");
    assert!(status.success(), "tar packs the helper");
    let archive = std::fs::read(dir.path().join(&helper_asset)).unwrap();
    // The model: 2 MiB of deterministic bytes, pinned by size and SHA-256 in the test lock.
    let model: Vec<u8> = (0..2 * 1024 * 1024u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
        .collect();
    std::fs::write(dir.path().join(MODEL_FILE), &model).unwrap();
    std::fs::write(
        dir.path().join("SHA256SUMS"),
        format!(
            "{}  workshop-{HELPER_VERSION}-{}.tar.gz\n{}  {helper_asset}\n",
            "0".repeat(64),
            platform(),
            sha256_hex(&archive)
        ),
    )
    .unwrap();
    let pin = serde_json::json!({
        "name": "Test model",
        "file": MODEL_FILE,
        "size": model.len(),
        "sha256": sha256_hex(&model),
        "upstream_url": "https://example.invalid/never-used"
    });
    let lock = serde_json::json!({
        "schema_version": 2,
        "tiers": ["turbo", "small", "base"],
        "models": { "turbo": pin, "small": pin, "base": pin },
        "mirror_url_template": "https://example.invalid/{release_repo}/v{version}/{file}",
        "selection": {
            "interim_budget_ms": 1000,
            "min_ram_bytes_for_probe": 3221225472u64,
            "probe_ratio_small_over_base": 3.9,
            "probe_ratio_turbo_over_base": 19.5
        },
        "engine": {
            "name": "voice-engine",
            "whisper_cpp_version": "test",
            "whisper_cpp_git": "test",
            "platforms": ["macos-aarch64", "macos-x86_64", "linux-x86_64", "linux-aarch64"]
        },
        "license": "MIT"
    });
    let lock_path = dir.path().join("MODEL.lock.json");
    std::fs::write(&lock_path, serde_json::to_vec_pretty(&lock).unwrap()).unwrap();
    let log = dir.path().join("requests.log");
    std::fs::write(&log, "").unwrap();
    Mirror {
        dir,
        lock: lock_path,
        log,
        helper_asset,
    }
}

/// Start the loopback mirror (killed on drop); returns it and its base URL.
#[allow(clippy::disallowed_methods)] // short-lived loopback fixture, killed by KillOnDrop
fn serve(mirror: &Mirror) -> (KillOnDrop, String) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-voice-mirror.py");
    let mut child = std::process::Command::new("python3")
        .arg(script)
        .arg("--dir")
        .arg(mirror.dir.path())
        .arg("--log")
        .arg(&mirror.log)
        .arg("--slow-suffix")
        .arg(".bin")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn fake-voice-mirror.py");
    let mut url = String::new();
    std::io::BufReader::new(child.stdout.take().expect("stdout"))
        .read_line(&mut url)
        .expect("mirror prints its url");
    (KillOnDrop(child), url.trim().to_owned())
}

fn requests(mirror: &Mirror) -> Vec<String> {
    std::fs::read_to_string(&mirror.log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn wait_until(what: &str, secs: u64, j: &mut Journey, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    while !done() {
        assert!(
            std::time::Instant::now() < deadline,
            "{what} did not happen within {secs}s:\n{}",
            j.h.screen_contents()
        );
        j.h.update(Duration::from_millis(100));
    }
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (loopback mirror + fake opencode); run with --include-ignored"]
fn voice_gets_ready_in_the_background_after_the_first_reply() {
    let Some(bin) = bin_from_env() else { return };
    let mirror = build_mirror();
    let (_server, base) = serve(&mirror);
    let recorder = tempfile::tempdir().expect("tempdir");
    let fake = fake_opencode_answering(&recorder.path().join("prompts.jsonl"));
    let lock = mirror.lock.to_string_lossy().to_string();
    let mut j = spawn(
        "voice-prefetch",
        &bin,
        &[
            ("WORKSHOP_VOICE_MIRROR_BASE", base.as_str()),
            ("WORKSHOP_VOICE_LOCK", lock.as_str()),
        ],
        Some(fake.path()),
    );
    connect_big_pickle(&mut j);
    let helper = j.workshop_home().join("bin").join("voice-engine");
    let model = j.workshop_home().join("voice").join(MODEL_FILE);

    // 1. Nothing is fetched before the user's first message has been answered.
    j.h.update(Duration::from_millis(1500));
    assert!(
        requests(&mirror).is_empty(),
        "no fetch before the first reply"
    );
    assert!(!helper.exists());

    // 2. The first reply lands; the helper arrives (SHA256SUMS, then the archive), then the model
    //    starts. `/voice` mid-download says how far along it is instead of failing.
    send_prompt(&mut j, "hello");
    wait_for(&mut j.h, "Created hello.txt with the exact line.", 90);
    wait_until("the helper link", 30, &mut j, || helper.exists());
    #[cfg(unix)]
    assert_eq!(
        std::fs::read_link(&helper).unwrap(),
        PathBuf::from("../downloads").join(mirror.helper_asset.trim_end_matches(".tar.gz")),
        "the installer's layout: bin/voice-engine -> ../downloads/<versioned>"
    );
    send_prompt(&mut j, "/voice");
    wait_for(&mut j.h, "Voice is getting ready", 10);
    let screen = j.h.screen_contents();
    let line = screen
        .lines()
        .find(|l| l.contains("Voice is getting ready"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    snapshot(&j.h, &j.dir, "01-voice-getting-ready");
    assert!(
        line.contains('%') || line.ends_with('\u{2026}'),
        "the press reads the progress, never an error: {line:?}"
    );
    assert_no_plumbing(&j.h, "while voice gets ready");

    // 3. The model lands, verified; the mirror saw exactly the three files and nothing else.
    wait_until("the model file", 60, &mut j, || model.exists());
    j.h.update(Duration::from_millis(500));
    let seen = requests(&mirror);
    assert_eq!(
        seen,
        vec![
            "/SHA256SUMS".to_owned(),
            format!("/{}", mirror.helper_asset),
            format!("/{MODEL_FILE}"),
        ],
        "the setup fetched the helper's digest, the helper and the model — nothing else"
    );
    assert!(
        !j.workshop_home()
            .join("voice")
            .join(format!("{MODEL_FILE}.partial"))
            .exists(),
        "the finished download is renamed into place"
    );
    snapshot(&j.h, &j.dir, "02-voice-ready");
    quit_twice(&mut j);
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (loopback mirror + fake opencode); run with --include-ignored"]
fn opt_out_fetches_nothing_on_its_own_but_a_voice_press_still_asks() {
    let Some(bin) = bin_from_env() else { return };
    let mirror = build_mirror();
    let (_server, base) = serve(&mirror);
    let recorder = tempfile::tempdir().expect("tempdir");
    let fake = fake_opencode_answering(&recorder.path().join("prompts.jsonl"));
    let lock = mirror.lock.to_string_lossy().to_string();
    let mut j = spawn(
        "voice-prefetch-off",
        &bin,
        &[
            ("WORKSHOP_VOICE_MIRROR_BASE", base.as_str()),
            ("WORKSHOP_VOICE_LOCK", lock.as_str()),
            ("WORKSHOP_VOICE_AUTO", "0"),
        ],
        Some(fake.path()),
    );
    connect_big_pickle(&mut j);
    send_prompt(&mut j, "hello");
    wait_for(&mut j.h, "Created hello.txt with the exact line.", 90);
    j.h.update(Duration::from_millis(4000));
    assert!(
        requests(&mirror).is_empty(),
        "turned off: nothing is fetched on its own"
    );
    let helper = j.workshop_home().join("bin").join("voice-engine");
    assert!(!helper.exists());

    // The user asks for voice: that is a request, so the setup runs now.
    send_prompt(&mut j, "/voice");
    wait_for(&mut j.h, "Voice is getting ready", 10);
    wait_until("the helper link", 30, &mut j, || helper.exists());
    snapshot(&j.h, &j.dir, "01-voice-press-starts-setup");
    quit_twice(&mut j);
}

/// The fake microphone and helper for a dictation gate: recorders under the names Linux capture
/// looks for on `PATH` (silence, real-time paced) and a `voice-engine` stand-in that speaks the
/// helper protocol and returns a canned transcript. Returns the engine path for
/// `WORKSHOP_VOICE_ENGINE`.
fn install_fake_voice_stack(bin: &Path) -> PathBuf {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut installed = Vec::new();
    for name in ["pw-record", "parec", "arecord"] {
        let dst = bin.join(name);
        std::fs::copy(fixtures.join("fake-recorder.py"), &dst).expect("copy fake recorder");
        installed.push(dst);
    }
    let engine = bin.join("voice-engine");
    std::fs::copy(fixtures.join("fake-voice-engine.py"), &engine).expect("copy fake engine");
    installed.push(engine.clone());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in &installed {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    engine
}

/// `// start · // stop`, no hold: on an empty composer the second `/` starts dictation and the
/// composer shows the recording state; `//` again stops it and the transcript is in the composer.
/// A `/` typed inside other text — `https://` — is just typed and starts nothing. The `/voice`
/// row advertises the shortcut. Voice is set up before the run (helper via
/// `WORKSHOP_VOICE_ENGINE`, the pinned model already in `~/.workshop/voice`): the state a machine
/// is in once the background setup has finished.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (loopback mirror, fake opencode, fake microphone and voice helper); run with --include-ignored"]
fn double_slash_starts_and_stops_dictation_and_a_url_does_not() {
    let Some(bin) = bin_from_env() else { return };
    if !cfg!(target_os = "linux") {
        eprintln!("the fake microphone is a Linux recorder on PATH; skipping");
        return;
    }
    let mirror = build_mirror();
    let (_server, base) = serve(&mirror);
    let recorder = tempfile::tempdir().expect("tempdir");
    let fake = fake_opencode_answering(&recorder.path().join("prompts.jsonl"));
    let engine = install_fake_voice_stack(fake.path());
    let engine_s = engine.to_string_lossy().to_string();
    let lock = mirror.lock.to_string_lossy().to_string();
    let home = tempfile::tempdir().expect("tempdir");
    let voice_dir = home.path().join(".workshop").join("voice");
    std::fs::create_dir_all(&voice_dir).unwrap();
    std::fs::copy(
        mirror.dir.path().join(MODEL_FILE),
        voice_dir.join(MODEL_FILE),
    )
    .unwrap();
    let mut j = spawn_in(
        "voice-double-slash",
        &bin,
        &[
            ("WORKSHOP_VOICE_MIRROR_BASE", base.as_str()),
            ("WORKSHOP_VOICE_LOCK", lock.as_str()),
            ("WORKSHOP_VOICE_AUTO", "0"),
            ("WORKSHOP_VOICE_ENGINE", engine_s.as_str()),
            ("FAKE_VOICE_TRANSCRIPT", "hello from the fake microphone"),
        ],
        Some(fake.path()),
        home,
    );
    connect_big_pickle(&mut j);
    send_prompt(&mut j, "hello");
    wait_for(&mut j.h, "Created hello.txt with the exact line.", 90);
    j.h.update(Duration::from_millis(1000));
    assert!(
        requests(&mirror).is_empty(),
        "nothing to fetch: voice is set up"
    );

    // 1. A URL: its second slash is typed like any character; no voice, no setup.
    j.h.inject_keys(b"https://").unwrap();
    j.h.update(Duration::from_millis(1000));
    let screen = j.h.screen_contents();
    assert!(
        screen.contains("https://"),
        "the URL stays in the composer:\n{screen}"
    );
    assert!(
        !screen.contains("Recording") && !screen.contains("Voice is getting ready"),
        "a `/` inside text must not run /voice:\n{screen}"
    );
    assert!(
        requests(&mirror).is_empty(),
        "a `/` inside text must not start the voice setup"
    );
    snapshot(&j.h, &j.dir, "01-url-typed-as-usual");
    for _ in 0.."https://".len() {
        j.h.inject_keys(b"\x7f").unwrap();
    }
    j.h.update(Duration::from_millis(300));
    assert!(
        !j.h.screen_contents().contains("https://"),
        "the composer is empty again:\n{}",
        j.h.screen_contents()
    );

    // 2. The slash menu shows the shortcut next to the command.
    j.h.inject_keys(b"/voi").unwrap();
    wait_for(&mut j.h, "/voice", 5);
    let screen = j.h.screen_contents();
    assert!(
        screen.contains("/voice [// start \u{b7} // stop]"),
        "the /voice row carries the `// start \u{b7} // stop` hint:\n{screen}"
    );
    snapshot(&j.h, &j.dir, "02-voice-row-hint");
    for _ in 0..4 {
        j.h.inject_keys(b"\x7f").unwrap();
    }
    j.h.update(Duration::from_millis(300));

    // 3. `/` then `/` on the empty composer: dictation starts. The row reads "Recording" once the
    //    model is verified and the helper is up; `//` never lands in the composer.
    j.h.inject_keys(b"/").unwrap();
    wait_for(&mut j.h, "/model", 5);
    j.h.inject_keys(b"/").unwrap();
    wait_for(&mut j.h, "Recording", 60);
    let screen = j.h.screen_contents();
    assert!(
        screen.contains("[stop]"),
        "the recording row carries its stop control:\n{screen}"
    );
    assert!(
        !screen.lines().any(|l| l.contains("\u{276f} //")),
        "`//` must not stay in the composer:\n{screen}"
    );
    assert_no_plumbing(&j.h, "while recording");
    // The live preview: the helper's partial shows in the composer while the mic is open.
    wait_for(&mut j.h, "hello from the", 20);
    snapshot(&j.h, &j.dir, "03-double-slash-recording");

    // 4. `//` again: recording stops and the transcript is in the composer, ready to edit or send.
    j.h.inject_keys(b"/").unwrap();
    j.h.update(Duration::from_millis(300));
    j.h.inject_keys(b"/").unwrap();
    wait_for(&mut j.h, "hello from the fake microphone", 20);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while j.h.screen_contents().contains("Recording") {
        assert!(
            std::time::Instant::now() < deadline,
            "the recording row must go away after `//`:\n{}",
            j.h.screen_contents()
        );
        j.h.update(Duration::from_millis(100));
    }
    let screen = j.h.screen_contents();
    let composer = screen
        .lines()
        .find(|l| l.contains("hello from the fake microphone"))
        .unwrap_or_default()
        .to_owned();
    assert!(
        composer.contains('\u{276f}') && !composer.contains("//"),
        "the transcript sits in the composer with no slashes:\n{screen}"
    );
    assert!(
        requests(&mirror).is_empty(),
        "the helper and model were in place; the mirror saw nothing: {:?}",
        requests(&mirror)
    );
    snapshot(&j.h, &j.dir, "04-double-slash-stopped-transcript");
    quit_twice(&mut j);
}

/// Ctrl+C twice ends the session.
fn quit_twice(j: &mut Journey) {
    j.h.inject_keys(b"\x03").unwrap();
    j.h.update(Duration::from_millis(300));
    j.h.inject_keys(b"\x03").unwrap();
    let _ = j.h.wait_exit_code(Duration::from_secs(10));
}
