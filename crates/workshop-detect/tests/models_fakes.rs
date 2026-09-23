//! Model-list probes against fake vendor CLIs (`tests/fixtures/vendors`) that speak the documented
//! protocols: Claude's stream-json `initialize`, Codex's app-server `account/read` + `model/list`,
//! and `cursor-agent models`. Signed in, signed out, failing, hanging, and the one-child-at-a-time
//! and kill-grace rules.

#![cfg(unix)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use workshop_detect::copy;
use workshop_detect::models::CLAUDE_DOCUMENTED_ALIASES;
use workshop_detect::process::VendorSlot;
use workshop_detect::{
    DetectConfig, ModelsCache, ModelsError, Pill, Rail, RailModels, Refresh, Vendor, picker_rails,
    probe_all, probe_vendor, subscription_models,
};

const LIVE: Refresh = Refresh::Live {
    max_age: Duration::ZERO,
};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vendors")
}

/// The vendor slots are process-wide, and several tests leave a child running in its kill grace
/// on purpose; budgets measured in milliseconds only hold when each test starts with every slot
/// free. So tests here run one at a time, each after the previous one's children have exited.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for vendor in Vendor::ALL {
        drop(VendorSlot::acquire(vendor, Duration::from_secs(60)));
    }
    guard
}

struct Fake {
    state: tempfile::TempDir,
    cfg: DetectConfig,
    cache: ModelsCache,
}

impl Fake {
    fn new(extra_env: &[(&str, &str)]) -> Fake {
        let state = tempfile::tempdir().unwrap();
        let home = state.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let mut cfg = DetectConfig::hermetic(fixtures().as_os_str(), &home);
        cfg.timeout = Duration::from_secs(10);
        cfg.models_timeout = Duration::from_secs(10);
        cfg.claude_kill_grace = Duration::from_secs(10);
        cfg.extra_env.push((
            OsString::from("FAKE_CLI_STATE_DIR"),
            state.path().as_os_str().to_owned(),
        ));
        for (k, v) in extra_env {
            cfg.extra_env.push((OsString::from(k), OsString::from(v)));
        }
        let cache = ModelsCache::new(state.path().join("catalog-cache"));
        Fake { state, cfg, cache }
    }

    fn signed_in(extra_env: &[(&str, &str)]) -> Fake {
        let mut env = vec![
            ("FAKE_LOGIN_CLAUDE", "in"),
            ("FAKE_LOGIN_CODEX", "in"),
            ("FAKE_LOGIN_CURSOR", "in"),
        ];
        env.extend_from_slice(extra_env);
        Fake::new(&env)
    }

    fn file(&self, name: &str) -> String {
        std::fs::read_to_string(self.state.path().join(name)).unwrap_or_default()
    }

    fn exists(&self, name: &str) -> bool {
        self.state.path().join(name).exists()
    }

    fn bin(&self, vendor: Vendor) -> PathBuf {
        probe_vendor(vendor, &self.cfg)
            .binary
            .expect("fake installed")
            .path
    }
}

fn ids(r: &RailModels) -> Vec<String> {
    match r {
        RailModels::Listed { list, .. } => list.models.iter().map(|m| m.id.clone()).collect(),
        other => panic!("not listed: {other:?}"),
    }
}

fn model_calls(calls: &str) -> usize {
    calls
        .lines()
        .filter(|l| {
            l.contains("--input-format") || l.contains("app-server") || l.ends_with(" models")
        })
        .count()
}

#[test]
fn signed_in_rails_list_each_clis_own_models_and_plan() {
    let _serial = serial();
    let f = Fake::signed_in(&[]);
    let rails = picker_rails(&f.cfg, &f.cache, LIVE);
    for r in &rails {
        assert_eq!(r.pill, Pill::Ready, "{:?}", r.rail);
        assert!(r.empty_copy.is_none(), "{:?}", r.rail);
        assert!(!r.show_connect);
        assert!(
            matches!(
                &r.subscription,
                RailModels::Listed {
                    cached: false,
                    error: None,
                    ..
                }
            ),
            "{:?}",
            r.subscription
        );
    }
    assert_eq!(
        ids(&rails[0].subscription),
        ["default", "opus[1m]", "claude-fable-5-1", "sonnet", "haiku"]
    );
    assert_eq!(rails[0].models[0].display(), "Default (recommended)");
    // Codex: the server's default moves to the front; hidden models stay out.
    assert_eq!(
        ids(&rails[1].subscription),
        ["gpt-6-astra", "gpt-5.5", "gpt-6-sol"]
    );
    assert_eq!(
        ids(&rails[2].subscription),
        [
            "composer-2.5",
            "auto",
            "gpt-6-sol",
            "claude-opus-5-5-thinking"
        ]
    );
    assert_eq!(rails[2].models[3].display(), "Claude Opus 5.5 (Thinking)");

    let plan = |r: &RailModels| match r {
        RailModels::Listed { list, .. } => list
            .account
            .as_ref()
            .map(|a| (a.email.clone(), a.plan.clone())),
        _ => None,
    };
    let who = Some((Some("user@example.com".into()), Some("max".into())));
    assert_eq!(plan(&rails[0].subscription), who);
    let who = Some((Some("user@example.com".into()), Some("pro".into())));
    assert_eq!(plan(&rails[1].subscription), who);
    assert_eq!(plan(&rails[2].subscription), None);

    // Exactly the documented commands, once each; Codex never asked for a token refresh.
    assert!(
        f.file("calls.claude").contains(
            "claude -p --input-format stream-json --output-format stream-json --verbose --no-session-persistence --strict-mcp-config\n"
        ),
        "{}",
        f.file("calls.claude")
    );
    assert!(f.file("calls.codex").contains("codex app-server\n"));
    assert!(f.file("calls.cursor").contains("cursor models\n"));
    for v in ["claude", "codex", "cursor"] {
        assert_eq!(model_calls(&f.file(&format!("calls.{v}"))), 1, "{v}");
    }
    assert!(
        !f.exists("refresh.codex"),
        "account/read must not ask for refreshToken"
    );
    for rail in Rail::ALL {
        assert!(f.cache.load(rail).is_some(), "{rail:?} cached");
    }
}

#[test]
fn cache_only_shows_the_last_good_list_or_loading_and_starts_no_model_probe() {
    let _serial = serial();
    let f = Fake::signed_in(&[]);
    let rails = picker_rails(&f.cfg, &f.cache, Refresh::CacheOnly);
    for r in &rails {
        assert_eq!(r.pill, Pill::Ready);
        assert_eq!(r.subscription, RailModels::Loading, "{:?}", r.rail);
        assert!(r.models.is_empty(), "no placeholder rows while loading");
        assert_eq!(r.empty_copy, Some(copy::LOADING_MODELS));
        assert!(
            !r.show_connect,
            "a signed-in rail that is loading offers no Connect"
        );
    }
    for v in ["claude", "codex", "cursor"] {
        assert_eq!(model_calls(&f.file(&format!("calls.{v}"))), 0, "{v}");
    }

    let live = picker_rails(&f.cfg, &f.cache, LIVE);
    let cached = picker_rails(&f.cfg, &f.cache, Refresh::CacheOnly);
    for (l, c) in live.iter().zip(&cached) {
        assert_eq!(l.models, c.models, "{:?}", l.rail);
        assert!(matches!(
            c.subscription,
            RailModels::Listed { cached: true, .. }
        ));
    }
    // A fresh cache also satisfies a non-forced live refresh (`/model` right after `/auth`).
    let reused = picker_rails(
        &f.cfg,
        &f.cache,
        Refresh::Live {
            max_age: workshop_detect::models::FRESH_FOR,
        },
    );
    assert!(matches!(
        reused[0].subscription,
        RailModels::Listed { cached: true, .. }
    ));
    for v in ["claude", "codex", "cursor"] {
        assert_eq!(model_calls(&f.file(&format!("calls.{v}"))), 1, "{v}");
    }
}

#[test]
fn signing_out_drops_the_cached_list_and_runs_no_model_probe() {
    let _serial = serial();
    let f = Fake::signed_in(&[]);
    picker_rails(&f.cfg, &f.cache, LIVE);
    assert!(f.cache.load(Rail::Claude).is_some());

    let out = Fake::new(&[]);
    let cache = ModelsCache::new(f.state.path().join("catalog-cache"));
    let rails = picker_rails(&out.cfg, &cache, LIVE);
    for r in &rails {
        assert_eq!(r.pill, Pill::SignIn);
        assert_eq!(r.subscription, RailModels::NotReady);
        assert!(r.models.is_empty());
        assert!(cache.load(r.rail).is_none(), "{:?} cache dropped", r.rail);
    }
    for v in ["claude", "codex", "cursor"] {
        assert_eq!(model_calls(&out.file(&format!("calls.{v}"))), 0, "{v}");
    }
}

#[test]
fn a_cli_that_says_not_signed_in_drops_the_cache_and_fails() {
    let _serial = serial();
    // Status says signed in, the listing disagrees (expired login): Cursor's auth error, Codex's
    // `account: null`.
    let f = Fake::signed_in(&[]);
    picker_rails(&f.cfg, &f.cache, LIVE);
    let f2 = Fake::new(&[
        ("FAKE_LOGIN_CLAUDE", "in"),
        ("FAKE_LOGIN_CODEX", "in"),
        ("FAKE_ACCOUNT_CODEX", "none"),
        ("FAKE_LOGIN_CURSOR", "in"),
        ("FAKE_MODELS_CURSOR", "signed_out"),
    ]);
    let rails = picker_rails(&f2.cfg, &f.cache, LIVE);
    assert!(matches!(rails[0].subscription, RailModels::Listed { .. }));
    for r in &rails[1..] {
        assert_eq!(r.pill, Pill::Ready);
        assert!(
            matches!(&r.subscription, RailModels::Failed { .. }),
            "{:?}",
            r.subscription
        );
        assert!(r.models.is_empty());
        assert_eq!(r.empty_copy, Some(copy::MODELS_FAILED));
        assert!(f.cache.load(r.rail).is_none(), "{:?} cache dropped", r.rail);
    }
    let RailModels::Failed { reason } = &rails[2].subscription else {
        unreachable!()
    };
    assert!(
        reason.starts_with("Error: Authentication required."),
        "{reason}"
    );
}

#[test]
fn a_failure_keeps_the_last_good_list_and_without_one_shows_one_failed_row() {
    let _serial = serial();
    let f = Fake::signed_in(&[]);
    picker_rails(&f.cfg, &f.cache, LIVE);
    let failing = Fake::signed_in(&[
        ("FAKE_MODELS_CLAUDE", "hang_once"),
        ("FAKE_HANG_SECS", "1"),
        ("FAKE_MODELS_CODEX", "error"),
        ("FAKE_MODELS_CURSOR", "fail"),
    ]);
    let mut cfg = failing.cfg.clone();
    cfg.models_timeout = Duration::from_millis(300);
    let rails = picker_rails(&cfg, &f.cache, LIVE);
    for r in &rails {
        match &r.subscription {
            RailModels::Listed {
                cached: true,
                error: Some(_),
                ..
            } => {}
            other => panic!("{:?}: {other:?}", r.rail),
        }
        assert!(!r.models.is_empty());
    }

    // Nothing cached: one "Couldn't load models" line, never the old placeholder rows.
    let empty = ModelsCache::new(failing.state.path().join("empty-cache"));
    std::fs::remove_file(failing.state.path().join("claude.hung")).unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    let rails = picker_rails(&cfg, &empty, LIVE);
    for r in &rails {
        assert!(
            matches!(&r.subscription, RailModels::Failed { .. }),
            "{:?}: {:?}",
            r.rail,
            r.subscription
        );
        assert!(r.models.is_empty());
        assert_eq!(r.empty_copy, Some(copy::MODELS_FAILED));
        assert!(!r.show_connect);
    }
    let RailModels::Failed { reason } = &rails[1].subscription else {
        unreachable!()
    };
    assert_eq!(reason, "codex model/list: model catalog unavailable");
}

#[test]
fn claude_without_the_handshake_gets_its_documented_aliases_labelled_and_uncached() {
    let _serial = serial();
    let f = Fake::signed_in(&[("FAKE_MODELS_CLAUDE", "unsupported")]);
    let rails = picker_rails(&f.cfg, &f.cache, LIVE);
    let RailModels::Listed { list, cached, .. } = &rails[0].subscription else {
        panic!("{:?}", rails[0].subscription)
    };
    assert!(list.documented_aliases);
    assert!(!cached);
    assert_eq!(
        list.models
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        CLAUDE_DOCUMENTED_ALIASES
    );
    assert!(
        rails[0]
            .models
            .iter()
            .all(|m| m.display().ends_with(" (alias)"))
    );
    assert!(
        f.cache.load(Rail::Claude).is_none(),
        "aliases are never cached"
    );
}

#[test]
fn a_claude_probe_past_its_budget_returns_but_its_child_is_not_killed() {
    let _serial = serial();
    let f = Fake::signed_in(&[("FAKE_MODELS_CLAUDE", "hang_once"), ("FAKE_HANG_SECS", "2")]);
    let bin = f.bin(Vendor::Claude);
    let mut short = f.cfg.clone();
    short.models_timeout = Duration::from_millis(300);
    let started = Instant::now();
    assert_eq!(
        subscription_models(Rail::Claude, &bin, &short),
        Err(ModelsError::TimedOut)
    );
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "{:?}",
        started.elapsed()
    );
    assert!(!f.exists("claude.finished"));

    // The next probe waits for that child to finish by itself (an OAuth refresh in flight is never
    // cut), then answers; the two never overlap.
    let list = subscription_models(Rail::Claude, &bin, &f.cfg).expect("second probe answers");
    assert_eq!(list.models.len(), 5);
    assert!(
        f.exists("claude.finished"),
        "the timed-out child ran to completion"
    );
    assert!(started.elapsed() >= Duration::from_secs(2));
    assert_eq!(f.file("overlap.claude"), "");
}

#[test]
fn a_wedged_claude_child_is_killed_only_after_the_grace() {
    let _serial = serial();
    let f = Fake::signed_in(&[
        ("FAKE_MODELS_CLAUDE", "hang_once"),
        ("FAKE_HANG_SECS", "30"),
    ]);
    let bin = f.bin(Vendor::Claude);
    let mut cfg = f.cfg.clone();
    cfg.models_timeout = Duration::from_millis(200);
    cfg.claude_kill_grace = Duration::from_secs(1);
    let started = Instant::now();
    assert_eq!(
        subscription_models(Rail::Claude, &bin, &cfg),
        Err(ModelsError::TimedOut)
    );
    cfg.models_timeout = Duration::from_secs(10);
    let list = subscription_models(Rail::Claude, &bin, &cfg).expect("answers after the kill");
    assert_eq!(list.models.len(), 5);
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_secs(1),
        "killed before the grace: {waited:?}"
    );
    assert!(
        waited < Duration::from_secs(8),
        "grace not bounded: {waited:?}"
    );
    assert!(!f.exists("claude.finished"));
    assert_eq!(f.file("overlap.claude"), "");
}

#[test]
fn codex_and_cursor_probes_past_their_budget_are_bounded() {
    let _serial = serial();
    let f = Fake::signed_in(&[
        ("FAKE_MODELS_CODEX", "hang"),
        ("FAKE_MODELS_CURSOR", "hang"),
    ]);
    let mut cfg = f.cfg.clone();
    cfg.models_timeout = Duration::from_millis(400);
    for (rail, vendor) in [(Rail::Codex, Vendor::Codex), (Rail::Cursor, Vendor::Cursor)] {
        let bin = f.bin(vendor);
        let started = Instant::now();
        assert_eq!(
            subscription_models(rail, &bin, &cfg),
            Err(ModelsError::TimedOut),
            "{rail:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(2), "{rail:?}");
    }
}

#[test]
fn concurrent_probes_of_one_vendor_never_overlap() {
    let _serial = serial();
    let f = Fake::signed_in(&[]);
    let claude = f.bin(Vendor::Claude);
    let codex = f.bin(Vendor::Codex);
    std::thread::scope(|s| {
        for _ in 0..3 {
            s.spawn(|| subscription_models(Rail::Claude, &claude, &f.cfg).unwrap());
            s.spawn(|| probe_vendor(Vendor::Claude, &f.cfg));
            s.spawn(|| subscription_models(Rail::Codex, &codex, &f.cfg).unwrap());
            s.spawn(|| probe_all(&f.cfg));
        }
    });
    assert_eq!(f.file("overlap.claude"), "");
    assert_eq!(f.file("overlap.codex"), "");
    assert_eq!(f.file("overlap.cursor"), "");
}

#[test]
fn model_probes_run_in_home_with_the_credential_free_env() {
    let _serial = serial();
    // SAFETY: names are unique to this test binary's canaries; children only ever see the
    // allowlisted environment, whatever the ordering.
    unsafe {
        std::env::set_var("ANTHROPIC_API_KEY", "sk-canary-anthropic");
        std::env::set_var("OPENAI_API_KEY", "sk-canary-openai");
        std::env::set_var("CURSOR_API_KEY", "canary-cursor");
        std::env::set_var("WORKSHOP_CANARY", "canary-workshop");
    }
    let f = Fake::signed_in(&[]);
    let bin = f.bin(Vendor::Claude);
    subscription_models(Rail::Claude, &bin, &f.cfg).unwrap();
    assert_eq!(
        f.file("cwd.claude").trim(),
        f.cfg.home.as_ref().unwrap().to_string_lossy()
    );
    subscription_models(Rail::Codex, &f.bin(Vendor::Codex), &f.cfg).unwrap();
    subscription_models(Rail::Cursor, &f.bin(Vendor::Cursor), &f.cfg).unwrap();
    for v in ["claude", "codex", "cursor"] {
        let env = f.file(&format!("env.{v}"));
        assert!(env.contains("PATH="), "{v}");
        assert!(!env.contains("canary"), "{v} saw a credential:\n{env}");
    }
}
