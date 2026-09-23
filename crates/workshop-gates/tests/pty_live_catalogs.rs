//! Live model lists through the real TUI (hermetic, PTY-driven; opt-in via `WORKSHOP_BIN`):
//!
//! * `catalogs_are_fetched_only_after_the_user_acts` — the zero-egress gate. Every request that
//!   honours `HTTP(S)_PROXY` lands on a logging proxy inside the test that refuses to forward.
//!   A first run, `/auth` (and its Models view via Tab) and `workshop login` ask the proxy for
//!   nothing; `/model` asks for exactly the keyless catalog hosts (Kilo, OpenRouter, NVIDIA),
//!   never an xAI host; and with the proxy refusing, every list is shown as the dated seed with
//!   `refresh failed`. A returning launch (a connection is active) refreshes in the background.
//! * `model_lists_more_opencode_rows_than_the_seed_when_opencode_serve_is_up` — a fake `opencode`
//!   whose `serve` is a loopback HTTP/SSE stand-in (the adapter crate's captured fixtures). The
//!   first message starts it; `/model` then lists every free model the engine reports (8 in the
//!   fixture, not the one pinned seed), dated `fetched just now`, and the cache under
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

const FIRST_RUN_LABEL: &str = "OpenCode \u{b7} Big Pickle";
const SEED_NOTE: &str = "cached list from 2026-09-21";

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
        .find(|l| l.contains('\u{203a}'))
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

/// Rows of the open Models overlay whose provider column is `provider` (`OpenCode`, `Kilo Gateway`):
/// `name  provider  badge[ · active]` inside the box border.
fn overlay_rows_for(h: &PtyHarness, provider: &str) -> Vec<String> {
    h.screen_contents()
        .lines()
        .map(|l| l.trim().trim_end_matches('\u{2502}').trim().to_owned())
        .filter(|l| {
            l.contains(provider)
                && ["free", "free · active", "free · key", "key", "key needed"]
                    .iter()
                    .any(|badge| l.ends_with(badge))
        })
        .collect()
}

fn quit(mut h: PtyHarness) {
    h.inject_keys(b"\x03").unwrap();
    h.update(Duration::from_millis(400));
    h.inject_keys(b"\x03").unwrap();
    let _ = h.wait_exit_code(Duration::from_secs(5));
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

/// Zero egress before the user acts; `/model` reaches only the catalog hosts.
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

    // 1. First run: the composer, no fetch — not after 3 s either.
    let mut run = spawn(&bin, home.path(), &proxy.env(), None);
    wait_for(&mut run.h, FIRST_RUN_LABEL, 30);
    run.h.update(Duration::from_millis(3000));
    snapshot(&run.h, &dir, "01-first-run-composer");
    assert!(
        proxy.remote_hosts().is_empty(),
        "first run must not fetch catalogs, asked for {:?}",
        proxy.remote_hosts()
    );

    // 2. `/auth`, and its Models view via Tab: still nothing.
    slash(&mut run.h, "/auth");
    wait_for(&mut run.h, "Tab: Models", 10);
    run.h.inject_keys(b"\t").unwrap();
    wait_for(&mut run.h, "Tab: Subscriptions", 5);
    wait_for(&mut run.h, "Kilo", 10);
    run.h.update(Duration::from_millis(2000));
    snapshot(&run.h, &dir, "02-auth-then-tab-models-no-fetch");
    assert!(
        proxy.remote_hosts().is_empty(),
        "/auth (even on its Models view) must not fetch, asked for {:?}",
        proxy.remote_hosts()
    );
    run.h.inject_keys(b"\x1b").unwrap();
    wait_gone(&mut run.h, "Tab: Subscriptions", 5);

    // 3. `/model`: the cached rows first, then — the user asked for the lists — a refresh that
    //    reaches exactly the keyless catalog hosts, no xAI host.
    slash(&mut run.h, "/model");
    wait_for(&mut run.h, "Tab: Subscriptions", 10);
    wait_gone(&mut run.h, "loading\u{2026}", 20);
    let hosts = proxy.wait_for_hosts(3, 20);
    assert_eq!(
        hosts,
        BTreeSet::from([
            "api.kilo.ai".to_owned(),
            "openrouter.ai".to_owned(),
            "integrate.api.nvidia.com".to_owned(),
        ]),
        "/model fetches the keyless catalog hosts and nothing else"
    );
    let all = proxy.targets.lock().unwrap().clone();
    assert!(
        !all.iter()
            .any(|t| t.contains("x.ai") || t.contains("grok.com")),
        "no xAI host may ever be contacted: {all:?}"
    );
    // The proxy refused, so the rows are the dated seeds and say the refresh failed. The engine
    // seed row (highlighted: it is the active connection) is dated too.
    wait_gone(&mut run.h, "refreshing lists", 15);
    let screen = run.h.screen_contents();
    assert!(
        selected_line(&run.h).is_some_and(|l| l.contains("Big Pickle"))
            && screen.contains(SEED_NOTE)
            && screen.contains("live list arrives when the engine starts"),
        "the engine seed row is dated:\n{screen}"
    );
    move_selection_to(&mut run.h, "Auto Free");
    wait_for(&mut run.h, "refresh failed", 10);
    let screen = run.h.screen_contents();
    assert!(
        screen.contains(&format!("{SEED_NOTE} \u{b7} refresh failed")),
        "seed rows are marked as such after a failed fetch:\n{screen}"
    );
    snapshot(&run.h, &dir, "03-model-after-refused-refresh");
    assert!(
        !workshop_home.join("tools").exists(),
        "/model never installs or starts opencode"
    );
    quit(run.h);

    // 4. A returning launch has an active connection: the lists refresh in the background.
    proxy.targets.lock().unwrap().clear();
    let mut run = spawn(&bin, home.path(), &proxy.env(), None);
    wait_for(&mut run.h, FIRST_RUN_LABEL, 30);
    let hosts = proxy.wait_for_hosts(3, 20);
    assert!(
        hosts.contains("api.kilo.ai"),
        "a returning launch refreshes the catalogs in the background, asked for {hosts:?}"
    );
    quit(run.h);
    eprintln!("evidence: {}", dir.display());
}

/// A loopback stand-in for `opencode serve`: health, the captured `/config/providers` (8 free
/// rows, Big Pickle default), session create, the `/event` stream, and a prompt that replays the
/// captured turn. Started by the fake `opencode` binary's `serve` subcommand.
const FAKE_SERVE_PY: &str = r#"#!/usr/bin/env python3
import argparse, json, queue, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ap = argparse.ArgumentParser()
ap.add_argument("--port", type=int, required=True)
ap.add_argument("--providers", required=True)
ap.add_argument("--turn", required=True)
a = ap.parse_args()
PROVIDERS = open(a.providers, "rb").read()
TURN = [json.loads(l) for l in open(a.turn) if l.strip()]
SESSION = "ses_fake0001"
subs, lock = [], threading.Lock()

def broadcast(ev):
    with lock:
        targets = list(subs)
    for q in targets:
        q.put(ev)

def replay():
    time.sleep(0.05)
    for ev in TURN:
        broadcast(ev)
        time.sleep(0.01)

class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *args):
        pass
    def _raw(self, code, body, ctype="application/json"):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def _json(self, code, obj):
        self._raw(code, json.dumps(obj).encode())
    def do_GET(self):
        path = self.path.split("?")[0]
        if path == "/global/health":
            return self._json(200, {"healthy": True, "version": "1.18.31"})
        if path == "/config/providers":
            return self._raw(200, PROVIDERS)
        if path == "/session/" + SESSION:
            return self._json(200, {"id": SESSION, "title": "fake", "directory": "/work"})
        if path == "/session/" + SESSION + "/message":
            return self._json(200, [])
        if path == "/event":
            q = queue.Queue()
            with lock:
                subs.append(q)
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Cache-Control", "no-cache")
            self.send_header("Connection", "close")
            self.end_headers()
            try:
                self.wfile.write(b"data: " + json.dumps({"type": "server.connected", "properties": {}}).encode() + b"\n\n")
                self.wfile.flush()
                while True:
                    try:
                        ev = q.get(timeout=1.0)
                    except queue.Empty:
                        continue
                    self.wfile.write(b"data: " + json.dumps(ev).encode() + b"\n\n")
                    self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError, OSError):
                pass
            finally:
                with lock:
                    if q in subs:
                        subs.remove(q)
            return
        self._json(404, {"error": "no route GET " + path})
    def do_POST(self):
        path = self.path.split("?")[0]
        n = int(self.headers.get("Content-Length") or 0)
        if n:
            self.rfile.read(n)
        if path == "/session":
            return self._json(200, {"id": SESSION, "title": "Workshop", "directory": "/work"})
        if path == "/session/" + SESSION + "/prompt_async":
            threading.Thread(target=replay, daemon=True).start()
            self.send_response(204)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if path == "/session/" + SESSION + "/abort":
            return self._json(200, True)
        if path.startswith("/session/" + SESSION + "/permissions/"):
            return self._json(200, True)
        self._json(404, {"error": "no route POST " + path})

srv = ThreadingHTTPServer(("127.0.0.1", a.port), H)
srv.daemon_threads = True
print("opencode server listening on http://127.0.0.1:%d" % a.port, flush=True)
srv.serve_forever()
"#;

fn adapter_fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../workshop-adapters/tests/fixtures")
}

/// Write a fake `opencode` (identity + `auth list` + `serve` → the Python stand-in) into `bin`.
fn install_fake_opencode(bin: &Path) {
    std::fs::create_dir_all(bin).unwrap();
    let serve_py = bin.join("fake-opencode-serve.py");
    std::fs::write(&serve_py, FAKE_SERVE_PY).unwrap();
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
    let mut run = spawn(&bin, home.path(), &offline, Some(&fake_bin));
    wait_for(&mut run.h, FIRST_RUN_LABEL, 30);

    // Before: the engine has never run, so `/model` shows the one pinned engine row, dated.
    slash(&mut run.h, "/model");
    wait_for(&mut run.h, "Tab: Subscriptions", 10);
    wait_for(&mut run.h, "Kilo", 15);
    wait_gone(&mut run.h, "loading\u{2026}", 20);
    wait_gone(&mut run.h, "refreshing lists", 15);
    let before = overlay_rows_for(&run.h, "OpenCode");
    assert_eq!(before.len(), 1, "seed engine list is one row: {before:?}");
    move_selection_to(&mut run.h, "Big Pickle");
    wait_for(&mut run.h, SEED_NOTE, 5);
    snapshot(&run.h, &dir, "10-model-before-engine-seed-only");
    run.h.inject_keys(b"\x1b").unwrap();
    wait_gone(&mut run.h, "Tab: Subscriptions", 5);

    // The first message starts the (fake) engine; its turn replays the captured fixture.
    run.h.inject_keys(b"hello").unwrap();
    run.h.update(Duration::from_millis(300));
    run.h.inject_keys(b"\r").unwrap();
    wait_for(&mut run.h, "Created hello.txt with the exact line.", 90);
    snapshot(&run.h, &dir, "11-first-turn-through-fake-engine");
    let cache = workshop_home
        .join("catalog-cache")
        .join("opencode-engine.json");
    let cached: Vec<serde_json::Value> = serde_json::from_str(
        &std::fs::read_to_string(&cache).expect("engine start caches the live list"),
    )
    .unwrap();
    assert_eq!(
        cached.len(),
        8,
        "the engine's free list is cached: {cached:?}"
    );
    assert_eq!(cached[0]["model_ref"], "opencode/big-pickle");
    assert_eq!(cached[0]["is_default"], true);

    // After: `/model` lists every free model the engine reports, dated from the fetch.
    slash(&mut run.h, "/model");
    wait_for(&mut run.h, "Tab: Subscriptions", 10);
    wait_for(&mut run.h, "Nemotron 3 Ultra Free", 10);
    wait_gone(&mut run.h, "loading\u{2026}", 20);
    wait_gone(&mut run.h, "refreshing lists", 15);
    let after = overlay_rows_for(&run.h, "OpenCode");
    assert!(
        after.len() > before.len() && after.len() == 8,
        "live engine rows {} vs seed {}:\n{after:#?}",
        after.len(),
        before.len()
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
    wait_for(
        &mut run.h,
        "Model list fetched just now from opencode serve",
        5,
    );
    let screen = run.h.screen_contents();
    assert!(
        selected_line(&run.h).is_some_and(|l| l.contains("active")),
        "the active default is still highlighted:\n{screen}"
    );
    snapshot(&run.h, &dir, "12-model-after-engine-live-list");
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
    wait_for(&mut run.h, "Model list fetched", 5);
    snapshot(&run.h, &dir, "13-model-next-launch-cached-live-list");
    quit(run.h);
    eprintln!("evidence: {}", dir.display());
}
