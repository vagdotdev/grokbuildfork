//! ACP auth-method identity for Workshop's login surface.

/// Interactive ACP auth method Workshop advertises instead of `grok.com`.
/// The pager opens the connection picker for it; the agent never runs an
/// OIDC flow for it (it fails closed with [`NO_CONNECTION_ERROR`]).
pub const WORKSHOP_CONNECT_METHOD_ID: &str = "workshop.connect";

/// Display name of [`WORKSHOP_CONNECT_METHOD_ID`].
pub const WORKSHOP_CONNECT_METHOD_NAME: &str = "Connect a model or subscription";

/// Description of [`WORKSHOP_CONNECT_METHOD_ID`].
pub const WORKSHOP_CONNECT_METHOD_DESCRIPTION: &str =
    "Local model, API key, or an installed coding-subscription CLI";

/// Error the agent returns when `workshop.connect` (or a dead cached session
/// with no API key) reaches `authenticate`.
pub const NO_CONNECTION_ERROR: &str = workshop_branding::NO_CONNECTION_HINT;

/// Placeholder OAuth2 issuer baked into `GrokComConfig::default()` when no
/// `GROK_OAUTH2_*` / `GROK_OIDC_*` override is set and the user has not opted
/// in to xAI. `.invalid` never resolves, so an accidental discovery request
/// fails at DNS instead of reaching `auth.x.ai`.
pub const PLACEHOLDER_OAUTH2_ISSUER: &str = "https://auth.workshop.invalid";

/// Client id paired with [`PLACEHOLDER_OAUTH2_ISSUER`].
pub const PLACEHOLDER_OAUTH2_CLIENT_ID: &str = "workshop-no-default-issuer";

/// `true` when `issuer` is the Workshop placeholder, i.e. no real sign-in
/// provider is configured.
pub fn is_placeholder_issuer(issuer: &str) -> bool {
    issuer.trim_end_matches('/') == PLACEHOLDER_OAUTH2_ISSUER
}

/// Error shown when an interactive OIDC flow is requested against the placeholder issuer.
pub const PLACEHOLDER_ISSUER_ERROR: &str = "No sign-in provider is configured. Workshop does not sign in to xAI by default; choose the optional xAI card in the connection picker if you want that.";

/// Label of the optional xAI card (last on the Models tab, never preselected).
pub const XAI_OPTIONAL_LABEL: &str = "xAI (optional)";

/// One-line copy under the optional xAI card.
pub const XAI_OPTIONAL_COPY: &str = "Uses xAI accounts and auth.x.ai. Not required.";

/// Whether a Login / `/login` / welcome `l` request should open the connection
/// picker instead of starting the advertised interactive method directly.
///
/// The only methods that bypass the picker are a customer-configured
/// enterprise IdP (`oidc`, from `GROK_OIDC_*`) and an external auth-provider
/// command (`external_provider` meta), because both are explicit deployment
/// configuration and neither is xAI.
pub fn login_opens_picker(method_id: Option<&str>, external_provider_command: bool) -> bool {
    if external_provider_command {
        return false;
    }
    !matches!(method_id, Some("oidc"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_is_the_default_login_surface() {
        assert!(login_opens_picker(None, false));
        assert!(login_opens_picker(Some(WORKSHOP_CONNECT_METHOD_ID), false));
        assert!(login_opens_picker(Some("grok.com"), false));
        assert!(login_opens_picker(Some("cached_token"), false));
    }

    #[test]
    fn enterprise_idp_and_external_command_bypass_the_picker() {
        assert!(!login_opens_picker(Some("oidc"), false));
        assert!(!login_opens_picker(Some("grok.com"), true));
    }

    #[test]
    fn placeholder_issuer_is_recognised_with_or_without_slash() {
        assert!(is_placeholder_issuer(PLACEHOLDER_OAUTH2_ISSUER));
        assert!(is_placeholder_issuer("https://auth.workshop.invalid/"));
        assert!(!is_placeholder_issuer("https://auth.x.ai"));
    }
}
