//! Gate 1 — default issuer (`gate:no-xai`).
//!
//! `GrokComConfig::default()` in a production-shaped environment (no
//! `GROK_OAUTH2_*` / `GROK_OIDC_*`, no `GROK_LOCAL_AUTH`, empty Workshop home)
//! must not target `https://auth.x.ai` or the local xAI stand-in. Only the
//! explicit xAI opt-in marker may construct the xAI issuer.

use workshop_gates::{HermeticEnv, url_has_forbidden_host};
use xai_grok_login::GrokComConfig;

const XAI_PRODUCTION_ISSUER: &str = "https://auth.x.ai";
const XAI_LOCAL_ISSUER: &str = "http://localhost:22255";

fn default_issuer(cfg: &GrokComConfig) -> Option<String> {
    cfg.oidc
        .as_ref()
        .map(|o| o.issuer.clone())
        .or_else(|| cfg.oauth2.as_ref().map(|o| o.issuer.clone()))
}

#[test]
fn default_issuer_is_not_xai() {
    let _env = HermeticEnv::production();
    let cfg = GrokComConfig::default();
    let issuer = default_issuer(&cfg).expect("a default sign-in config must exist");
    assert_ne!(
        issuer, XAI_PRODUCTION_ISSUER,
        "GrokComConfig::default() still points cold-start login at auth.x.ai"
    );
    assert_ne!(issuer, XAI_LOCAL_ISSUER);
    assert!(
        !url_has_forbidden_host(&issuer),
        "default issuer {issuer} is an xAI host"
    );
}

#[test]
fn default_client_id_is_not_the_xai_oauth_client() {
    let _env = HermeticEnv::production();
    let cfg = GrokComConfig::default();
    let client_id = cfg
        .oauth2
        .as_ref()
        .map(|o| o.client_id.clone())
        .unwrap_or_default();
    assert_ne!(
        client_id, "b1a00492-073a-47ea-816f-4c329264a828",
        "default config still carries the xAI OAuth client id"
    );
}

#[test]
fn default_auth_scope_is_not_an_xai_scope() {
    let _env = HermeticEnv::production();
    let scope = GrokComConfig::default().auth_scope();
    assert!(
        !scope.contains("x.ai"),
        "auth.json scope {scope} would look up / store an xAI session by default"
    );
}

#[test]
fn explicit_env_override_still_wins() {
    // Enterprise deployments keep working: an explicit GROK_OAUTH2_* issuer is
    // honoured verbatim. This is not an xAI default, it is operator config.
    let mut env = HermeticEnv::production();
    env.set("GROK_OAUTH2_ISSUER", "https://sso.example.com");
    env.set("GROK_OAUTH2_CLIENT_ID", "example-client");
    let cfg = GrokComConfig::default();
    assert_eq!(
        default_issuer(&cfg).as_deref(),
        Some("https://sso.example.com")
    );
}
