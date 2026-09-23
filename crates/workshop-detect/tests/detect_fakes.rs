//! Conformance tests against fake vendor CLIs (`tests/fixtures`). The fakes reproduce the exact
//! `--version` / status output shapes of the real CLIs observed on 2026-09-21.

#![cfg(unix)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use workshop_detect::copy;
use workshop_detect::{
    DetectConfig, IdentifyError, LoginState, ModelsCache, Pill, Rail, RailModels, Refresh,
    SubscriptionModels, Vendor, composer_label, probe_all, probe_vendor, rails, rails_models,
};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn cfg_with(dirs: &[&str], state: &tempfile::TempDir, extra_env: &[(&str, &str)]) -> DetectConfig {
    let path = std::env::join_paths(dirs.iter().map(|d| fixtures().join(d))).unwrap();
    let mut cfg = DetectConfig::hermetic(path, state.path().join("home"));
    cfg.timeout = Duration::from_secs(10);
    cfg.extra_env.push((
        OsString::from("FAKE_CLI_STATE_DIR"),
        state.path().as_os_str().to_owned(),
    ));
    for (k, v) in extra_env {
        cfg.extra_env.push((OsString::from(k), OsString::from(v)));
    }
    cfg
}

fn calls(state: &tempfile::TempDir, vendor: &str) -> String {
    std::fs::read_to_string(state.path().join(format!("calls.{vendor}"))).unwrap_or_default()
}

#[test]
fn all_four_vendors_detected_and_signed_out() {
    let state = tempfile::tempdir().unwrap();
    let cfg = cfg_with(&["vendors"], &state, &[]);
    let probe = probe_all(&cfg);

    for vendor in Vendor::ALL {
        let vp = probe.get(vendor);
        let id = vp
            .binary
            .as_ref()
            .unwrap_or_else(|| panic!("{vendor:?} not detected"));
        assert_eq!(id.vendor, vendor);
        assert_eq!(vp.login, Some(LoginState::LoggedOut), "{vendor:?}");
        assert!(vp.rejected.is_empty(), "{vendor:?}: {:?}", vp.rejected);
    }
    assert_eq!(probe.claude.binary.as_ref().unwrap().version, "2.1.278");
    assert_eq!(probe.codex.binary.as_ref().unwrap().version, "0.155.1");
    assert_eq!(
        probe.cursor.binary.as_ref().unwrap().version,
        "2026.09.18-9a7762b"
    );
    assert_eq!(probe.opencode.binary.as_ref().unwrap().version, "1.18.31");
    // `cursor-agent` is preferred over `agent` when both exist in the same directory.
    assert!(
        probe
            .cursor
            .binary
            .as_ref()
            .unwrap()
            .path
            .ends_with("cursor-agent")
    );

    let rails = rails(&probe, |_| RailModels::Loading);
    assert_eq!(rails.each_ref().map(|r| r.rail), Rail::ALL);
    for r in &rails {
        assert_eq!(r.pill, Pill::SignIn, "{:?}", r.rail);
        assert!(r.installed);
        assert!(r.models.is_empty(), "signed-out rail shows no models");
        assert!(r.show_connect, "signed-out empty rail shows Connect");
    }
    assert_eq!(rails[0].empty_copy, Some(copy::SIGN_IN_CLAUDE));
    assert_eq!(rails[1].empty_copy, Some(copy::SIGN_IN_CODEX));
    assert_eq!(rails[2].empty_copy, Some(copy::SIGN_IN_CURSOR));

    // Only the documented commands were run: --version, --help (Cursor/OpenCode), status.
    assert_eq!(
        calls(&state, "claude"),
        "claude --version\nclaude auth status\n"
    );
    assert_eq!(
        calls(&state, "codex"),
        "codex --version\ncodex login status\n"
    );
    assert_eq!(
        calls(&state, "cursor"),
        "cursor --version\ncursor --help\ncursor status --format json\n"
    );
    assert_eq!(
        calls(&state, "opencode"),
        "opencode --version\nopencode --help\nopencode auth list\n"
    );
}

#[test]
fn signed_in_rails_are_ready_with_models() {
    let state = tempfile::tempdir().unwrap();
    let cfg = cfg_with(
        &["vendors"],
        &state,
        &[
            ("FAKE_LOGIN_CLAUDE", "in"),
            ("FAKE_LOGIN_CODEX", "in"),
            ("FAKE_LOGIN_CURSOR", "in"),
            ("FAKE_LOGIN_OPENCODE", "in"),
        ],
    );
    let probe = probe_all(&cfg);
    for vendor in Vendor::ALL {
        assert_eq!(
            probe.get(vendor).login,
            Some(LoginState::LoggedIn),
            "{vendor:?}"
        );
        assert!(probe.get(vendor).ready());
    }
    let cache = ModelsCache::new(state.path().join("catalog-cache"));
    let [claude, codex, cursor] = rails_models(
        &probe,
        &cfg,
        &cache,
        Refresh::Live {
            max_age: Duration::ZERO,
        },
    );
    let rails = rails(&probe, |rail| match rail {
        Rail::Claude => claude.clone(),
        Rail::Codex => codex.clone(),
        Rail::Cursor => cursor.clone(),
    });
    for r in &rails {
        assert_eq!(r.pill, Pill::Ready, "{:?}", r.rail);
        assert!(!r.models.is_empty());
        assert!(r.empty_copy.is_none());
        assert!(!r.show_connect, "rail with models never shows Connect");
    }
    // The CLIs' own lists, in their order with the default first, keyed provider:model.
    let claude_keys: Vec<String> = rails[0].models.iter().map(|m| m.key()).collect();
    assert_eq!(
        claude_keys,
        [
            "anthropic:default",
            "anthropic:opus[1m]",
            "anthropic:claude-fable-5-1",
            "anthropic:sonnet",
            "anthropic:haiku"
        ]
    );
    assert_eq!(
        composer_label(Rail::Claude, &rails[0].models[1]),
        "Claude · Opus (1M context)"
    );
    assert_eq!(rails[1].models[0].key(), "openai:gpt-6-astra");
    assert_eq!(
        composer_label(Rail::Cursor, &rails[2].models[0]),
        "Cursor · Composer 2.5"
    );
}

#[test]
fn ready_rail_with_no_models_shows_no_models_copy() {
    let state = tempfile::tempdir().unwrap();
    let cfg = cfg_with(
        &["vendors"],
        &state,
        &[("FAKE_LOGIN_CLAUDE", "in"), ("FAKE_LOGIN_CURSOR", "in")],
    );
    let probe = probe_all(&cfg);
    let empty = |rail| RailModels::Listed {
        list: SubscriptionModels {
            rail,
            models: Vec::new(),
            account: None,
            documented_aliases: false,
            fetched_at_secs: 0,
        },
        cached: false,
        error: None,
    };
    let rails = rails(&probe, empty);
    assert_eq!(rails[0].pill, Pill::Ready);
    assert_eq!(rails[0].empty_copy, Some(copy::NO_MODELS));
    assert!(
        rails[0].show_connect,
        "Claude ready-but-empty still offers Connect"
    );
    assert_eq!(rails[2].pill, Pill::Ready);
    assert_eq!(rails[2].empty_copy, Some(copy::NO_MODELS));
    assert!(
        !rails[2].show_connect,
        "Cursor ready-but-empty does not offer Connect"
    );
}

#[test]
fn impostor_agent_is_not_cursor() {
    let state = tempfile::tempdir().unwrap();
    // Impostor first on PATH, real vendor dir second: the real one must win.
    let cfg = cfg_with(&["impostors", "vendors"], &state, &[]);
    let vp = probe_vendor(Vendor::Cursor, &cfg);
    let id = vp
        .binary
        .as_ref()
        .expect("real cursor-agent found behind the impostor");
    assert!(id.path.starts_with(fixtures().join("vendors")));
    assert_eq!(vp.rejected.len(), 1);
    assert!(vp.rejected[0].path.ends_with("impostors/agent"));
    assert!(
        vp.rejected[0].reason.contains("not Cursor Agent"),
        "{}",
        vp.rejected[0].reason
    );

    // Impostor alone: Cursor is not installed.
    let cfg = cfg_with(&["impostors"], &state, &[]);
    let probe = probe_all(&cfg);
    assert!(probe.cursor.binary.is_none());
    assert_eq!(probe.cursor.login, None);
    let rails = rails(&probe, |_| RailModels::Loading);
    assert_eq!(rails[2].pill, Pill::SignIn);
    assert!(!rails[2].installed);
    assert_eq!(rails[2].empty_copy, Some(copy::CURSOR_DESKTOP_ONLY));
}

#[test]
fn cursor_app_without_cli_gets_install_agent_copy() {
    let state = tempfile::tempdir().unwrap();
    let app = state.path().join("Cursor.app");
    std::fs::create_dir_all(&app).unwrap();
    let mut cfg = cfg_with(&["impostors"], &state, &[]);
    cfg.cursor_app_paths = Some(vec![app]);
    let probe = probe_all(&cfg);
    assert!(probe.cursor.app_present);
    assert!(probe.cursor.binary.is_none());
    let rails = rails(&probe, |_| RailModels::Loading);
    assert_eq!(rails[2].empty_copy, Some(copy::CURSOR_APP_WITHOUT_CLI));
    assert!(rails[2].show_connect);
}

#[test]
fn wrong_claude_binary_means_not_installed() {
    let state = tempfile::tempdir().unwrap();
    let cfg = cfg_with(&["impostors"], &state, &[]);
    let probe = probe_all(&cfg);
    assert!(probe.claude.binary.is_none());
    assert_eq!(probe.claude.rejected.len(), 1);
    let rails = rails(&probe, |_| RailModels::Loading);
    assert_eq!(rails[0].empty_copy, Some(copy::INSTALL_CLAUDE));
    assert_eq!(rails[1].empty_copy, Some(copy::INSTALL_CODEX));
    assert!(rails[0].show_connect && rails[1].show_connect);
}

#[test]
fn broken_version_command_is_rejected_not_trusted() {
    let state = tempfile::tempdir().unwrap();
    let cfg = cfg_with(&["broken"], &state, &[]);
    let vp = probe_vendor(Vendor::Codex, &cfg);
    assert!(vp.binary.is_none());
    assert_eq!(vp.rejected.len(), 1);
    assert!(
        vp.rejected[0].reason.contains("exited with Some(2)"),
        "{}",
        vp.rejected[0].reason
    );
}

#[test]
fn hung_cli_times_out_instead_of_blocking() {
    let state = tempfile::tempdir().unwrap();
    let mut cfg = cfg_with(&["slow"], &state, &[]);
    cfg.timeout = Duration::from_millis(400);
    let started = std::time::Instant::now();
    let vp = probe_vendor(Vendor::OpenCode, &cfg);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "probe must respect the timeout"
    );
    assert!(vp.binary.is_none());
    let expected = IdentifyError::TimedOut {
        path: fixtures().join("slow/opencode"),
    }
    .to_string();
    assert_eq!(vp.rejected[0].reason, expected);
}

#[test]
fn garbage_status_output_is_unknown_and_shows_sign_in() {
    let state = tempfile::tempdir().unwrap();
    let cfg = cfg_with(
        &["vendors"],
        &state,
        &[
            ("FAKE_LOGIN_CLAUDE", "garbage"),
            ("FAKE_LOGIN_CODEX", "garbage"),
            ("FAKE_LOGIN_CURSOR", "garbage"),
            ("FAKE_LOGIN_OPENCODE", "garbage"),
        ],
    );
    let probe = probe_all(&cfg);
    for vendor in Vendor::ALL {
        assert!(
            matches!(probe.get(vendor).login, Some(LoginState::Unknown { .. })),
            "{vendor:?}: {:?}",
            probe.get(vendor).login
        );
        assert!(!probe.get(vendor).ready());
    }
    for r in rails(&probe, |_| RailModels::Loading) {
        assert_eq!(
            r.pill,
            Pill::SignIn,
            "unknown login state fails closed to Sign in"
        );
    }
}

#[test]
fn presence_only_scan_never_runs_status_commands() {
    let state = tempfile::tempdir().unwrap();
    let mut cfg = cfg_with(&["vendors"], &state, &[("FAKE_LOGIN_CLAUDE", "in")]);
    cfg.check_login = false;
    let probe = probe_all(&cfg);
    for vendor in Vendor::ALL {
        assert!(probe.get(vendor).installed());
        assert_eq!(probe.get(vendor).login, None);
    }
    for vendor in ["claude", "codex", "cursor", "opencode"] {
        let c = calls(&state, vendor);
        assert!(
            !c.contains("status") && !c.contains("auth list"),
            "{vendor}: {c}"
        );
    }
    for r in rails(&probe, |_| RailModels::Loading) {
        assert_eq!(r.pill, Pill::SignIn);
    }
}

#[test]
fn children_never_see_credentials_or_workshop_secrets() {
    let state = tempfile::tempdir().unwrap();
    // These may also be set by the developer's shell; either way they must not reach the child.
    // SAFETY: names are unique to this test; other tests only read the environment through
    // `minimal_env`, which filters these names regardless of ordering.
    unsafe {
        std::env::set_var("OPENAI_API_KEY", "sk-canary-openai");
        std::env::set_var("ANTHROPIC_API_KEY", "sk-canary-anthropic");
        std::env::set_var("CURSOR_API_KEY", "canary-cursor");
        std::env::set_var("CLAUDE_CODE_OAUTH_TOKEN", "canary-claude-oauth");
        std::env::set_var("WORKSHOP_CANARY", "canary-workshop");
        std::env::set_var("XAI_API_KEY", "canary-xai");
    }
    let cfg = cfg_with(&["vendors"], &state, &[]);
    let probe = probe_all(&cfg);
    assert!(probe.claude.installed());
    for vendor in ["claude", "codex", "cursor", "opencode"] {
        let env = std::fs::read_to_string(state.path().join(format!("env.{vendor}"))).unwrap();
        assert!(
            !env.contains("canary"),
            "{vendor} child saw a credential:\n{env}"
        );
        for forbidden in [
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "CURSOR_API_KEY",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "WORKSHOP_",
            "XAI_",
        ] {
            assert!(
                !env.contains(forbidden),
                "{vendor} child env contains {forbidden}:\n{env}"
            );
        }
        assert!(env.contains("PATH="), "{vendor} child still gets PATH");
        assert!(env.contains("NO_COLOR=1"));
    }
}

#[test]
fn extra_dirs_are_scanned_after_path() {
    let state = tempfile::tempdir().unwrap();
    let mut cfg = cfg_with(&["impostors"], &state, &[]);
    cfg.extra_dirs.push(fixtures().join("vendors"));
    let vp = probe_vendor(Vendor::Codex, &cfg);
    assert!(vp.binary.as_ref().unwrap().path.ends_with("vendors/codex"));
}
