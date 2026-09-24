//! Live model lists through the real TUI (hermetic, PTY-driven; opt-in via `WORKSHOP_BIN`):
//!
//! * `catalogs_are_fetched_only_after_the_user_acts` — the egress gate. Every request that
//!   honours `HTTP(S)_PROXY` lands on a logging proxy inside the test that refuses to forward.
//!   `workshop login` asks the proxy for nothing. A first run asks for exactly one host at launch
//!   — `opencode.ai`, the vendor's installer for the `opencode` CLI that is brought up in the
//!   background — and `/auth` (and its Models view via Tab) add nothing; on a fresh home, where
//!   no API-key provider is configured and so none is listed, `/model` adds nothing either. Once
//!   a key is configured (`NVIDIA_API_KEY`), that provider is listed and `/model` (and a
//!   returning launch, in the background) asks for exactly its host, never an xAI host; with the
//!   proxy refusing, its list is the dated seed with `refresh failed`. Kilo Gateway is never
//!   listed and never fetched.
//! * `model_lists_more_opencode_rows_than_the_seed_when_opencode_serve_is_up` — a fake `opencode`
//!   whose `serve` is a loopback HTTP/SSE stand-in (the adapter crate's captured fixtures). With
//!   no engine to start, `/model` shows the one pinned seed row, dated; with the fake on `PATH`
//!   the engine starts at launch and `/model` lists every free model it reports (8 in the
//!   fixture), dated `fetched just now`, before and after the first turn; the cache under
//!   `$WORKSHOP_HOME/catalog-cache/` holds the same list for the next launch.
//!
//! Evidence (text + HTML screenshots) lands in `WORKSHOP_PTY_EVIDENCE_DIR`
//! (default `target/pty-evidence/live-catalogs`).

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use xai_grok_pager_pty_harness::PtyHarness;

/// The composer label on a first run: the model name only, no provider.
const FIRST_RUN_LABEL: &str = "Big Pickle";
const SEED_NOTE: &str = "cached list from 2026-09-21";
/// The picker overlay is on screen (its search line starts with this glyph).
const PICKER: &str = "\u{2315}";

fn evidence_dir() -> PathBuf {
    let dir = std::env::var_os("WORKSHOP_PTY_EVIDENCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/pty-evidence"))
        .join("live-catalogs");
    std::fs::create_dir_all(&dir).expect("create evidence dir");
    dir
}

fn snapshot(h: &PtyHarness, dir: &Path, name: &str) {
    std::fs::write(dir.join(format!("{name}.txt")), h.screen_contents()).expect("write txt");
    std::fs::write(dir.join(format!("{name}.html")), h.screen_html()).expect("write html");
}

fn wait_for(h: &mut PtyHarness, text: &str, secs: u64) {
    if let Err(e) = h.wait_for_text(text, Duration::from_secs(secs)) {
        panic!(
            "timed out waiting for {text:?}: {e}\nscreen:\n{}",
            h.screen_contents()
        );
    }
}

fn wait_gone(h: &mut PtyHarness, text: &str, secs: u64) {
    if let Err(e) = h.wait_for_text_absent(text, Duration::from_secs(secs)) {
        panic!(
            "timed out waiting for {text:?} to disappear: {e}\nscreen:\n{}",
            h.screen_contents()
        );
    }
}

/// Type a slash command into the composer and submit it.
fn slash(h: &mut PtyHarness, cmd: &str) {
    h.inject_keys(cmd.as_bytes()).unwrap();
    h.update(Duration::from_millis(400));
    h.inject_keys(b"\r").unwrap();
}

/// The `›`-marked (highlighted) line of the screen.
fn selected_line(h: &PtyHarness) -> Option<String> {
    h.screen_contents()
        .lines()
        .find(|l| l.contains('\u{203a}') && !l.contains("Models \u{203a}"))
        .map(str::to_owned)
}

/// Press Down (then Up) until the highlighted line contains `needle`.
fn move_selection_to(h: &mut PtyHarness, needle: &str) {
    for key in [b"\x1b[B", b"\x1b[A"] {
        for _ in 0..60 {
            if selected_line(h).is_some_and(|l| l.contains(needle)) {
                return;
            }
            h.inject_keys(key).unwrap();
            h.update(Duration::from_millis(80));
        }
    }
    panic!(
        "never reached a highlighted line containing {needle:?}\nscreen:\n{}",
        h.screen_contents()
    );
}

/// Rows of the open picker under the `group` header (`OpenCode`, `Claude`): the row lines
/// between that header and the next one, inside the box border. The overlay floats over the
/// transcript, so only the text between the box's first and last `│` counts — whatever the
/// transcript shows to the left or right of the box (a timestamp, a tool row) is not the row.
/// Each row comes back as `name  state` with the `›` marker stripped.
fn overlay_rows_for(h: &PtyHarness, group: &str) -> Vec<String> {
    let inner: Vec<String> = h
        .screen_contents()
        .lines()
        .map(|l| match (l.find('\u{2502}'), l.rfind('\u{2502}')) {
            (Some(first), Some(last)) if last > first => {
                l[first + '\u{2502}'.len_utf8()..last].to_owned()
            }
            _ => l.to_owned(),
        })
        .collect();
    let mut rows = Vec::new();
    let mut in_group = false;
    for line in inner {
        let trimmed = line.trim();
        if trimmed == group {
            in_group = true;
            continue;
        }
        if !in_group {
            continue;
        }
        // The next header is an unmarked, unindented word; rows are indented by their marker.
        let is_row =
            line.starts_with("  ") || line.starts_with(" \u{203a}") || line.starts_with('\u{203a}');
        if !is_row || trimmed.starts_with('\u{2500}') || trimmed.is_empty() {
            break;
        }
        let row = trimmed.trim_start_matches('\u{203a}').trim();
        if ["free", "active", "key", "key needed", "\u{25b8}"]
            .iter()
            .any(|state| row.ends_with(state))
        {
            rows.push(row.to_owned());
        } else {
            break;
        }
    }
    rows
}

fn quit(mut h: PtyHarness) {
    // Close any open overlay first, then the two-step Ctrl+C quit; otherwise the first Ctrl+C only
    // closes the overlay and the process lingers on the home the next launch reuses.
    h.inject_keys(b"\x1b").unwrap();
    h.update(Duration::from_millis(300));
    h.inject_keys(b"\x03").unwrap();
    h.update(Duration::from_millis(400));
    h.inject_keys(b"\x03").unwrap();
    let _ = h.wait_exit_code(Duration::from_secs(10));
    let _ = h.quit();
}

fn bin_from_env() -> Option<PathBuf> {
    let b = std::env::var_os("WORKSHOP_BIN").map(PathBuf::from);
    if b.is_none() {
        eprintln!("WORKSHOP_BIN not set; skipping");
    }
    b
}

fn git_init(dir: &Path) {
    std::process::Command::new("git")
        .args(["init", "-q", "."])
        .current_dir(dir)
        .status()
        .expect("git init");
}

/// A logging HTTP proxy that records every `CONNECT host:port` / absolute-form target and refuses
/// to forward (502). Anything in the binary that honours `HTTP(S)_PROXY` shows up here by exact
/// hostname without a packet leaving the machine.
struct LogProxy {
    url: String,
    targets: Arc<Mutex<Vec<String>>>,
}

impl LogProxy {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind proxy");
        let url = format!("http://{}", listener.local_addr().unwrap());
        let targets = Arc::new(Mutex::new(Vec::new()));
        let seen = targets.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { break };
                let seen = seen.clone();
                std::thread::spawn(move || {
                    let _ = conn.set_read_timeout(Some(Duration::from_secs(5)));
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 65536 {
                        match conn.read(&mut chunk) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let head = String::from_utf8_lossy(&buf);
                    let first = head.lines().next().unwrap_or_default();
                    let mut parts = first.split(' ');
                    let (method, target) = (
                        parts.next().unwrap_or_default(),
                        parts.next().unwrap_or_default(),
                    );
                    let target = if method == "CONNECT" {
                        target.to_owned()
                    } else {
                        // absolute-form http request: the Host header names the peer.
                        head.lines()
                            .find_map(|l| l.strip_prefix("Host: ").or(l.strip_prefix("host: ")))
                            .map(|h| h.trim().to_owned())
                            .unwrap_or_else(|| target.to_owned())
                    };
                    seen.lock().unwrap().push(format!("{method} {target}"));
                    let _ = conn.write_all(
                        b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                });
            }
        });
        Self { url, targets }
    }

    fn env(&self) -> Vec<(&'static str, String)> {
        [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ]
        .into_iter()
        .map(|k| (k, self.url.clone()))
        .collect()
    }

    /// Hosts asked for so far, loopback excluded (patch 0003's neutral `127.0.0.1:1` sentinel is
    /// the only loopback peer a first run ever names).
    fn remote_hosts(&self) -> BTreeSet<String> {
        self.targets
            .lock()
            .unwrap()
            .iter()
            .map(|t| {
                t.split(' ')
                    .nth(1)
                    .unwrap_or_default()
                    .rsplit_once(':')
                    .map(|(h, _)| h.to_owned())
                    .unwrap_or_default()
            })
            .filter(|h| !matches!(h.as_str(), "127.0.0.1" | "localhost" | "::1" | "[::1]" | ""))
            .collect()
    }

    fn wait_for_hosts(&self, n: usize, secs: u64) -> BTreeSet<String> {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(secs) {
            let hosts = self.remote_hosts();
            if hosts.len() >= n {
                return hosts;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        self.remote_hosts()
    }
}

struct Run {
    h: PtyHarness,
    _cwd: tempfile::TempDir,
}

/// Spawn the TUI on `workshop_home` (fresh git repo as cwd) with `extra_env` and, optionally, a
/// `bin/` of fakes prepended to `PATH`.
fn spawn(bin: &Path, home: &Path, extra_env: &[(&str, String)], extra_path: Option<&Path>) -> Run {
    let cwd = tempfile::tempdir().expect("tempdir");
    git_init(cwd.path());
    let workshop_home = home.join(".workshop");
    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let path_s = match extra_path {
        Some(p) => format!("{}:{inherited_path}", p.display()),
        None => inherited_path,
    };
    let owned: Vec<(String, String)> = [
        ("HOME".to_owned(), home.to_string_lossy().to_string()),
        (
            "WORKSHOP_HOME".to_owned(),
            workshop_home.to_string_lossy().to_string(),
        ),
        ("PATH".to_owned(), path_s),
        ("TERM".to_owned(), "xterm-256color".to_owned()),
        ("NO_COLOR".to_owned(), "1".to_owned()),
        ("GROK_DISABLE_AUTOUPDATER".to_owned(), "1".to_owned()),
    ]
    .into_iter()
    .chain(extra_env.iter().map(|(k, v)| ((*k).to_owned(), v.clone())))
    .collect();
    let env: Vec<(&str, &str)> = owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let mut h = PtyHarness::new_inherited_env(bin, 45, 120, &[], &env, Some(cwd.path()))
        .expect("spawn workshop in pty");
    h.set_respond_to_queries(true);
    Run { h, _cwd: cwd }
}

/// The launch-time `opencode` install is the one request before the user acts; `/model` reaches
/// only the hosts of the lists it shows (none on a fresh home).
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (logging proxy, no network); run with --include-ignored"]
fn catalogs_are_fetched_only_after_the_user_acts() {
    let Some(bin) = bin_from_env() else { return };
    let dir = evidence_dir();
    let proxy = LogProxy::start();
    let home = tempfile::tempdir().expect("tempdir");
    let workshop_home = home.path().join(".workshop");

    // `workshop login` (the CLI picker text) is served from the compiled seeds, dated, offline.
    let out = std::process::Command::new(&bin)
        .arg("login")
        .env("HOME", home.path())
        .env("WORKSHOP_HOME", &workshop_home)
        .env("NO_COLOR", "1")
        .env("GROK_DISABLE_AUTOUPDATER", "1")
        .envs(proxy.env())
        .output()
        .expect("run workshop login");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    std::fs::write(dir.join("00-cli-login.txt"), &text).unwrap();
    assert!(
        text.contains("Lists:") && text.contains(SEED_NOTE),
        "workshop login shows the dated seed lists:\n{text}"
    );
    assert!(
        proxy.remote_hosts().is_empty(),
        "workshop login must not fetch anything, asked for {:?}",
        proxy.remote_hosts()
    );

    // 1. First run: the composer, and — with nothing typed — the engine bring-up: the vendor's
    //    installer (`opencode.ai`, refused here) is the one host asked for; no catalog, no xAI
    //    host, not after 3 s either, and nothing about it on screen.
    let installer_only = BTreeSet::from(["opencode.ai".to_owned()]);
    let mut run = spawn(&bin, home.path(), &proxy.env(), None);
    wait_for(&mut run.h, FIRST_RUN_LABEL, 30);
    let hosts = proxy.wait_for_hosts(1, 60);
    assert_eq!(
        hosts, installer_only,
        "a first run asks for the engine installer at launch and nothing else"
    );
    run.h.update(Duration::from_millis(3000));
    snapshot(&run.h, &dir, "01-first-run-composer");
    assert_eq!(
        proxy.remote_hosts(),
        installer_only,
        "first run must not fetch catalogs"
    );
    let screen = run.h.screen_contents();
    assert!(
        !screen.contains("Installing") && !screen.contains("Starting the OpenCode engine"),
        "the launch bring-up never shows on the composer:\n{screen}"
    );

    // 2. `/auth` (the same picker, with the models listed above the subscriptions): still
    //    nothing more.
    slash(&mut run.h, "/auth");
    wait_for(&mut run.h, PICKER, 10);
    wait_for(&mut run.h, "OpenCode", 10);
    run.h.update(Duration::from_millis(2000));
    snapshot(&run.h, &dir, "02-auth-no-fetch");
    assert_eq!(proxy.remote_hosts(), installer_only, "/auth must not fetch");
    run.h.inject_keys(b"\x1b").unwrap();
    wait_gone(&mut run.h, PICKER, 5);

    // 3. `/model` on a fresh home lists OpenCode's models (the dated seed row until the first
    //    message) and the installed subscription CLIs — no hosted API-key provider has a key, so
    //    there is no list to fetch and the proxy is never asked. Kilo Gateway is never a row.
    slash(&mut run.h, "/model");
    wait_for(&mut run.h, PICKER, 10);
    wait_gone(&mut run.h, "loading\u{2026}", 20);
    wait_gone(&mut run.h, "refreshing lists", 15);
    run.h.update(Duration::from_millis(2000));
    let screen = run.h.screen_contents();
    assert_eq!(
        proxy.remote_hosts(),
        installer_only,
        "/model on a fresh home has no hosted list to fetch (the launch's installer request is the only one)"
    );
    assert!(
        selected_line(&run.h).is_some_and(|l| l.contains("Big Pickle"))
            && screen.contains(SEED_NOTE),
        "the OpenCode seed row is highlighted and dated:\n{screen}"
    );
    for plumbing in ["opencode serve", "opencode.ai", "CLI"] {
        assert!(
            !screen.contains(plumbing),
            "no plumbing under a model ({plumbing:?}):\n{screen}"
        );
    }
    assert!(
        !screen.contains("Kilo")
            && !screen.contains("refresh failed")
            && !screen.contains("engine"),
        "nothing hosted is listed or fetched on a fresh home, Kilo never, no plumbing:\n{screen}"
    );
    snapshot(&run.h, &dir, "03-model-fresh-home-no-fetch");
    assert!(
        !workshop_home
            .join("tools/opencode/.opencode/bin/opencode")
            .exists(),
        "the refused launch install left no binary behind, and /model never installs one"
    );
    quit(run.h);

    // 4. A returning launch (a connection is active) starts `opencode` again and refreshes the
    //    lists it shows in the background — on this home no API key is configured, so the
    //    installer host is still the only one.
    proxy.targets.lock().unwrap().clear();
    let mut run = spawn(&bin, home.path(), &proxy.env(), None);
    wait_for(&mut run.h, FIRST_RUN_LABEL, 30);
    run.h.update(Duration::from_millis(3000));
    assert!(
        proxy.remote_hosts().is_subset(&installer_only),
        "a returning launch with no configured API key has no list to fetch, asked for {:?}",
        proxy.remote_hosts()
    );
    quit(run.h);

    // 5. With an API key configured (NVIDIA's environment variable here), that provider is listed
    //    on the Models view and its list is refreshed — its host (plus the launch's installer
    //    request), nothing else, never an xAI host: in the background on this returning launch
    //    and again by `/model`. The proxy refuses, so its rows stay the dated seed and say the
    //    refresh failed.
    proxy.targets.lock().unwrap().clear();
    let mut env = proxy.env();
    env.push(("NVIDIA_API_KEY", "test-key-never-sent".to_owned()));
    let mut run = spawn(&bin, home.path(), &env, None);
    wait_for(&mut run.h, FIRST_RUN_LABEL, 30);
    let nvidia = "integrate.api.nvidia.com".to_owned();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !proxy.remote_hosts().contains(&nvidia) {
        assert!(
            Instant::now() < deadline,
            "a returning launch refreshes the configured provider's list, asked for {:?}",
            proxy.remote_hosts()
        );
        run.h.update(Duration::from_millis(200));
    }
    let allowed: BTreeSet<String> = installer_only
        .iter()
        .cloned()
        .chain([nvidia.clone()])
        .collect();
    assert!(
        proxy.remote_hosts().is_subset(&allowed),
        "a returning launch refreshes the configured provider's list, and only that: {:?}",
        proxy.remote_hosts()
    );
    slash(&mut run.h, "/model");
    wait_for(&mut run.h, PICKER, 10);
    wait_gone(&mut run.h, "loading\u{2026}", 20);
    wait_gone(&mut run.h, "refreshing lists", 15);
    wait_for(&mut run.h, "NVIDIA", 10);
    move_selection_to(&mut run.h, "nemotron-3-super");
    wait_for(&mut run.h, "refresh failed", 10);
    let screen = run.h.screen_contents();
    assert!(
        screen.contains(&format!("{SEED_NOTE} \u{b7} refresh failed")),
        "seed rows are marked as such after a failed fetch:\n{screen}"
    );
    assert!(
        !screen.contains("Kilo") && !screen.contains("OpenRouter"),
        "only the configured provider joins the Models view:\n{screen}"
    );
    assert!(
        proxy.remote_hosts().is_subset(&allowed) && proxy.remote_hosts().contains(&nvidia),
        "/model fetches the listed provider's host and nothing else: {:?}",
        proxy.remote_hosts()
    );
    let all = proxy.targets.lock().unwrap().clone();
    assert!(
        !all.iter()
            .any(|t| t.contains("x.ai") || t.contains("grok.com")),
        "no xAI host may ever be contacted: {all:?}"
    );
    snapshot(&run.h, &dir, "04-model-with-key-after-refused-refresh");
    assert!(
        !workshop_home
            .join("tools/opencode/.opencode/bin/opencode")
            .exists(),
        "the refused launch install left no binary behind, and /model never installs one"
    );
    quit(run.h);
    eprintln!("evidence: {}", dir.display());
}

fn adapter_fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../workshop-adapters/tests/fixtures")
}

/// Write a fake `opencode` (identity + `auth list` + `serve` → the shared Python stand-in
/// `tests/fixtures/fake-opencode-serve-turn.py`, which replays the adapter crate's captured
/// turn) into `bin`.
fn install_fake_opencode(bin: &Path) {
    std::fs::create_dir_all(bin).unwrap();
    let serve_py =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-opencode-serve-turn.py");
    let providers = adapter_fixtures().join("opencode_serve_providers.json");
    let turn = adapter_fixtures().join("opencode_serve_turn.jsonl");
    let script = format!(
        r#"#!/bin/sh
case "$1" in
  --version) echo '1.18.31'; exit 0 ;;
  --help) printf 'Commands:\n  opencode run [message..]     run opencode with a message\n' >&2; exit 0 ;;
esac
if [ "$*" = "auth list" ]; then
  printf '%s\n' '┌  Credentials ~/.local/share/opencode/auth.json' '│' '└  0 credentials'; exit 0
fi
if [ "$1" = "serve" ]; then
  # serve --hostname 127.0.0.1 --port N
  exec python3 '{serve}' --port "$5" --providers '{providers}' --turn '{turn}'
fi
echo "fake opencode: unexpected $*" >&2
exit 2
"#,
        serve = serve_py.display(),
        providers = providers.display(),
        turn = turn.display(),
    );
    let path = bin.join("opencode");
    std::fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// `/model` shows the engine's live free list, not the pinned seed, once `opencode serve` is up.
#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); hermetic (fake opencode serve on loopback); run with --include-ignored"]
fn model_lists_more_opencode_rows_than_the_seed_when_opencode_serve_is_up() {
    let Some(bin) = bin_from_env() else { return };
    let dir = evidence_dir();
    let fakes = tempfile::tempdir().expect("fakes dir");
    let fake_bin = fakes.path().join("bin");
    install_fake_opencode(&fake_bin);
    let home = tempfile::tempdir().expect("tempdir");
    let workshop_home = home.path().join(".workshop");
    // The hosted lists are not the point here: a closed proxy port makes their refresh fail fast
    // and offline, so the run needs no network.
    let offline: Vec<(&str, String)> = ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"]
        .into_iter()
        .map(|k| (k, "http://127.0.0.1:9".to_owned()))
        .collect();
    // Before: no engine to start (none on PATH; the launch install is refused by the closed
    // proxy), so `/model` shows the one pinned engine row, dated.
    let mut run = spawn(&bin, home.path(), &offline, None);
    wait_for(&mut run.h, FIRST_RUN_LABEL, 30);
    slash(&mut run.h, "/model");
    wait_for(&mut run.h, PICKER, 10);
    wait_for(&mut run.h, "OpenCode", 15);
    wait_gone(&mut run.h, "loading\u{2026}", 20);
    wait_gone(&mut run.h, "refreshing lists", 15);
    let before = overlay_rows_for(&run.h, "OpenCode");
    assert_eq!(before.len(), 1, "seed engine list is one row: {before:?}");
    move_selection_to(&mut run.h, "Big Pickle");
    wait_for(&mut run.h, SEED_NOTE, 5);
    snapshot(&run.h, &dir, "10-model-before-engine-seed-only");
    run.h.inject_keys(b"\x1b").unwrap();
    wait_gone(&mut run.h, PICKER, 5);
    quit(run.h);

    // With the fake on PATH the engine starts at launch — nothing typed — and its free list is
    // cached at once, so `/model` lists it before the first message.
    let mut run = spawn(&bin, home.path(), &offline, Some(&fake_bin));
    wait_for(&mut run.h, FIRST_RUN_LABEL, 30);
    let cache = workshop_home
        .join("catalog-cache")
        .join("opencode-engine.json");
    let deadline = Instant::now() + Duration::from_secs(60);
    while !cache.is_file() && Instant::now() < deadline {
        run.h.update(Duration::from_millis(200));
    }
    let cached: Vec<serde_json::Value> = serde_json::from_str(
        &std::fs::read_to_string(&cache).expect("the launch start caches the live list"),
    )
    .unwrap();
    assert_eq!(
        cached.len(),
        8,
        "the engine's free list is cached: {cached:?}"
    );
    assert_eq!(cached[0]["model_ref"], "opencode/big-pickle");
    assert_eq!(cached[0]["is_default"], true);
    slash(&mut run.h, "/model");
    wait_for(&mut run.h, PICKER, 10);
    wait_for(&mut run.h, "Nemotron 3 Ultra Free", 10);
    wait_gone(&mut run.h, "loading\u{2026}", 20);
    wait_gone(&mut run.h, "refreshing lists", 15);
    // One row per model (a model with effort levels opens into them; it is not a row per level).
    let at_launch = overlay_rows_for(&run.h, "OpenCode");
    assert_eq!(
        at_launch.len(),
        8,
        "the engine started at launch: its live list is there before the first message:\n{at_launch:#?}"
    );
    move_selection_to(&mut run.h, "Big Pickle");
    wait_for(&mut run.h, "list fetched", 5);
    snapshot(&run.h, &dir, "11-model-at-launch-live-list");
    run.h.inject_keys(b"\x1b").unwrap();
    wait_gone(&mut run.h, PICKER, 5);

    // The first message goes through the engine that started at launch; its turn replays the
    // captured fixture.
    run.h.inject_keys(b"hello").unwrap();
    run.h.update(Duration::from_millis(300));
    run.h.inject_keys(b"\r").unwrap();
    wait_for(&mut run.h, "Created hello.txt with the exact line.", 90);
    snapshot(&run.h, &dir, "12-first-turn-through-fake-engine");

    // After: `/model` still lists every free model the engine reports, dated from the fetch.
    slash(&mut run.h, "/model");
    wait_for(&mut run.h, PICKER, 10);
    wait_for(&mut run.h, "Nemotron 3 Ultra Free", 10);
    wait_gone(&mut run.h, "loading\u{2026}", 20);
    wait_gone(&mut run.h, "refreshing lists", 15);
    // Every model is one row; a model with effort levels is marked as opening into them.
    let after = overlay_rows_for(&run.h, "OpenCode");
    assert!(
        after.len() > before.len() && after.len() == 8,
        "live engine models {} vs seed {}:\n{after:#?}",
        after.len(),
        before.len()
    );
    assert!(
        after
            .iter()
            .any(|r| r.starts_with("Ling 3.0 Flash Fin Free") && r.ends_with('\u{25b8}')),
        "a model with effort levels opens into them: {after:#?}"
    );
    assert!(
        !after.iter().any(|r| r.contains(" (")),
        "no `Name (level)` rows: {after:#?}"
    );
    for name in [
        "Big Pickle",
        "Nemotron 3 Ultra Free",
        "Nemotron 3.5 Lightning Free",
        "MiMo-V2.6-Flash Free",
        "Ling 3.0 Flash Fin Free",
    ] {
        assert!(
            after.iter().any(|r| r.contains(name)),
            "{name} missing from the engine rows: {after:#?}"
        );
    }
    assert!(
        !after.iter().any(|r| r.contains("Old Thing Free")),
        "deprecated rows are not offered: {after:#?}"
    );
    move_selection_to(&mut run.h, "Big Pickle");
    wait_for(&mut run.h, "list fetched just now", 5);
    let screen = run.h.screen_contents();
    assert!(
        selected_line(&run.h).is_some_and(|l| l.contains("active")),
        "the active default is still highlighted:\n{screen}"
    );
    assert!(
        !screen.contains("opencode serve") && !screen.contains("/config/providers"),
        "the freshness note names no endpoint:\n{screen}"
    );
    snapshot(&run.h, &dir, "13-model-after-engine-live-list");
    quit(run.h);

    // The next launch shows the cached live list at once (`fetched <age>`), before any turn.
    let mut run = spawn(&bin, home.path(), &offline, Some(&fake_bin));
    wait_for(&mut run.h, FIRST_RUN_LABEL, 30);
    slash(&mut run.h, "/model");
    wait_for(&mut run.h, "Nemotron 3 Ultra Free", 15);
    let rows = overlay_rows_for(&run.h, "OpenCode");
    assert_eq!(
        rows.len(),
        8,
        "cached engine list on the next launch: {rows:#?}"
    );
    move_selection_to(&mut run.h, "Big Pickle");
    wait_for(&mut run.h, "list fetched", 5);
    snapshot(&run.h, &dir, "14-model-next-launch-cached-live-list");
    quit(run.h);
    eprintln!("evidence: {}", dir.display());
}
