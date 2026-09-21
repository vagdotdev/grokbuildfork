//! Gate 6 — identity and dead inherited services (milestone B).
//!
//! Home is `~/.workshop` / `$WORKSHOP_HOME`; telemetry has no baked events
//! URL, API key, or Mixpanel token and is disabled unless the user enables it.

use workshop_gates::HermeticEnv;

#[test]
fn default_home_is_dot_workshop() {
    let home = xai_dirs::default_grok_home();
    assert!(
        home.ends_with(workshop_branding::HOME_DIR_NAME),
        "default home is {} (expected …/{})",
        home.display(),
        workshop_branding::HOME_DIR_NAME
    );
    assert!(
        !home.ends_with(".grok") && !home.ends_with(".docking"),
        "default home still uses an inherited directory name: {}",
        home.display()
    );
}

#[test]
fn workshop_home_env_overrides_home() {
    let mut env = HermeticEnv::production();
    env.remove("GROK_HOME");
    env.set("WORKSHOP_HOME", "/tmp/workshop-gate-home");
    assert_eq!(
        xai_dirs::resolve_grok_home(),
        Some(std::path::PathBuf::from("/tmp/workshop-gate-home")),
        "$WORKSHOP_HOME is not honoured"
    );
}

#[test]
fn telemetry_has_no_baked_sinks() {
    let cfg = xai_grok_telemetry::config::TelemetryConfig::default();
    assert_eq!(cfg.events_url, None, "events URL baked into the build");
    assert_eq!(cfg.events_api_key, None, "events API key baked into the build");
    assert_eq!(cfg.mixpanel_token, None, "Mixpanel token baked into the build");
    assert!(!cfg.mixpanel_enabled, "Mixpanel enabled by default");
    assert_ne!(cfg.enabled, Some(true), "telemetry enabled by default");
}
