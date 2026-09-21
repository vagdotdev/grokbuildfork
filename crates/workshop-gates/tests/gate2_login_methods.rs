//! Gate 2 — Login does not advertise `grok.com` (`gate:no-xai`).
//!
//! `build_auth_methods` for a brand-new user (no key, no cached session, no
//! enterprise IdP, no pin) is exactly what the pager's startup and Login paths
//! consume. It must not contain `GROK_COM_METHOD_ID`; the optional xAI card
//! advertises its labeled method only after the user chooses it.

use xai_grok_login::PreferredAuthMethod;
use xai_grok_shell::agent::auth_method::{
    AuthMethodsBuildInputs, GROK_COM_METHOD_ID, build_auth_methods,
};

fn fresh_user() -> AuthMethodsBuildInputs<'static> {
    AuthMethodsBuildInputs {
        has_external_api_key: false,
        has_cached_token: false,
        has_enterprise_oidc: false,
        enterprise_oidc_issuer: None,
        login_label: None,
        has_auth_provider_command: false,
        preferred_method: None,
    }
}

fn ids(inputs: AuthMethodsBuildInputs<'_>) -> Vec<String> {
    build_auth_methods(inputs)
        .methods
        .iter()
        .map(|m| m.id().0.to_string())
        .collect()
}

#[test]
fn fresh_user_is_not_offered_grok_com() {
    let ids = ids(fresh_user());
    assert!(
        !ids.iter().any(|id| id == GROK_COM_METHOD_ID),
        "cold start still advertises {GROK_COM_METHOD_ID}: {ids:?}"
    );
    assert!(
        !ids.is_empty(),
        "Workshop must offer an interactive method (the connection picker), not an empty list"
    );
}

#[test]
fn fresh_user_first_method_is_interactive_but_not_xai() {
    // The pager decides "show the login screen" from methods.first(); that
    // screen must exist (so the picker opens) without being grok.com.
    let built = build_auth_methods(fresh_user());
    let first = built.methods.first().expect("an interactive method");
    assert_ne!(first.id().0.as_ref(), GROK_COM_METHOD_ID);
    assert!(
        built.default_auth_method_id.is_none(),
        "no method may auto-run for a fresh user"
    );
}

#[test]
fn oidc_pin_without_enterprise_idp_does_not_fall_back_to_grok_com() {
    let ids = ids(AuthMethodsBuildInputs {
        preferred_method: Some(PreferredAuthMethod::Oidc),
        ..fresh_user()
    });
    assert!(
        !ids.iter().any(|id| id == GROK_COM_METHOD_ID),
        "preferred_method=oidc without GROK_OIDC_* still resolves to grok.com: {ids:?}"
    );
}

#[test]
fn api_key_kill_switch_without_session_does_not_offer_grok_com() {
    // Admin disabled API keys and there is no session: upstream sends this user
    // to grok.com. Workshop sends them to the picker.
    let ids = ids(AuthMethodsBuildInputs {
        has_external_api_key: false,
        ..fresh_user()
    });
    assert!(!ids.iter().any(|id| id == GROK_COM_METHOD_ID), "{ids:?}");
}

#[test]
fn cached_session_user_is_not_offered_grok_com_as_fallback() {
    let ids = ids(AuthMethodsBuildInputs {
        has_cached_token: true,
        ..fresh_user()
    });
    assert!(
        !ids.iter().any(|id| id == GROK_COM_METHOD_ID),
        "expired-session re-login still advertises grok.com: {ids:?}"
    );
}

#[test]
fn enterprise_idp_is_still_advertised() {
    // A customer IdP configured through GROK_OIDC_* is not xAI and keeps working.
    let ids = ids(AuthMethodsBuildInputs {
        has_enterprise_oidc: true,
        enterprise_oidc_issuer: Some("https://sso.example.com"),
        ..fresh_user()
    });
    assert_eq!(ids, vec!["oidc".to_string()]);
}
