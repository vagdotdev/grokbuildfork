//! The silent updater through the real TUI against a loopback release channel (hermetic; opt-in via
//! `WORKSHOP_BIN`, run with `--include-ignored`):
//!
//! * `older_channel_version_is_not_installed` — a channel that has fallen behind the running
//!   version (the 0.2.2 → 0.2.1 case) installs nothing and writes nothing.
//! * `newer_channel_version_installs_silently_and_keeps_always_approve` — a newer version is
//!   downloaded, verified and linked in the background while the composer stays usable (upstream's
//!   one-line restart tip is all that shows); the config gains only `[cli] installer`, never a `[ui]`
//!   key; the next launch still opens in always-approve.
//! * `legacy_yolo_false_is_not_a_mode_choice` — a config that carries the bare `yolo = false` a
//!   0.2.2 update left behind still opens in always-approve; an explicit `permission_mode = "ask"`
//!   still opens in the asking mode.
//!
//! A debug build runs the background updater only against a loopback `WORKSHOP_CLI_BASE_URL` (the
//! channel each gate stands up), so these gates drive the real update path of the CI binary.
//! Evidence (text + HTML screenshots) lands in `WORKSHOP_PTY_EVIDENCE_DIR/updater-*`.

#![cfg(unix)]

mod pty_common;

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::time::Duration;

use pty_common::{Journey, KillOnDrop, bin_from_env, fake_opencode, snapshot, spawn_in, wait_for};

const LABEL: &str = "Big Pickle";
const ALWAYS_APPROVE: &str = "Big Pickle \u{b7} always-approve";

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

/// A release version strictly behind the binary under test (`workshop 0.2.1-dev (…)` → `0.2.0`),
/// the way a channel that has not caught up with a release is. A build that names no release
/// (`0.0.0-dev`: no `v*` tag in its checkout, no `WORKSHOP_VERSION`) has nothing behind it, and
/// the gate says so rather than testing an update that is legitimately newer.
#[allow(clippy::disallowed_methods)] // one `--version` of the binary under test
fn version_behind(bin: &Path) -> String {
    let out = std::process::Command::new(bin)
        .arg("--version")
        .output()
        .expect("workshop --version");
    let text = String::from_utf8_lossy(&out.stdout);
    let version = text
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .split(['-', '+'])
        .next()
        .unwrap_or_default();
    let mut parts: Vec<u64> = version.split('.').filter_map(|p| p.parse().ok()).collect();
    assert_eq!(
        parts.len(),
        3,
        "`workshop --version` names a semver: {text:?}"
    );
    assert!(
        parts.iter().any(|p| *p > 0),
        "the binary under test is `{version}`: build it from a checkout with its `v*` tags (or with WORKSHOP_VERSION set) so a channel can be behind it"
    );
    for i in (0..3).rev() {
        if parts[i] > 0 {
            parts[i] -= 1;
            for later in parts.iter_mut().skip(i + 1) {
                *later = 0;
            }
            break;
        }
    }
    format!("{}.{}.{}", parts[0], parts[1], parts[2])
}

/// A loopback release channel: `stable.json` pointing at one `tar.gz` (a `workshop` stub that
/// answers `--version`) served from the same base, every request logged.
struct Channel {
    _dir: tempfile::TempDir,
    _server: KillOnDrop,
    base_url: String,
    log: PathBuf,
}

impl Channel {
    fn requests(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

#[allow(clippy::disallowed_methods)] // short-lived loopback fixture, killed by KillOnDrop
fn serve_channel(version: &str) -> Channel {
    let dir = tempfile::tempdir().expect("channel dir");
    let log = dir.path().join("requests.log");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-voice-mirror.py");
    let mut child = std::process::Command::new("python3")
        .arg(script)
        .arg("--dir")
        .arg(dir.path())
        .arg("--log")
        .arg(&log)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn fake-voice-mirror.py");
    let mut base_url = String::new();
    std::io::BufReader::new(child.stdout.take().expect("stdout"))
        .read_line(&mut base_url)
        .expect("server prints its url");
    let base_url = base_url.trim().trim_end_matches('/').to_owned();

    // The release: a `workshop` that identifies itself, packed the way the release scripts do.
    let stage = dir.path().join("stage");
    std::fs::create_dir_all(&stage).unwrap();
    std::fs::write(
        stage.join("workshop"),
        format!(
            "#!/bin/sh\ncase \"$1\" in --version) echo 'Workshop - v{version}'; exit 0;; esac\necho 'stub workshop {version}: not a real build' >&2\nexit 2\n"
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            stage.join("workshop"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let asset = format!("workshop-{version}-{}.tar.gz", platform());
    let status = std::process::Command::new("tar")
        .args(["-czf"])
        .arg(dir.path().join(&asset))
        .args(["-C"])
        .arg(&stage)
        .arg("workshop")
        .status()
        .expect("tar");
    assert!(status.success(), "tar packs the release");
    let archive = std::fs::read(dir.path().join(&asset)).unwrap();
    let manifest = serde_json::json!({
        "schema_version": 1,
        "product": "workshop",
        "channel": "stable",
        "version": version,
        "tag": format!("v{version}"),
        "published_at": "2026-09-23T00:00:00Z",
        "release_repo": "example/workshop",
        "release_url": format!("{base_url}/release"),
        "checksums_url": format!("{base_url}/SHA256SUMS"),
        "attested": false,
        "artifacts": {
            platform(): {
                "url": format!("{base_url}/{asset}"),
                "sha256": sha256_hex(&archive),
                "size": archive.len(),
                "format": "tar.gz",
                "binary": "workshop",
            }
        }
    });
    std::fs::write(dir.path().join("stable.json"), manifest.to_string()).unwrap();
    std::fs::write(
        dir.path().join("SHA256SUMS"),
        format!("{}  {asset}\n", sha256_hex(&archive)),
    )
    .unwrap();
    Channel {
        _dir: dir,
        _server: KillOnDrop(child),
        base_url,
        log,
    }
}

/// Spawn the TUI with the updater on and pointed at `channel` (loopback only; every other host is
/// a closed proxy port). The crash-mode fake `opencode` keeps the launch warm-up hermetic.
fn spawn_with_channel(
    journey: &str,
    bin: &Path,
    channel: &Channel,
    fake: &Path,
    home: tempfile::TempDir,
) -> Journey {
    spawn_in(
        journey,
        bin,
        &[
            ("GROK_DISABLE_AUTOUPDATER", ""),
            ("WORKSHOP_CLI_BASE_URL", channel.base_url.as_str()),
            ("WORKSHOP_INSTALLER", "internal"),
            ("HTTP_PROXY", "http://127.0.0.1:9"),
            ("HTTPS_PROXY", "http://127.0.0.1:9"),
            ("ALL_PROXY", "http://127.0.0.1:9"),
            ("NO_PROXY", "127.0.0.1,localhost"),
            ("no_proxy", "127.0.0.1,localhost"),
        ],
        Some(fake),
        home,
    )
}

fn unified_log(j: &Journey) -> String {
    std::fs::read_to_string(j.workshop_home().join("logs").join("unified.jsonl"))
        .unwrap_or_default()
}

fn config_toml(j: &Journey) -> String {
    std::fs::read_to_string(j.workshop_home().join("config.toml")).unwrap_or_default()
}

fn quit(j: &mut Journey) {
    j.h.inject_keys(b"\x03").unwrap();
    j.h.update(Duration::from_millis(400));
    j.h.inject_keys(b"\x03").unwrap();
    let _ = j.h.wait_exit_code(Duration::from_secs(10));
}

/// A channel behind the running version: checked, then left alone.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (loopback channel); run with --include-ignored"]
fn older_channel_version_is_not_installed() {
    let Some(bin) = bin_from_env() else { return };
    let behind = version_behind(&bin);
    let channel = serve_channel(&behind);
    let fake = fake_opencode("crash");
    let mut j = spawn_with_channel(
        "updater-older",
        &bin,
        &channel,
        fake.path(),
        tempfile::tempdir().unwrap(),
    );
    wait_for(&mut j.h, LABEL, 45);
    // The check reaches the channel, and only the channel.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !channel.requests().iter().any(|r| r.contains("stable.json")) {
        assert!(
            std::time::Instant::now() < deadline,
            "the updater never read the channel: {:?}",
            channel.requests()
        );
        j.h.update(Duration::from_millis(200));
    }
    j.h.update(Duration::from_millis(4000));
    snapshot(&j.h, &j.dir, "01-composer-older-channel");
    let requests = channel.requests();
    assert!(
        !requests.iter().any(|r| r.contains("tar.gz")),
        "an older version ({behind}) is never downloaded: {requests:?}"
    );
    let log = unified_log(&j);
    assert!(
        !log.contains("update.download_started") && !log.contains("update.installed"),
        "no download, no install:\n{log}"
    );
    assert!(
        !j.workshop_home().join("bin").join("workshop").exists(),
        "nothing was linked into bin/"
    );
    let config = config_toml(&j);
    assert!(
        !config.contains("[cli]") && !config.contains("yolo"),
        "a check that installs nothing writes nothing:\n{config}"
    );
    assert!(
        j.h.screen_contents().contains(ALWAYS_APPROVE),
        "still always-approve:\n{}",
        j.h.screen_contents()
    );
    quit(&mut j);
}

/// A newer version installs in the background — nothing on screen, only the installer marker in the
/// config — and the next launch still opens in always-approve.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (loopback channel); run with --include-ignored"]
fn newer_channel_version_installs_silently_and_keeps_always_approve() {
    let Some(bin) = bin_from_env() else { return };
    let channel = serve_channel("9.9.9");
    let fake = fake_opencode("crash");
    let mut j = spawn_with_channel(
        "updater-newer",
        &bin,
        &channel,
        fake.path(),
        tempfile::tempdir().unwrap(),
    );
    wait_for(&mut j.h, ALWAYS_APPROVE, 45);
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    while !unified_log(&j).contains("update.installed") {
        assert!(
            std::time::Instant::now() < deadline,
            "the newer version was never installed; channel saw {:?}\nlog:\n{}",
            channel.requests(),
            unified_log(&j)
        );
        j.h.update(Duration::from_millis(300));
    }
    j.h.update(Duration::from_millis(1500));
    snapshot(&j.h, &j.dir, "01-composer-while-updating");
    let screen = j.h.screen_contents();
    assert!(
        screen.contains(ALWAYS_APPROVE)
            && !screen.contains("Downloading")
            && !screen.contains("Installing"),
        "the update never blocks the composer or shows progress (upstream's one-line restart tip is all):\n{screen}"
    );
    let link = j.workshop_home().join("bin").join("workshop");
    let target = std::fs::read_link(&link).expect("bin/workshop links at the new version");
    assert!(
        target.to_string_lossy().contains("9.9.9"),
        "bin/workshop -> {}",
        target.display()
    );
    let config = config_toml(&j);
    assert!(
        config.contains("[cli]") && config.contains("installer = \"internal\""),
        "the install records its installer:\n{config}"
    );
    assert!(
        !config.contains("yolo") && !config.contains("[ui]"),
        "the install writes nothing but its marker:\n{config}"
    );
    quit(&mut j);

    // The next launch: the mode is still the default (not the updater's pick).
    let home = j.home;
    let channel_again = serve_channel("9.9.9");
    let mut j = spawn_with_channel("updater-newer", &bin, &channel_again, fake.path(), home);
    wait_for(&mut j.h, LABEL, 45);
    j.h.update(Duration::from_millis(1500));
    snapshot(&j.h, &j.dir, "02-next-launch-still-always-approve");
    let screen = j.h.screen_contents();
    assert!(
        screen.contains(ALWAYS_APPROVE),
        "always-approve survives a silent update:\n{screen}"
    );
    quit(&mut j);
}

/// The key a 0.2.2 update left behind (`[ui] yolo = false`) is not a mode choice; a real pick is.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic; run with --include-ignored"]
fn legacy_yolo_false_is_not_a_mode_choice() {
    let Some(bin) = bin_from_env() else { return };
    let fake = fake_opencode("crash");
    let offline: &[(&str, &str)] = &[
        ("HTTP_PROXY", "http://127.0.0.1:9"),
        ("HTTPS_PROXY", "http://127.0.0.1:9"),
        ("ALL_PROXY", "http://127.0.0.1:9"),
    ];
    // 1. A first run, as any 0.2.2 home began.
    let mut j = spawn_in(
        "updater-legacy-yolo",
        &bin,
        offline,
        Some(fake.path()),
        tempfile::tempdir().unwrap(),
    );
    wait_for(&mut j.h, ALWAYS_APPROVE, 45);
    quit(&mut j);
    let home = j.home;
    let config_path = home.path().join(".workshop").join("config.toml");

    // 2. What the 0.2.2 updater's whole-config save left behind: `[ui]` defaults, `yolo = false`.
    let mut config = std::fs::read_to_string(&config_path).unwrap_or_default();
    assert!(
        !config.contains("[ui]"),
        "a fresh home carries no [ui] table:\n{config}"
    );
    config.push_str("\n[ui]\nmax_thoughts_width = 120\nyolo = false\ncompact_mode = false\n");
    std::fs::write(&config_path, &config).unwrap();
    let mut j = spawn_in(
        "updater-legacy-yolo",
        &bin,
        offline,
        Some(fake.path()),
        home,
    );
    wait_for(&mut j.h, LABEL, 45);
    j.h.update(Duration::from_millis(1200));
    snapshot(&j.h, &j.dir, "01-yolo-false-still-always-approve");
    let screen = j.h.screen_contents();
    assert!(
        screen.contains(ALWAYS_APPROVE),
        "a bare `yolo = false` is not a choice:\n{screen}"
    );
    quit(&mut j);

    // 3. An explicit pick sticks.
    let home = j.home;
    let mut config = std::fs::read_to_string(&config_path).unwrap_or_default();
    config = config.replace(
        "yolo = false\n",
        "yolo = false\npermission_mode = \"ask\"\n",
    );
    std::fs::write(&config_path, &config).unwrap();
    let mut j = spawn_in(
        "updater-legacy-yolo",
        &bin,
        offline,
        Some(fake.path()),
        home,
    );
    wait_for(&mut j.h, LABEL, 45);
    j.h.update(Duration::from_millis(1200));
    snapshot(&j.h, &j.dir, "02-explicit-ask-sticks");
    let screen = j.h.screen_contents();
    assert!(
        !screen.contains("always-approve"),
        "an explicit `permission_mode = \"ask\"` opens in the asking mode:\n{screen}"
    );
    quit(&mut j);
}
