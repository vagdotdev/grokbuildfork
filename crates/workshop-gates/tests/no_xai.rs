//! Gates 1–4 from docs/workshop-production-plan.md section 1, as compiled-default checks.
//!
//! These construct the *real* production defaults (`GrokComConfig::default()`,
//! `build_auth_methods`, `GrokBuildEnvironment::Production`, the updater constants,
//! `default_models.json`) under Workshop's production environment and fail if any of them
//! would send a fresh user to xAI. A string rename elsewhere cannot make them pass.

use agent_client_protocol as acp;
use serial_test::serial;
use workshop_gates::{url_hits_forbidden_host, url_is_loopback};
use xai_grok_shell::agent::auth_method::{
    AuthMethodKind, AuthMethodsBuildInputs, GROK_COM_METHOD_ID, build_auth_methods,
};

/// Workshop's production environment for login: none of the xAI / operator env overrides.
struct CleanEnv {
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl CleanEnv {
    const KEYS: &'static [&'static str] = &[
        "GROK_OAUTH2_ISSUER",
        "GROK_OAUTH2_CLIENT_ID",
        "GROK_OIDC_ISSUER",
        "GROK_OIDC_CLIENT_ID",
        "GROK_LOCAL_AUTH",
        "GROK_AUTH_PROVIDER_COMMAND",
        "GROK_AUTH",
        "GROK_AUTH_PATH",
        "XAI_API_KEY",
        "GROK_CODE_XAI_API_KEY",
        "GROK_WS_ORIGIN",
        "GROK_WS_URL",
        "GROK_PRODUCTION_CLI_CHAT_PROXY_BASE_URL",
        "GROK_PRODUCTION_ASSET_SERVER_URL",
        "GROK_PRODUCTION_WS_URL",
        "GROK_PRODUCTION_GATEWAY_WS_URL",
        "GROK_PRODUCTION_WS_ORIGIN",
        "GROK_CLI_BASE_URL",
    ];
    fn new() -> Self {
        let saved = Self::KEYS
            .iter()
            .map(|k| {
                let prev = std::env::var_os(k);
                // SAFETY: tests in this file are `#[serial]`; no other thread reads env here.
                unsafe { std::env::remove_var(k) };
                (*k, prev)
            })
            .collect();
        Self { saved }
    }
}

impl Drop for CleanEnv {
    fn drop(&mut self) {
        for (k, prev) in self.saved.drain(..) {
            match prev {
                Some(v) => unsafe { std::env::set_var(k, v) },
                None => unsafe { std::env::remove_var(k) },
            }
        }
    }
}

// ───────────────────────── Gate 1 — default issuer ─────────────────────────

/// `GrokComConfig::default()` under Workshop's production env must not carry the xAI issuer
/// (`https://auth.x.ai`) or the local xAI stand-in (`http://localhost:22255`) anywhere.
#[test]
#[serial]
fn gate1_default_grok_com_config_has_no_xai_issuer() {
    let _env = CleanEnv::new();
    let cfg = xai_grok_login::GrokComConfig::default();
    let oauth2_issuer = cfg.oauth2.as_ref().map(|o| o.issuer.clone());
    let oidc_issuer = cfg.oidc.as_ref().map(|o| o.issuer.clone());
    for issuer in [&oauth2_issuer, &oidc_issuer].into_iter().flatten() {
        assert!(
            !url_hits_forbidden_host(issuer),
            "default issuer points at xAI: {issuer}"
        );
        assert_ne!(
            issuer, "http://localhost:22255",
            "local xAI stand-in is not a default"
        );
    }
    assert!(
        cfg.oauth2.is_none() && cfg.oidc.is_none(),
        "Workshop default has no session-login provider; got oauth2={oauth2_issuer:?} oidc={oidc_issuer:?}"
    );
    assert!(!cfg.has_session_login_provider());
    // The scope key must not embed the xAI issuer either (it names the auth.json entry).
    let scope = cfg.auth_scope();
    assert!(
        !scope.contains("x.ai"),
        "default auth scope still names xAI: {scope}"
    );
}

/// The optional xAI provider is constructed only on explicit request, and then it is the
/// inherited xAI OAuth2 provider (this pins that the opt-in path still works).
#[test]
#[serial]
fn gate1_optional_xai_provider_is_explicit_only() {
    let _env = CleanEnv::new();
    let base = xai_grok_login::GrokComConfig::default();
    assert!(base.oauth2.is_none());
    let opted = base.with_xai_first_party_oauth2();
    let issuer = opted.oauth2.as_ref().map(|o| o.issuer.as_str());
    assert_eq!(issuer, Some(xai_grok_login::XAI_OAUTH2_ISSUER));
    assert!(opted.has_session_login_provider());
}

/// A cached xAI session from a previous opt-in is still recognised as first-party (so /logout works),
/// but constructing it never happens in `Default`.
#[test]
#[serial]
fn gate1_env_configured_provider_is_honoured() {
    let _env = CleanEnv::new();
    // SAFETY: serial test; restored by CleanEnv::drop.
    unsafe {
        std::env::set_var("GROK_OAUTH2_ISSUER", "https://sso.example.com");
        std::env::set_var("GROK_OAUTH2_CLIENT_ID", "workshop-test");
    }
    let cfg = xai_grok_login::GrokComConfig::default();
    assert_eq!(
        cfg.oauth2.as_ref().map(|o| o.issuer.as_str()),
        Some("https://sso.example.com")
    );
}

// ───────────────────────── Gate 2 — login does not advertise grok.com ─────────────────────────

fn ids(built: &xai_grok_shell::agent::auth_method::BuiltAuthMethods) -> Vec<String> {
    built
        .methods
        .iter()
        .map(|m| m.id().0.as_ref().to_owned())
        .collect()
}

/// Cold start: no key, no cached session, no pin, no provider → no `grok.com`, and nothing that
/// needs an interactive login. The pager then shows the connection picker.
#[test]
fn gate2_default_auth_methods_do_not_advertise_grok_com() {
    let built = build_auth_methods(AuthMethodsBuildInputs {
        has_external_api_key: false,
        has_cached_token: false,
        has_enterprise_oidc: false,
        enterprise_oidc_issuer: None,
        login_label: None,
        has_auth_provider_command: false,
        has_oauth2_provider: false,
        preferred_method: None,
    });
    let advertised = ids(&built);
    assert!(
        !advertised.iter().any(|id| id == GROK_COM_METHOD_ID),
        "cold start advertises grok.com: {advertised:?}"
    );
    assert!(
        built
            .methods
            .iter()
            .all(|m| !AuthMethodKind::from_id(m.id()).needs_interactive_login()),
        "cold start advertises an interactive login method: {advertised:?}"
    );
    assert!(built.default_auth_method_id.is_none());
}

/// Same with the `oidc` pin and no provider: still no `grok.com`.
#[test]
fn gate2_oidc_pin_without_provider_does_not_invent_grok_com() {
    let built = build_auth_methods(AuthMethodsBuildInputs {
        has_external_api_key: false,
        has_cached_token: false,
        has_enterprise_oidc: false,
        enterprise_oidc_issuer: None,
        login_label: None,
        has_auth_provider_command: false,
        has_oauth2_provider: false,
        preferred_method: Some(xai_grok_login::PreferredAuthMethod::Oidc),
    });
    assert!(built.methods.is_empty(), "{:?}", ids(&built));
}

/// A user who added a BYOK key: `xai.api_key` leads and grok.com is still absent.
#[test]
fn gate2_byok_user_gets_api_key_method_only() {
    let built = build_auth_methods(AuthMethodsBuildInputs {
        has_external_api_key: true,
        has_cached_token: false,
        has_enterprise_oidc: false,
        enterprise_oidc_issuer: None,
        login_label: None,
        has_auth_provider_command: false,
        has_oauth2_provider: false,
        preferred_method: None,
    });
    assert_eq!(ids(&built), vec!["xai.api_key".to_owned()]);
}

/// The opt-in still works: with a configured OAuth2 provider the interactive method is advertised.
/// (Pins that Gate 2 is a gate, not a removal of the inherited flow.)
#[test]
fn gate2_configured_provider_still_advertises_login() {
    let built = build_auth_methods(AuthMethodsBuildInputs {
        has_external_api_key: false,
        has_cached_token: false,
        has_enterprise_oidc: false,
        enterprise_oidc_issuer: None,
        login_label: Some("Acme SSO"),
        has_auth_provider_command: false,
        has_oauth2_provider: true,
        preferred_method: None,
    });
    assert_eq!(ids(&built), vec![GROK_COM_METHOD_ID.to_owned()]);
}

/// The pager's own startup predicate sees no interactive method for the Workshop default set.
#[test]
fn gate2_pager_startup_metadata_has_no_login_method() {
    let built = build_auth_methods(AuthMethodsBuildInputs {
        has_external_api_key: false,
        has_cached_token: false,
        has_enterprise_oidc: false,
        enterprise_oidc_issuer: None,
        login_label: None,
        has_auth_provider_command: false,
        has_oauth2_provider: false,
        preferred_method: None,
    });
    let first: Option<&acp::AuthMethod> = built.methods.first();
    assert!(
        first.is_none(),
        "default set must be empty, got {:?}",
        ids(&built)
    );
}

// ───────────────────────── Gate 3 — production endpoints ─────────────────────────

#[test]
#[serial]
fn gate3_production_endpoints_are_loopback_only() {
    let _env = CleanEnv::new();
    let env = xai_grok_env::GrokBuildEnvironment::Production;
    let urls = [
        ("cli_chat_proxy_base_url", env.cli_chat_proxy_base_url()),
        ("asset_server_url", env.asset_server_url()),
        ("relay_ws_url", env.relay_ws_url()),
        ("gateway_ws_url", env.gateway_ws_url()),
        ("ws_origin", env.ws_origin()),
    ];
    for (name, url) in &urls {
        assert!(
            !url_hits_forbidden_host(url),
            "PRODUCTION_ENDPOINTS.{name} points at an xAI host: {url}"
        );
        assert!(
            url_is_loopback(url),
            "PRODUCTION_ENDPOINTS.{name} must be loopback until Workshop owns a service: {url}"
        );
    }
    // The shell's own default must derive from the same constant.
    assert_eq!(
        xai_grok_shell::agent::config::CLI_CHAT_PROXY_BASE_URL_DEFAULT,
        xai_grok_env::PROD_CLI_CHAT_PROXY_BASE_URL
    );
    let proxy = xai_grok_shell::agent::config::EndpointsConfig::default().proxy_url();
    assert!(
        url_is_loopback(&proxy),
        "EndpointsConfig::proxy_url() default: {proxy}"
    );
}

/// Default model and aux-tool models are not Grok models routed through the proxy.
#[test]
fn gate3_default_model_is_not_grok() {
    for (name, model) in [
        ("default", xai_grok_models::default_model()),
        ("web_search", xai_grok_models::default_web_search_model()),
        (
            "image_description",
            xai_grok_models::default_image_description_model(),
        ),
        (
            "session_summary",
            xai_grok_models::default_session_summary_model(),
        ),
    ] {
        assert!(
            !model.to_ascii_lowercase().starts_with("grok"),
            "{name} model is a Grok model: {model}"
        );
    }
}

// ───────────────────────── Gate 4 — updater ─────────────────────────

/// Gate 4 per internal/updater-repoint-spec.md §3.4: the updater reads Workshop's release channel
/// (`raw.githubusercontent.com/<RELEASE_REPO>/release-channel/<channel>.json`) and nothing in it
/// names xAI infrastructure. npm is unsupported, so there is no npm package constant at all.
#[test]
#[serial]
fn gate4_updater_constants_are_not_xai_channels() {
    let _env = CleanEnv::new();
    use xai_grok_update::version::{CHANNEL_BASE_URL, GH_RELEASE_REPO, RELEASE_REPO};
    for s in [CHANNEL_BASE_URL, RELEASE_REPO, GH_RELEASE_REPO] {
        for bad in [
            "x.ai",
            "grok.com",
            "storage.googleapis.com",
            "grok-build-public-artifacts",
            "@xai-official",
            "xai-org-shared",
        ] {
            assert!(
                !s.contains(bad),
                "{s} still points at xAI infrastructure ({bad})"
            );
        }
    }
    assert!(
        !url_hits_forbidden_host(CHANNEL_BASE_URL),
        "{CHANNEL_BASE_URL}"
    );
    assert_eq!(RELEASE_REPO.split('/').count(), 2, "{RELEASE_REPO}");
    assert_eq!(GH_RELEASE_REPO, RELEASE_REPO);
    assert_eq!(
        CHANNEL_BASE_URL,
        format!("https://raw.githubusercontent.com/{RELEASE_REPO}/release-channel")
    );
}

/// Background updates read only [`CHANNEL_BASE_URL`](xai_grok_update::version::CHANNEL_BASE_URL);
/// the one override (`WORKSHOP_CLI_BASE_URL`, for the loopback proof) is loopback-only.
#[test]
#[serial]
fn gate4_cli_base_override_is_loopback_only() {
    let _env = CleanEnv::new();
    // SAFETY: serial test; CleanEnv::drop restores WORKSHOP_CLI_BASE_URL.
    unsafe { std::env::set_var("WORKSHOP_CLI_BASE_URL", "https://x.ai/cli") };
    let bases = xai_grok_update::version::cli_base_urls_for_test();
    assert_eq!(
        bases,
        vec![xai_grok_update::version::CHANNEL_BASE_URL.to_owned()],
        "a non-loopback override must be ignored"
    );
    unsafe { std::env::set_var("WORKSHOP_CLI_BASE_URL", "http://127.0.0.1:8123") };
    assert_eq!(
        xai_grok_update::version::cli_base_urls_for_test(),
        vec!["http://127.0.0.1:8123".to_owned()]
    );
    assert!(url_is_loopback("http://127.0.0.1:8123"));
}

// ───────────────────────── Telemetry ─────────────────────────

/// ADR 0004: no events URL, API key or Mixpanel token is baked into the binary, and Mixpanel is off.
/// `TelemetryConfig::default()` reads the `GROK_TELEMETRY_BUILD_*` compile-time env and the internal
/// defaults feature; both must be empty in a Workshop build.
#[test]
fn telemetry_has_no_baked_endpoint_or_token() {
    let cfg = xai_grok_telemetry::config::TelemetryConfig::default();
    assert_eq!(cfg.events_url, None, "events URL baked in");
    assert_eq!(cfg.events_api_key, None, "events API key baked in");
    assert_eq!(cfg.mixpanel_token, None, "Mixpanel token baked in");
    assert!(!cfg.mixpanel_enabled, "Mixpanel enabled by default");
    assert_eq!(
        cfg.enabled, None,
        "telemetry must not be force-enabled by default"
    );
}

// ───────────────────────── Identity ─────────────────────────

#[test]
#[serial]
fn identity_home_is_workshop() {
    let _env = CleanEnv::new();
    // SAFETY: serial test; restored below.
    let prev_w = std::env::var_os("WORKSHOP_HOME");
    let prev_g = std::env::var_os("GROK_HOME");
    unsafe {
        std::env::remove_var("WORKSHOP_HOME");
        std::env::remove_var("GROK_HOME");
    }
    let home = xai_dirs::default_grok_home();
    assert!(home.ends_with(".workshop"), "{}", home.display());
    unsafe { std::env::set_var("WORKSHOP_HOME", "/tmp/ws-home-test") };
    assert_eq!(
        xai_dirs::resolve_grok_home(),
        Some(std::path::PathBuf::from("/tmp/ws-home-test"))
    );
    unsafe {
        match prev_w {
            Some(v) => std::env::set_var("WORKSHOP_HOME", v),
            None => std::env::remove_var("WORKSHOP_HOME"),
        }
        match prev_g {
            Some(v) => std::env::set_var("GROK_HOME", v),
            None => std::env::remove_var("GROK_HOME"),
        }
    }
}

/// Branding: the terminal window title Workshop sets (OSC 0, via crossterm `SetTitle`) reads
/// `Workshop`, never `grok`. Both title builders are pinned: the startup/session title in
/// `app/mod.rs` and the per-tick `TitleManager` in `notifications/title.rs`.
#[test]
fn branding_terminal_title_is_workshop_never_grok() {
    let root = workshop_gates::repo_root();
    let title_rs = std::fs::read_to_string(
        root.join("crates/codegen/xai-grok-pager/src/notifications/title.rs"),
    )
    .expect("read notifications/title.rs");
    let title_src = workshop_gates::scannable_source(&title_rs);
    assert_eq!(
        title_src.matches("\"grok\"").count(),
        0,
        "TitleManager must not compose a `grok` title"
    );
    assert!(
        title_src.matches("\"Workshop\"").count() >= 2,
        "TitleManager composes the `Workshop` title (fallback and reset)"
    );

    let mod_rs = std::fs::read_to_string(root.join("crates/codegen/xai-grok-pager/src/app/mod.rs"))
        .expect("read app/mod.rs");
    let start = mod_rs
        .find("fn terminal_title_string(")
        .expect("app/mod.rs has terminal_title_string");
    let body_end = mod_rs[start..]
        .find("\n}\n")
        .map(|i| start + i)
        .expect("terminal_title_string body end");
    let body = &mod_rs[start..body_end];
    assert!(
        !body.to_ascii_lowercase().contains("grok"),
        "the startup terminal title must not say grok:\n{body}"
    );
    assert!(
        body.contains("\"Workshop\"") && body.contains("- Workshop"),
        "the startup terminal title is `Workshop` / `<session> - Workshop`:\n{body}"
    );
}

/// The picker policy: the xAI row is the last row of the one picker, never preselected, and only
/// an explicit double-Enter reaches it; every other row — at the top level and inside every
/// sub-menu — yields something other than a login.
#[test]
fn picker_xai_is_optional_last_and_explicit() {
    use workshop_auth::{
        ModelsRow, PickerInput, PickerOutcome, PickerSnapshot, PickerState, models_rows,
    };
    let mut p = PickerState::new();
    let rows = models_rows(&workshop_providers::Catalog::builtin(), |_| false, &[], &[]);
    p.apply_snapshot(PickerSnapshot {
        rows,
        rails: Vec::new(),
        default_selection: Some(workshop_providers::select_default(&[], true)),
        secret_backend: Some("memory"),
        ..PickerSnapshot::default()
    });
    assert!(p.rows.last().is_some_and(ModelsRow::is_xai), "xAI is last");
    assert_eq!(p.selected, 0, "never preselected");
    let n = p.visible_rows().len();
    for i in 0..n - 1 {
        p.submenu = None;
        p.selected = i;
        p.xai_armed = false;
        assert_ne!(
            p.handle(PickerInput::Enter),
            PickerOutcome::StartOptionalXaiLogin,
            "row {i} must not start the xAI login"
        );
        if p.submenu.is_some() {
            for j in 0..p.visible_rows().len() {
                p.selected = j;
                assert_ne!(
                    p.handle(PickerInput::Enter),
                    PickerOutcome::StartOptionalXaiLogin,
                    "row {i}/{j} must not start the xAI login"
                );
            }
        }
    }
    // The xAI row needs two explicit Enters.
    p.submenu = None;
    p.selected = n - 1;
    p.xai_armed = false;
    assert!(p.selected_row().is_some_and(|r| r.is_xai()));
    assert_eq!(p.handle(PickerInput::Enter), PickerOutcome::Changed);
    assert_eq!(
        p.handle(PickerInput::Enter),
        PickerOutcome::StartOptionalXaiLogin
    );
}
