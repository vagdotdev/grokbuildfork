//! First-user hardening: things a new user's machine already has, or a slow first minute, must
//! not turn the first message into homework or a frozen line.
//!
//! Opt-in: set `WORKSHOP_BIN` to the built binary and run with `--include-ignored`.

mod pty_common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use pty_common::*;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

#[cfg(unix)]
fn write_exec(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A healthy fake `opencode` (identifies as 1.18.31, `serve` answers health checks) in `bin`.
#[cfg(unix)]
fn install_fake_opencode(bin: &Path) {
    std::fs::create_dir_all(bin).unwrap();
    std::fs::copy(
        fixtures().join("fake-opencode-serve.py"),
        bin.join("fake-opencode-serve.py"),
    )
    .unwrap();
    std::fs::write(bin.join("mode"), "silent").unwrap();
    write_exec(
        &bin.join("opencode"),
        &std::fs::read_to_string(fixtures().join("fake-opencode.sh")).unwrap(),
    );
}

/// Poll the engine log until a `start:` line shows which binary `serve` was launched from.
fn wait_for_serve_start(j: &mut Journey, secs: u64) -> String {
    let log = j.workshop_home().join("logs").join("opencode-engine.log");
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Ok(text) = std::fs::read_to_string(&log)
            && let Some(line) = text.lines().find(|l| l.contains("start:"))
        {
            return line.to_owned();
        }
        assert!(
            Instant::now() < deadline,
            "no `serve` start within {secs}s; log: {:?}\nscreen:\n{}",
            std::fs::read_to_string(&log).ok(),
            j.h.screen_contents()
        );
        j.h.update(Duration::from_millis(200));
    }
}

fn has_byte_count(screen: &str) -> Option<String> {
    screen
        .lines()
        .find(|l| {
            let words: Vec<&str> = l.split_whitespace().collect();
            words.windows(2).any(|w| {
                (w[1].starts_with("KB") || w[1].starts_with("MB"))
                    && w[0].parse::<f64>().is_ok_and(|n| n > 0.0)
            })
        })
        .map(|l| l.trim().to_owned())
}

#[test]
#[cfg(unix)]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); run with --include-ignored"]
fn a_stale_opencode_on_path_yields_to_the_managed_copy() {
    let Some(bin) = bin_from_env() else { return };
    // A Homebrew-style opencode a few releases behind the supported pin, first on PATH.
    let stale = tempfile::tempdir().unwrap();
    write_exec(
        &stale.path().join("opencode"),
        "#!/bin/sh\ncase \"$1\" in --version) echo 1.17.0;; *) echo 'opencode run [message..]' >&2;; esac\n",
    );
    let home = tempfile::tempdir().unwrap();
    let managed = home.path().join(".workshop/tools/opencode/.opencode/bin");
    install_fake_opencode(&managed);
    let mut j = spawn_in("stale-opencode", &bin, &[], Some(stale.path()), home);
    connect_big_pickle(&mut j);
    send_prompt(&mut j, "hello");
    let start = wait_for_serve_start(&mut j, 30);
    snapshot(&j.h, &j.dir, "managed-copy-used");
    assert!(
        start.contains(&managed.join("opencode").display().to_string()),
        "the managed copy serves, not the stale one: {start}"
    );
    let screen = j.h.screen_contents();
    assert!(
        !screen.contains("older than") && !screen.contains("update it"),
        "no update homework on the first message:\n{screen}"
    );
}

#[test]
#[cfg(unix)]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); run with --include-ignored"]
fn the_first_install_shows_bytes_climbing_while_the_archive_arrives() {
    let Some(bin) = bin_from_env() else { return };
    // No opencode anywhere; `curl … | bash` gets a stand-in vendor script that downloads the
    // archive into $TMPDIR in slow chunks (like a real link), then installs a healthy fake.
    let fake = tempfile::tempdir().unwrap();
    let fix = fixtures();
    let script = format!(
        "d=\"${{TMPDIR:-/tmp}}/opencode_install_$$\"; mkdir -p \"$d\"\n\
         for i in 1 2 3 4 5 6 7 8 9 10; do head -c 300000 /dev/zero >> \"$d/opencode-linux-x64.tar.gz\"; sleep 0.6; done\n\
         b=\"$HOME/.opencode/bin\"; mkdir -p \"$b\"; cp {fix}/fake-opencode.sh \"$b/opencode\"; chmod 755 \"$b/opencode\"\n\
         cp {fix}/fake-opencode-serve.py \"$b/\"; echo silent > \"$b/mode\"; rm -rf \"$d\"",
        fix = fix.display()
    );
    write_exec(
        &fake.path().join("curl"),
        &format!("#!/bin/sh\ncat <<'SCRIPT'\n{script}\nSCRIPT\n"),
    );
    let mut j = spawn("install-progress", &bin, &[], Some(fake.path()));
    connect_big_pickle(&mut j);
    send_prompt(&mut j, "hello");
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut counts = Vec::new();
    while Instant::now() < deadline && counts.len() < 3 {
        if let Some(line) = has_byte_count(&j.h.screen_contents())
            && counts.last() != Some(&line)
        {
            counts.push(line);
        }
        j.h.update(Duration::from_millis(150));
    }
    snapshot(&j.h, &j.dir, "install-progress");
    assert!(
        counts.len() >= 2,
        "the waiting line shows a byte count that changes while the archive arrives: {counts:?}\n{}",
        j.h.screen_contents()
    );
    let start = wait_for_serve_start(&mut j, 30);
    assert!(
        start.contains(".workshop/tools/opencode/.opencode/bin/opencode"),
        "{start}"
    );
}
